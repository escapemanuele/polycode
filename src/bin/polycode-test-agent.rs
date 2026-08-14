use std::ffi::OsString;
use std::io::{Read, Write};
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = arguments.next().unwrap_or_default();
    match mode.to_string_lossy().as_ref() {
        "success" => {
            std::io::stdout().write_all(b"quick-success\n")?;
        }
        "slow" => {
            let milliseconds = parse_u64(arguments.next())?;
            std::thread::sleep(Duration::from_millis(milliseconds));
            std::io::stdout().write_all(b"slow-success\n")?;
        }
        "stderr" => {
            std::io::stderr().write_all(b"separate-stderr\n")?;
        }
        "fail-42" => {
            std::io::stderr().write_all(b"expected-failure\n")?;
            std::process::exit(42);
        }
        "partial" => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(b"{\"message\":\"par")?;
            stdout.flush()?;
            std::thread::sleep(Duration::from_millis(250));
            stdout.write_all(b"tial\"}\n")?;
        }
        "wait-interrupt" => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(b"ready-for-interrupt\n")?;
            stdout.flush()?;
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        "large" => {
            let length = parse_u64(arguments.next())?;
            let mut remaining = length;
            let block = vec![b'x'; 64 * 1024];
            let mut stdout = std::io::stdout().lock();
            while remaining > 0 {
                let count = usize::try_from(remaining.min(block.len() as u64)).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid length")
                })?;
                stdout.write_all(&block[..count])?;
                remaining -= count as u64;
            }
        }
        "inspect" => {
            let cwd = std::env::current_dir()?;
            let inherited = std::env::var_os("HOME").unwrap_or_default();
            let overridden = std::env::var_os("POLYCODE_TEST_OVERRIDE").unwrap_or_default();
            let remaining: Vec<OsString> = arguments.collect();
            writeln!(
                std::io::stdout(),
                "cwd={}\ninherited={}\noverride={}\nargs={}",
                cwd.display(),
                inherited.to_string_lossy(),
                overridden.to_string_lossy(),
                remaining
                    .iter()
                    .map(|value| value.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("|")
            )?;
        }
        "stdin" => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes)?;
            std::io::stdout().write_all(&bytes)?;
        }
        _ => {
            std::io::stderr().write_all(b"unknown fixture mode\n")?;
            std::process::exit(64);
        }
    }
    Ok(())
}

fn parse_u64(value: Option<OsString>) -> std::io::Result<u64> {
    value
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing integer"))
}
