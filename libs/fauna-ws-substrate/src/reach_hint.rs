//! The process-wide **reach table** — the session connector's half of the
//! reach hint's dial rule (`docs/goal/behavior/onboarding-provisioning.md`
//! § Reach hint → *The session connector's half*).
//!
//! The launch machine dials a freshly-provisioned box by its reach hint when
//! the account's domain does not resolve yet, and lands the app `Online`. The
//! session that then opens — the bearer mint and the supervised bearer socket,
//! both in `fauna-client` / `fauna-anon-client` — used to dial the domain and
//! nothing else, so an account the hint opened reached a mounted app whose
//! session never connected until the zone published. This table carries the
//! launch's decision to every later native dial in the process:
//!
//! - **The launch machine installs** ([`install`]) the hint — the socket
//!   address and a deleter for the persisted slot — only when the hint carried
//!   the launch, and clears the entry ([`clear`]) at the start of every launch
//!   phase, so the phase's own domain-first dial is never answered from here
//!   and a stale entry never outlives the launch that would have re-decided it.
//! - **Every native dial consults it on a reachability failure**
//!   ([`hint_for`]): the pre-identity connect (`connect_anonymous`) and the
//!   trusted bearer dial (`dial_ws_trusted`) dial the domain first, as the rule
//!   says, and only when that fails to *connect* ([`is_reachability_failure`]:
//!   unresolvable, refused, timed out — never a nest that answered and never a
//!   TLS or HTTP verdict) open a socket to the hint instead. The request is the
//!   same object on both attempts, so SNI, `Host`, the demanded cert and the
//!   pin key all stay the domain's — the hint moves the socket and nothing
//!   else, exactly as on the launch's own hint dial (`transport-trust.md`
//!   § Transport trust, the reach hint keys on the domain).
//! - **The first bearer socket that opens over the domain deletes the hint**
//!   ([`domain_reached`]): the trusted dial reports a domain success, the entry
//!   leaves the table and its deleter removes the persisted slot — the rule's
//!   "the first successful domain dial deletes the hint", now on the session's
//!   side too. A domain success is a completed WS upgrade under the trusted
//!   dial's own policy (the graduated pin or strict WebPKI), so the box that
//!   answered is the pinned one; a hint dial that fails deletes nothing (a
//!   failed fallback is not a wrong address).
//!
//! **Keyed on the domain, lowercased, port-free** ([`host_key_of_uri`]): the
//! hint is a fact about one domain, the port on every dial is the request's
//! own, and an HTTP resolver (`fauna-client`'s `pinned_http_client`) sees a
//! bare name. Process-global for the same reason the dial budget is: the
//! session client is built from a URL and a secret on every app
//! (`FfiNestClient::new`), nothing threads a launch-time fact through seven
//! apps' glue, and the rule says shared Rust, never per-app glue. One live
//! session per domain per process is the shape every app has; the deleter runs
//! on whatever the launch persistence then names as the active account, which
//! a session that outlived an account switch could misdirect — such a session
//! is itself the bug `transport-connection.md` § *No dialer outlives its
//! owner* forbids, so the table does not guard against it twice.
//!
//! A shipped build carries this table too: it is production behaviour, not an
//! automation seam — a real account on a real dark domain is the case it
//! serves.
//!
//! **No proof rides in an entry here**, unlike the page table's one-shot proof
//! per bearer (`fauna_rpc_wasm::reach_hint`): natively every dial over the hint,
//! the bearer socket included, is held at its own handshake to the domain's
//! identity — the graduated SPKI pin, or WebPKI for the domain's own name — and
//! that is the per-connection proof the browser cannot run.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use tokio_tungstenite::tungstenite::http::Uri;

/// What a domain success runs: the launch persistence's deletion of the
/// persisted hint, bound at install time so this crate never learns what a
/// long-term store is.
pub type OnDomainReached = Arc<dyn Fn() + Send + Sync>;

struct Entry {
    addr: SocketAddr,
    on_domain_reached: OnDomainReached,
}

fn table() -> &'static Mutex<HashMap<String, Entry>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> {
    table().lock().unwrap_or_else(|p| p.into_inner())
}

/// The table key for a dial: the request's host, lowercased, no port — the
/// domain the hint stands in for.
pub fn host_key_of_uri(uri: &Uri) -> String {
    uri.host().unwrap_or_default().to_ascii_lowercase()
}

/// The table key for a nest URL (`https://nest.example[:port]`), `None` when
/// the string parses to no host.
pub fn host_key_of_url(nest_url: &str) -> Option<String> {
    let uri: Uri = nest_url.parse().ok()?;
    let host = host_key_of_uri(&uri);
    (!host.is_empty()).then_some(host)
}

/// Install `addr` as the reach hint for `host`, replacing any entry it had.
/// `on_domain_reached` runs once, when a bearer socket opens over the domain
/// ([`domain_reached`]).
pub fn install(host: &str, addr: SocketAddr, on_domain_reached: OnDomainReached) {
    lock().insert(
        host.to_ascii_lowercase(),
        Entry {
            addr,
            on_domain_reached,
        },
    );
}

/// Forget host's hint without running its deleter — the launch phase's
/// opening move, so its own domain dial takes no fallback from here.
pub fn clear(host: &str) {
    lock().remove(&host.to_ascii_lowercase());
}

/// The socket a dial to `host` falls back to after a reachability failure.
pub fn hint_for(host: &str) -> Option<SocketAddr> {
    lock().get(&host.to_ascii_lowercase()).map(|e| e.addr)
}

/// A bearer socket opened over `host` itself: the hint is spent. Removes the
/// entry and runs its deleter — outside the lock, since the deleter writes the
/// long-term store. A host with no entry is the common case and costs a map
/// lookup.
pub fn domain_reached(host: &str) {
    let spent = lock().remove(&host.to_ascii_lowercase());
    if let Some(entry) = spent {
        tracing::info!(
            host,
            "the domain reached the nest; the reach hint is deleted"
        );
        (entry.on_domain_reached)();
    }
}

/// Whether a failed dial failed to *reach* anything — the one failure class the
/// hint answers. An I/O error is the socket never opening (name resolution,
/// `ECONNREFUSED`, a timeout, a reset during the handshake); a TLS verdict, an
/// HTTP refusal (`401`/`426`/`429`) or a protocol error is a box that answered,
/// and the rule says a second dial would then ask a question already answered
/// — or, for a pin refusal, re-ask the one the pin exists to refuse.
pub fn is_reachability_failure(err: &tokio_tungstenite::tungstenite::Error) -> bool {
    matches!(err, tokio_tungstenite::tungstenite::Error::Io(_))
}

/// Empty the whole table — the e2e flavors' per-test factory reset, beside
/// `dial_budget::clear_for_test`: a reset app stands for a new process.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn clear_all_for_test() {
    lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two tables' keys must agree: the launch machine writes one key per
    /// target, and the wasm table's is computed by another route.
    #[test]
    fn the_wasm_tables_key_agrees_with_this_ones() {
        for url in [
            "https://Nest.Example",
            "https://nest.example:8443",
            "http://203.0.113.9:3000",
            "https://sub.Nest.example:443/path",
        ] {
            assert_eq!(
                host_key_of_url(url),
                fauna_core::web::reach_host_key(url),
                "{url}"
            );
        }
    }
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([203, 0, 113, 9], port))
    }

    /// One test over the lifecycle rather than one per transition: the table
    /// is process-global and the crate's tests run in parallel, so each
    /// scenario here uses a host of its own and the sequence is asserted in
    /// the order the launch machine and the dials drive it.
    #[test]
    fn the_hint_is_installed_consulted_and_spent_by_the_domains_first_success() {
        let fired = Arc::new(AtomicUsize::new(0));
        let deleter: OnDomainReached = {
            let fired = Arc::clone(&fired);
            Arc::new(move || {
                fired.fetch_add(1, Ordering::SeqCst);
            })
        };

        assert_eq!(hint_for("spent.example"), None, "nothing installed yet");

        install("Spent.Example", addr(8443), deleter);
        assert_eq!(
            hint_for("spent.example"),
            Some(addr(8443)),
            "the key is the lowercased host, however the installer spelt it"
        );

        domain_reached("SPENT.example");
        assert_eq!(fired.load(Ordering::SeqCst), 1, "the deleter ran once");
        assert_eq!(hint_for("spent.example"), None, "the hint is spent");

        domain_reached("spent.example");
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "a later domain success finds no entry and runs nothing"
        );
    }

    #[test]
    fn clearing_forgets_the_hint_without_deleting_the_slot() {
        let fired = Arc::new(AtomicUsize::new(0));
        let deleter: OnDomainReached = {
            let fired = Arc::clone(&fired);
            Arc::new(move || {
                fired.fetch_add(1, Ordering::SeqCst);
            })
        };
        install("cleared.example", addr(443), deleter);
        clear("cleared.example");
        assert_eq!(hint_for("cleared.example"), None);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "a launch phase re-deciding from scratch must not spend the persisted hint"
        );
    }

    #[test]
    fn the_key_is_the_domain_alone() {
        let wss: Uri = "wss://Nest.Example:8443/api/v1/ws/aa".parse().unwrap();
        assert_eq!(host_key_of_uri(&wss), "nest.example");
        assert_eq!(
            host_key_of_url("https://Nest.Example:8443").as_deref(),
            Some("nest.example"),
            "a nest URL and the WS request built from it share one key"
        );
        assert_eq!(host_key_of_url("not a url"), None);
    }

    #[test]
    fn only_a_socket_that_never_opened_is_a_reachability_failure() {
        use tokio_tungstenite::tungstenite::Error;
        let io = Error::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert!(is_reachability_failure(&io));
        let http = Error::Http(
            tokio_tungstenite::tungstenite::http::Response::builder()
                .status(401)
                .body(None)
                .unwrap(),
        );
        assert!(
            !is_reachability_failure(&http),
            "a nest that answered is not a box the hint should be tried for"
        );
        assert!(!is_reachability_failure(&Error::Url(
            tokio_tungstenite::tungstenite::error::UrlError::NoHostName
        )));
    }
}
