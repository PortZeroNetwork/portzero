//! Hidden `portzero demo-server`: the tiny web app behind `portzero demo`.
//!
//! A std-only HTTP server (no extra dependencies) that binds 127.0.0.1 port 0
//! — the whole point of the product: the OS picks a free port, and the daemon
//! discovers both the port and the `PZ_TUNNEL` value from this process's
//! environment (set by `portzero demo` at spawn time). Any request gets a
//! small self-contained HTML page confirming the tunnel works.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result};

/// The page served for every request.
const PAGE_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Port Zero demo</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto; padding: 0 1rem; line-height: 1.6; }
  h1 { font-size: 1.6rem; }
  code, pre { background: #f0f0f0; border-radius: 4px; padding: 0.1rem 0.3rem; }
  pre { padding: 0.6rem; overflow-x: auto; }
  @media (prefers-color-scheme: dark) {
    body { background: #111; color: #eee; }
    code, pre { background: #222; }
  }
</style>
</head>
<body>
<h1>It works! &#127881; This page is served through Port Zero</h1>
<p>A tiny web server on this machine bound <strong>port 0</strong>, so the
operating system picked a random free port &mdash; no port conflicts, ever.</p>
<p>The server was started with <code>PZ_TUNNEL=hello.portzero.local:80</code>
in its environment; the Port Zero daemon discovered it and routed
<code>http://hello.portzero.local</code> to it. This is a Local tunnel: free,
local-only, no account needed.</p>
<p>Do the same with your own dev command &mdash; set its port to 0 and run:</p>
<pre>PZ_TUNNEL=web.myapp.portzero.local:80 &lt;your dev command&gt;</pre>
</body>
</html>
"#;

/// Run the demo HTTP server until the parent `portzero demo` goes away (or
/// the process is killed). Binds 127.0.0.1:0 and serves [`PAGE_HTML`] to
/// every request.
pub fn serve() -> Result<()> {
    watch_parent_via_stdin();
    let listener = TcpListener::bind("127.0.0.1:0").context(
        "The demo server could not bind a port on 127.0.0.1.\n\n\
         This is unusual (port 0 lets the OS pick any free port); check that \
         local networking is available and try `portzero demo` again.",
    )?;
    let addr = listener
        .local_addr()
        .context("The demo server could not read its own listening address.")?;
    println!("demo server listening on {addr} (OS-assigned port via port 0)");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // One thread per connection so a stalled client never blocks the next
        // request; the demo sees a handful of requests at most.
        std::thread::spawn(move || {
            let _ = handle_connection(stream);
        });
    }
    Ok(())
}

/// Exit when the parent `portzero demo` process goes away. `demo` spawns this
/// server with a pipe on stdin and never writes to it, so reading EOF (or an
/// error) means the parent exited — including a hard kill where it never got
/// to clean up — and the server should stop rather than linger orphaned,
/// keeping a stale `hello.portzero.local` route alive.
fn watch_parent_via_stdin() {
    std::thread::spawn(|| {
        let mut sink = [0u8; 64];
        let mut stdin = std::io::stdin();
        loop {
            match stdin.read(&mut sink) {
                Ok(0) | Err(_) => std::process::exit(0),
                Ok(_) => {}
            }
        }
    });
}

/// Serve one connection: read the request (one read is enough for the demo's
/// small GETs), answer with the demo page, and close.
fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf)?;
    stream.write_all(response().as_bytes())
}

/// The full HTTP/1.1 response (headers + demo page).
fn response() -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {PAGE_HTML}",
        PAGE_HTML.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_is_valid_http_with_matching_content_length() {
        let resp = response();
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"));
        let (headers, body) = resp.split_once("\r\n\r\n").expect("header/body split");
        assert!(headers.contains("Content-Type: text/html; charset=utf-8"));
        assert!(headers.contains(&format!("Content-Length: {}", body.len())));
    }

    #[test]
    fn page_explains_the_magic_moment() {
        assert!(PAGE_HTML.contains("It works!"));
        assert!(PAGE_HTML.contains("served through Port Zero"));
        assert!(PAGE_HTML.contains("PZ_TUNNEL=web.myapp.portzero.local:80"));
    }

    #[test]
    fn handle_connection_serves_the_page_to_a_get_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            handle_connection(stream).expect("handle connection");
        });

        let mut client = TcpStream::connect(addr).expect("connect");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: hello.portzero.local\r\n\r\n")
            .expect("send request");
        let mut resp = String::new();
        client.read_to_string(&mut resp).expect("read response");
        server.join().expect("server thread");

        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(resp.contains("It works!"));
    }
}
