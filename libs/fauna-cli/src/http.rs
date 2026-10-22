//! MCP's streamable-HTTP transport, loopback only (`apps/cli.md` § The device
//! shell): one endpoint, `POST /mcp`, JSON in and JSON out.
//!
//! **No auth header of its own — loopback is the trust boundary.** The
//! principal session is the credential, so the one attack the listener must
//! close itself is DNS rebinding: a page in the user's own browser reaching the
//! loopback port as if it were a remote origin. MCP's transport rule is the
//! defence, enforced on every request — a request whose `Host` is not this
//! listener's own loopback name and port, or whose `Origin` is present and not
//! a loopback origin, is refused `403` and never reaches the method layer. The bind itself is `127.0.0.1` and the
//! caller ([`bind`]) takes no address, so a non-loopback bind is not possible
//! by construction.
//!
//! **What it serves.** A request is answered with `application/json` (`200`), a
//! notification or a bare response with `202`. The shell offers no server-sent
//! stream and no session id (the catalog is stateless per request over one
//! principal), so `GET` and `DELETE` answer `405` — the spec's "no SSE stream
//! here" shape. Every connection is answered once and closed.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::mcp::Server;
use crate::tools::Host;

/// The one path the transport serves.
pub const ENDPOINT: &str = "/mcp";

/// A connection that has not delivered its whole request by now is dropped.
const READ_BUDGET: Duration = Duration::from_secs(10);
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 4 * 1024 * 1024;

/// Bind a free loopback port.
pub async fn bind() -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await
}

/// The URL a host dials for `listener`.
pub fn url(listener: &TcpListener) -> std::io::Result<String> {
    Ok(format!(
        "http://127.0.0.1:{}{ENDPOINT}",
        listener.local_addr()?.port()
    ))
}

/// Serve `server` on `listener` until the process ends.
pub async fn serve<H: Host>(server: &Server<H>, listener: TcpListener) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    loop {
        let (stream, _) = listener.accept().await?;
        // A failed connection is that peer's problem, never the listener's.
        let _ = handle_connection(server, stream, port).await;
    }
}

async fn handle_connection<H: Host>(
    server: &Server<H>,
    mut stream: TcpStream,
    port: u16,
) -> std::io::Result<()> {
    let request = match tokio::time::timeout(READ_BUDGET, read_request(&mut stream)).await {
        Ok(Ok(request)) => request,
        Ok(Err(refusal)) => {
            return respond(&mut stream, refusal.status, refusal.reason, &[], "").await;
        }
        Err(_) => return respond(&mut stream, 408, "Request Timeout", &[], "").await,
    };
    respond_to(server, &mut stream, request, port).await
}

async fn respond_to<H: Host, W: AsyncWrite + Unpin>(
    server: &Server<H>,
    out: &mut W,
    request: Request,
    port: u16,
) -> std::io::Result<()> {
    if !host_allowed(request.header("host"), port) {
        return respond(out, 403, "Forbidden", &[], "unexpected Host").await;
    }
    if let Some(origin) = request.header("origin")
        && !origin_allowed(origin)
    {
        return respond(out, 403, "Forbidden", &[], "unexpected Origin").await;
    }
    if request.path != ENDPOINT {
        return respond(out, 404, "Not Found", &[], "").await;
    }
    if request.method != "POST" {
        return respond(out, 405, "Method Not Allowed", &[("Allow", "POST")], "").await;
    }
    match server.handle(&String::from_utf8_lossy(&request.body)).await {
        Some(answer) => {
            respond(
                out,
                200,
                "OK",
                &[("Content-Type", "application/json")],
                &answer.to_string(),
            )
            .await
        }
        None => respond(out, 202, "Accepted", &[], "").await,
    }
}

/// `Host` is required and must name this listener: `127.0.0.1`, `localhost` or
/// `[::1]`, with the listener's own port.
pub fn host_allowed(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return false };
    let Some((name, p)) = host.rsplit_once(':') else {
        return false;
    };
    p.parse::<u16>() == Ok(port) && is_loopback_name(name)
}

/// An `Origin` is allowed when it is a loopback origin (`scheme://loopback[:port]`).
pub fn origin_allowed(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let name = match rest.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map(|(inside, _)| format!("[{inside}]")),
        None => Some(rest.split(':').next().unwrap_or("").to_owned()),
    };
    name.is_some_and(|n| is_loopback_name(&n))
}

fn is_loopback_name(name: &str) -> bool {
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

struct Refusal {
    status: u16,
    reason: &'static str,
}

fn malformed() -> Refusal {
    Refusal {
        status: 400,
        reason: "Bad Request",
    }
}

async fn read_request<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Request, Refusal> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Err(Refusal {
                status: 431,
                reason: "Request Header Fields Too Large",
            });
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.map_err(|_| malformed())?;
        if n == 0 {
            return Err(malformed());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| malformed())?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(path), Some(_version)) = (
        request_line.next(),
        request_line.next(),
        request_line.next(),
    ) else {
        return Err(malformed());
    };
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let mut request = Request {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body: Vec::new(),
    };
    // A body is read whole under a size and time budget; chunked is refused.
    if request.header("transfer-encoding").is_some() {
        return Err(Refusal {
            status: 501,
            reason: "Not Implemented",
        });
    }
    let length: usize = match request.header("content-length") {
        None => 0,
        Some(v) => v.parse().map_err(|_| malformed())?,
    };
    if length > MAX_BODY {
        return Err(Refusal {
            status: 413,
            reason: "Content Too Large",
        });
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let mut chunk = [0u8; 8192];
        let n = stream.read(&mut chunk).await.map_err(|_| malformed())?;
        if n == 0 {
            return Err(malformed());
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    request.body = body;
    Ok(request)
}

async fn respond<W: AsyncWrite + Unpin>(
    out: &mut W,
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes()).await?;
    out.write_all(body.as_bytes()).await?;
    out.flush().await?;
    out.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_listeners_own_loopback_host_is_allowed() {
        assert!(host_allowed(Some("127.0.0.1:4000"), 4000));
        assert!(host_allowed(Some("localhost:4000"), 4000));
        assert!(host_allowed(Some("[::1]:4000"), 4000));
        assert!(!host_allowed(Some("127.0.0.1:4001"), 4000));
        assert!(!host_allowed(Some("evil.example:4000"), 4000));
        assert!(!host_allowed(Some("localhost.evil.example:4000"), 4000));
        assert!(!host_allowed(Some("127.0.0.1"), 4000));
        assert!(!host_allowed(None, 4000));
    }

    #[test]
    fn only_a_loopback_origin_is_allowed() {
        assert!(origin_allowed("http://localhost:3000"));
        assert!(origin_allowed("http://127.0.0.1"));
        assert!(origin_allowed("https://[::1]:8443"));
        assert!(!origin_allowed("https://evil.example"));
        assert!(!origin_allowed("http://localhost.evil.example"));
        assert!(!origin_allowed("null"));
        assert!(!origin_allowed("file://localhost"));
    }
}
