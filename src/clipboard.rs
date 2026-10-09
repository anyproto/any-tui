//! Copying text to the system clipboard: the platform tool (`pbcopy` on
//! macOS; `wl-copy` under Wayland, `xclip` / `xsel` under X11), and the
//! OSC 52 escape as the fallback — and in addition over SSH, where the local
//! tool would fill the remote box's clipboard. OSC 52 reaches the terminal
//! you are actually looking at (through mosh; through tmux with
//! `set-clipboard on`).

use std::io::Write;
use std::process::{Command, Stdio};

/// How the text got out, for the toast.
pub enum Copied {
    Tool(&'static str),
    Osc52,
}

pub fn copy(text: &str) -> Copied {
    let remote = std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    let tool = candidates().into_iter().find(|(name, args)| run(name, args, text));
    if remote || tool.is_none() {
        osc52(text);
    }
    match tool {
        Some((name, _)) if !remote => Copied::Tool(name),
        _ => Copied::Osc52,
    }
}

fn candidates() -> Vec<(&'static str, &'static [&'static str])> {
    if cfg!(target_os = "macos") {
        return vec![("pbcopy", &[])];
    }
    let mut v: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        v.push(("wl-copy", &[]));
    }
    if std::env::var_os("DISPLAY").is_some() {
        v.push(("xclip", &["-selection", "clipboard"]));
        v.push(("xsel", &["--clipboard", "--input"]));
    }
    v
}

/// Pipes `text` into the tool. `xclip` / `wl-copy` fork a child that keeps
/// serving the selection; the process we wait on exits once stdin closes.
/// All output is nulled so nothing scribbles over the TUI.
fn run(name: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(name)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|s| s.success()) && wrote
}

fn osc52(text: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64("ё".as_bytes()), "0ZE=");
    }
}
