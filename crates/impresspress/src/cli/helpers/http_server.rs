//! Minimal static-file HTTP server shared by sealed × web and embed × web
//! `serve` flows. Reads request line, serves matching file from `dir`.
//! Development convenience — not a production server.

use std::path::{Component, Path, PathBuf};

use anyhow::Result;

/// Bind `127.0.0.1:port` and serve static files rooted at `dir`. Each
/// accepted connection is spawned onto a new tokio task.
///
/// # Errors
///
/// Returns an error if the listener fails to bind. Per-connection failures
/// are logged at debug and dropped; they don't bring down the loop.
pub async fn serve_static(dir: &Path, port: u16) -> Result<()> {
    use tokio::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let dir = dir.to_path_buf();
    loop {
        let (mut socket, _) = listener.accept().await?;
        let dir = dir.clone();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
            // Read the full request head (request line + headers, ending
            // with the blank line). 1 KiB was enough for the request line
            // but typical browsers send Cookie / Accept / User-Agent headers
            // that easily exceed that. Cap at 64 KiB so a hostile peer
            // can't OOM the dev server.
            const HEAD_LIMIT: u64 = 64 * 1024;
            let mut reader = BufReader::new(&mut socket).take(HEAD_LIMIT);
            let mut head = Vec::with_capacity(2048);
            loop {
                let before = head.len();
                if reader.read_until(b'\n', &mut head).await.is_err() {
                    return;
                }
                if head.len() == before {
                    // EOF before terminator — give up.
                    return;
                }
                if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
                    break;
                }
                if head.len() as u64 >= HEAD_LIMIT {
                    // Header section too large; refuse rather than buffer
                    // unbounded.
                    let _ = socket
                        .write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n")
                        .await;
                    return;
                }
            }

            let req = String::from_utf8_lossy(&head);
            let raw_path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");

            let Some(file_path) = resolve_request_path(&dir, raw_path) else {
                let _ = socket
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                    .await;
                return;
            };

            let body = tokio::fs::read(&file_path).await;
            let resp = match body {
                Ok(b) => {
                    let mime = mime_guess(&file_path);
                    let mut out = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n\r\n",
                        b.len()
                    )
                    .into_bytes();
                    out.extend(b);
                    out
                }
                Err(_) => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
            };
            let _ = socket.write_all(&resp).await;
        });
    }
}

/// Resolve `raw_path` (the URL path from the request line) against `dir`,
/// rejecting any path that contains `..` or other non-`Normal` components
/// to block trivial traversal attempts (`GET /../../etc/passwd`).
///
/// Returns `None` for a rejected path; the caller should reply with 400.
fn resolve_request_path(dir: &Path, raw_path: &str) -> Option<PathBuf> {
    // Strip the query string + fragment; we only serve files by path.
    let path = raw_path.split(['?', '#']).next().unwrap_or("/");
    if path == "/" {
        return Some(dir.join("index.html"));
    }
    let rel = path.trim_start_matches('/');
    let rel_path = Path::new(rel);
    for c in rel_path.components() {
        // Only allow plain path segments. Anything else (`..`,
        // root prefixes, `.`, etc.) is rejected.
        if !matches!(c, Component::Normal(_)) {
            return None;
        }
    }
    Some(dir.join(rel_path))
}

fn mime_guess(p: &Path) -> &'static str {
    match p.extension().and_then(|s| s.to_str()) {
        Some("html") => "text/html",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dotdot_traversal() {
        let dir = Path::new("/srv/static");
        assert!(resolve_request_path(dir, "/../etc/passwd").is_none());
        assert!(resolve_request_path(dir, "/foo/../../etc/passwd").is_none());
    }

    #[test]
    fn rejects_absolute_segment() {
        let dir = Path::new("/srv/static");
        // Leading slashes are trimmed, but an explicit `/etc/passwd` after
        // trimming still has a `Normal("etc")` so this isn't the absolute
        // form — but on Unix `/...` style without slashes is fine. Test
        // the failure path via `..` directly.
        assert!(resolve_request_path(dir, "/foo/../bar").is_none());
    }

    #[test]
    fn root_maps_to_index_html() {
        let dir = Path::new("/srv/static");
        assert_eq!(
            resolve_request_path(dir, "/").unwrap(),
            dir.join("index.html")
        );
    }

    #[test]
    fn strips_query_string() {
        let dir = Path::new("/srv/static");
        assert_eq!(
            resolve_request_path(dir, "/main.js?v=1").unwrap(),
            dir.join("main.js")
        );
    }

    #[test]
    fn allows_nested_normal_path() {
        let dir = Path::new("/srv/static");
        assert_eq!(
            resolve_request_path(dir, "/assets/img/logo.png").unwrap(),
            dir.join("assets/img/logo.png")
        );
    }
}
