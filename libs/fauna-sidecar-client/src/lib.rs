//! The lean one-shot **sidecar dialer**: connect to a nest internal sidecar route,
//! run the `fauna.sidecar.hello` token handshake, and hand back an authenticated,
//! scope-bound [`RpcDispatcher`]. ONE audited copy of the dial+handshake glue,
//! shared by every co-located Rust sidecar that dials nest — today the
//! `fauna-iroh-relay` server binary.
//!
//! **Why a shared crate, not `fauna-ws-substrate`.** `fauna-ws-substrate` owns the
//! substrate-neutral *mechanism* (the tungstenite `Bytes` adapter, keepalive, the
//! capturing TLS verifier for the loopback self-signed hop) and deliberately holds
//! no channel-*protocol* logic — the nest↔nest federation channel's sign-over-CID
//! handshake lives in its consumer (`fauna_nest::federation_channel`), not the
//! substrate. The sidecar `fauna.sidecar.hello` handshake is the same kind of
//! protocol glue, but its consumer is a *separate binary* (the relay depends on
//! shared crates only, NOT `fauna-nest`, so the dialer must live below
//! `fauna-nest`, where nest's own channel tests can drive it too), so its home is
//! a shared crate one layer up.
//!
//! The scope is carried as the **wire string** (`SidecarScope::as_str()`), so this
//! crate has no `fauna-nest` dependency and the relay binary stays lean. Callers
//! that own a `SidecarScope` enum just pass `scope.as_str()`.

/// Source-side half of the sidecar log plane (queue + flush + the
/// disable-on-refusal rule), shared by every Rust sidecar that reports
/// admin-meaningful events. See `observability.md` § The sidecar log plane.
pub mod log_plane;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use fauna_protocol::reconnect::Backoff;
use fauna_protocol::sidecar::{KIND_SIDECAR_HELLO, SidecarHello};
use fauna_protocol::{RpcDispatcher, Value};
use fauna_ws_substrate::{
    KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, TungsteniteAdapter, capturing_client_config,
    ensure_tls_provider,
};

/// Deadline for each one-shot sidecar exchange (the hello, or a post-handshake
/// one-shot fetch) — a fast co-located loopback RPC.
pub const SIDECAR_REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// Deadline for the WS/TLS **connect** phase itself — the TCP handshake through
/// the WS upgrade, before any request is sent. The same shape as
/// `fauna_nest::federation_channel::CONNECT_DEADLINE`, and a separate constant
/// from [`SIDECAR_REQUEST_DEADLINE`] because it bounds a different phase (a
/// call that never got as far as a request has nothing for that deadline to
/// apply to; `transport.md` § Request lifecycle, `:274`). Bounding it matters
/// because [`retry_while_reaching_nest`] / [`serve_while_reaching_nest`] retry
/// in-process "for as long as [nest's] failures are ... simply not being
/// reachable" (`transport.md` § Future directions, `:1387-1389`) — a policy
/// that can only converge if each attempt is itself bounded, since an
/// unbounded connect against a nest that accepts TCP but never completes the
/// upgrade would hang the retry loop forever on its first attempt.
pub const SIDECAR_CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// Why a sidecar dial / handshake / one-shot request failed. None are recoverable
/// in-band — the caller retries the whole dial (the sidecars do, with backoff,
/// through [`retry_while_reaching_nest`] / [`serve_while_reaching_nest`]).
#[derive(Debug, thiserror::Error)]
pub enum SidecarDialError {
    /// The WS/TLS connection to nest could not be **established** — nest is not
    /// listening on the loopback port yet (still booting, or restarting), or the
    /// connect attempt ran past [`SIDECAR_CONNECT_DEADLINE`] without completing
    /// (nest accepted the TCP connection but never finished the WS upgrade). Our
    /// credential is not implicated, so this is the one failure class a sidecar
    /// retries in-process forever; see [`SidecarDialError::credential_implicated`].
    #[error("sidecar connect to nest failed: {0}")]
    Connect(String),
    /// A transport-level failure **after** the connection was established (a send
    /// on the open socket) — no authenticated reply.
    #[error("sidecar transport error: {0}")]
    Transport(String),
    /// The nest replied with an error (`ok = false`); the payload carries the
    /// `RpcError` code (e.g. `fauna.protocol.unauthenticated` for a bad token).
    #[error("sidecar request rejected by nest: {0}")]
    Rejected(String),
    /// A payload could not be (de)serialized to/from the dispatcher `Value` form.
    #[error("sidecar payload codec error: {0}")]
    Codec(String),
}

/// Whether a sidecar's failure could mean the credential it is holding is stale.
/// Implemented by [`SidecarDialError`] and by any richer per-sidecar error that
/// wraps it (the relay's cert fetch also fails for reasons *after* the handshake,
/// which prove the credential good), so both share one retry policy —
/// [`retry_while_reaching_nest`] / [`serve_while_reaching_nest`].
pub trait CredentialVerdict {
    /// `true` ⇒ only a restart can refresh what we hold; `false` ⇒ retry in-process.
    fn credential_implicated(&self) -> bool;
}

impl CredentialVerdict for SidecarDialError {
    /// Whether this failure could mean **our token is stale** — i.e. the socket to
    /// nest opened, so nest is up, and what failed is the `fauna.sidecar.hello`.
    ///
    /// Only [`SidecarDialError::Connect`] clears the credential: nest was not
    /// reachable at all, so it never judged our token. Everything else is
    /// credential-implicated, deliberately including a bare
    /// `fauna.protocol.disconnected`. Being wrong in this direction costs one
    /// supervised restart; being wrong the other way costs the process's whole
    /// remaining lifetime (`transport.md` § Future directions → the sidecar
    /// channel's credential lifetime).
    ///
    /// **Unchanged 2026-08-30, but for a narrower reason than it was written
    /// with.** This used to hold because a nest listener's teardown could outrun
    /// the `unauthenticated` frame it had queued, so a refusal and a
    /// mid-handshake drop were genuinely the same event on the wire. That is
    /// fixed — a nest rejection now arrives as its real code (`transport.md`
    /// § Layers, the L3 driver-lifetime rule). A real
    /// mid-handshake network drop still arrives as `disconnected` and is still
    /// indistinguishable from a refusal *to this verdict*, so the treat-alike
    /// rule stands on its own asymmetry argument above. Do not narrow it on the
    /// strength of the fix.
    ///
    /// ⚠ This verdict is only sound for the **dial**. A failure of a request sent
    /// *after* a successful handshake proves the opposite — nest accepted the
    /// token — so a caller that keeps using the channel must classify those
    /// separately rather than reusing this impl (the relay does; see its
    /// `RelayCertError`).
    fn credential_implicated(&self) -> bool {
        !matches!(self, SidecarDialError::Connect(_))
    }
}

/// A sidecar's `FAUNA_SIDECAR_TOKEN` is no longer the one nest honours, so the
/// process must **end**: the token is read once, at start, by the s6 run-script
/// (as root — the relay UID may never read the 0600 token file itself,
/// `security.md` § UID isolation), so nothing inside this process can refresh it.
/// Exiting hands that job to the supervisor, whose restart re-reads the file.
///
/// The nest re-mints every sidecar token on **each boot**
/// (`bins/fauna-nest/src/sidecar_tokens.rs::generate_sidecar_tokens`), so any nest
/// restart strands every already-running sidecar on a token nest has forgotten —
/// which is why this is a routine, expected end for a sidecar process rather than
/// a deployment error.
#[derive(Debug, thiserror::Error)]
#[error(
    "nest refused this sidecar's credential ({0}); ending the process so the \
     supervisor's restart re-reads the token file"
)]
pub struct CredentialRefused(pub String);

/// Exit status a sidecar binary uses for [`CredentialRefused`] — `EX_CONFIG` from
/// `sysexits.h`. Distinct from a crash so `docker logs` reads honestly: the
/// process ended on purpose, to be restarted with a fresh token.
pub const EXIT_CREDENTIAL_REFUSED: i32 = 78;

/// The floor of every sidecar's dial backoff — the first retry after a failure.
pub const DIAL_BACKOFF_FLOOR: Duration = Duration::from_millis(500);

/// The ceiling of every sidecar's dial backoff — how long a sidecar waits between
/// dials once nest has been unreachable for a while.
pub const DIAL_BACKOFF_CEILING: Duration = Duration::from_secs(30);

/// Retry `attempt` with capped exponential backoff **for as long as nest is simply
/// not reachable**, and return its first success.
///
/// The retry ends the moment a failure is credential-implicated
/// ([`SidecarDialError::credential_implicated`]): our token is a start-time value,
/// so retrying it is retrying the same rejected bytes forever. Returning
/// [`CredentialRefused`] lets the binary exit and the supervisor restart it with
/// the token file's current contents.
///
/// This is the one shared retry policy for every co-located Rust sidecar — the
/// same reason the dial+handshake glue itself lives here.
/// `on_retry` observes each retried failure and the delay before the next attempt
/// — the seam a sidecar reports the retry on its log plane through
/// (`apps/observability.md` § The sidecar log plane). Pass `|_, _| {}` for a
/// sidecar whose catalogue is not written yet.
pub async fn retry_while_reaching_nest<T, E, F, Fut, R>(
    mut attempt: F,
    mut on_retry: R,
) -> Result<T, CredentialRefused>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: CredentialVerdict + std::fmt::Display,
    R: FnMut(&E, Duration),
{
    let mut backoff = Backoff::new(DIAL_BACKOFF_FLOOR, DIAL_BACKOFF_CEILING);
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(e) if e.credential_implicated() => return Err(CredentialRefused(e.to_string())),
            Err(e) => {
                let delay = backoff.ceiling();
                warn_unreachable(&e, delay);
                on_retry(&e, delay);
                tokio::time::sleep(delay).await;
                backoff.grow();
            }
        }
    }
}

/// The long-lived twin of [`retry_while_reaching_nest`]: a success means *the
/// channel served and then closed*, so the sidecar dials again (with the backoff
/// reset) rather than returning. Only [`CredentialRefused`] ends it.
pub async fn serve_while_reaching_nest<E, F, Fut, R>(
    mut attempt: F,
    mut on_retry: R,
) -> CredentialRefused
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
    E: CredentialVerdict + std::fmt::Display,
    R: FnMut(&E, Duration),
{
    let mut backoff = Backoff::new(DIAL_BACKOFF_FLOOR, DIAL_BACKOFF_CEILING);
    loop {
        match attempt().await {
            Ok(()) => {
                tracing::warn!("sidecar channel closed by nest; reconnecting");
                // A serve that ended is a dial we are about to repeat, so it pays
                // the floor. Nothing else rate-limits this arm: `attempt` returns
                // `Ok` as soon as the inbound receiver closes, so a channel nest
                // drops the instant it opens would re-dial — TLS connect, hello,
                // teardown — at full speed. The loop this replaced slept
                // unconditionally, *after* the match, which covered the `Ok` arm
                // too; keeping the floor here restores that guarantee exactly.
                backoff.reset();
                tokio::time::sleep(backoff.ceiling()).await;
            }
            Err(e) if e.credential_implicated() => return CredentialRefused(e.to_string()),
            Err(e) => {
                let delay = backoff.ceiling();
                warn_unreachable(&e, delay);
                on_retry(&e, delay);
                tokio::time::sleep(delay).await;
                backoff.grow();
            }
        }
    }
}

/// The one dial-retry log line, shared by both loops above.
fn warn_unreachable(e: &impl std::fmt::Display, backoff: Duration) {
    tracing::warn!(
        error = %e,
        backoff_ms = backoff.as_millis() as u64,
        "sidecar could not reach nest; retrying"
    );
}

/// An authenticated, scope-bound sidecar channel: the driven [`RpcDispatcher`] plus
/// the `JoinHandle` of its driver task. The caller drives the dispatcher (a
/// one-shot originate, or a long-lived serve loop) and `abort()`s `driver` when the
/// exchange is done.
pub struct SidecarDial {
    /// The handshake-authenticated dispatcher, bound to the requested scope.
    pub dispatcher: Arc<RpcDispatcher>,
    /// The dispatcher's driver task — abort it when finished with the channel.
    pub driver: JoinHandle<()>,
}

/// Derive nest's `ws|wss://…<route>` channel URL from its HTTP base. `route` is the
/// absolute path of the sidecar's internal listener (e.g.
/// `/internal/relay/ws`). `http`→`ws`, `https`→`wss`; a trailing `/` on the base is
/// trimmed so the join never doubles the slash.
pub fn sidecar_ws_url(nest_base: &str, route: &str) -> String {
    let base = fauna_core::web::http_to_ws(nest_base);
    format!("{}{}", base.trim_end_matches('/'), route)
}

/// Open the WS connection to a nest sidecar route. **Scheme-aware.** A deploy nest
/// with a configured domain serves HTTPS on its bind port — a real ACME cert, or a
/// self-signed bootstrap cert for a localhost/LAN/ACME-off box — so the co-located
/// sidecar dials `wss://` over loopback and provisionally accepts the served
/// self-signed cert via the shared capturing TLS verifier (the same loopback-https
/// hop the federation dialer + Go mail bridge take). A no-domain plain-HTTP nest is
/// dialed over plain `ws://`. The channel authenticates by the `fauna.sidecar.hello`
/// token over the co-located loopback hop, not by a WebPKI chain — so, unlike
/// federation, the captured SPKI is not read/bound.
pub async fn connect_sidecar_ws(ws_url: &str) -> Result<TungsteniteAdapter, SidecarDialError> {
    let request = ws_url
        .into_client_request()
        .map_err(|e| SidecarDialError::Transport(format!("invalid ws url {ws_url:?}: {e}")))?;
    let is_wss = ws_url.starts_with("wss://");

    // The whole TCP connect + TLS/WS-upgrade phase (whichever branch runs
    // below) is wrapped in one `SIDECAR_CONNECT_DEADLINE` bound from the
    // outside — deliberately external to `tokio_tungstenite`'s own connect
    // futures, none of which take a deadline — so bounding must happen at this
    // call site regardless of what either branch does internally (the
    // identical shape `fauna_nest::federation_channel::connect_federation_ws`
    // uses for its own connect phase).
    // Cap inbound message/frame size symmetric with the nest listener's 2 MiB
    // sidecar-channel limit (the shared native WS-RPC cap) — tungstenite's
    // 64 MiB / 16 MiB default would let the peer force unbounded buffering.
    let ws_config = fauna_ws_substrate::rpc_ws_config();
    let connect = async move {
        if is_wss {
            ensure_tls_provider();
            let (config, _capture) = capturing_client_config();
            let connector = tokio_tungstenite::Connector::Rustls(config);
            let (ws_stream, _resp) = tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(ws_config),
                false,
                Some(connector),
            )
            .await
            .map_err(|e| SidecarDialError::Connect(format!("wss connect to nest: {e}")))?;
            return Ok(TungsteniteAdapter::new(
                ws_stream,
                KEEPALIVE_INTERVAL,
                KEEPALIVE_TIMEOUT,
            ));
        }

        let (ws_stream, _resp) =
            tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false)
                .await
                .map_err(|e| SidecarDialError::Connect(format!("ws connect to nest: {e}")))?;
        Ok(TungsteniteAdapter::new(
            ws_stream,
            KEEPALIVE_INTERVAL,
            KEEPALIVE_TIMEOUT,
        ))
    };

    match tokio::time::timeout(SIDECAR_CONNECT_DEADLINE, connect).await {
        Ok(result) => result,
        Err(_elapsed) => Err(SidecarDialError::Connect(format!(
            "sidecar connect to nest timed out after {SIDECAR_CONNECT_DEADLINE:?}"
        ))),
    }
}

/// Run the `fauna.sidecar.hello` token handshake over an already-established
/// dispatcher: originate the hello (declaring `scope`, attesting `extra`) and await
/// the nest's reply. `Ok(())` ⇒ the channel is authenticated + scope-bound and the
/// caller may originate / serve over the same duplex. `scope` is the wire scope
/// string (`SidecarScope::as_str()`); `extra` carries scope-specific handshake
/// attestations (the relay passes its X25519 public key under
/// `fauna_protocol::relay::HELLO_EXTRA_X25519` — the seal recipient).
pub async fn sidecar_hello(
    dispatcher: &Arc<RpcDispatcher>,
    token: &str,
    scope: &str,
    extra: BTreeMap<String, Value>,
) -> Result<(), SidecarDialError> {
    let hello = SidecarHello {
        token: token.to_string(),
        scope: scope.to_string(),
        extra,
    };
    // The reply carries nothing — reaching it *is* the handshake's success — so
    // it is decoded as the generic node and dropped.
    let _: Value = one_shot(dispatcher, KIND_SIDECAR_HELLO, &hello).await?;
    Ok(())
}

/// One typed exchange over an authenticated sidecar dispatcher, bounded by
/// [`SIDECAR_REQUEST_DEADLINE`] at both ends.
///
/// The ceremony itself is [`RpcDispatcher::request_typed`], shared with every
/// other fauna transport; what is this crate's own is the fixed deadline, the
/// per-call random key (these are one-shot exchanges on a fresh connection), and
/// the mapping onto [`SidecarDialError`]. The local backstop is armed at the
/// same duration the wire deadline already declares, so it can only fire against
/// a sidecar that ignored its own deadline — before, such a peer hung the dial
/// forever.
async fn one_shot<Req, Rep>(
    dispatcher: &Arc<RpcDispatcher>,
    kind: &str,
    req: &Req,
) -> Result<Rep, SidecarDialError>
where
    Req: serde::Serialize,
    Rep: serde::de::DeserializeOwned,
{
    dispatcher
        .request_typed(
            kind,
            fresh_idem(),
            req,
            SIDECAR_REQUEST_DEADLINE,
            tokio::time::sleep(SIDECAR_REQUEST_DEADLINE),
        )
        .await
        .map_err(|e| {
            use fauna_protocol::TypedRequestError as T;
            match e {
                T::Codec(msg) => SidecarDialError::Codec(msg),
                T::Dispatch(e) => SidecarDialError::Transport(format!("{kind} send failed: {e}")),
                // A mid-flight drop reaches this crate the way it always has:
                // as the dispatcher's own synthesised code, undistinguished from
                // a sidecar that refused.
                T::Disconnected => {
                    SidecarDialError::Rejected(fauna_protocol::DISCONNECTED_CODE.to_string())
                }
                T::Rpc(e) => SidecarDialError::Rejected(e.code),
                T::Timeout => {
                    SidecarDialError::Transport(format!("{kind} timed out after the deadline"))
                }
            }
        })
}

/// Dial a nest sidecar route end-to-end: derive the URL, open the (scheme-aware) WS
/// connection, set up the dispatcher + spawn its driver, and run the
/// `fauna.sidecar.hello` handshake. On success the returned [`SidecarDial`]'s
/// dispatcher is authenticated + scope-bound. On any failure the driver task is
/// aborted before returning, so a failed dial never leaks a task.
///
/// `nest_base` is nest's HTTP base (e.g. `http://127.0.0.1:3000`); `route` is the
/// sidecar's internal listener path; `scope`/`token`/`extra` are as in
/// [`sidecar_hello`].
pub async fn dial_sidecar(
    nest_base: &str,
    route: &str,
    token: &str,
    scope: &str,
    extra: BTreeMap<String, Value>,
) -> Result<SidecarDial, SidecarDialError> {
    let ws_url = sidecar_ws_url(nest_base, route);
    let adapter = connect_sidecar_ws(&ws_url).await?;
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    let dispatcher = Arc::new(dispatcher);
    let driver = tokio::spawn(driver);

    match sidecar_hello(&dispatcher, token, scope, extra).await {
        Ok(()) => Ok(SidecarDial { dispatcher, driver }),
        Err(e) => {
            driver.abort();
            Err(e)
        }
    }
}

/// Originate ONE typed request over an authenticated dispatcher and decode the
/// typed reply — a lean one-shot sidecar exchange after the handshake (e.g. the
/// relay's `fauna.relay.fetch_tls_cert`): the dial-then-fetch-then-drop shape.
pub async fn request_typed<Req, Rep>(
    dispatcher: &Arc<RpcDispatcher>,
    kind: &str,
    req: &Req,
) -> Result<Rep, SidecarDialError>
where
    Req: serde::Serialize,
    Rep: serde::de::DeserializeOwned,
{
    one_shot(dispatcher, kind, req).await
}

// ── idempotency helper ───────────────────────────────────────────────────────

/// A fresh random 16-byte idempotency key for an originated request. The sidecar
/// dial exchanges are one-shot per fresh connection, so a per-call random key is
/// sufficient.
fn fresh_idem() -> [u8; 16] {
    let mut key = [0u8; 16];
    getrandom::fill(&mut key).expect("getrandom failed");
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::{decode_strict, encode_canonical};

    #[test]
    fn ws_url_derivation_maps_scheme_and_trims() {
        assert_eq!(
            sidecar_ws_url("http://127.0.0.1:3000", "/internal/relay/ws"),
            "ws://127.0.0.1:3000/internal/relay/ws"
        );
        assert_eq!(
            sidecar_ws_url("https://nest.example.com/", "/internal/relay/ws"),
            "wss://nest.example.com/internal/relay/ws"
        );
        // No domain, plain HTTP, no trailing slash.
        assert_eq!(
            sidecar_ws_url("http://127.0.0.1:3000/", "/internal/relay/ws"),
            "ws://127.0.0.1:3000/internal/relay/ws"
        );
    }

    /// The typed → `Value` → typed round trip this crate's exchanges ride on.
    /// It is `fauna-protocol`'s since the shared request path took it over, so
    /// the assertion follows it there rather than to a local copy that no longer
    /// exists.
    #[test]
    fn hello_round_trips_through_the_shared_payload_encoding() {
        let hello = SidecarHello {
            token: "tok".into(),
            scope: "relay".into(),
            extra: Default::default(),
        };
        let v = fauna_protocol::encode_payload(&hello).expect("encode");
        let bytes = encode_canonical(&v).expect("re-encode");
        let back: SidecarHello = decode_strict(&bytes).expect("decode");
        assert_eq!(back.token, "tok");
        assert_eq!(back.scope, "relay");
    }

    #[test]
    fn fresh_idem_is_nonzero_and_varies() {
        let a = fresh_idem();
        let b = fresh_idem();
        assert_ne!(a, [0u8; 16]);
        assert_ne!(a, b, "two fresh idempotency keys should differ");
    }

    // ── The credential lifetime: unreachable ≠ refused ───────────────────────
    //
    // The whole convergence property rests on this one split, so it is asserted
    // directly rather than only through the two loops below.

    #[test]
    fn only_a_failed_connect_clears_the_credential() {
        assert!(
            !SidecarDialError::Connect("connection refused".into()).credential_implicated(),
            "nest was never reached, so it never judged our token — keep retrying"
        );
        // A refused hello arrives as `unauthenticated`…
        assert!(
            SidecarDialError::Rejected("fauna.protocol.unauthenticated".into())
                .credential_implicated()
        );
        // …or, when the listener's teardown outruns the error frame it queued, as a
        // bare disconnect. Indistinguishable on the wire ⇒ treated alike.
        assert!(
            SidecarDialError::Rejected("fauna.protocol.disconnected".into())
                .credential_implicated(),
            "a refusal lost to the listener's teardown must still end the process"
        );
        assert!(SidecarDialError::Transport("hello send failed".into()).credential_implicated());
        assert!(SidecarDialError::Codec("to Value".into()).credential_implicated());
    }

    /// Nest not listening yet (the relay is enabled in `services.json` before nest
    /// finished booting): the sidecar keeps dialing and succeeds when nest arrives.
    /// Deterministic — the paused clock auto-advances the backoff sleeps, so the
    /// test asserts the *sequence*, never elapsed time.
    #[tokio::test(start_paused = true)]
    async fn an_unreachable_nest_is_retried_until_it_answers() {
        let attempts = std::cell::Cell::new(0u32);
        let got = retry_while_reaching_nest(
            || {
                let n = attempts.get() + 1;
                attempts.set(n);
                async move {
                    if n < 4 {
                        Err(SidecarDialError::Connect("connection refused".into()))
                    } else {
                        Ok(n)
                    }
                }
            },
            |_, _| {},
        )
        .await
        .expect("an unreachable nest must never end the process");
        assert_eq!(got, 4);
        assert_eq!(attempts.get(), 4, "three retries, then the success");
    }

    /// The bug this policy exists for: nest re-minted its tokens (it restarted),
    /// so our start-time token is refused. Retrying it re-sends the same rejected
    /// bytes forever — the loop must END so the supervisor restarts us with the
    /// token file's current contents.
    #[tokio::test(start_paused = true)]
    async fn a_refused_credential_ends_the_retry_on_the_first_refusal() {
        let attempts = std::cell::Cell::new(0u32);
        let refused = retry_while_reaching_nest::<(), _, _, _, _>(
            || {
                attempts.set(attempts.get() + 1);
                async {
                    Err(SidecarDialError::Rejected(
                        "fauna.protocol.unauthenticated".into(),
                    ))
                }
            },
            |_, _| unreachable!("a refused credential is never retried"),
        )
        .await
        .expect_err("a refused credential must end the retry");
        assert_eq!(attempts.get(), 1, "no retry of a token nest has forgotten");
        assert!(
            refused.0.contains("unauthenticated"),
            "the refusal reason is carried to the exit line: {refused}"
        );
    }

    /// The long-lived twin: a clean channel close is a reconnect, a refusal is the
    /// end of the process.
    #[tokio::test(start_paused = true)]
    async fn a_served_channel_reconnects_but_a_refusal_ends_the_serve_loop() {
        let attempts = std::cell::Cell::new(0u32);
        let retried = std::cell::Cell::new(0u32);
        let refused = serve_while_reaching_nest(
            || {
                let n = attempts.get() + 1;
                attempts.set(n);
                async move {
                    match n {
                        // Served, then nest closed the socket — dial again.
                        1 => Ok(()),
                        // Nest is restarting: not reachable, so keep trying.
                        2 => Err(SidecarDialError::Connect("connection refused".into())),
                        // Nest is back — with a token map that no longer holds ours.
                        _ => Err(SidecarDialError::Rejected(
                            "fauna.protocol.disconnected".into(),
                        )),
                    }
                }
            },
            |_, _| retried.set(retried.get() + 1),
        )
        .await;
        assert_eq!(attempts.get(), 3);
        assert_eq!(
            retried.get(),
            1,
            "only the unreachable attempt is a retry the sidecar reports"
        );
        assert!(refused.0.contains("disconnected"));
    }

    /// `connect_sidecar_ws` itself — the TCP/WS-upgrade phase, one step before
    /// the `fauna.sidecar.hello` handshake — must not hang forever either. The
    /// identical unbounded shape was fixed in
    /// `fauna_nest::federation_channel::connect_federation_ws`
    /// (`connect_federation_ws_bounds_a_blackholed_peer` is the model for this
    /// test); this crate's own dial had the same gap until now. Exercises the
    /// plain `ws://` branch — no TLS needed to reproduce the missing bound.
    #[tokio::test(start_paused = true)]
    async fn connect_sidecar_ws_bounds_a_blackholed_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral listener");
        let addr = listener.local_addr().expect("listener local_addr");

        // Accept the TCP connection but never write the WS-upgrade response —
        // a nest that accepted the connection but hasn't finished booting, not
        // a fast connection-refused.
        // spawn-ok(test)
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            std::future::pending::<()>().await;
            drop(stream); // unreachable — keeps `stream` alive for the compiler
        });

        let ws_url = format!("ws://{addr}/internal/relay/ws");
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(120), connect_sidecar_ws(&ws_url))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "connect_sidecar_ws must not hang past SIDECAR_CONNECT_DEADLINE \
                     ({SIDECAR_CONNECT_DEADLINE:?}) when the peer withholds the WS upgrade"
                )
            });
        let elapsed = started.elapsed();

        // `result`'s `Ok` side (`TungsteniteAdapter`) isn't `Debug`, so match
        // instead of formatting the whole `Result` via `expect_err`.
        match &result {
            Err(SidecarDialError::Connect(_)) => {}
            Err(other) => panic!(
                "a connect timeout must map to SidecarDialError::Connect — it is the \
                 only variant `credential_implicated()` treats as nest-unreachable, \
                 so a sidecar keeps retrying it in-process instead of ending the \
                 process over a merely slow nest: got {other:?}"
            ),
            Ok(_) => panic!(
                "expected a connect-timeout error, but connect succeeded — the \
                 blackholed listener didn't actually block the WS upgrade"
            ),
        }
        assert!(
            elapsed >= SIDECAR_CONNECT_DEADLINE,
            "timeout fired before the deadline: {elapsed:?} < {SIDECAR_CONNECT_DEADLINE:?}"
        );
        // A lower bound alone also passes a regression to a longer bound (the
        // 120s outer ceiling, or a fresh timer) — pin the upper side too, with
        // a small tolerance for the connect future's own select overhead.
        assert!(
            elapsed < SIDECAR_CONNECT_DEADLINE + Duration::from_millis(20),
            "timeout fired well after the deadline: {elapsed:?} >= \
             {SIDECAR_CONNECT_DEADLINE:?} + 20ms"
        );
    }

    /// A reconnect is never free. The loop this policy replaced slept
    /// unconditionally — after the `Ok` arm too — so a channel nest closed the
    /// instant it opened still cost one [`DIAL_BACKOFF_FLOOR`] per dial. Nothing
    /// else rate-limits the re-dial, and the sequence assertions above cannot see
    /// a missing delay, so the floor is asserted directly, in clock time.
    #[tokio::test(start_paused = true)]
    async fn a_channel_that_closes_at_once_still_pays_the_backoff_floor() {
        const CLOSES: u32 = 5;
        let attempts = std::cell::Cell::new(0u32);
        let start = tokio::time::Instant::now();
        let _ended = serve_while_reaching_nest(
            || {
                let n = attempts.get() + 1;
                attempts.set(n);
                async move {
                    if n <= CLOSES {
                        // Served, then nest closed the socket at once.
                        Ok(())
                    } else {
                        Err(SidecarDialError::Rejected(
                            "fauna.protocol.unauthenticated".into(),
                        ))
                    }
                }
            },
            |_, _| {},
        )
        .await;
        let elapsed = tokio::time::Instant::now() - start;
        assert!(
            elapsed >= DIAL_BACKOFF_FLOOR * CLOSES,
            "{CLOSES} reconnects must wait at least {CLOSES}×{DIAL_BACKOFF_FLOOR:?}, \
             not {elapsed:?} — an immediately-closed channel would otherwise re-dial \
             at full speed, unthrottled"
        );
    }
}
