use serde::{Deserialize, Serialize};

use crate::signing::{Keypair, verify_event};
use crate::types::{Event, Tag, UnsignedEvent, kind};

/// Build a NIP-42 AUTH event (kind 22242) to prove identity to a relay.
pub fn build_auth_event(relay_url: &str, challenge: &str, keypair: &Keypair) -> Event {
    auth_event_template(
        relay_url,
        challenge,
        fauna_core::data::Timestamp::now_secs() as u64,
    )
    .sign(keypair)
}

/// The unsigned kind-22242 AUTH event a nest hands an **external** signer so
/// the signer can prove it holds a pubkey — the proof-of-possession object
/// behind a `nip07` / `remote` Nostr account link (`docs/goal/ui/nostr.md`
/// § Errors & edge cases → *Proof of possession*).
///
/// It is exactly the NIP-42 shape (`relay` + `challenge` tags, empty content)
/// because that is the one event every signer already knows how to sign for
/// exactly this purpose — "prove this key to this relay" — and because NIP-42
/// binds the signature to the relay URL, so a proof minted for one nest is
/// dead at every other. The template carries no `pubkey`: a NIP-07 extension
/// fills its own in, and a NIP-46 bunker signs as the user key it holds. The
/// fields are the four `window.nostr.signEvent` expects, under their NIP-01
/// names, so an app hands the template straight to the signer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthEventTemplate {
    pub kind: u64,
    pub created_at: u64,
    pub tags: Vec<Tag>,
    pub content: String,
}

/// The template for `challenge` at `relay_url`, stamped `created_at`.
pub fn auth_event_template(relay_url: &str, challenge: &str, created_at: u64) -> AuthEventTemplate {
    AuthEventTemplate {
        kind: kind::AUTH,
        created_at,
        tags: vec![
            Tag::new(vec!["relay".into(), relay_url.into()]),
            Tag::new(vec!["challenge".into(), challenge.into()]),
        ],
        content: String::new(),
    }
}

impl AuthEventTemplate {
    /// The template as `pubkey`'s unsigned event.
    pub fn into_unsigned(self, pubkey: [u8; 32]) -> UnsignedEvent {
        UnsignedEvent {
            pubkey,
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags,
            content: self.content,
        }
    }

    /// Sign the template with a key held locally (a test signer, or the
    /// relay-auth leg [`build_auth_event`] wraps).
    pub fn sign(self, keypair: &Keypair) -> Event {
        let unsigned = self.into_unsigned(keypair.public_key_bytes());
        keypair.sign_event(unsigned)
    }
}

/// Why a proof-of-possession event was refused. Every arm is a reason the
/// caller surfaces verbatim; none leaks anything the presenter did not send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkProofError {
    /// Not a kind-22242 AUTH event.
    WrongKind,
    /// No `relay` tag naming the relay the challenge was minted for.
    WrongRelay,
    /// No `challenge` tag carrying the nonce the nest minted.
    WrongChallenge,
    /// The event id or the schnorr signature does not verify.
    InvalidSignature,
    /// The event is signed by a key other than the one the caller claimed.
    PubkeyMismatch,
}

impl std::fmt::Display for LinkProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongKind => "proof is not a kind-22242 AUTH event",
            Self::WrongRelay => "proof does not name this nest's relay",
            Self::WrongChallenge => "proof does not carry the challenge this nest issued",
            Self::InvalidSignature => "proof signature does not verify",
            Self::PubkeyMismatch => "proof is signed by a different key than the one claimed",
        })
    }
}

impl std::error::Error for LinkProofError {}

/// Verify a signed [`AuthEventTemplate`] as proof that its author holds the
/// key: kind 22242, a `relay` tag equal to `relay_url`, a `challenge` tag equal
/// to `challenge`, a valid id + schnorr signature, and — when the caller named
/// the key it is proving — that `event.pubkey` is that key. Exact-string relay
/// matching is right here (unlike the relay's own [`verify_auth_event_for_host`]):
/// the nest authored the template, so the signer echoes the URL verbatim.
pub fn verify_link_proof(
    event: &Event,
    challenge: &str,
    relay_url: &str,
    expected_pubkey: Option<&str>,
) -> Result<(), LinkProofError> {
    if event.kind != kind::AUTH {
        return Err(LinkProofError::WrongKind);
    }
    let has_relay = event
        .tags
        .iter()
        .any(|t| t.name() == Some("relay") && t.value() == Some(relay_url));
    if !has_relay {
        return Err(LinkProofError::WrongRelay);
    }
    let has_challenge = event
        .tags
        .iter()
        .any(|t| t.name() == Some("challenge") && t.value() == Some(challenge));
    if !has_challenge {
        return Err(LinkProofError::WrongChallenge);
    }
    if let Some(expected) = expected_pubkey
        && !event.pubkey.eq_ignore_ascii_case(expected)
    {
        return Err(LinkProofError::PubkeyMismatch);
    }
    if !verify_event(event) {
        return Err(LinkProofError::InvalidSignature);
    }
    Ok(())
}

/// Verify a NIP-42 AUTH event from a client.
///
/// Checks:
/// - Kind is 22242
/// - Has a "relay" tag matching the expected relay URL
/// - Has a "challenge" tag matching the expected challenge
/// - Signature is valid
pub fn verify_auth_event(event: &Event, challenge: &str, relay_url: &str) -> bool {
    if event.kind != kind::AUTH {
        return false;
    }

    let has_relay = event
        .tags
        .iter()
        .any(|t| t.name() == Some("relay") && t.value() == Some(relay_url));
    if !has_relay {
        return false;
    }

    verify_challenge_and_signature(event, challenge)
}

/// Verify a NIP-42 AUTH event against the relay's *host* rather than an exact
/// URL string.
///
/// NIP-42's relay-tag check exists to stop cross-relay AUTH replay; the
/// discriminator is **which relay** the client believed it was dialing — its
/// host — not the exact string form. Real clients normalize the URL they
/// connected to inconsistently (`ws://` in dev or behind a TLS-terminating
/// proxy, explicit vs. default port, trailing slash, bare host vs. endpoint
/// path), so an exact string match locks real clients out — the N2 interop
/// harness caught `nostr-sdk` echoing `ws://127.0.0.1:<port>/nostr` against a
/// relay that would only accept two hand-built `wss://<domain>…` forms.
///
/// `expected_host` is the authority the relay is reachable at (`host` or
/// `host:port`); `expected_path` is the relay's endpoint path (`/nostr`).
/// The tag URL matches when:
/// - scheme is `ws`/`wss`/`http`/`https` (scheme never discriminates between
///   our relay and another — host does);
/// - host is equal case-insensitively;
/// - effective port is equal, treating `ws`/`http` as 80 and `wss`/`https` as
///   443 when a side omits it;
/// - path is empty, `/`, or `expected_path`, trailing-slash-insensitive.
pub fn verify_auth_event_for_host(
    event: &Event,
    challenge: &str,
    expected_host: &str,
    expected_path: &str,
) -> bool {
    if event.kind != kind::AUTH {
        return false;
    }

    let has_relay = event.tags.iter().any(|t| {
        t.name() == Some("relay")
            && t.value()
                .is_some_and(|u| relay_url_matches(u, expected_host, expected_path))
    });
    if !has_relay {
        return false;
    }

    verify_challenge_and_signature(event, challenge)
}

fn verify_challenge_and_signature(event: &Event, challenge: &str) -> bool {
    let has_challenge = event
        .tags
        .iter()
        .any(|t| t.name() == Some("challenge") && t.value() == Some(challenge));
    if !has_challenge {
        return false;
    }

    verify_event(event)
}

/// The host/port/path-normalized relay-URL comparison behind
/// [`verify_auth_event_for_host`] (see there for the rules).
pub fn relay_url_matches(tag_url: &str, expected_host: &str, expected_path: &str) -> bool {
    let Some((scheme, rest)) = tag_url.split_once("://") else {
        return false;
    };
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "ws" | "http" => 80,
        "wss" | "https" => 443,
        _ => return false,
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // Strip userinfo if present; a relay URL should not carry one, but a
    // client that includes it is still naming this host.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);

    let (tag_host, tag_port) = split_host_port(authority, default_port);
    // The expected side has no scheme; its implicit ports are the two web
    // defaults (an expected host with no explicit port accepts both).
    let (exp_host, exp_port) = split_host_port(expected_host, 0);

    if !tag_host.eq_ignore_ascii_case(exp_host) {
        return false;
    }
    let port_ok = if exp_port == 0 {
        tag_port == 80 || tag_port == 443
    } else {
        tag_port == exp_port
    };
    if !port_ok {
        return false;
    }

    let norm = |p: &str| p.trim_end_matches('/').to_string();
    let path = norm(path);
    path.is_empty() || path == norm(expected_path)
}

/// Split `host[:port]`, defaulting the port. IPv6 literals (`[::1]:port`)
/// keep their brackets on the host side — this compares relay URLs, so the
/// host must stay in the spelling a URL uses. Only the port-defaulting is
/// local; the split itself is [`fauna_core::web::split_host_port`].
fn split_host_port(authority: &str, default_port: u16) -> (&str, u16) {
    let (host, port) = fauna_core::web::split_host_port(authority);
    (host, port.unwrap_or(default_port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_auth_event_has_correct_kind_and_tags() {
        let kp = Keypair::generate();
        let event = build_auth_event("wss://relay.example.com", "challenge123", &kp);
        assert_eq!(event.kind, kind::AUTH);
        assert!(event.tags.iter().any(|t| {
            t.name() == Some("relay") && t.value() == Some("wss://relay.example.com")
        }));
        assert!(
            event
                .tags
                .iter()
                .any(|t| { t.name() == Some("challenge") && t.value() == Some("challenge123") })
        );
    }

    #[test]
    fn verify_auth_event_roundtrip() {
        let kp = Keypair::generate();
        let event = build_auth_event("wss://relay.example.com", "ch1", &kp);
        assert!(verify_auth_event(&event, "ch1", "wss://relay.example.com"));
    }

    #[test]
    fn verify_auth_event_wrong_challenge_fails() {
        let kp = Keypair::generate();
        let event = build_auth_event("wss://relay.example.com", "ch1", &kp);
        assert!(!verify_auth_event(
            &event,
            "wrong",
            "wss://relay.example.com"
        ));
    }

    #[test]
    fn verify_auth_event_wrong_relay_fails() {
        let kp = Keypair::generate();
        let event = build_auth_event("wss://relay.example.com", "ch1", &kp);
        assert!(!verify_auth_event(&event, "ch1", "wss://other.relay.com"));
    }

    #[test]
    fn verify_auth_event_wrong_kind_fails() {
        let kp = Keypair::generate();
        let mut event = build_auth_event("wss://relay.example.com", "ch1", &kp);
        event.kind = 1; // tamper kind
        assert!(!verify_auth_event(&event, "ch1", "wss://relay.example.com"));
    }

    #[test]
    fn relay_url_matches_accepts_real_client_normalizations() {
        // The forms real clients actually send for a relay at
        // example.com/nostr (production, implicit 443):
        for url in [
            "wss://example.com/nostr",
            "wss://example.com/nostr/",
            "wss://EXAMPLE.com/nostr",
            "wss://example.com:443/nostr",
            "wss://example.com",
            "wss://example.com/",
            "ws://example.com/nostr", // TLS-terminating proxy in front
        ] {
            assert!(
                relay_url_matches(url, "example.com", "/nostr"),
                "{url} must match example.com"
            );
        }
        // A dev/test relay bound with an explicit port:
        for url in [
            "ws://127.0.0.1:45405/nostr",
            "ws://127.0.0.1:45405/nostr/",
            "ws://127.0.0.1:45405",
        ] {
            assert!(
                relay_url_matches(url, "127.0.0.1:45405", "/nostr"),
                "{url} must match 127.0.0.1:45405"
            );
        }
    }

    #[test]
    fn relay_url_matches_rejects_other_relays() {
        for (url, host) in [
            ("wss://other.example.com/nostr", "example.com"), // different host
            ("wss://example.com.evil.net/nostr", "example.com"), // suffix trick
            ("wss://example.com:8443/nostr", "example.com"),  // wrong port
            ("ws://127.0.0.1:45406/nostr", "127.0.0.1:45405"), // wrong port
            ("wss://example.com/other", "example.com"),       // wrong path
            ("wss://example.com/nostr/deeper", "example.com"), // wrong path
            ("ftp://example.com/nostr", "example.com"),       // wrong scheme
            ("example.com/nostr", "example.com"),             // no scheme
        ] {
            assert!(
                !relay_url_matches(url, host, "/nostr"),
                "{url} must NOT match {host}"
            );
        }
    }

    #[test]
    fn verify_auth_event_for_host_end_to_end() {
        let kp = Keypair::generate();
        // The client echoes the URL as IT normalized it.
        let event = build_auth_event("ws://127.0.0.1:45405/nostr", "ch1", &kp);
        assert!(verify_auth_event_for_host(
            &event,
            "ch1",
            "127.0.0.1:45405",
            "/nostr"
        ));
        assert!(!verify_auth_event_for_host(
            &event,
            "wrong-challenge",
            "127.0.0.1:45405",
            "/nostr"
        ));
        assert!(!verify_auth_event_for_host(
            &event,
            "ch1",
            "relay.example.com",
            "/nostr"
        ));
    }

    // ── proof of possession (the link-challenge object) ────────────────

    const RELAY: &str = "wss://nest.example/nostr";

    #[test]
    fn a_template_signed_by_the_claimed_key_verifies() {
        let kp = Keypair::generate();
        let proof = auth_event_template(RELAY, "nonce-1", 1_700_000_000).sign(&kp);
        assert_eq!(proof.kind, kind::AUTH);
        assert_eq!(proof.created_at, 1_700_000_000);
        assert_eq!(
            verify_link_proof(&proof, "nonce-1", RELAY, Some(&kp.public_key_hex())),
            Ok(())
        );
        // A caller that has not yet learned the key (the remote arm before
        // `get_public_key`) verifies the signature alone.
        assert_eq!(verify_link_proof(&proof, "nonce-1", RELAY, None), Ok(()));
    }

    #[test]
    fn a_proof_signed_by_another_key_is_a_pubkey_mismatch() {
        let holder = Keypair::generate();
        let claimant = Keypair::generate();
        let proof = auth_event_template(RELAY, "nonce-1", 1).sign(&holder);
        assert_eq!(
            verify_link_proof(&proof, "nonce-1", RELAY, Some(&claimant.public_key_hex())),
            Err(LinkProofError::PubkeyMismatch)
        );
    }

    #[test]
    fn a_proof_over_another_nonce_or_relay_is_refused() {
        let kp = Keypair::generate();
        let proof = auth_event_template(RELAY, "nonce-1", 1).sign(&kp);
        assert_eq!(
            verify_link_proof(&proof, "nonce-2", RELAY, None),
            Err(LinkProofError::WrongChallenge)
        );
        assert_eq!(
            verify_link_proof(&proof, "nonce-1", "wss://other.example/nostr", None),
            Err(LinkProofError::WrongRelay)
        );
    }

    #[test]
    fn a_tampered_or_forged_proof_is_an_invalid_signature() {
        let kp = Keypair::generate();
        let mut proof = auth_event_template(RELAY, "nonce-1", 1).sign(&kp);
        proof.created_at += 1; // id no longer matches the content
        assert_eq!(
            verify_link_proof(&proof, "nonce-1", RELAY, None),
            Err(LinkProofError::InvalidSignature)
        );

        // A stranger who knows the nonce but not the key: correct tags, the
        // victim's pubkey pasted in, garbage signature.
        let victim = Keypair::generate();
        let mut forged = auth_event_template(RELAY, "nonce-1", 1).sign(&kp);
        forged.pubkey = victim.public_key_hex();
        assert_eq!(
            verify_link_proof(&forged, "nonce-1", RELAY, Some(&victim.public_key_hex())),
            Err(LinkProofError::InvalidSignature)
        );
    }

    #[test]
    fn a_non_auth_event_is_the_wrong_kind() {
        let kp = Keypair::generate();
        let mut template = auth_event_template(RELAY, "nonce-1", 1);
        template.kind = kind::TEXT_NOTE;
        let proof = template.sign(&kp);
        assert_eq!(
            verify_link_proof(&proof, "nonce-1", RELAY, None),
            Err(LinkProofError::WrongKind)
        );
    }

    #[test]
    fn the_template_serializes_under_the_nip01_field_names() {
        let json =
            serde_json::to_value(auth_event_template(RELAY, "nonce-1", 1_700_000_000)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": 22242,
                "created_at": 1_700_000_000,
                "tags": [["relay", RELAY], ["challenge", "nonce-1"]],
                "content": "",
            })
        );
    }
}
