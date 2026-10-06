use crate::types::Event;

/// The Nostr event kind for zap receipts (NIP-57).
pub const ZAP_RECEIPT_KIND: u64 = 9735;

/// A parsed NIP-57 zap receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZapReceipt {
    /// Event ID of the zapped event, if the zap targeted a specific event.
    pub target_event_id: Option<String>,
    /// Public key of the zap recipient.
    pub target_pubkey: String,
    /// Public key of the sender, extracted from the zap request description.
    pub sender_pubkey: Option<String>,
    /// Amount in millisatoshis, parsed from the bolt11 invoice.
    pub amount_msats: Option<u64>,
    /// The raw JSON string of the zap request (from the `description` tag).
    pub zap_request: Option<String>,
}

/// Returns `true` if the event is a kind-9735 zap receipt.
pub fn is_zap_receipt(event: &Event) -> bool {
    event.kind == ZAP_RECEIPT_KIND
}

/// Parse a kind-9735 zap receipt event into a [`ZapReceipt`].
///
/// Returns an error if the event is not kind 9735 or if the required `p` tag is missing.
pub fn parse_zap_receipt(event: &Event) -> anyhow::Result<ZapReceipt> {
    if !is_zap_receipt(event) {
        return Err(anyhow::anyhow!(
            "expected kind 9735, got kind {}",
            event.kind
        ));
    }

    // Required: `p` tag — pubkey of zap recipient
    let target_pubkey = event
        .tags
        .iter()
        .find(|t| t.name() == Some("p"))
        .and_then(|t| t.value())
        .ok_or_else(|| anyhow::anyhow!("zap receipt missing required `p` tag"))?
        .to_string();

    // Optional: `e` tag — event ID of zapped note
    let target_event_id = event
        .tags
        .iter()
        .find(|t| t.name() == Some("e"))
        .and_then(|t| t.value())
        .map(|s| s.to_string());

    // Optional: `bolt11` tag — BOLT-11 invoice
    let bolt11 = event
        .tags
        .iter()
        .find(|t| t.name() == Some("bolt11"))
        .and_then(|t| t.value())
        .map(|s| s.to_string());

    // Optional: `description` tag — zap request JSON
    let zap_request = event
        .tags
        .iter()
        .find(|t| t.name() == Some("description"))
        .and_then(|t| t.value())
        .map(|s| s.to_string());

    // Parse sender pubkey from the zap request JSON
    let sender_pubkey = zap_request
        .as_deref()
        .and_then(parse_sender_pubkey_from_description);

    // Parse amount from bolt11 invoice
    let amount_msats = bolt11.as_deref().and_then(parse_bolt11_amount_msats);

    Ok(ZapReceipt {
        target_event_id,
        target_pubkey,
        sender_pubkey,
        amount_msats,
        zap_request,
    })
}

// ── The trusted-receipt gate ────────────────────────────────────────────
//
// A kind-9735 receipt is signed by the *recipient's* LNURL/wallet server's
// nostr key — never by the sender, and never by anyone Fauna knows a priori.
// The event's own signature therefore proves only "somebody signed this",
// which is free: a kind-9735 is plain signed JSON, and its `bolt11` tag is
// never checked against a real Lightning payment by anyone in this codebase.
//
// So a receipt means something only relative to a payee-designated list of
// trusted signer pubkeys. The designation is app UI + nest state per the
// one-configuration-surface invariant, and deliberately NOT derived by
// fetching the payee's LNURL metadata — that would move the trust root
// off-box onto an attacker-influenceable URL. Full ratification, including
// that rejection and the rejection of the webhook-shaped `PaymentProvider`
// trait: docs/goal/behavior/monetization.md § Zap receipts — the trust model.
//
// This module stays pure and transport-free: both nest ingress points (the
// external-relay sweep and the nest's own relay endpoint) verify the event
// signature themselves and then ask this function what the receipt means.

/// A receipt the gate believed, and the *only* thing that can drive an
/// accounting write.
///
/// The fields are private and there is no public constructor: the sole way to
/// obtain a `TrustedZap` is out of [`ZapVerdict::Trusted`], which only
/// [`classify_zap_receipt`] mints. So a caller cannot fabricate one and record
/// an unjudged receipt — the accounting write is inexpressible on any event
/// the gate did not pass. It captures the zap's own event id and timestamp
/// alongside the parsed receipt so the writer needs nothing off the raw event,
/// which is what makes an event/receipt mismatch unrepresentable at the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedZap {
    receipt: ZapReceipt,
    zap_event_id: String,
    created_at: u64,
}

impl TrustedZap {
    /// The parsed receipt the gate believed.
    pub fn receipt(&self) -> &ZapReceipt {
        &self.receipt
    }
    /// The kind-9735 event's own id (the accounting row's primary key).
    pub fn zap_event_id(&self) -> &str {
        &self.zap_event_id
    }
    /// The kind-9735 event's `created_at`.
    pub fn created_at(&self) -> u64 {
        self.created_at
    }
}

/// What a kind-9735 receipt is worth to a given payee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZapVerdict {
    /// Believable: signed by a signer this payee designated, addressed to
    /// this payee, internally consistent per NIP-57, and — when it names a
    /// zapped event — that event is one this box holds *authored by this
    /// payee*. Carries the write-authorizing [`TrustedZap`] witness.
    Trusted(TrustedZap),
    /// Not believable — the caller must treat it as if it had never arrived.
    /// Carries the reason for logs/tests only; no caller may branch on it to
    /// partially believe a receipt.
    Untrusted(ZapUntrustedReason),
}

/// Why a receipt was not believed. Diagnostics only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZapUntrustedReason {
    /// Not a kind-9735 event, or missing the required `p` tag.
    NotAZapReceipt,
    /// `event.pubkey` is not among the payee's designated trusted signers.
    /// The ordinary case for a forged or third-party receipt.
    UndesignatedSigner,
    /// The receipt's `p` tag names somebody other than this payee.
    NotAddressedToPayee,
    /// The `description` zap request's own `p` tag disagrees with the
    /// receipt's — NIP-57 requires them to match, and a mismatch means the
    /// signer stapled a genuine zap request onto a different recipient.
    ZapRequestRecipientMismatch,
    /// The receipt names a zapped event (an `e` tag) that this box does not
    /// hold. A total for an event the box cannot see is unrenderable anyway,
    /// so it is refused rather than left to a reader to filter — the same
    /// "decide it at ingest, never at read" discipline as the other conjuncts.
    SubjectEventNotHeld,
    /// The receipt names a zapped event this box holds, but whose stored
    /// author is **not** the payee. A designated signer may speak only about
    /// the payee's *own* events; attributing a zap to somebody else's post is
    /// the forgery this conjunct closes.
    SubjectAuthorMismatch,
}

/// Decide what a **signature-verified** kind-9735 event means for `payee_pubkey`.
///
/// The caller MUST have verified `event`'s own Schnorr signature first (both
/// nest ingress points already do — the external-relay sweep via
/// `verify_event`, the relay endpoint via its own `verify_event` gate). This
/// function deliberately does not re-verify: it answers the question the
/// signature cannot, which is *whose* signature counts.
///
/// `trusted_signers` is the payee's designated list. An empty list means the
/// payee has designated nobody, so **every** receipt is untrusted — the
/// correct default for a payee who has not opted in (works-out-of-the-box
/// means no zap is silently believed, not that zaps are silently believed).
///
/// `held_subject_author` binds the *subject* — the fourth property. The first three conjuncts are all about the `p`
/// tag (who may speak for this payee's money); none constrains the `e` tag
/// (what event they may speak *about*). Without this, a signer a payee
/// designated to report their *own* income could attribute a zap to any other
/// user's post. So when the receipt names a zapped event, the caller passes
/// that event's **stored author** as seen on this box (`None` if the box does
/// not hold it, which also covers a store-read failure — failing closed):
///
/// * event not held (`None`) → [`ZapUntrustedReason::SubjectEventNotHeld`];
/// * held but authored by someone other than the payee →
///   [`ZapUntrustedReason::SubjectAuthorMismatch`].
///
/// A profile zap (no `e` tag) names no subject, so no binding applies and
/// `held_subject_author` is ignored. The lookup lives at ingest, never at read
/// (`monetization.md` § Zap receipts — the trust model), so every downstream
/// consumer inherits the guarantee structurally.
pub fn classify_zap_receipt(
    event: &Event,
    payee_pubkey: &str,
    trusted_signers: &[String],
    held_subject_author: Option<&str>,
) -> ZapVerdict {
    let Ok(receipt) = parse_zap_receipt(event) else {
        return ZapVerdict::Untrusted(ZapUntrustedReason::NotAZapReceipt);
    };

    // Who signed it. Checked before addressing so that a receipt from a
    // stranger reads as `UndesignatedSigner` whoever it names.
    if !trusted_signers
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&event.pubkey))
    {
        return ZapVerdict::Untrusted(ZapUntrustedReason::UndesignatedSigner);
    }

    if !receipt.target_pubkey.eq_ignore_ascii_case(payee_pubkey) {
        return ZapVerdict::Untrusted(ZapUntrustedReason::NotAddressedToPayee);
    }

    // NIP-57: the receipt embeds the signed kind-9734 zap request in its
    // `description` tag, and the request's own `p` names the recipient. A
    // trusted signer stapling a genuine request onto a *different* recipient
    // is one forgery a designated signer could still attempt, so the two
    // must agree. A receipt carrying no parseable request is not rejected
    // here — `description` is optional in the wild and the signer is already
    // designated; only an actual disagreement is fatal.
    if let Some(request_recipient) = receipt
        .zap_request
        .as_deref()
        .and_then(parse_recipient_pubkey_from_description)
        && !request_recipient.eq_ignore_ascii_case(&receipt.target_pubkey)
    {
        return ZapVerdict::Untrusted(ZapUntrustedReason::ZapRequestRecipientMismatch);
    }

    // The subject binding. When the receipt names a zapped
    // event, that event must be one this box holds *authored by this payee* —
    // otherwise a designated signer of one payee writes onto another payee's
    // post. A profile zap (no `e` tag) names no subject, so it is exempt.
    if receipt.target_event_id.is_some() {
        match held_subject_author {
            None => {
                return ZapVerdict::Untrusted(ZapUntrustedReason::SubjectEventNotHeld);
            }
            Some(author) if !author.eq_ignore_ascii_case(&receipt.target_pubkey) => {
                return ZapVerdict::Untrusted(ZapUntrustedReason::SubjectAuthorMismatch);
            }
            Some(_) => {}
        }
    }

    ZapVerdict::Trusted(TrustedZap {
        zap_event_id: event.id.clone(),
        created_at: event.created_at,
        receipt,
    })
}

/// Extract the recipient (`p` tag) from a zap-request JSON string.
fn parse_recipient_pubkey_from_description(description: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(description).ok()?;
    value
        .get("tags")?
        .as_array()?
        .iter()
        .find_map(|tag| {
            let arr = tag.as_array()?;
            (arr.first()?.as_str()? == "p").then(|| arr.get(1)?.as_str())?
        })
        .map(|s| s.to_string())
}

/// Extract the `pubkey` field from a zap request JSON string.
fn parse_sender_pubkey_from_description(description: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(description).ok()?;
    value
        .get("pubkey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Parse the amount in millisatoshis from a BOLT-11 invoice.
///
/// BOLT-11 invoices have a human-readable part (HRP) of the form
/// `ln<network>[<amount><multiplier>]` followed by the bech32 separator `1`.
/// The HRP is everything before the **last** `1` in the invoice string.
///
/// Multipliers (BTC fractions):
/// - `m` (milli,  1e-3): amount × 100_000_000 msats
/// - `u` (micro,  1e-6): amount × 100_000 msats
/// - `n` (nano,   1e-9): amount × 100 msats
/// - `p` (pico,  1e-12): amount / 10 msats (truncated)
/// - (none)             : amount × 100_000_000_000 msats
fn parse_bolt11_amount_msats(invoice: &str) -> Option<u64> {
    let lower = invoice.to_lowercase();

    // The bech32 separator is the last '1' in the string.
    let sep_pos = lower.rfind('1')?;
    let hrp = &lower[..sep_pos];

    // Must start with "ln"
    if !hrp.starts_with("ln") {
        return None;
    }

    // After "ln" comes the network (alphabetic chars), then optionally an amount + multiplier.
    let after_ln = &hrp[2..];

    // Skip the alphabetic network prefix (e.g. "bc", "tb", "bcrt", "tbs").
    // `?` returns None for amount-less invoices (no digits after the network prefix).
    let amount_start = after_ln.find(|c: char| c.is_ascii_digit())?;
    let amount_and_multiplier = &after_ln[amount_start..];

    if amount_and_multiplier.is_empty() {
        return None;
    }

    // Split the trailing multiplier letter from the numeric part.
    let last_char = amount_and_multiplier.chars().last()?;
    let (number_str, multiplier) = if last_char.is_ascii_alphabetic() {
        (
            &amount_and_multiplier[..amount_and_multiplier.len() - 1],
            Some(last_char),
        )
    } else {
        (amount_and_multiplier, None)
    };

    if number_str.is_empty() {
        return None;
    }

    let number: u64 = number_str.parse().ok()?;

    // 1 BTC = 100_000_000_000 msats (1e11)
    // Multipliers are fractions of BTC:
    //   m (milli,  1e-3): msats = number * 1e11 * 1e-3 = number * 1e8
    //   u (micro,  1e-6): msats = number * 1e11 * 1e-6 = number * 1e5
    //   n (nano,   1e-9): msats = number * 1e11 * 1e-9 = number * 1e2
    //   p (pico,  1e-12): msats = number * 1e11 * 1e-12 = number / 10 (truncate)
    //   (none):           msats = number * 1e11
    let msats = match multiplier {
        Some('m') => number.checked_mul(100_000_000)?,
        Some('u') => number.checked_mul(100_000)?,
        Some('n') => number.checked_mul(100)?,
        Some('p') => number / 10,
        None => number.checked_mul(100_000_000_000)?,
        _ => return None,
    };

    Some(msats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    fn make_zap_receipt(tags: Vec<Tag>) -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: ZAP_RECEIPT_KIND,
            tags,
            content: "".into(),
            sig: "c".repeat(128),
        }
    }

    // ── is_zap_receipt ──────────────────────────────────────────────────────

    #[test]
    fn is_zap_receipt_true_for_kind_9735() {
        let event = make_zap_receipt(vec![Tag::new(vec!["p".into(), "pk".into()])]);
        assert!(is_zap_receipt(&event));
    }

    #[test]
    fn is_zap_receipt_false_for_other_kinds() {
        let mut event = make_zap_receipt(vec![]);
        event.kind = 1;
        assert!(!is_zap_receipt(&event));

        event.kind = 9734;
        assert!(!is_zap_receipt(&event));

        event.kind = 0;
        assert!(!is_zap_receipt(&event));
    }

    // ── parse_zap_receipt — error cases ────────────────────────────────────

    #[test]
    fn parse_zap_receipt_wrong_kind_errors() {
        let mut event = make_zap_receipt(vec![Tag::new(vec!["p".into(), "pk".into()])]);
        event.kind = 1;
        assert!(parse_zap_receipt(&event).is_err());
    }

    #[test]
    fn parse_zap_receipt_missing_p_tag_errors() {
        let event = make_zap_receipt(vec![]);
        assert!(parse_zap_receipt(&event).is_err());
    }

    // ── parse_zap_receipt — basic parsing ───────────────────────────────────

    #[test]
    fn parse_zap_receipt_basic() {
        let target_pk = "d".repeat(64);
        let event = make_zap_receipt(vec![Tag::new(vec!["p".into(), target_pk.clone()])]);
        let receipt = parse_zap_receipt(&event).unwrap();
        assert_eq!(receipt.target_pubkey, target_pk);
        assert!(receipt.target_event_id.is_none());
        assert!(receipt.sender_pubkey.is_none());
        assert!(receipt.amount_msats.is_none());
        assert!(receipt.zap_request.is_none());
    }

    #[test]
    fn parse_zap_receipt_with_target_event() {
        let target_pk = "d".repeat(64);
        let target_eid = "e".repeat(64);
        let event = make_zap_receipt(vec![
            Tag::new(vec!["p".into(), target_pk.clone()]),
            Tag::new(vec!["e".into(), target_eid.clone()]),
        ]);
        let receipt = parse_zap_receipt(&event).unwrap();
        assert_eq!(receipt.target_pubkey, target_pk);
        assert_eq!(
            receipt.target_event_id.as_deref(),
            Some(target_eid.as_str())
        );
    }

    // ── parse_zap_receipt — zap request / sender pubkey ─────────────────────

    #[test]
    fn parse_zap_receipt_extracts_sender_from_description() {
        let sender_pk = "f".repeat(64);
        let zap_req_json = format!(
            r#"{{"pubkey":"{}","kind":9734,"tags":[],"content":""}}"#,
            sender_pk
        );
        let event = make_zap_receipt(vec![
            Tag::new(vec!["p".into(), "d".repeat(64)]),
            Tag::new(vec!["description".into(), zap_req_json.clone()]),
        ]);
        let receipt = parse_zap_receipt(&event).unwrap();
        assert_eq!(receipt.sender_pubkey.as_deref(), Some(sender_pk.as_str()));
        assert_eq!(receipt.zap_request.as_deref(), Some(zap_req_json.as_str()));
    }

    #[test]
    fn parse_zap_receipt_invalid_description_json_gives_none_sender() {
        let event = make_zap_receipt(vec![
            Tag::new(vec!["p".into(), "d".repeat(64)]),
            Tag::new(vec!["description".into(), "not-valid-json".into()]),
        ]);
        let receipt = parse_zap_receipt(&event).unwrap();
        assert!(receipt.sender_pubkey.is_none());
        // The raw zap_request string is preserved even if JSON is invalid
        assert_eq!(receipt.zap_request.as_deref(), Some("not-valid-json"));
    }

    // ── bolt11 amount parsing ───────────────────────────────────────────────

    #[test]
    fn bolt11_amount_milli_multiplier() {
        // lnbc10m = 10 milli-BTC = 10 * 100_000_000 msats = 1_000_000_000
        assert_eq!(
            parse_bolt11_amount_msats("lnbc10m1pvjluez..."),
            Some(1_000_000_000)
        );
    }

    #[test]
    fn bolt11_amount_micro_multiplier() {
        // lnbc500u = 500 micro-BTC = 500 * 100_000 msats = 50_000_000
        assert_eq!(parse_bolt11_amount_msats("lnbc500u1..."), Some(50_000_000));
    }

    #[test]
    fn bolt11_amount_nano_multiplier() {
        // lnbc1000n = 1000 nano-BTC = 1000 * 100 msats = 100_000
        assert_eq!(parse_bolt11_amount_msats("lnbc1000n1..."), Some(100_000));
    }

    #[test]
    fn bolt11_amount_pico_multiplier() {
        // lnbc10p = 10 pico-BTC = 10 / 10 = 1 msat
        assert_eq!(parse_bolt11_amount_msats("lnbc10p1..."), Some(1));
    }

    #[test]
    fn bolt11_amount_no_multiplier() {
        // lnbc1 = 1 BTC = 100_000_000_000 msats
        assert_eq!(
            parse_bolt11_amount_msats("lnbc11..."),
            Some(100_000_000_000)
        );
    }

    #[test]
    fn bolt11_amount_testnet_network() {
        // lntb250m = 250 milli-BTC testnet = 250 * 100_000_000 = 25_000_000_000
        assert_eq!(
            parse_bolt11_amount_msats("lntb250m1..."),
            Some(25_000_000_000)
        );
    }

    #[test]
    fn bolt11_amount_regtest_network() {
        // lnbcrt100u = 100 micro-BTC regtest = 100 * 100_000 = 10_000_000
        assert_eq!(
            parse_bolt11_amount_msats("lnbcrt100u1..."),
            Some(10_000_000)
        );
    }

    #[test]
    fn bolt11_amount_uppercase_invoice() {
        // Parser should be case-insensitive
        assert_eq!(parse_bolt11_amount_msats("LNBC500U1..."), Some(50_000_000));
    }

    #[test]
    fn bolt11_amount_no_amount_in_hrp() {
        // lnbc without amount is valid BOLT-11 (amount-less), we return None
        assert_eq!(parse_bolt11_amount_msats("lnbc1..."), None);
    }

    #[test]
    fn bolt11_amount_not_a_lightning_invoice() {
        assert_eq!(parse_bolt11_amount_msats("not_an_invoice"), None);
    }

    // ── parse_zap_receipt with bolt11 tag ───────────────────────────────────

    #[test]
    fn parse_zap_receipt_parses_bolt11_amount() {
        // 1000 sats = 1_000_000 msats = lnbc10u
        let event = make_zap_receipt(vec![
            Tag::new(vec!["p".into(), "d".repeat(64)]),
            Tag::new(vec!["bolt11".into(), "lnbc10u1pvjluez...".into()]),
        ]);
        let receipt = parse_zap_receipt(&event).unwrap();
        assert_eq!(receipt.amount_msats, Some(1_000_000));
    }

    #[test]
    fn parse_zap_receipt_full() {
        let target_pk = "d".repeat(64);
        let target_eid = "e".repeat(64);
        let sender_pk = "f".repeat(64);
        let zap_req = format!(r#"{{"pubkey":"{}","kind":9734}}"#, sender_pk);

        let event = make_zap_receipt(vec![
            Tag::new(vec!["p".into(), target_pk.clone()]),
            Tag::new(vec!["e".into(), target_eid.clone()]),
            Tag::new(vec!["bolt11".into(), "lnbc1000n1pvjluez...".into()]),
            Tag::new(vec!["description".into(), zap_req.clone()]),
        ]);
        let receipt = parse_zap_receipt(&event).unwrap();
        assert_eq!(receipt.target_pubkey, target_pk);
        assert_eq!(
            receipt.target_event_id.as_deref(),
            Some(target_eid.as_str())
        );
        assert_eq!(receipt.sender_pubkey.as_deref(), Some(sender_pk.as_str()));
        assert_eq!(receipt.amount_msats, Some(100_000)); // 1000n = 1000 * 100 msats
        assert_eq!(receipt.zap_request.as_deref(), Some(zap_req.as_str()));
    }

    // ── the trusted-receipt gate ────────────────────────────────────────

    // Hex pubkeys carrying letters, deliberately: all-digit fixtures make
    // `to_uppercase()` a no-op and silently vacate the case-folding test
    // (caught by the mutation matrix, not by a green run).
    const PAYEE: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const LNURL_SIGNER: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
    const STRANGER: &str = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";
    const OTHER_USER: &str = "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4";

    /// A receipt as the payee's own LNURL server would sign it.
    fn receipt_from(signer: &str, recipient: &str, tags: Vec<Tag>) -> Event {
        let mut all = vec![Tag::new(vec!["p".into(), recipient.into()])];
        all.extend(tags);
        let mut event = make_zap_receipt(all);
        event.pubkey = signer.to_string();
        event
    }

    fn zap_request_naming(recipient: &str) -> Tag {
        Tag::new(vec![
            "description".into(),
            format!(
                r#"{{"pubkey":"{}","kind":9734,"tags":[["p","{}"]],"content":""}}"#,
                STRANGER, recipient
            ),
        ])
    }

    /// A `p`-tag-only (profile) zap carries no subject, so `held_subject_author`
    /// is irrelevant; most tier_1 fixtures use this. The two args that vary per
    /// test are the signer list and the subject author.
    fn classify(event: &Event, signers: &[String], subject_author: Option<&str>) -> ZapVerdict {
        classify_zap_receipt(event, PAYEE, signers, subject_author)
    }

    /// An event-targeting receipt (`e` tag present) authored by `signer`,
    /// naming `PAYEE`, for `event_id`.
    fn receipt_for_event(signer: &str, event_id: &str) -> Event {
        receipt_from(
            signer,
            PAYEE,
            vec![Tag::new(vec!["e".into(), event_id.to_string()])],
        )
    }

    #[test]
    fn a_receipt_from_the_designated_signer_is_trusted() {
        let event = receipt_from(LNURL_SIGNER, PAYEE, vec![]);
        let verdict = classify(&event, &[LNURL_SIGNER.to_string()], None);
        match verdict {
            ZapVerdict::Trusted(z) => {
                assert_eq!(z.receipt().target_pubkey, PAYEE);
                assert_eq!(z.zap_event_id(), &event.id);
            }
            other => panic!("expected Trusted, got {other:?}"),
        }
    }

    #[test]
    fn a_receipt_from_an_undesignated_signer_is_inert() {
        // The whole forgery surface: a kind-9735 is plain signed JSON that
        // anyone can mint naming any recipient. Its own valid signature must
        // buy it nothing.
        let event = receipt_from(STRANGER, PAYEE, vec![]);
        assert_eq!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Untrusted(ZapUntrustedReason::UndesignatedSigner)
        );
    }

    #[test]
    fn a_payee_who_designated_nobody_believes_nobody() {
        // The out-of-the-box default must be "no zap is believed", never
        // "every zap is believed".
        let event = receipt_from(LNURL_SIGNER, PAYEE, vec![]);
        assert_eq!(
            classify(&event, &[], None),
            ZapVerdict::Untrusted(ZapUntrustedReason::UndesignatedSigner)
        );
    }

    #[test]
    fn another_users_designated_signer_does_not_carry_to_this_payee() {
        // Trust is per-payee. A signer OTHER_USER designated must not make a
        // receipt addressed to PAYEE believable.
        let event = receipt_from(LNURL_SIGNER, OTHER_USER, vec![]);
        assert_eq!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Untrusted(ZapUntrustedReason::NotAddressedToPayee)
        );
    }

    #[test]
    fn a_designated_signer_may_not_staple_a_request_for_someone_else() {
        // The one forgery a designated signer could still attempt *at the `p`
        // layer*: take a genuine zap request naming OTHER_USER and issue a
        // receipt claiming PAYEE was paid.
        let event = receipt_from(LNURL_SIGNER, PAYEE, vec![zap_request_naming(OTHER_USER)]);
        assert_eq!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Untrusted(ZapUntrustedReason::ZapRequestRecipientMismatch)
        );
    }

    #[test]
    fn a_designated_signer_cannot_attribute_a_zap_to_anothers_event() {
        // Every `p`-tag conjunct
        // passes (designated signer, addressed to PAYEE, no conflicting
        // request), but the `e`-tagged event is authored by OTHER_USER. The
        // designation speaks for PAYEE's money, not for what PAYEE's signer may
        // say about somebody else's post.
        let event = receipt_for_event(LNURL_SIGNER, &"e".repeat(64));
        assert_eq!(
            classify(&event, &[LNURL_SIGNER.to_string()], Some(OTHER_USER)),
            ZapVerdict::Untrusted(ZapUntrustedReason::SubjectAuthorMismatch)
        );
    }

    #[test]
    fn a_receipt_naming_an_event_this_box_does_not_hold_is_refused() {
        // The not-held sub-question, ruled REFUSE: an event the box cannot see
        // is unrenderable, and accepting-but-marking would push the check to
        // every reader.
        let event = receipt_for_event(LNURL_SIGNER, &"e".repeat(64));
        assert_eq!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Untrusted(ZapUntrustedReason::SubjectEventNotHeld)
        );
    }

    #[test]
    fn a_receipt_for_the_payees_own_event_is_trusted() {
        // The positive control for the subject binding: when the zapped event
        // is the payee's own, the receipt is believed.
        let event = receipt_for_event(LNURL_SIGNER, &"e".repeat(64));
        assert!(matches!(
            classify(&event, &[LNURL_SIGNER.to_string()], Some(PAYEE)),
            ZapVerdict::Trusted(_)
        ));
    }

    #[test]
    fn the_subject_author_match_is_case_insensitive() {
        // A stored author arriving upper-cased must still match the payee, for
        // the same reason the signer/payee comparisons fold case.
        let event = receipt_for_event(LNURL_SIGNER, &"e".repeat(64));
        assert!(matches!(
            classify(
                &event,
                &[LNURL_SIGNER.to_string()],
                Some(&PAYEE.to_uppercase())
            ),
            ZapVerdict::Trusted(_)
        ));
    }

    #[test]
    fn a_consistent_zap_request_is_trusted() {
        let event = receipt_from(LNURL_SIGNER, PAYEE, vec![zap_request_naming(PAYEE)]);
        assert!(matches!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Trusted(_)
        ));
    }

    #[test]
    fn an_unparseable_zap_request_does_not_by_itself_reject() {
        // `description` is optional in the wild; the signer is already
        // designated. Only an actual disagreement is fatal.
        let event = receipt_from(
            LNURL_SIGNER,
            PAYEE,
            vec![Tag::new(vec!["description".into(), "not-json".into()])],
        );
        assert!(matches!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Trusted(_)
        ));
    }

    #[test]
    fn a_non_zap_event_is_inert() {
        let mut event = receipt_from(LNURL_SIGNER, PAYEE, vec![]);
        event.kind = 1;
        assert_eq!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Untrusted(ZapUntrustedReason::NotAZapReceipt)
        );
    }

    #[test]
    fn signer_and_payee_matching_is_case_insensitive() {
        // Nostr hex pubkeys are lowercase by convention but arrive over the
        // wire from third parties; a case difference must not silently make
        // a designated signer untrusted.
        let event = receipt_from(&LNURL_SIGNER.to_uppercase(), &PAYEE.to_uppercase(), vec![]);
        assert!(matches!(
            classify(&event, &[LNURL_SIGNER.to_string()], None),
            ZapVerdict::Trusted(_)
        ));
    }

    #[test]
    fn one_designated_signer_among_several_suffices() {
        let event = receipt_from(LNURL_SIGNER, PAYEE, vec![]);
        let signers = vec![
            STRANGER.to_string(),
            LNURL_SIGNER.to_string(),
            OTHER_USER.to_string(),
        ];
        assert!(matches!(
            classify(&event, &signers, None),
            ZapVerdict::Trusted(_)
        ));
    }
}
