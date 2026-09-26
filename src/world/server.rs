//! The Senate's local-only HTTP server: bound to `127.0.0.1` only, guarded
//! against DNS rebinding by an exact `Host` check, and against cross-site
//! reads/writes by a per-run token and an `Origin` check on mutations. Every
//! response is built from an in-memory embedded-asset table or the mission
//! projection; nothing here reads the filesystem at request time.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cli::WorldArgs;
use crate::domain::MissionId;

use super::{assets, projection};

/// Body cap for any request; the Senate's API bodies are small JSON, never
/// uploads.
const MAX_BODY_BYTES: usize = 64 * 1024;
/// Per-connection read timeout: this is a local, single-user tool, not a
/// service that must tolerate slow clients.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the single-instance probe waits for an existing server to
/// answer before deciding to start a new one.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// How long the accept loop sleeps between polls of the shutdown flag.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Serialize, Deserialize)]
struct InstanceState {
    pid: u32,
    port: u16,
    token: String,
}

/// Opens the Senate: reuses a healthy running instance, or binds a fresh
/// one and blocks until Ctrl-C/SIGTERM.
///
/// # Errors
/// Returns an error when the data directory, the state file, the listener,
/// or the shutdown-signal handlers cannot be set up.
pub fn run(args: &WorldArgs) -> anyhow::Result<()> {
    let state_path = crate::store::world_state_file()?;

    if let Some(existing) = existing_instance(&state_path) {
        let url = browser_url(
            existing.port,
            &existing.token,
            args.demo.as_deref(),
            args.mission,
        );
        if !args.no_open {
            open_browser(&url);
        }
        println!("The Senate is open at {url}  (ctrl-c to close; agents keep working)");
        return Ok(());
    }

    let listener = TcpListener::bind(("127.0.0.1", args.port))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let token = generate_token()?;

    let state = InstanceState {
        pid: std::process::id(),
        port,
        token: token.clone(),
    };
    write_state_file(&state_path, &state)?;

    let shutdown = Arc::new(AtomicBool::new(false));
    if let Err(error) = register_shutdown_signals(&shutdown) {
        let _ = std::fs::remove_file(&state_path);
        return Err(error.into());
    }

    let url = browser_url(port, &token, args.demo.as_deref(), args.mission);
    if !args.no_open {
        open_browser(&url);
    }
    println!("The Senate is open at {url}  (ctrl-c to close; agents keep working)");

    let mission = args.mission;
    let token = Arc::new(token);
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let token = Arc::clone(&token);
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, port, &token, mission);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => std::thread::sleep(ACCEPT_POLL_INTERVAL),
        }
    }

    let _ = std::fs::remove_file(&state_path);
    Ok(())
}

fn register_shutdown_signals(flag: &Arc<AtomicBool>) -> std::io::Result<()> {
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(flag))?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(flag))?;
    Ok(())
}

fn generate_token() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// The tab's URL. A named mission rides in the query so a running server,
/// whose own fallback mission was fixed when it started, still shows the
/// one this `W` asked for.
fn browser_url(port: u16, token: &str, demo: Option<&str>, mission: Option<MissionId>) -> String {
    let mut params = Vec::new();
    if let Some(scenario) = demo {
        params.push(format!("demo={scenario}"));
    }
    if let Some(mission) = mission {
        params.push(format!("mission={mission}"));
    }
    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    format!("http://127.0.0.1:{port}/{query}#token={token}")
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// An already-running instance whose recorded port and token still answer
/// `/api/health`, read from the marker file this server writes on start.
fn existing_instance(state_path: &Path) -> Option<InstanceState> {
    let contents = std::fs::read(state_path).ok()?;
    let state: InstanceState = serde_json::from_slice(&contents).ok()?;
    probe_health(&state).then_some(state)
}

fn probe_health(state: &InstanceState) -> bool {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HEALTH_PROBE_TIMEOUT))
        .build()
        .into();
    let url = format!("http://127.0.0.1:{}/api/health", state.port);
    agent
        .get(url)
        .header("X-Senate-Token", state.token.as_str())
        .call()
        .is_ok()
}

#[cfg(unix)]
fn write_state_file(path: &Path, state: &InstanceState) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let bytes = serde_json::to_vec(state)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_state_file(path: &Path, state: &InstanceState) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(state)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The campaign a tab asked for with `?mission=<id>`, if it names one.
    fn mission(&self) -> Option<MissionId> {
        let query = self.path.split_once('?')?.1.split('#').next()?;
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("mission="))
            .and_then(|value| value.parse().ok())
    }

    /// The path with any query string or fragment removed.
    fn route_path(&self) -> &str {
        self.path
            .split(['?', '#'])
            .next()
            .unwrap_or(self.path.as_str())
    }
}

struct HttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    cache_no_store: bool,
    content_security_policy: bool,
}

impl HttpResponse {
    fn json(status: u16, value: &serde_json::Value) -> Self {
        let body = serde_json::to_vec(value)
            .unwrap_or_else(|_| b"{\"error\":\"internal error\"}".to_vec());
        Self {
            status,
            content_type: "application/json",
            body,
            cache_no_store: true,
            content_security_policy: false,
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &serde_json::json!({ "error": message }))
    }

    fn asset(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        let is_html = content_type.starts_with("text/html");
        Self {
            status,
            content_type,
            body,
            cache_no_store: false,
            content_security_policy: is_html,
        }
    }
}

fn handle_connection(
    stream: TcpStream,
    port: u16,
    token: &str,
    mission: Option<MissionId>,
) -> std::io::Result<()> {
    // Accepted sockets inherit the listener's non-blocking mode on macOS and
    // the BSDs; a large asset would then be cut off at the first full send
    // buffer. Each connection has its own thread, so block.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let request = match read_request(&mut reader) {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(()),
        Err(_) => {
            return write_response(&mut writer, &HttpResponse::error(400, "malformed request"));
        }
    };

    let response = route(&request, port, token, mission);
    write_response(&mut writer, &response)
}

fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<HttpRequest>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    if method.is_empty() || path.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty request line",
        ));
    }

    let mut headers = Vec::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_owned();
            let value = value.trim().to_owned();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    if content_length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request body exceeds the 64 KiB cap",
        ));
    }
    let mut body = vec![0_u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(HttpRequest {
        method,
        path,
        headers,
        body,
    }))
}

fn route(
    request: &HttpRequest,
    port: u16,
    token: &str,
    mission: Option<MissionId>,
) -> HttpResponse {
    let Some(host) = request.header("host") else {
        return HttpResponse::error(403, "Host header is required");
    };
    if !host_matches(host, port) {
        return HttpResponse::error(403, "Host header does not match this server");
    }

    let path = request.route_path();
    if let Some(api_path) = path.strip_prefix("/api") {
        return route_api(request, api_path, port, token, mission);
    }

    if request.method != "GET" {
        return HttpResponse::error(404, "not found");
    }
    route_asset(path)
}

fn route_api(
    request: &HttpRequest,
    api_path: &str,
    port: u16,
    token: &str,
    mission: Option<MissionId>,
) -> HttpResponse {
    let Some(candidate) = request.header("x-senate-token") else {
        return HttpResponse::error(401, "missing X-Senate-Token");
    };
    if !token_matches(candidate, token) {
        return HttpResponse::error(401, "invalid token");
    }

    if request.method == "POST"
        && let Some(origin) = request.header("origin")
        && !origin_allowed(origin, port)
    {
        return HttpResponse::error(403, "Origin header does not match this server");
    }

    // A tab opened from the terminal names its campaign; one server serves
    // every campaign, so a second `W` never shows the wrong one.
    let mission = request.mission().or(mission);
    match (request.method.as_str(), api_path) {
        ("GET", "/health") => HttpResponse::json(200, &serde_json::json!({ "ok": true })),
        ("GET", "/world") => match projection::snapshot(mission) {
            Ok(state) => HttpResponse::json(
                200,
                &serde_json::to_value(state)
                    .unwrap_or_else(|_| serde_json::json!({"error": "unrepresentable state"})),
            ),
            Err(error) => HttpResponse::error(500, &error.to_string()),
        },
        ("GET", path) if path.starts_with("/order/") => {
            match projection::order_detail(mission, &path["/order/".len()..]) {
                Ok(Some(detail)) => {
                    HttpResponse::json(200, &serde_json::to_value(detail).unwrap_or_default())
                }
                Ok(None) => HttpResponse::error(404, "no such order"),
                Err(error) => HttpResponse::error(500, &error.to_string()),
            }
        }
        ("GET", "/consul") => match projection::consul_log(mission) {
            Ok(log) => HttpResponse::json(200, &serde_json::to_value(log).unwrap_or_default()),
            Err(error) => HttpResponse::error(500, &error.to_string()),
        },
        ("POST", "/consul") => ask(request, mission),
        _ => HttpResponse::error(404, "not found"),
    }
}

fn ask(request: &HttpRequest, mission: Option<MissionId>) -> HttpResponse {
    let Some(message) = serde_json::from_slice::<serde_json::Value>(&request.body)
        .ok()
        .and_then(|body| body.get("message")?.as_str().map(ToOwned::to_owned))
    else {
        return HttpResponse::error(400, "expected {\"message\": \"...\"}");
    };
    let refused = |reason: &str| {
        HttpResponse::json(
            200,
            &serde_json::json!({ "accepted": false, "reason": reason }),
        )
    };
    match projection::ask_consul(mission, &message) {
        Ok(Ok(())) => HttpResponse::json(202, &serde_json::json!({ "accepted": true })),
        Ok(Err(projection::AskRefused::Busy)) => {
            refused("The Consul is still answering your last message.")
        }
        Ok(Err(projection::AskRefused::NoCampaign)) => {
            refused("There is no campaign to ask about.")
        }
        Ok(Err(projection::AskRefused::Empty)) => refused("Write something first."),
        Err(error) => HttpResponse::error(500, &error.to_string()),
    }
}

fn route_asset(path: &str) -> HttpResponse {
    let found = if path == "/" {
        assets::index()
    } else {
        assets::lookup(path)
    };
    match found {
        Some((content_type, bytes)) => HttpResponse::asset(200, content_type, bytes.to_vec()),
        None => HttpResponse::error(404, "not found"),
    }
}

/// Guards against DNS rebinding: only an exact `127.0.0.1:<port>` or
/// `localhost:<port>` Host header is accepted, whatever a hostile page's
/// own DNS says it resolves to.
fn host_matches(host: &str, port: u16) -> bool {
    let host = host.trim();
    host.eq_ignore_ascii_case(&format!("127.0.0.1:{port}"))
        || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
}

/// A mutation is only accepted from a page this server itself served.
fn origin_allowed(origin: &str, port: u16) -> bool {
    let origin = origin.trim();
    origin.eq_ignore_ascii_case(&format!("http://127.0.0.1:{port}"))
        || origin.eq_ignore_ascii_case(&format!("http://localhost:{port}"))
}

/// Constant-time token comparison: the API token is a bearer credential, so
/// comparing it byte-by-byte with early exit would leak its prefix through
/// timing.
fn token_matches(candidate: &str, expected: &str) -> bool {
    let candidate = candidate.as_bytes();
    let expected = expected.as_bytes();
    if candidate.len() != expected.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (byte_a, byte_b) in candidate.iter().zip(expected.iter()) {
        diff |= byte_a ^ byte_b;
    }
    diff == 0
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "",
    }
}

fn write_response(writer: &mut TcpStream, response: &HttpResponse) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason_phrase(response.status)
    );
    let _ = writeln!(head, "Content-Type: {}\r", response.content_type);
    let _ = writeln!(head, "Content-Length: {}\r", response.body.len());
    head.push_str("Connection: close\r\n");
    head.push_str("X-Content-Type-Options: nosniff\r\n");
    if response.cache_no_store {
        head.push_str("Cache-Control: no-store\r\n");
    }
    if response.content_security_policy {
        head.push_str(
            "Content-Security-Policy: default-src 'self'; img-src 'self' data: blob:; \
style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'\r\n",
        );
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes())?;
    writer.write_all(&response.body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_header_accepts_only_this_servers_loopback_names() {
        assert!(host_matches("127.0.0.1:4123", 4123));
        assert!(host_matches("LOCALHOST:4123", 4123));
        assert!(!host_matches("127.0.0.1:4123", 9999));
        assert!(!host_matches("evil.example:4123", 4123));
        assert!(!host_matches("127.0.0.1", 4123));
    }

    #[test]
    fn token_comparison_requires_an_exact_match() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc123", "abc124"));
        assert!(!token_matches("abc12", "abc123"));
        assert!(!token_matches("", "abc123"));
    }

    #[test]
    fn origin_check_accepts_only_this_servers_own_origin() {
        assert!(origin_allowed("http://127.0.0.1:4123", 4123));
        assert!(origin_allowed("http://localhost:4123", 4123));
        assert!(!origin_allowed("http://127.0.0.1:4123", 9999));
        assert!(!origin_allowed("https://127.0.0.1:4123", 4123));
        assert!(!origin_allowed("http://evil.example", 4123));
    }

    #[test]
    fn browser_url_puts_the_token_in_the_fragment_and_demo_before_it() {
        assert_eq!(
            browser_url(4123, "deadbeef", None, None),
            "http://127.0.0.1:4123/#token=deadbeef"
        );
        assert_eq!(
            browser_url(4123, "deadbeef", Some("review"), None),
            "http://127.0.0.1:4123/?demo=review#token=deadbeef"
        );
    }

    #[test]
    fn browser_url_names_the_mission_and_the_server_routes_to_it() {
        let asked = MissionId::from_u128(7);
        let url = browser_url(4123, "deadbeef", None, Some(asked));
        assert_eq!(
            url,
            format!("http://127.0.0.1:4123/?mission={asked}#token=deadbeef")
        );
        assert_eq!(
            browser_url(4123, "deadbeef", Some("review"), Some(asked)),
            format!("http://127.0.0.1:4123/?demo=review&mission={asked}#token=deadbeef")
        );

        // The page forwards its own query to the API, so a server started
        // for another mission still answers for the one this tab names.
        let request = HttpRequest {
            method: "GET".to_owned(),
            path: format!("/api/world?mission={asked}"),
            headers: Vec::new(),
            body: Vec::new(),
        };
        assert_eq!(request.mission(), Some(asked));
        assert_eq!(request.route_path(), "/api/world");
    }

    #[test]
    fn accepted_goes_out_as_202_accepted() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let mut raw = String::new();
            std::io::Read::read_to_string(&mut stream, &mut raw).unwrap();
            raw
        });
        let (mut server_side, _) = listener.accept().unwrap();
        let response = HttpResponse::json(202, &serde_json::json!({ "accepted": true }));
        write_response(&mut server_side, &response).unwrap();
        drop(server_side);

        let raw = client.join().unwrap();
        assert!(raw.starts_with("HTTP/1.1 202 Accepted\r\n"), "{raw}");
        assert_eq!(reason_phrase(418), "", "unknown codes are not errors");
    }

    fn get(
        server_port: u16,
        path: &str,
        token: Option<&str>,
        host: Option<&str>,
    ) -> std::io::Result<(u16, String)> {
        use std::io::Read as _;

        let mut stream = TcpStream::connect(("127.0.0.1", server_port))?;
        let host_header = host.map_or_else(|| format!("127.0.0.1:{server_port}"), str::to_owned);
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {host_header}\r\n");
        if let Some(token) = token {
            let _ = writeln!(request, "X-Senate-Token: {token}\r");
        }
        request.push_str("Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes())?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        Ok((status, text))
    }

    #[test]
    fn health_endpoint_requires_the_token_and_answers_ok() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        listener.set_nonblocking(true).expect("nonblocking");
        let shutdown = Arc::new(AtomicBool::new(false));
        let token = "test-token".to_owned();

        let worker_shutdown = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            while !worker_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let token = token.clone();
                        std::thread::spawn(move || {
                            let _ = handle_connection(stream, port, &token, None);
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });

        let (status, _) = get(port, "/api/health", Some("test-token"), None).unwrap();
        assert_eq!(status, 200);

        let (status, _) = get(port, "/api/health", None, None).unwrap();
        assert_eq!(status, 401);

        let (status, _) = get(port, "/api/health", Some("wrong"), None).unwrap();
        assert_eq!(status, 401);

        let (status, _) = get(port, "/api/health", Some("test-token"), Some("evil:1")).unwrap();
        assert_eq!(status, 403);

        // A large asset arrives whole even when the reader is slower than
        // the writer (accepted sockets must not stay non-blocking).
        let (_, expected) = assets::lookup("/vendor/three.module.min.js").expect("vendored");
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "GET /vendor/three.module.min.js HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut raw).unwrap();
        let body_at = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert_eq!(raw.len() - body_at, expected.len());

        shutdown.store(true, Ordering::Relaxed);
        handle.join().expect("server thread joins");
    }
}
