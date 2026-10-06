//! Phase D.4 round-trip — `fauna_mail::outbound::dkim::sign` produces a
//! signature that mail-auth's verifier accepts when the corresponding
//! DKIM public-key TXT record is published.
//!
//! Closes the deferred follow-up flagged in
//! `libs/fauna-mail/src/outbound/dkim.rs::tests` (no DNS resolver fixture
//! yet at D.0 lift time). The stub resolver is just a `HashMap`-backed
//! `ResolverCache<Box<str>, Txt>` pre-populated with the DKIM TXT record,
//! handed to `verify_dkim` via the public `Parameters::with_txt_cache`
//! API — no real DNS queries fire.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine;
use mail_auth::common::crypto::Ed25519Key;
use mail_auth::common::parse::TxtRecordParser;
use mail_auth::common::verify::DomainKey;
use mail_auth::{
    AuthenticatedMessage, DkimResult, MessageAuthenticator, Parameters, ResolverCache, Txt,
};

use fauna_mail::outbound::dkim::{SigningAlg, SigningKey, sign};

/// In-memory `ResolverCache<Box<str>, Txt>` impl for the D.4 round-trip.
/// Mirrors mail-auth's internal `DummyCache` shape (which is `pub(crate)`
/// — duplicated here so the test stays inside the public API). The key type
/// is `Box<str>` to match mail-auth 0.9.2's `Parameters`/`verify_dkim`
/// bound (`TXT: ResolverCache<Box<str>, Txt>`, was `String` pre-0.9). No
/// expiry semantics: `insert` ignores `valid_until` because the test
/// populates once and reads once.
#[derive(Default)]
struct StaticTxtCache {
    inner: Mutex<HashMap<Box<str>, Txt>>,
}

impl StaticTxtCache {
    fn new() -> Self {
        Self::default()
    }

    fn put(&self, key: impl Into<Box<str>>, txt: Txt) {
        self.inner.lock().unwrap().insert(key.into(), txt);
    }
}

impl ResolverCache<Box<str>, Txt> for StaticTxtCache {
    fn get<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.lock().unwrap().get(name).cloned()
    }

    fn remove<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.inner.lock().unwrap().remove(name)
    }

    fn insert(&self, key: Box<str>, value: Txt, _valid_until: Instant) {
        self.inner.lock().unwrap().insert(key, value);
    }
}

#[tokio::test]
async fn ed25519_sign_round_trips_against_stub_resolver() {
    // 1. Generate a fresh Ed25519 keypair via ring (mail-auth's default).
    let pkcs8_der = Ed25519Key::generate_pkcs8().expect("generate ed25519 pkcs8");
    let parsed = Ed25519Key::from_pkcs8_maybe_unchecked_der(&pkcs8_der).expect("re-parse pkcs8");
    let pubkey_bytes = parsed.public_key();
    let pubkey_b64 = base64::engine::general_purpose::STANDARD.encode(&pubkey_bytes);

    // 2. Sign a known message via the lifted primitive.
    let keys = [SigningKey {
        alg: SigningAlg::Ed25519,
        priv_key: pkcs8_der,
        selector: "ed1".to_string(),
        domain: "example.test".to_string(),
    }];
    let raw = concat!(
        "From: alice@example.test\r\n",
        "To: bob@external.test\r\n",
        "Subject: Phase D.4 round-trip\r\n",
        "Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n",
        "Message-ID: <round-trip-1@example.test>\r\n",
        "\r\n",
        "body of the round-trip test message\r\n",
    );
    let signed = sign(raw.as_bytes(), &keys).expect("dkim sign");

    // 3. Pre-populate the stub resolver with the DKIM TXT record at
    //    `<selector>._domainkey.<domain>.`. The trailing dot mirrors
    //    mail-auth's internal lookup convention (fully-qualified name).
    let txt_record = format!("v=DKIM1; k=ed25519; p={pubkey_b64}");
    let domain_key = DomainKey::parse(txt_record.as_bytes()).expect("parse DKIM TXT");
    let cache = StaticTxtCache::new();
    cache.put(
        "ed1._domainkey.example.test.",
        Txt::DomainKey(Arc::new(domain_key)),
    );

    // 4. Verify. The Parameters builder layers the stub cache onto the
    //    public verify_dkim API; mail-auth consults the cache before any
    //    DNS query so the test never hits the network.
    let message = AuthenticatedMessage::parse(&signed).expect("parse signed message");
    let resolver = MessageAuthenticator::new_system_conf().expect("resolver init");
    let parameters = Parameters::new(&message).with_txt_cache(&cache);
    let dkim_results = resolver.verify_dkim(parameters).await;

    assert!(
        dkim_results
            .iter()
            .any(|r| matches!(r.result(), DkimResult::Pass)),
        "expected at least one DKIM Pass; got {:?}",
        dkim_results
            .iter()
            .map(|r| format!("{:?}", r.result()))
            .collect::<Vec<_>>()
    );
}
