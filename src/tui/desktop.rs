//! The two things a pull request URL is for once it is on screen: opening it
//! in the browser and putting it on the clipboard. Both go through whatever
//! the platform ships rather than a dependency, and both report a failure as
//! a sentence for the footer instead of a panic.

use std::io::Write;
use std::process::{Command, Stdio};

/// Opens `url` with the platform's default browser.
///
/// # Errors
/// Returns a one-line reason when no opener could be started.
pub(crate) fn open_in_browser(url: &str) -> Result<(), String> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["open"]
    } else {
        &["xdg-open", "wslview"]
    };
    let mut last = String::from("no browser opener found");
    for program in candidates {
        match Command::new(program)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(source) => last = format!("{program}: {source}"),
        }
    }
    Err(last)
}

/// Puts `text` on the system clipboard.
///
/// # Errors
/// Returns a one-line reason when no clipboard tool accepted it.
pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    let mut last = String::from("no clipboard tool found");
    for (program, args) in candidates {
        let child = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(source) => {
                last = format!("{program}: {source}");
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(source) = stdin.write_all(text.as_bytes()) {
                last = format!("{program}: {source}");
                continue;
            }
        }
        match child.wait() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => last = format!("{program} exited with {status}"),
            Err(source) => last = format!("{program}: {source}"),
        }
    }
    Err(last)
}
