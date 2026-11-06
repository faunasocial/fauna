//! The web source transport: TLS inside the client, over the nest's blind
//! WebSocket byte tunnel (`mailbox-migration.md` § Where the IMAP client runs →
//! *The web tunnel*).
//!
//! The browser has no raw TCP, so the web app reaches a foreign IMAP server
//! (and the Microsoft identity authority) through one WebSocket route on its
//! own nest, which dials the named destination and ferries bytes both ways
//! without reading them. Everything that makes that safe lives on this side:
//! the client runs a sans-io [`rustls::ClientConnection`] over the tunnel's
//! frames, so TLS ends **inside the client** and the nest never sees the
//! source password, the token, or a byte of the mailbox.
//!
//! What this module owns, all of it target-agnostic shared Rust:
//!
//! - [`TunnelPipe`] / [`TunnelDialer`] — the two seams the platform shell
//!   supplies: one opened WebSocket carrying binary frames, and the act of
//!   opening one (the browser's `WebSocket` on web; a test's in-memory pipe).
//! - [`open_tunnel`] — the open exchange: the DAG-CBOR
//!   [`TunnelOpen`](fauna_protocol::mail_import_tunnel::TunnelOpen) first
//!   frame and the typed answer, a refusal surfacing as
//!   [`TunnelError::Refused`] before any byte reaches the destination.
//! - [`TunnelTlsTransport`] — the [`ImapTransport`] over an opened tunnel, both
//!   TLS modes (implicit, and STARTTLS with the in-place upgrade).
//! - [`TunnelOAuthHttp`] — the [`OAuthHttp`] seam: one HTTP/1.1 form POST
//!   through the same kind of TLS session, keeping one tunnel per authority
//!   across the device-code polls.
//! - [`tunnel_client_config`] — the `rustls` config: the workspace's `ring`
//!   provider, the bundled roots, and a clock that is not `SystemTime` (which
//!   panics on `wasm32-unknown-unknown`).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_protocol::mail_import_tunnel::{
    MAX_FRAME_BYTES, PORT_HTTPS, TunnelAnswer, TunnelOpen, TunnelRefusal,
};
use rustls::ClientConnection;
use rustls::pki_types::{ServerName, UnixTime};

/// The `rustls` client config [`tunnel_client_config`] builds, named here so a
/// consumer holds one without taking `rustls` as a dependency of its own.
pub use rustls::ClientConfig;

use super::ImapClientError;
use super::oauth_device::{OAuthHttp, OAuthHttpResponse};
use super::transport::{ImapTransport, TlsMode};

/// What can go wrong between the client and the far end of a tunnel.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TunnelError {
    /// The nest refused the open, before dialing anything, for one of the
    /// closed set of reasons.
    #[error("the nest refused the mail-import tunnel: {}", .0.as_str())]
    Refused(TunnelRefusal),
    /// The WebSocket could not be opened, or failed under us — no nest route
    /// (an older nest answers the upgrade 404), a network drop, a close.
    #[error("mail-import tunnel: {0}")]
    Pipe(String),
    /// The tunnel closed before TLS was up.
    #[error("the mail-import tunnel closed during the TLS handshake")]
    ClosedInHandshake,
    /// The TLS session failed: a certificate that does not verify, an alert.
    #[error("TLS to the source failed: {0}")]
    Tls(String),
    /// Something on the tunnel was not what its wire allows.
    #[error("mail-import tunnel protocol: {0}")]
    Protocol(String),
}

/// One opened tunnel WebSocket: binary frames out, binary frames in.
///
/// AFIT for the same per-impl `Send` inference as [`ImapTransport`].
#[allow(async_fn_in_trait)]
pub trait TunnelPipe {
    /// Send one binary frame. Frames are at most
    /// [`MAX_FRAME_BYTES`]; [`TunnelTlsTransport`] never sends a larger one.
    async fn send(&mut self, frame: Vec<u8>) -> Result<(), TunnelError>;

    /// The next binary frame; `Ok(None)` once the tunnel has closed (the far
    /// end's EOF, an idle close). Text frames are not part of the wire and an
    /// implementation may skip them.
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TunnelError>;
}

/// Opens a tunnel WebSocket to the signed-in actor's nest (the route, the
/// subprotocol and the bearer are the implementation's).
#[allow(async_fn_in_trait)]
pub trait TunnelDialer {
    type Pipe: TunnelPipe;

    async fn dial(&self) -> Result<Self::Pipe, TunnelError>;
}

/// Open a tunnel to `host:port` and wait for the nest's answer. `starttls`
/// says the client will send one `STARTTLS` line in the clear before its
/// ClientHello (the 143 mode); the nest's TLS-shape gate holds it to that.
pub async fn open_tunnel<D: TunnelDialer>(
    dialer: &D,
    host: &str,
    port: u16,
    starttls: bool,
) -> Result<D::Pipe, TunnelError> {
    let mut pipe = dialer.dial().await?;
    let open = TunnelOpen {
        host: host.to_string(),
        port,
        starttls,
        ..Default::default()
    };
    let frame = fauna_protocol::codec::encode_canonical(&open)
        .map_err(|e| TunnelError::Protocol(format!("encode the open: {e}")))?;
    pipe.send(frame.to_vec()).await?;
    let Some(answer) = pipe.recv().await? else {
        return Err(TunnelError::Pipe(
            "the tunnel closed before the nest answered the open".into(),
        ));
    };
    match fauna_protocol::codec::decode_strict::<TunnelAnswer>(&answer) {
        Ok(TunnelAnswer::Opened {}) => Ok(pipe),
        Ok(TunnelAnswer::Refused { reason }) => Err(TunnelError::Refused(reason)),
        Err(e) => Err(TunnelError::Protocol(format!("the nest's answer: {e}"))),
    }
}

/// The wall clock `rustls` checks certificate validity against.
/// `SystemTime::now` panics on `wasm32-unknown-unknown`, so the tunnel's
/// config reads time through [`fauna_core::data::Timestamp`], which has a
/// `js-sys` backend there and `SystemTime` on native.
#[derive(Debug)]
struct WallClock;

impl rustls::time_provider::TimeProvider for WallClock {
    fn current_time(&self) -> Option<UnixTime> {
        let ms = fauna_core::data::Timestamp::now_millis_or_zero();
        (ms > 0).then(|| UnixTime::since_unix_epoch(Duration::from_millis(ms)))
    }
}

/// The client config every tunnel TLS session uses: the workspace's one
/// provider (`ring`), the bundled WebPKI roots native verifies against, and
/// [`WallClock`]. `extra_root_pem` ADDS trust anchors — only ever the e2e
/// source-IMAP trust seed, read by a test-capable build at its own reading
/// site (`e2e-gated-seeds.md` § The source-IMAP trust seed); never an
/// accept-any switch.
pub fn tunnel_client_config(
    extra_root_pem: Option<&str>,
) -> Result<Arc<ClientConfig>, TunnelError> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let mut roots = fauna_tls_bootstrap::webpki_root_store();
    if let Some(pem) = extra_root_pem.filter(|p| !p.trim().is_empty()) {
        for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let cert = cert.map_err(|e| TunnelError::Tls(format!("extra root PEM: {e}")))?;
            roots
                .add(cert)
                .map_err(|e| TunnelError::Tls(format!("extra root: {e}")))?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_details(provider, Arc::new(WallClock))
        .with_safe_default_protocol_versions()
        .map_err(|e| TunnelError::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// An [`ImapTransport`] (and the [`TunnelOAuthHttp`] connection) over an
/// opened tunnel: plaintext until TLS is up on the STARTTLS path, then every
/// byte through a sans-io [`ClientConnection`].
pub struct TunnelTlsTransport<P> {
    pipe: P,
    config: Arc<ClientConfig>,
    server_name: ServerName<'static>,
    tls: Option<ClientConnection>,
    /// Received bytes not yet consumed: plaintext before TLS, ciphertext
    /// waiting for `read_tls` after.
    rx: Vec<u8>,
    /// The tunnel has closed; nothing more will arrive.
    closed: bool,
}

impl<P: TunnelPipe> TunnelTlsTransport<P> {
    /// Open a tunnel to `host:port` through `dialer` and bring the transport
    /// up in `mode`: implicit TLS handshakes now; STARTTLS stays plaintext
    /// until [`ImapTransport::upgrade_tls`].
    pub async fn connect<D: TunnelDialer<Pipe = P>>(
        dialer: &D,
        config: Arc<ClientConfig>,
        host: &str,
        port: u16,
        mode: TlsMode,
    ) -> Result<Self, TunnelError> {
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|e| TunnelError::Tls(format!("{host}: {e}")))?;
        let pipe = open_tunnel(dialer, host, port, mode == TlsMode::StartTls).await?;
        let mut transport = Self {
            pipe,
            config,
            server_name,
            tls: None,
            rx: Vec::new(),
            closed: false,
        };
        if mode == TlsMode::Implicit {
            transport.start_tls().await?;
        }
        Ok(transport)
    }

    /// Start the TLS session over the pipe and run its handshake to the end.
    async fn start_tls(&mut self) -> Result<(), TunnelError> {
        let mut conn = ClientConnection::new(Arc::clone(&self.config), self.server_name.clone())
            .map_err(|e| TunnelError::Tls(e.to_string()))?;
        conn.set_buffer_limit(None);
        self.tls = Some(conn);
        self.flush_tls().await?;
        while self.tls_conn().is_handshaking() {
            if !self.feed_tls().await? {
                return Err(TunnelError::ClosedInHandshake);
            }
            self.flush_tls().await?;
        }
        Ok(())
    }

    fn tls_conn(&mut self) -> &mut ClientConnection {
        self.tls.as_mut().expect("TLS is up")
    }

    /// Send everything the TLS session wants written, in frames the tunnel
    /// takes.
    async fn flush_tls(&mut self) -> Result<(), TunnelError> {
        let mut out = Vec::new();
        {
            let conn = self.tls_conn();
            while conn.wants_write() {
                conn.write_tls(&mut out)
                    .map_err(|e| TunnelError::Tls(e.to_string()))?;
            }
        }
        for chunk in out.chunks(MAX_FRAME_BYTES) {
            self.pipe.send(chunk.to_vec()).await?;
        }
        Ok(())
    }

    /// Hand the TLS session the next ciphertext: what is buffered, else the
    /// next frame. `false` when the tunnel has closed and nothing is left.
    async fn feed_tls(&mut self) -> Result<bool, TunnelError> {
        if self.rx.is_empty() {
            if self.closed {
                return Ok(false);
            }
            match self.pipe.recv().await? {
                Some(frame) => self.rx = frame,
                None => {
                    self.closed = true;
                    return Ok(false);
                }
            }
        }
        let mut cursor: &[u8] = &self.rx;
        let conn = self.tls.as_mut().expect("TLS is up");
        let n = conn
            .read_tls(&mut cursor)
            .map_err(|e| TunnelError::Tls(e.to_string()))?;
        self.rx.drain(..n);
        conn.process_new_packets()
            .map_err(|e| TunnelError::Tls(e.to_string()))?;
        Ok(true)
    }

    /// Plaintext bytes from the far end into `buf`; `Ok(0)` at its close.
    async fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize, TunnelError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.tls.is_none() {
            if self.rx.is_empty() {
                if self.closed {
                    return Ok(0);
                }
                match self.pipe.recv().await? {
                    Some(frame) => self.rx = frame,
                    None => {
                        self.closed = true;
                        return Ok(0);
                    }
                }
            }
            let n = self.rx.len().min(buf.len());
            buf[..n].copy_from_slice(&self.rx[..n]);
            self.rx.drain(..n);
            return Ok(n);
        }
        loop {
            match self.tls_conn().reader().read(buf) {
                Ok(n) => return Ok(n), // 0 = the peer's close_notify
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(TunnelError::Tls(e.to_string())),
            }
            if !self.feed_tls().await? {
                return Ok(0);
            }
            // Post-handshake messages (session tickets, key updates) may
            // want an answer.
            self.flush_tls().await?;
        }
    }

    async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), TunnelError> {
        if self.tls.is_none() {
            for chunk in bytes.chunks(MAX_FRAME_BYTES) {
                self.pipe.send(chunk.to_vec()).await?;
            }
            return Ok(());
        }
        self.tls_conn()
            .writer()
            .write_all(bytes)
            .map_err(|e| TunnelError::Tls(e.to_string()))?;
        self.flush_tls().await
    }
}

impl<P: TunnelPipe> ImapTransport for TunnelTlsTransport<P> {
    type Error = TunnelError;

    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.write_bytes(bytes).await
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.read_bytes(buf).await
    }

    async fn upgrade_tls(&mut self) -> Result<(), ImapClientError> {
        if self.tls.is_some() {
            return Err(ImapClientError::Protocol(
                "STARTTLS attempted on a connection that is already TLS".into(),
            ));
        }
        // § The two TLS modes: bytes the server sent after the STARTTLS `OK`
        // but before TLS came up belong to the plaintext stream; crediting
        // them to the authenticated server is the STARTTLS command-injection
        // class. A well-behaved server never sends any, so refuse.
        if !self.rx.is_empty() {
            return Err(ImapClientError::Protocol(
                "the source sent bytes between STARTTLS and the TLS handshake".into(),
            ));
        }
        self.start_tls().await.map_err(ImapClientError::transport)
    }
}

/// The largest identity-platform answer read back. The device-code and token
/// answers are a few KiB; anything near this is not one of them.
const MAX_OAUTH_RESPONSE_BYTES: usize = 1024 * 1024;

/// The Microsoft sign-in's [`OAuthHttp`] over the tunnel: Microsoft's
/// `/devicecode` and `/token` send no CORS headers, so a browser `fetch`
/// cannot read them, and the POST is written as HTTP/1.1 into a TLS session
/// this client terminates.
///
/// One tunnel per authority host and port is kept across calls (HTTP/1.1
/// keep-alive), so the device-code poll does not open a tunnel per poll; a
/// kept connection the far end has dropped is reopened once.
pub struct TunnelOAuthHttp<D: TunnelDialer> {
    dialer: D,
    config: Arc<ClientConfig>,
    kept: Mutex<Option<KeptConnection<D::Pipe>>>,
}

struct KeptConnection<P> {
    host: String,
    port: u16,
    transport: TunnelTlsTransport<P>,
}

impl<D: TunnelDialer> TunnelOAuthHttp<D> {
    pub fn new(dialer: D, config: Arc<ClientConfig>) -> Self {
        Self {
            dialer,
            config,
            kept: Mutex::new(None),
        }
    }

    fn take_kept(&self, host: &str, port: u16) -> Option<TunnelTlsTransport<D::Pipe>> {
        let mut kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
        match kept.take() {
            Some(k) if k.host == host && k.port == port => Some(k.transport),
            _ => None,
        }
    }

    fn keep(&self, host: &str, port: u16, transport: TunnelTlsTransport<D::Pipe>) {
        *self.kept.lock().unwrap_or_else(|p| p.into_inner()) = Some(KeptConnection {
            host: host.to_string(),
            port,
            transport,
        });
    }
}

impl<D: TunnelDialer> OAuthHttp for TunnelOAuthHttp<D> {
    type Error = TunnelError;

    async fn post_form(&self, url: &str, form: &str) -> Result<OAuthHttpResponse, TunnelError> {
        let target = HttpsTarget::parse(url)?;
        let request = target.post_request(form);
        if let Some(mut transport) = self.take_kept(&target.host, target.port) {
            // A kept connection may have been closed by the far end since the
            // last poll; a failure here before any answer is read is that, and
            // the request goes again on a fresh tunnel.
            if let Ok(Some((response, keep))) = exchange(&mut transport, &request).await {
                if keep {
                    self.keep(&target.host, target.port, transport);
                }
                return Ok(response);
            }
        }
        let mut transport = TunnelTlsTransport::connect(
            &self.dialer,
            Arc::clone(&self.config),
            &target.host,
            target.port,
            TlsMode::Implicit,
        )
        .await?;
        match exchange(&mut transport, &request).await? {
            Some((response, keep)) => {
                if keep {
                    self.keep(&target.host, target.port, transport);
                }
                Ok(response)
            }
            None => Err(TunnelError::Protocol(
                "the identity platform closed without an answer".into(),
            )),
        }
    }
}

/// An `https://` URL split into what the POST needs.
#[derive(Debug, PartialEq, Eq)]
struct HttpsTarget {
    host: String,
    port: u16,
    /// The `Host` header's value (with the port when it is not 443).
    authority: String,
    path: String,
}

impl HttpsTarget {
    /// Only `https://` — a token never crosses a plaintext hop.
    fn parse(url: &str) -> Result<Self, TunnelError> {
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| TunnelError::Protocol(format!("not an https:// URL: {url}")))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        // A bracketed IPv6 literal is never an identity authority; refused.
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port = p
                    .parse::<u16>()
                    .map_err(|_| TunnelError::Protocol(format!("bad port in {url}")))?;
                (h, port)
            }
            None => (authority, PORT_HTTPS),
        };
        if host.is_empty() || host.contains(['@', '[', ']']) {
            return Err(TunnelError::Protocol(format!("bad authority in {url}")));
        }
        Ok(Self {
            host: host.to_string(),
            port,
            authority: authority.to_string(),
            path: path.to_string(),
        })
    }

    fn post_request(&self, form: &str) -> Vec<u8> {
        format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-www-form-urlencoded\r\n\
             Accept: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            self.path,
            self.authority,
            form.len(),
            form
        )
        .into_bytes()
    }
}

/// Write one request and read one response. `Ok(None)` when the connection
/// closed before any byte of an answer (a kept connection gone stale);
/// otherwise the response and whether the connection may be kept.
async fn exchange<P: TunnelPipe>(
    transport: &mut TunnelTlsTransport<P>,
    request: &[u8],
) -> Result<Option<(OAuthHttpResponse, bool)>, TunnelError> {
    transport.write_bytes(request).await?;
    let mut parser = ResponseParser::default();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = transport.read_bytes(&mut buf).await?;
        if n == 0 {
            return match parser.finish_at_eof()? {
                Some(done) => Ok(Some((done, false))),
                None => Ok(None),
            };
        }
        if let Some((response, keep)) = parser.push(&buf[..n])? {
            return Ok(Some((response, keep)));
        }
    }
}

/// An incremental HTTP/1.1 response reader: status line, headers, and a body
/// framed by `Content-Length`, `chunked`, or the close.
#[derive(Default)]
struct ResponseParser {
    data: Vec<u8>,
    head: Option<ResponseHead>,
}

#[derive(Clone, Copy)]
struct ResponseHead {
    status: u16,
    body_start: usize,
    framing: Framing,
    keep_alive: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Framing {
    Length(usize),
    Chunked,
    UntilClose,
}

impl ResponseParser {
    /// Add bytes; the response once it is complete.
    fn push(&mut self, bytes: &[u8]) -> Result<Option<(OAuthHttpResponse, bool)>, TunnelError> {
        self.data.extend_from_slice(bytes);
        if self.data.len() > MAX_OAUTH_RESPONSE_BYTES {
            return Err(TunnelError::Protocol(
                "identity platform answer too large".into(),
            ));
        }
        if self.head.is_none() {
            let Some(end) = find(&self.data, b"\r\n\r\n") else {
                return Ok(None);
            };
            self.head = Some(parse_head(&self.data[..end], end + 4)?);
        }
        let head = self.head.expect("set above");
        let body = &self.data[head.body_start..];
        let complete = match head.framing {
            Framing::Length(n) if body.len() >= n => Some(body[..n].to_vec()),
            Framing::Length(_) | Framing::UntilClose => None,
            Framing::Chunked => decode_chunked(body)?,
        };
        Ok(complete.map(|b| (response(head.status, b), head.keep_alive)))
    }

    /// The close came: complete only for a body framed by it.
    fn finish_at_eof(&mut self) -> Result<Option<OAuthHttpResponse>, TunnelError> {
        match self.head {
            None if self.data.is_empty() => Ok(None),
            Some(head) if head.framing == Framing::UntilClose => Ok(Some(response(
                head.status,
                self.data[head.body_start..].to_vec(),
            ))),
            _ => Err(TunnelError::Protocol(
                "the identity platform closed mid-answer".into(),
            )),
        }
    }
}

fn response(status: u16, body: Vec<u8>) -> OAuthHttpResponse {
    OAuthHttpResponse {
        status,
        body: fauna_core::secret::SecretString::new(String::from_utf8_lossy(&body).into_owned()),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_head(head: &[u8], body_start: usize) -> Result<ResponseHead, TunnelError> {
    let text = std::str::from_utf8(head)
        .map_err(|_| TunnelError::Protocol("non-UTF-8 response head".into()))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|_| version.starts_with("HTTP/1."))
        .ok_or_else(|| TunnelError::Protocol(format!("bad status line: {status_line}")))?;
    let mut framing = Framing::UntilClose;
    let mut keep_alive = version == "HTTP/1.1";
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" if framing != Framing::Chunked => {
                let n = value
                    .parse::<usize>()
                    .map_err(|_| TunnelError::Protocol("bad Content-Length".into()))?;
                framing = Framing::Length(n);
            }
            "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                framing = Framing::Chunked;
            }
            "connection" => {
                let v = value.to_ascii_lowercase();
                if v.contains("close") {
                    keep_alive = false;
                } else if v.contains("keep-alive") {
                    keep_alive = true;
                }
            }
            _ => {}
        }
    }
    if framing == Framing::UntilClose {
        keep_alive = false;
    }
    Ok(ResponseHead {
        status,
        body_start,
        framing,
        keep_alive,
    })
}

/// A complete `chunked` body, or `None` while more is to come.
fn decode_chunked(mut body: &[u8]) -> Result<Option<Vec<u8>>, TunnelError> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = find(body, b"\r\n") else {
            return Ok(None);
        };
        let size_text = std::str::from_utf8(&body[..line_end])
            .map_err(|_| TunnelError::Protocol("bad chunk size".into()))?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| TunnelError::Protocol("bad chunk size".into()))?;
        body = &body[line_end + 2..];
        if size == 0 {
            // Trailers end at an empty line.
            return Ok(find(body, b"\r\n").map(|_| out));
        }
        if body.len() < size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap_client::ImapSession;
    use crate::imap_client::transport::testing::block_on;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    const HOST: &str = "imap.tunnel.test";

    /// The scripted server behind the far end: called once per received line.
    type Responder = Box<dyn FnMut(&mut FarEnd, &str)>;

    /// The far end of an in-memory tunnel: the nest's open exchange, then a
    /// rustls server running a scripted line-at-a-time responder.
    struct FarEnd {
        answer: TunnelAnswer,
        opened: Option<TunnelOpen>,
        tls: Option<rustls::ServerConnection>,
        server_config: Arc<rustls::ServerConfig>,
        /// Plaintext the server has received.
        received: Vec<u8>,
        /// Frames waiting for the client's `recv`.
        to_client: VecDeque<Vec<u8>>,
        respond: Responder,
        /// Before TLS: STARTTLS mode's plaintext phase.
        plaintext: bool,
        closed: bool,
    }

    impl FarEnd {
        fn send_plain(&mut self, bytes: &[u8]) {
            if self.plaintext {
                self.to_client.push_back(bytes.to_vec());
                return;
            }
            let tls = self.tls.as_mut().unwrap();
            tls.writer().write_all(bytes).unwrap();
            let mut out = Vec::new();
            while tls.wants_write() {
                tls.write_tls(&mut out).unwrap();
            }
            self.to_client.push_back(out);
        }

        fn start_tls(&mut self) {
            self.plaintext = false;
            self.tls =
                Some(rustls::ServerConnection::new(Arc::clone(&self.server_config)).unwrap());
        }

        fn on_frame(&mut self, frame: Vec<u8>) {
            if self.opened.is_none() {
                let open: TunnelOpen =
                    fauna_protocol::codec::decode_strict(&frame).expect("first frame is the open");
                self.opened = Some(open.clone());
                let answer = fauna_protocol::codec::encode_canonical(&self.answer).unwrap();
                self.to_client.push_back(answer.to_vec());
                if matches!(self.answer, TunnelAnswer::Opened {}) {
                    if open.starttls {
                        self.plaintext = true;
                        self.send_plain(b"* OK ready\r\n");
                    } else {
                        self.start_tls();
                    }
                }
                return;
            }
            if self.plaintext {
                self.received.extend_from_slice(&frame);
                self.drain_lines();
                return;
            }
            let tls = self.tls.as_mut().unwrap();
            let was_handshaking = tls.is_handshaking();
            let mut cursor: &[u8] = &frame;
            while !cursor.is_empty() {
                tls.read_tls(&mut cursor).unwrap();
                tls.process_new_packets().unwrap();
            }
            let mut plain = Vec::new();
            let _ = tls.reader().read_to_end(&mut plain);
            let mut out = Vec::new();
            while tls.wants_write() {
                tls.write_tls(&mut out).unwrap();
            }
            if !out.is_empty() {
                self.to_client.push_back(out);
            }
            let handshake_done = was_handshaking && !self.tls.as_ref().unwrap().is_handshaking();
            self.received.extend_from_slice(&plain);
            if handshake_done && self.received.is_empty() && self.greet_after_tls() {
                self.send_plain(b"* OK ready\r\n");
            }
            self.drain_lines();
        }

        /// Implicit-TLS IMAP greets once the handshake ends; STARTTLS already
        /// did, and an HTTPS far end (443) speaks only when asked.
        fn greet_after_tls(&self) -> bool {
            let open = self.opened.as_ref().unwrap();
            !open.starttls && open.port != PORT_HTTPS
        }

        fn drain_lines(&mut self) {
            while let Some(i) = find(&self.received, b"\r\n") {
                let line = String::from_utf8_lossy(&self.received[..i]).into_owned();
                self.received.drain(..i + 2);
                let mut respond = std::mem::replace(&mut self.respond, Box::new(|_, _| {}));
                respond(self, &line);
                self.respond = respond;
            }
        }
    }

    #[derive(Clone)]
    struct MemPipe(Rc<RefCell<FarEnd>>);

    impl TunnelPipe for MemPipe {
        async fn send(&mut self, frame: Vec<u8>) -> Result<(), TunnelError> {
            assert!(frame.len() <= MAX_FRAME_BYTES);
            self.0.borrow_mut().on_frame(frame);
            Ok(())
        }

        async fn recv(&mut self) -> Result<Option<Vec<u8>>, TunnelError> {
            let mut far = self.0.borrow_mut();
            match far.to_client.pop_front() {
                Some(f) => Ok(Some(f)),
                None if far.closed => Ok(None),
                None => {
                    far.closed = true;
                    Ok(None)
                }
            }
        }
    }

    struct MemDialer(MemPipe);

    impl TunnelDialer for MemDialer {
        type Pipe = MemPipe;
        async fn dial(&self) -> Result<MemPipe, TunnelError> {
            Ok(self.0.clone())
        }
    }

    struct Certs {
        server: Arc<rustls::ServerConfig>,
        ca_pem: String,
    }

    fn certs(host: &str) -> Certs {
        let cert = rcgen::generate_simple_self_signed(vec![host.to_string()]).unwrap();
        let der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
        let key =
            rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der], key)
        .unwrap();
        Certs {
            server: Arc::new(server),
            ca_pem: cert.cert.pem(),
        }
    }

    fn far_end(
        server: Arc<rustls::ServerConfig>,
        answer: TunnelAnswer,
        respond: impl FnMut(&mut FarEnd, &str) + 'static,
    ) -> MemDialer {
        MemDialer(MemPipe(Rc::new(RefCell::new(FarEnd {
            answer,
            opened: None,
            tls: None,
            server_config: server,
            received: Vec::new(),
            to_client: VecDeque::new(),
            respond: Box::new(respond),
            plaintext: false,
            closed: false,
        }))))
    }

    fn imap_responder(far: &mut FarEnd, line: &str) {
        let (tag, rest) = line.split_once(' ').unwrap();
        let verb = rest.split(' ').next().unwrap().to_ascii_uppercase();
        match verb.as_str() {
            "STARTTLS" => {
                far.send_plain(format!("{tag} OK begin TLS\r\n").as_bytes());
                far.start_tls();
            }
            "CAPABILITY" => {
                far.send_plain(format!("* CAPABILITY IMAP4rev1\r\n{tag} OK done\r\n").as_bytes())
            }
            "LOGIN" => far.send_plain(format!("{tag} OK logged in\r\n").as_bytes()),
            "LIST" => {
                far.send_plain(format!("* LIST () \"/\" INBOX\r\n{tag} OK done\r\n").as_bytes())
            }
            _ => far.send_plain(format!("{tag} BAD unknown\r\n").as_bytes()),
        }
    }

    #[test]
    fn an_implicit_tls_session_signs_in_and_lists_through_the_tunnel() {
        let c = certs(HOST);
        let dialer = far_end(c.server, TunnelAnswer::Opened {}, imap_responder);
        let config = tunnel_client_config(Some(&c.ca_pem)).unwrap();
        block_on(async {
            let transport =
                TunnelTlsTransport::connect(&dialer, config, HOST, 993, TlsMode::Implicit)
                    .await
                    .unwrap();
            let mut session = ImapSession::connect(transport).await.unwrap();
            session.login("user", "secret").await.unwrap();
            let boxes = session.list_mailboxes().await.unwrap();
            assert_eq!(boxes[0].name, "INBOX");
        });
        let far = dialer.0.0.borrow();
        let open = far.opened.as_ref().unwrap();
        assert_eq!(
            (open.host.as_str(), open.port, open.starttls),
            (HOST, 993, false)
        );
    }

    #[test]
    fn a_starttls_session_upgrades_in_place_and_names_the_mode_in_the_open() {
        let c = certs(HOST);
        let dialer = far_end(c.server, TunnelAnswer::Opened {}, imap_responder);
        let config = tunnel_client_config(Some(&c.ca_pem)).unwrap();
        block_on(async {
            let transport =
                TunnelTlsTransport::connect(&dialer, config, HOST, 143, TlsMode::StartTls)
                    .await
                    .unwrap();
            let mut session = ImapSession::connect_starttls(transport).await.unwrap();
            session.login("user", "secret").await.unwrap();
        });
        let far = dialer.0.0.borrow();
        assert!(far.opened.as_ref().unwrap().starttls);
        assert!(far.tls.is_some(), "the server side reached TLS");
    }

    #[test]
    fn a_certificate_outside_the_trust_store_is_refused() {
        let c = certs(HOST);
        let dialer = far_end(c.server, TunnelAnswer::Opened {}, imap_responder);
        // No extra root: the self-signed source is not WebPKI-chained.
        let config = tunnel_client_config(None).unwrap();
        let err = block_on(TunnelTlsTransport::connect(
            &dialer,
            config,
            HOST,
            993,
            TlsMode::Implicit,
        ))
        .err()
        .unwrap();
        assert!(matches!(err, TunnelError::Tls(_)), "{err:?}");
    }

    #[test]
    fn a_refused_open_is_typed_before_any_tls() {
        let c = certs(HOST);
        let dialer = far_end(
            c.server,
            TunnelAnswer::Refused {
                reason: TunnelRefusal::Port,
            },
            imap_responder,
        );
        let config = tunnel_client_config(None).unwrap();
        let err = block_on(TunnelTlsTransport::connect(
            &dialer,
            config,
            HOST,
            25,
            TlsMode::Implicit,
        ))
        .err()
        .unwrap();
        assert_eq!(err, TunnelError::Refused(TunnelRefusal::Port));
    }

    #[test]
    fn bytes_between_the_starttls_ok_and_the_handshake_are_refused() {
        let c = certs(HOST);
        let dialer = far_end(c.server, TunnelAnswer::Opened {}, |far, line| {
            let (tag, _) = line.split_once(' ').unwrap();
            // The injection: plaintext riding behind the OK in one frame.
            far.send_plain(format!("{tag} OK begin TLS\r\n* OK injected\r\n").as_bytes());
        });
        let config = tunnel_client_config(Some(&c.ca_pem)).unwrap();
        let result = block_on(async {
            let transport =
                TunnelTlsTransport::connect(&dialer, config, HOST, 143, TlsMode::StartTls)
                    .await
                    .unwrap();
            ImapSession::connect_starttls(transport).await.map(|_| ())
        });
        assert!(result.is_err());
    }

    const AUTHORITY: &str = "login.tunnel.test";

    fn http_responder(answer: &'static str) -> impl FnMut(&mut FarEnd, &str) + 'static {
        // One answer per request: reply at the head's end (the empty line);
        // the form body has no trailing CRLF so it is never a "line".
        move |far: &mut FarEnd, line: &str| {
            if line.is_empty() {
                far.received.clear();
                far.send_plain(answer.as_bytes());
            }
        }
    }

    #[test]
    fn the_oauth_post_reads_a_length_framed_answer_and_keeps_the_tunnel() {
        let c = certs(AUTHORITY);
        let body = "{\"error\":\"authorization_pending\"}";
        let answer: &'static str = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .leak();
        let dialer = far_end(c.server, TunnelAnswer::Opened {}, http_responder(answer));
        let config = tunnel_client_config(Some(&c.ca_pem)).unwrap();
        let http = TunnelOAuthHttp::new(dialer, config);
        let url = format!("https://{AUTHORITY}/organizations/oauth2/v2.0/token");
        for _ in 0..2 {
            let r = block_on(http.post_form(&url, "grant_type=x")).unwrap();
            assert_eq!(r.status, 400);
            assert_eq!(r.body.as_str(), body);
        }
        let far = http.dialer.0.0.borrow();
        let open = far.opened.as_ref().unwrap();
        assert_eq!(
            (open.host.as_str(), open.port, open.starttls),
            (AUTHORITY, 443, false)
        );
    }

    #[test]
    fn a_chunked_answer_is_reassembled() {
        let body = decode_chunked(b"4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n").unwrap();
        assert_eq!(body.as_deref(), Some(&b"{\"a\":1}"[..]));
        assert_eq!(decode_chunked(b"4\r\n{\"a").unwrap(), None);
    }

    #[test]
    fn the_target_parses_host_port_and_path_and_refuses_plain_http() {
        let t = HttpsTarget::parse("https://127.0.0.1:8443/tenant/oauth2/v2.0/devicecode").unwrap();
        assert_eq!(
            (
                t.host.as_str(),
                t.port,
                t.authority.as_str(),
                t.path.as_str()
            ),
            (
                "127.0.0.1",
                8443,
                "127.0.0.1:8443",
                "/tenant/oauth2/v2.0/devicecode"
            )
        );
        let t = HttpsTarget::parse("https://login.microsoftonline.com/common").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port),
            ("login.microsoftonline.com", 443)
        );
        assert!(HttpsTarget::parse("http://login.microsoftonline.com/").is_err());
    }
}
