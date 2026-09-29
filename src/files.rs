//! Opening and saving message attachments (`o` / `s`), on Linux and macOS.
//!
//! Files come from the daemon (`GET …/files/{fileId}/content`), never from the
//! network directly. `open` downloads into a per-user cache and hands the path
//! to the desktop opener (`xdg-open` / `open`); `save` downloads into the
//! Downloads folder under a name that never overwrites an existing file.

use crate::api::Api;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tokio::io::AsyncWriteExt;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Where `s` saves: on Linux the XDG download dir (`XDG_DOWNLOAD_DIR` in
/// `user-dirs.dirs`, the file `xdg-user-dir` reads — read directly, since
/// that tool often isn't installed), else `~/Downloads`, else `~`.
pub fn download_dir() -> PathBuf {
    if cfg!(target_os = "linux")
        && let Some(p) = xdg_download_dir().filter(|p| p.is_dir() && *p != home())
    {
        return p;
    }
    let d = home().join("Downloads");
    if d.is_dir() { d } else { home() }
}

fn xdg_download_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    parse_user_dirs(&text, &home().to_string_lossy())
}

/// `XDG_DOWNLOAD_DIR="$HOME/Downloads"` → `/home/me/Downloads`. The format
/// only allows `$HOME/…` or an absolute path.
fn parse_user_dirs(text: &str, home: &str) -> Option<PathBuf> {
    let line = text.lines().find(|l| l.trim_start().starts_with("XDG_DOWNLOAD_DIR="))?;
    let v = line.trim_start()["XDG_DOWNLOAD_DIR=".len()..].trim().trim_matches('"');
    let v = v.replacen("$HOME", home, 1);
    v.starts_with('/').then(|| PathBuf::from(v))
}

/// Where `o` downloads before opening: `$XDG_CACHE_HOME` (or `~/.cache`) on
/// Linux, `~/Library/Caches` on macOS.
pub fn cache_dir() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home().join(".cache"))
    };
    base.join("any-tui").join("files")
}

/// A sender-chosen name made safe to use as one path component.
pub fn safe_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() { fallback.to_string() } else { cleaned }
}

/// `dir/name`, or `dir/stem (1).ext`, `(2)`, … — the first that doesn't exist.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some suffix is free")
}

/// Hands a path or URL to the desktop's default handler, detached from the
/// terminal (all stdio nulled so it can't scribble over the TUI).
pub fn open_external(target: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let mut child = Command::new(opener)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("run {opener}"))?;
    // Reap it off the UI thread; xdg-open can linger while the app starts.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Shows `paths` selected in one file-manager window: Finder via `open -R` on macOS;
/// on Linux the freedesktop `FileManager1.ShowItems` D-Bus call (Nautilus,
/// Dolphin, Nemo, Thunar, … answer it), else `xdg-open` on the folder.
/// Blocking (the D-Bus call is waited on to know whether to fall back), so
/// call it off the UI thread.
pub fn reveal(paths: &[PathBuf]) -> Result<()> {
    let Some(first) = paths.first() else { return Ok(()) };
    if cfg!(target_os = "macos") {
        let ok = Command::new("open")
            .arg("-R")
            .args(paths)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run open -R")?
            .success();
        return if ok { Ok(()) } else { bail!("open -R failed") };
    }
    let shown = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!(
            "array:string:{}",
            paths.iter().map(|p| file_uri(p)).collect::<Vec<_>>().join(",")
        ))
        .arg("string:")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if shown {
        return Ok(());
    }
    open_external(&first.parent().unwrap_or(first).to_string_lossy())
}

/// `file://` URI with everything outside the unreserved set percent-encoded
/// (a D-Bus `array:string:` also splits on commas, which this covers).
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Streams a file's content to `dest` via a `.part` sibling (renamed on
/// success, so a cut download never looks complete). `progress` gets
/// (bytes so far, total) as chunks land.
pub async fn download(
    api: &Api,
    space_id: &str,
    file_id: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    let mut resp = api.file_content(space_id, file_id).await?;
    let total = resp.content_length().unwrap_or(0);
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("create {}", dir.display()))?;
    }
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    ));
    let mut f = tokio::fs::File::create(&part)
        .await
        .with_context(|| format!("create {}", part.display()))?;
    let mut got = 0u64;
    let res: Result<()> = async {
        while let Some(chunk) = resp.chunk().await.context("download interrupted")? {
            f.write_all(&chunk).await?;
            got += chunk.len() as u64;
            progress(got, total);
        }
        f.flush().await?;
        if total > 0 && got != total {
            bail!("download cut short ({got} of {total} bytes)");
        }
        Ok(())
    }
    .await;
    if let Err(e) = res {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, dest)
        .await
        .with_context(|| format!("move into {}", dest.display()))?;
    Ok(())
}

/// "980 KB", "4.2 MB" — for the attachment line.
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 || v >= 10.0 {
        format!("{} {}", v.round() as u64, UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("a/b\\c.png", "id"), "a_b_c.png");
        assert_eq!(safe_name("..", "id"), "id");
        assert_eq!(safe_name(".bashrc", "id"), "bashrc");
        assert_eq!(safe_name("  ", "id"), "id");
        assert_eq!(safe_name("Screenshot 15.55.png", "id"), "Screenshot 15.55.png");
    }

    #[test]
    fn unique_path_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("any-tui-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a.png"));
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a (1).png"));
        std::fs::write(dir.join("a (1).png"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a (2).png"));
        assert_eq!(unique_path(&dir, "noext"), dir.join("noext"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn user_dirs() {
        let t = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/dl\"\n";
        assert_eq!(parse_user_dirs(t, "/home/me"), Some(PathBuf::from("/home/me/dl")));
        assert_eq!(parse_user_dirs("XDG_DOWNLOAD_DIR=\"/data/dl\"", "/h"), Some(PathBuf::from("/data/dl")));
        assert_eq!(parse_user_dirs("nothing", "/h"), None);
    }

    #[test]
    fn uris_are_encoded() {
        assert_eq!(
            file_uri(Path::new("/home/me/Down loads/a,b (1).png")),
            "file:///home/me/Down%20loads/a%2Cb%20%281%29.png"
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1003573), "980 KB");
        assert_eq!(human_size(4_404_019), "4.2 MB");
    }
}
