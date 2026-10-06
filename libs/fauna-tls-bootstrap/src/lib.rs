//! The boring `rustls` preamble every native TLS dialer in this workspace needs:
//! process-wide `CryptoProvider` installation, and the bundled WebPKI root store.
//!
//! rustls 0.23 resolves its crypto backend from a process-global default;
//! nothing installs one automatically, and `connect_async` (tokio-tungstenite)
//! and native TLS dials alike panic without it. `fauna-ws-substrate` and
//! `fauna-mail` each hand-rolled an identical `Once`-guarded install — found
//! by the dev-fleet near-duplicate-function scanner's cross-crate pass (0.623
//! similarity; `fauna-mail`'s own copy documented itself as "the same `Once`
//! guard nest's WS dial path uses" without calling it).
//!
//! # Why this crate exists rather than one side depending on the other
//!
//! `fauna-ws-substrate` is a full native WS-transport stack
//! (tokio-tungstenite, the reconnect supervisor); `fauna-mail` dials TLS only
//! for its native IMAP client. Depending on the transport crate just to reuse
//! a three-line `Once` guard would pull an unrelated stack into a mail crate.
//! `rustls` is the only dependency either side already carries for this, so
//! it is the only dependency here — any future native TLS dialer can take
//! this crate for free.
//!
//! # The second dependency, and what the rule above actually protects
//!
//! `webpki-roots` was admitted 2026-08-30, and the sentence above needs reading
//! precisely rather than as a dependency count. What it protects against is
//! **pulling an unrelated stack into a crate that only wanted three lines** —
//! a transport crate, a runtime, a protocol implementation. `webpki-roots` is
//! none of those: it is bundled Mozilla root certificates and no code at all.
//!
//! And admitting it made the workspace's graph **smaller**, not larger. The
//! same `RootCertStore::empty()` + `.extend(TLS_SERVER_ROOTS)` pair was built
//! in `fauna-ws-substrate` and again in `fauna-mail`'s native IMAP connector,
//! and in each crate that was the *only* use of `webpki-roots` anywhere — so
//! the lift moved one dependency here and let **both** callers drop theirs. It
//! also closed a cross-crate reach the rest of this module doc argues against:
//! the nest's mail-deliverability STARTTLS probe was calling into the full WS
//! *transport* crate to get a root store.
//!
//! The test for a third dependency is the same one: does it carry a stack, or
//! is it the data/primitive the preamble is made of?

/// Install the ring `CryptoProvider` as the process default, once. Idempotent;
/// a provider another component already set is left in place.
pub fn ensure_tls_provider() {
    static TLS_PROVIDER: std::sync::Once = std::sync::Once::new();
    TLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The bundled Mozilla WebPKI roots as a `rustls::RootCertStore` — the store
/// every native TLS client dial in this workspace verifies against.
///
/// The one owner. Three callers take it from here rather than rebuilding it
/// from `webpki_roots::TLS_SERVER_ROOTS`: federation's capturing verifier
/// (`fauna_ws_substrate::tls_verify`), the native IMAP connector
/// (`fauna_mail::imap_client::native`), and the nest's mail-deliverability
/// STARTTLS probe — which called the first of those until 2026-08-30, and now
/// calls this directly.
///
/// A caller that needs *additional* trust (a test rig's own CA) extends the
/// returned store — it is owned, not shared, so that stays a local decision.
pub fn webpki_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_tls_provider_is_idempotent() {
        ensure_tls_provider();
        ensure_tls_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    /// The store is non-empty and each call hands back an INDEPENDENT one — a
    /// caller that extends it with its own CA (the native IMAP connector's e2e
    /// arm does exactly that) must not be widening the trust of every other
    /// dialer in the process.
    #[test]
    fn webpki_root_store_is_populated_and_owned_per_call() {
        let a = webpki_root_store();
        assert!(
            !a.is_empty(),
            "the bundled Mozilla roots should not be empty"
        );

        let mut b = webpki_root_store();
        let before = b.len();
        b.roots.extend(a.roots.iter().cloned());
        assert!(b.len() > before, "the returned store must be extensible");
        assert_eq!(
            webpki_root_store().len(),
            before,
            "extending one caller's store must not affect the next caller's"
        );
    }
}
