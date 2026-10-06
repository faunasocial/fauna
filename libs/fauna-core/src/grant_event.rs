//! Client-side, signed, append-only capability-grant event log — the
//! forensic record backing the Nests-page Now/History lens (design tracked
//! internally — "the log MUST be client-authoritative — signed by the
//! minting client, append-only — or a malicious box forges/erases its own
//! entries").
//!
//! Lives in `fauna-core` (not `fauna-mls`, where the capability wire types —
//! `GrantBlob`/`ScopeTuple`/`GrantWindow` — live) because [`GrantEvent`] is a
//! `fauna.state.succession-ledger` value, and `fauna-core` cannot depend on
//! `fauna-mls` (which already depends on `fauna-core` for the ledger types /
//! `ActorId` — the reverse edge would cycle). [`GrantEventScope`] mirrors
//! `ScopeTuple`'s wire shape (`class`/`kind`/`tier`) as an independent type
//! for that same layering reason.
//!
//! One event per grant lifecycle transition (mint / renew / revoke), signed
//! by the minting client's identity key ([`crate::identity::ActorKeypair`])
//! so a box holding the granted capability can never forge or erase its own
//! history — only the owner's own clients can produce a verifying event.
//! "Now" (current grants) and "History" (the raw timeline) are two folds of
//! the same log — event-sourced, one build — computed client-side (see
//! `fauna-client-capabilities`, which owns the fold + the mint/renew/revoke
//! recording helpers, mirroring how `fauna-client-subscriptions::custody`
//! sits atop `SubscriptionsConfig`).

use std::collections::{BTreeMap, BTreeSet};

use crate::identity::ActorId;
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};

/// Ed25519 signature length.
pub const GRANT_EVENT_SIGNATURE_LEN: usize = 64;

/// The `GrantEventScope.tier` marker a **bounded** (crypto-time-boxed) mail
/// grant's `Mint`/`Renew` events carry on their `content.read{mail}` tuple —
/// the log-side regime discriminator (content-sealing-epochs design § 5; the
/// rotation-heal driver enumerates bounded grants by it, and the Nests-page
/// regime copy renders from it). **Log-only convention:** the `GrantBlob`'s
/// own `ScopeTuple` stays `tier: None` — nest-side scope matching is
/// untouched, and the nest's regime-crossing renew guard (not this marker)
/// remains the enforcer, so a wrong marker is fail-safe in both directions.
/// It rides an *existing* signed field because this type's shape is frozen
/// (see [`GrantEvent`]).
pub const GRANT_SCOPE_TIER_BOUNDED: &str = "bounded";

/// Which grant lifecycle transition a [`GrantEvent`] records.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantEventKind {
    /// The grant was created (`fauna.capabilities.mint`).
    Mint,
    /// The grant's window was extended (`fauna.capabilities.renew`).
    Renew,
    /// The grant was revoked (`fauna.capabilities.revoke`).
    Revoke,
}

/// One atomic scope tuple, mirroring `fauna_mls::wrapped_blob::ScopeTuple`'s
/// wire shape (`class` / `kind` / `tier`) — an independent type (see module
/// docs for why), not a key-bearing artifact itself: just the declared-scope
/// description the History view renders.
///
/// **The factor fold.** The blob's tuple grew a `factor` qualifier (a
/// per-labeler grant confines every tuple to one bus factor,
/// `labeler:<hex>` — `content-moderation-and-ranking.md` § Tier-3 →
/// *Subscribing = minting a capability*), and this type cannot grow a field
/// (see [`GrantEvent`]). The factor therefore rides **inside `kind`**, after a
/// single space: `kind: "mail labeler:<hex>"`, or `"labeler:<hex>"` alone on a
/// kind-less tuple such as `content.label-write`. Read it back through
/// [`Self::base_kind`] / [`Self::factor`], never by comparing `kind` raw.
///
/// Why `kind` and not `tier`: every consumer that acts on a mail tuple keys on
/// `kind == "mail"` — the bounded-regime predicate, the succession re-mint's
/// payload derivation, the rotation-heal driver, the scope copy. A device
/// older than the fold sees an unknown kind and **leaves the grant alone**:
/// its re-mint fails closed (unknown read kind), its heal skips it (not
/// bounded), its copy falls back to the raw text. Folded into `tier` instead,
/// that same older device would read a plain `content.read{mail}` and re-mint
/// a standing, factor-less mail key — a widening the owner never consented
/// to. The fold lands where an older reader's ignorance is fail-safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantEventScope {
    pub class: String,
    pub kind: Option<String>,
    pub tier: Option<String>,
}

/// The separator between a tuple's kind and its folded factor inside
/// [`GrantEventScope::kind`]. A kind is a bare word (`mail`, `post`, …) and a
/// factor is `<namespace>:<hex>`, so neither contains it.
const GRANT_SCOPE_FACTOR_SEPARATOR: char = ' ';

impl GrantEventScope {
    /// Fold `factor` into this tuple's `kind` (the type-doc's encoding). A
    /// tuple already carrying a factor is re-folded, never doubled.
    #[must_use]
    pub fn with_factor(mut self, factor: &str) -> Self {
        let base = self.base_kind().map(str::to_string);
        self.kind = Some(match base {
            Some(kind) => format!("{kind}{GRANT_SCOPE_FACTOR_SEPARATOR}{factor}"),
            None => factor.to_string(),
        });
        self
    }

    /// The tuple's kind with any folded factor stripped — what every
    /// `kind == "mail"` comparison must read. `None` for a kind-less tuple,
    /// including one whose `kind` holds only a factor.
    pub fn base_kind(&self) -> Option<&str> {
        let kind = self.kind.as_deref()?;
        let (head, _) = split_kind(kind);
        (!head.is_empty()).then_some(head)
    }

    /// The bus factor folded into this tuple's `kind`, if any
    /// (`labeler:<hex>` for a per-labeler grant).
    pub fn factor(&self) -> Option<&str> {
        let kind = self.kind.as_deref()?;
        split_kind(kind).1
    }
}

/// `(base kind, folded factor)` of a raw `kind` string. A lone `<ns>:<hex>`
/// token is a factor on a kind-less tuple; a bare word is a plain kind.
fn split_kind(kind: &str) -> (&str, Option<&str>) {
    match kind.split_once(GRANT_SCOPE_FACTOR_SEPARATOR) {
        Some((head, factor)) => (head, Some(factor)),
        None if kind.contains(':') => ("", Some(kind)),
        None => (kind, None),
    }
}

/// The **`content.write`** class — a holder's writer key may author rows of
/// one `ext.*` kind (`third-party-kinds.md` § Principal write authority).
/// Keyless: the delegable pair is one symmetric unit with no write half, so
/// the tuple conveys no key, only the owner-signed authorization. Spelled
/// here, below `fauna-mls`, because the replica-side admission reads it off
/// this log (`fauna_mls::wrapped_blob::ScopeTuple::CLASS_CONTENT_WRITE` is
/// this constant).
pub const CLASS_CONTENT_WRITE: &str = "content.write";

/// The **`deposit`** class — a holder may post files into one folder's inbox
/// segment, which the nest seals on the owner's behalf to the owner's
/// recipient key (`file-sync.md` § Third-party deposit ingress). Keyless: the
/// holder writes blind and reads nothing, so the tuple is the owner-signed
/// audit + revocation record alone (`encryption-at-rest.md` § Capability
/// tiering → *Third-party holders*). The tuple's `set` names the folder by
/// its row id, in decimal (`fauna_mls::wrapped_blob::ScopeTuple::folder_deposit`).
pub const CLASS_DEPOSIT: &str = "deposit";

/// The factor namespace a `content.write` tuple's writer key rides in — the
/// tuple's license confined to that one key, exactly as `labeler:<hex>`
/// confines a labeler's (`fauna_mls::wrapped_blob::ScopeTuple::factor`, folded
/// into [`GrantEventScope::kind`] here). Riding the factor is what keeps both
/// frozen shapes frozen: neither the blob's tuple nor this event grows a
/// field (see [`GrantEvent`]).
pub const WRITER_FACTOR_PREFIX: &str = "writer:";

/// The `writer:<hex>` factor naming `writer`, an Ed25519 public key —
/// canonical lowercase hex, so equal keys fold to equal strings.
#[must_use]
pub fn writer_factor(writer: &[u8; 32]) -> String {
    format!("{WRITER_FACTOR_PREFIX}{}", crate::hex32::encode(writer))
}

/// The writer key a `writer:<hex>` factor names, or `None` for any other
/// string — a labeler factor, or a non-canonical spelling (refused, never
/// repaired, so one key has one factor).
#[must_use]
pub fn parse_writer_factor(factor: &str) -> Option<[u8; 32]> {
    let hex = factor.strip_prefix(WRITER_FACTOR_PREFIX)?;
    let key = crate::hex32::decode(hex).ok()?;
    (crate::hex32::encode(&key) == hex).then_some(key)
}

/// **Who may author each kind, by the owner's own word** — every
/// `(kind, writer)` pair a `Mint` or `Renew` event of `events` declares through
/// a `content.write` tuple carrying a writer factor (`third-party-kinds.md`
/// § Principal write authority, position (2)).
///
/// **Ever minted, revoked or not**, so this is deliberately NOT a fold over
/// [`latest_live_events_of`]: a row the user kept is the user's data, and a
/// row carries no trusted time a revocation window could be checked against.
/// What ends a principal's writing is the nest's door closing with its
/// session; what this answers is whether a row already written was ever
/// authorized. Signature verification is the caller's — the account's log is
/// the succession ledger's chain-signed fold, which keeps only events its
/// chain signed.
#[must_use]
pub fn content_write_authorizations(events: &[GrantEvent]) -> BTreeMap<String, BTreeSet<[u8; 32]>> {
    let mut out: BTreeMap<String, BTreeSet<[u8; 32]>> = BTreeMap::new();
    for event in events {
        if event.kind == GrantEventKind::Revoke {
            continue;
        }
        for tuple in event
            .scope
            .iter()
            .filter(|t| t.class == CLASS_CONTENT_WRITE)
        {
            let (Some(kind), Some(writer)) = (
                tuple.base_kind(),
                tuple.factor().and_then(parse_writer_factor),
            ) else {
                continue;
            };
            out.entry(kind.to_string()).or_default().insert(writer);
        }
    }
    out
}

/// One signed, immutable entry in the owner's capability-grant event log.
///
/// **⚠ The field set is frozen for this major — never add a field.** The
/// signature covers this struct's canonical dag-cbor with `sig` zeroed, and
/// [`Self::verify`] re-encodes the *deserialized* struct — so an older device
/// (whose serde drops an unknown field) re-encodes different bytes and the
/// verify fails; worse, the ledger fold **drops** events that fail
/// verification (`SuccessionLedger::fold`, the grant-events read), silently
/// erasing the new event from every pre-addition device's log. New facts
/// therefore ride existing fields — the bounded-mail regime marker
/// [`GRANT_SCOPE_TIER_BOUNDED`] in `scope[].tier`, the per-labeler factor
/// folded into `scope[].kind` ([`GrantEventScope::with_factor`]) — never a
/// new field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantEvent {
    /// The grant this event is about — matches `GrantIndex.grant_id` (16
    /// bytes, chosen by the minting client at mint time).
    #[serde(with = "serde_bytes")]
    pub grant_id: Vec<u8>,
    /// The granted box's X25519 pubkey — matches `GrantBlob.holder` (32
    /// bytes). Named generically (not "nest"/"bridge") so the shape
    /// generalizes to any future participant kind the north-star unified
    /// Participants surface introduces.
    #[serde(with = "serde_bytes")]
    pub holder: Vec<u8>,
    pub kind: GrantEventKind,
    /// The declared scope as of this event. Empty for `Revoke` (nothing new
    /// is declared; the prior Mint/Renew event(s) already recorded it).
    pub scope: Vec<GrantEventScope>,
    /// `[epoch_start, epoch_end]` as of this event, in seconds since the
    /// Unix epoch (matches `fauna_mls::wrapped_blob::GrantWindow`'s wire
    /// unit — `bins/fauna-nest/src/db/mod.rs::now_epoch_secs`). Zeroed for
    /// `Revoke` (no window is declared at revocation).
    pub window_start: u64,
    pub window_end: u64,
    /// When this event occurred, seconds since the Unix epoch (the log's own
    /// clock — independent of `window_start`/`window_end`, which describe
    /// the grant's authorization window, not the event's timestamp).
    pub at: u64,
    /// Ed25519 signature by the minting client's identity key over this
    /// struct's canonical dag-cbor with `sig` zeroed — the placeholder
    /// pattern `SubmissionToken` uses
    /// (`fauna-mls/src/wrapped_blob/submission_token.rs`).
    #[serde(with = "serde_bytes")]
    pub sig: Vec<u8>,
}

/// A [`GrantEvent`]'s signature failed to verify or had the wrong shape.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantEventError {
    /// `sig` was not exactly [`GRANT_EVENT_SIGNATURE_LEN`] bytes.
    #[error("grant event sig must be {GRANT_EVENT_SIGNATURE_LEN} bytes, got {0}")]
    WrongSignatureLength(usize),
    /// The canonical dag-cbor encode failed (unreachable for this
    /// fixed, float-free shape — the same "no floats" invariant the
    /// other account-state value types uphold).
    #[error("canonical encode: {0}")]
    Encode(String),
    /// The Ed25519 verify failed — a tampered field, or a signature by a
    /// key other than the claimed owner's.
    #[error("signature verification failed")]
    SignatureFailed,
}

impl GrantEvent {
    /// Sign with `signing_key` (the minting client's identity key). `sig` is
    /// set to zeros, the struct is canonical-dag-cbor-encoded, signed, and
    /// the placeholder is replaced with the real signature.
    ///
    /// # Errors
    ///
    /// Returns [`GrantEventError::Encode`] if canonical encoding fails
    /// (practically unreachable — no floats, no cycles).
    pub fn sign(mut self, signing_key: &SigningKey) -> Result<Self, GrantEventError> {
        self.sig = vec![0u8; GRANT_EVENT_SIGNATURE_LEN];
        let bytes = fauna_cbor::encode_canonical(&self)
            .map_err(|e| GrantEventError::Encode(e.to_string()))?;
        let sig = signing_key.sign(&bytes);
        self.sig = sig.to_bytes().to_vec();
        Ok(self)
    }

    /// Verify this event's signature against `owner` — the minting client's
    /// public identity ([`ActorId`], the Ed25519 verifying key).
    ///
    /// # Errors
    ///
    /// - [`GrantEventError::WrongSignatureLength`] if `sig` isn't
    ///   [`GRANT_EVENT_SIGNATURE_LEN`] bytes.
    /// - [`GrantEventError::Encode`] if the placeholder re-encode fails
    ///   (unreachable, see [`Self::sign`]).
    /// - [`GrantEventError::SignatureFailed`] if the Ed25519 verify fails.
    pub fn verify(&self, owner: &ActorId) -> Result<(), GrantEventError> {
        if self.sig.len() != GRANT_EVENT_SIGNATURE_LEN {
            return Err(GrantEventError::WrongSignatureLength(self.sig.len()));
        }
        let mut sig_bytes = [0u8; GRANT_EVENT_SIGNATURE_LEN];
        sig_bytes.copy_from_slice(&self.sig);
        let sig = Signature::from_bytes(&sig_bytes);

        let mut placeholder = self.clone();
        placeholder.sig = vec![0u8; GRANT_EVENT_SIGNATURE_LEN];
        let bytes = fauna_cbor::encode_canonical(&placeholder)
            .map_err(|e| GrantEventError::Encode(e.to_string()))?;

        if !crate::identity::verify_detached(&owner.0, &bytes, &sig.to_bytes()) {
            return Err(GrantEventError::SignatureFailed);
        }
        Ok(())
    }
}

/// One grant's current (folded) state — the Nests-page "Now" lens.
///
/// Lives here beside [`GrantEvent`] (moved from `fauna-client-capabilities`,
/// which re-exports it) because the fold is pure logic over
/// the ledger's own grant log and is consumed below the
/// capabilities crate — the succession aftermath's mark write
/// (`fauna-client-config`) stamps every live grant at the re-key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentGrant {
    pub grant_id: Vec<u8>,
    pub holder: Vec<u8>,
    pub scope: Vec<GrantEventScope>,
    pub window_start: u64,
    pub window_end: u64,
    /// When this current state was established (the folded event's `at`).
    pub as_of: u64,
}

/// The chronologically-latest `Mint`/`Renew` event per **surviving**
/// (non-revoked) grant — the selection [`current_grants`] projects and the
/// re-mint leg verifies signer provenance on. A grant is revoked iff its log
/// contains **any** `Revoke` event (revocation is terminal — decided by
/// presence, not recency, so a same-second merge tie-break can never resurrect
/// a revoked grant); the survivor's latest event is picked by `at`, tie-broken
/// on `sig` bytes for a deterministic order regardless of merge order.
pub fn latest_live_events(ledger: &crate::succession_ledger::SuccessionLedger) -> Vec<&GrantEvent> {
    latest_live_events_of(&ledger.grant_events)
}

/// [`latest_live_events`] over a bare log.
pub fn latest_live_events_of(events: &[GrantEvent]) -> Vec<&GrantEvent> {
    let revoked: BTreeSet<&[u8]> = events
        .iter()
        .filter(|e| e.kind == GrantEventKind::Revoke)
        .map(|e| e.grant_id.as_slice())
        .collect();
    let mut latest: BTreeMap<&[u8], &GrantEvent> = BTreeMap::new();
    for e in events {
        // Skip every event of a revoked grant (its `Revoke` is terminal), which
        // also means only `Mint`/`Renew` events reach the fold.
        if revoked.contains(e.grant_id.as_slice()) {
            continue;
        }
        latest
            .entry(e.grant_id.as_slice())
            .and_modify(|cur| {
                if (e.at, &e.sig) > (cur.at, &cur.sig) {
                    *cur = e;
                }
            })
            .or_insert(e);
    }
    latest.into_values().collect()
}

/// Fold the log to each grant's current state (the Nests-page "Now" lens) —
/// [`latest_live_events`] projected to [`CurrentGrant`] rows.
pub fn current_grants(ledger: &crate::succession_ledger::SuccessionLedger) -> Vec<CurrentGrant> {
    current_grants_of(&ledger.grant_events)
}

/// [`current_grants`] over a bare log (see [`latest_live_events_of`]).
pub fn current_grants_of(events: &[GrantEvent]) -> Vec<CurrentGrant> {
    latest_live_events_of(events)
        .into_iter()
        .map(|e| CurrentGrant {
            grant_id: e.grant_id.clone(),
            holder: e.holder.clone(),
            scope: e.scope.clone(),
            window_start: e.window_start,
            window_end: e.window_end,
            as_of: e.at,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ActorKeypair;

    fn mint_event(grant_id: [u8; 16], holder: [u8; 32]) -> GrantEvent {
        GrantEvent {
            grant_id: grant_id.to_vec(),
            holder: holder.to_vec(),
            kind: GrantEventKind::Mint,
            scope: vec![GrantEventScope {
                class: "content.read".into(),
                kind: Some("mail".into()),
                tier: None,
            }],
            window_start: 1000,
            window_end: 1000 + 90 * 24 * 60 * 60,
            at: 1000,
            sig: vec![0u8; GRANT_EVENT_SIGNATURE_LEN],
        }
    }

    fn write_tuple(kind: &str, writer: &[u8; 32]) -> GrantEventScope {
        GrantEventScope {
            class: CLASS_CONTENT_WRITE.into(),
            kind: Some(kind.into()),
            tier: None,
        }
        .with_factor(&writer_factor(writer))
    }

    #[test]
    fn the_writer_factor_round_trips_and_refuses_other_spellings() {
        let key = [0xAB; 32];
        let factor = writer_factor(&key);
        assert_eq!(factor, format!("writer:{}", "ab".repeat(32)));
        assert_eq!(parse_writer_factor(&factor), Some(key));
        assert_eq!(parse_writer_factor(&factor.to_uppercase()), None);
        assert_eq!(
            parse_writer_factor(&format!("labeler:{}", "ab".repeat(32))),
            None
        );
        assert_eq!(parse_writer_factor("writer:abcd"), None);
        // The fold leaves the kind readable as itself.
        let tuple = write_tuple("ext.example.com.notes", &key);
        assert_eq!(tuple.base_kind(), Some("ext.example.com.notes"));
        assert_eq!(tuple.factor(), Some(factor.as_str()));
    }

    #[test]
    fn write_authority_is_every_minted_writer_revoked_or_not() {
        let (principal, other) = ([0x11; 32], [0x22; 32]);
        let mut minted = mint_event([1u8; 16], [2u8; 32]);
        minted.scope = vec![
            write_tuple("ext.example.com.notes", &principal),
            // A read tuple over the same kind authorizes no writer.
            GrantEventScope {
                class: "content.read".into(),
                kind: Some("ext.example.com.todo".into()),
                tier: None,
            },
        ];
        let mut revoke = mint_event([1u8; 16], [2u8; 32]);
        revoke.kind = GrantEventKind::Revoke;
        revoke.scope = vec![write_tuple("ext.example.com.todo", &other)];
        let mut factorless = mint_event([3u8; 16], [2u8; 32]);
        factorless.scope = vec![GrantEventScope {
            class: CLASS_CONTENT_WRITE.into(),
            kind: Some("ext.example.com.todo".into()),
            tier: None,
        }];

        let authority = content_write_authorizations(&[minted, revoke, factorless]);
        assert_eq!(
            authority,
            BTreeMap::from([(
                "ext.example.com.notes".to_string(),
                BTreeSet::from([principal])
            )]),
            "the revoked grant's mint still authorizes its rows; a Revoke's own \
             scope and a writer-less tuple authorize nothing"
        );
    }

    #[test]
    fn sign_then_verify_succeeds() {
        let kp = ActorKeypair::generate();
        let signed = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        signed.verify(&kp.actor_id()).unwrap();
    }

    #[test]
    fn verify_with_wrong_owner_fails() {
        let kp = ActorKeypair::generate();
        let other = ActorKeypair::generate();
        let signed = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        let err = signed.verify(&other.actor_id()).unwrap_err();
        assert!(matches!(err, GrantEventError::SignatureFailed));
    }

    #[test]
    fn tampered_event_fails_verify() {
        let kp = ActorKeypair::generate();
        let mut signed = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        signed.window_end += 1; // tamper
        let err = signed.verify(&kp.actor_id()).unwrap_err();
        assert!(matches!(err, GrantEventError::SignatureFailed));
    }

    #[test]
    fn wrong_length_sig_reports_invalid_format_not_signature_failed() {
        let kp = ActorKeypair::generate();
        let mut signed = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        signed.sig = vec![0u8; 32];
        let err = signed.verify(&kp.actor_id()).unwrap_err();
        assert!(matches!(err, GrantEventError::WrongSignatureLength(32)));
    }

    #[test]
    fn signing_is_deterministic() {
        // Ed25519 is deterministic, so two devices independently re-signing
        // identical event content converge on identical bytes — this is
        // what lets the ledger merge dedup-by-equality (no vector clock
        // needed for this append-only, immutable-once-signed log).
        let kp = ActorKeypair::generate();
        let a = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        let b = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn revoke_event_with_empty_scope_and_zeroed_window_round_trips() {
        // The documented Revoke shape: nothing new declared, no window.
        let kp = ActorKeypair::generate();
        let event = GrantEvent {
            grant_id: [1u8; 16].to_vec(),
            holder: [2u8; 32].to_vec(),
            kind: GrantEventKind::Revoke,
            scope: vec![],
            window_start: 0,
            window_end: 0,
            at: 2000,
            sig: vec![0u8; GRANT_EVENT_SIGNATURE_LEN],
        };
        let signed = event.sign(kp.signing_key()).unwrap();
        signed.verify(&kp.actor_id()).unwrap();
    }

    #[test]
    fn renew_event_signs_and_verifies() {
        let kp = ActorKeypair::generate();
        let mut event = mint_event([1u8; 16], [2u8; 32]);
        event.kind = GrantEventKind::Renew;
        event.window_end += 90 * 24 * 60 * 60;
        event.at = 2000;
        let signed = event.sign(kp.signing_key()).unwrap();
        signed.verify(&kp.actor_id()).unwrap();
    }

    #[test]
    fn multiple_scope_tuples_round_trip() {
        let kp = ActorKeypair::generate();
        let mut event = mint_event([1u8; 16], [2u8; 32]);
        event.scope.push(GrantEventScope {
            class: "content.read".into(),
            kind: Some("caldav".into()),
            tier: Some("full".into()),
        });
        let signed = event.sign(kp.signing_key()).unwrap();
        signed.verify(&kp.actor_id()).unwrap();
        assert_eq!(signed.scope.len(), 2);
    }

    #[test]
    fn tampered_kind_fails_verify() {
        // The signature must cover `kind` itself — flipping a signed Mint
        // into a Revoke post-hoc (or vice versa) must not verify, or a
        // malicious box could relabel its own history.
        let kp = ActorKeypair::generate();
        let mut signed = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        signed.kind = GrantEventKind::Revoke;
        let err = signed.verify(&kp.actor_id()).unwrap_err();
        assert!(matches!(err, GrantEventError::SignatureFailed));
    }

    #[test]
    fn tampered_holder_fails_verify() {
        // Re-pointing a signed grant at a different holder must not verify
        // — otherwise a box could redirect someone else's grant to itself.
        let kp = ActorKeypair::generate();
        let mut signed = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        signed.holder = [9u8; 32].to_vec();
        let err = signed.verify(&kp.actor_id()).unwrap_err();
        assert!(matches!(err, GrantEventError::SignatureFailed));
    }

    /// The factor fold: a per-labeler tuple keeps its kind readable through
    /// `base_kind` and its factor through `factor`, a kind-less tuple folds to
    /// the factor alone, and a plain tuple reads back unchanged.
    #[test]
    fn factor_folds_into_kind_and_reads_back() {
        let factor = "labeler:aa";
        let mail = GrantEventScope {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: Some(GRANT_SCOPE_TIER_BOUNDED.into()),
        }
        .with_factor(factor);
        assert_eq!(mail.kind.as_deref(), Some("mail labeler:aa"));
        assert_eq!(mail.base_kind(), Some("mail"));
        assert_eq!(mail.factor(), Some(factor));
        assert_eq!(mail.tier.as_deref(), Some(GRANT_SCOPE_TIER_BOUNDED));

        let label_write = GrantEventScope {
            class: "content.label-write".into(),
            kind: None,
            tier: None,
        }
        .with_factor(factor);
        assert_eq!(label_write.kind.as_deref(), Some(factor));
        assert_eq!(
            label_write.base_kind(),
            None,
            "a kind-less tuple stays kind-less"
        );
        assert_eq!(label_write.factor(), Some(factor));

        let plain = GrantEventScope {
            class: "content.read".into(),
            kind: Some("mail".into()),
            tier: None,
        };
        assert_eq!(plain.base_kind(), Some("mail"));
        assert_eq!(plain.factor(), None);

        // Re-folding replaces, never doubles.
        let refolded = mail.clone().with_factor("labeler:bb");
        assert_eq!(refolded.kind.as_deref(), Some("mail labeler:bb"));
    }

    /// The fold rides an existing signed field, so a folded event signs and
    /// verifies exactly like any other — the frozen-shape guarantee.
    #[test]
    fn folded_factor_event_signs_and_verifies() {
        let kp = ActorKeypair::generate();
        let mut event = mint_event([1u8; 16], [2u8; 32]);
        event.scope[0] = event.scope[0].clone().with_factor("labeler:aa");
        let signed = event.sign(kp.signing_key()).unwrap();
        signed.verify(&kp.actor_id()).unwrap();
        assert_eq!(signed.scope[0].factor(), Some("labeler:aa"));
    }

    #[test]
    fn tampered_scope_fails_verify() {
        let kp = ActorKeypair::generate();
        let mut signed = mint_event([1u8; 16], [2u8; 32])
            .sign(kp.signing_key())
            .unwrap();
        signed.scope[0].tier = Some("upgraded".into());
        let err = signed.verify(&kp.actor_id()).unwrap_err();
        assert!(matches!(err, GrantEventError::SignatureFailed));
    }
}
