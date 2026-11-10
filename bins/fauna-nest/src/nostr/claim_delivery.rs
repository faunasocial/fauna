//! Delivering the unlock to a stranger who paid — the advertised sale's
//! completion (`docs/goal/behavior/monetization.md` § The Nostr advertisement
//! of a sold post → *Delivering the unlock to a stranger who paid*).
//!
//! A zap purchase by a buyer with no account on this box mints a claim code
//! (§ Pillar 3, Q4). When the receipt's sender is self-certified — its kind-9734
//! request is signed by the key it names (§ Zap receipts, check (iv)) — the
//! nest sends that key the code as a NIP-17 DM from the payee's custodial key.
//! It does so through the in-process Nostr DM leg exactly as the payee's own
//! app would: the room `(payee, buyer pubkey)` found or minted, the Sent copy
//! sealed to the payee's recipient key, the outbound item sealed to the leg's
//! key, and [`super::bridge_leg::drain_outbox`] gift-wrapping it. So the
//! author's Conversations show the message in a room with the buyer, and a
//! reply lands there too.
//!
//! **Once per zap.** The delivery's record (`nostr_zap_claim_deliveries`, keyed
//! on the zap's event id) is written in the send's own transaction
//! ([`crate::db::CacheDb::send_bridged_message_once`]), so a redelivered
//! receipt — or a crash replay, which reaches the waist again because the
//! purchase is applied before the zap is recorded — sends nothing twice and
//! never loses the one send.
//!
//! **The words are a public text.** A stranger reads the message before any
//! sign-in, so its sentence is a catalog string in an `app-strings-public`
//! region of `i18n/strings/en.yaml`, landed only by its public-texts round
//! (`docs/goal/architecture/public-texts.md`). The nest renders it to finished
//! English itself — a Nostr DM carries plaintext, never a key and args — and
//! sends nothing while the catalog does not hold the sentence: the mechanism
//! waits on its wording rather than inventing prose.
//!
//! **The link.** A buyer with no account redeems on the post's web page (the
//! advertisement's `r` tag), so when the purchased post is web-published the
//! message carries that page's URL — derived by the one nest-side rule,
//! [`crate::web_content::public_url::public_post_url`] — in the with-link
//! sentence; an unpublished post, or a URL that cannot be read, gets the
//! code-only one.

use std::sync::Arc;

use fauna_bridge_nostr::nip57::TrustedZap;

use crate::bridge_legs::NOSTR;
use crate::db::bridged_conversations::{
    BridgeSeat, BridgedRefused, NOSTR_LEG_BRIDGE_ID, NOSTR_LEG_PRINCIPAL_ID, RoomShape,
    nostr_leg_block,
};
use crate::nostr::bridge_leg::{SendRefused, far_room_id, send_precheck};
use crate::nostr::db;
use crate::routes::AppState;

/// The catalog key of the delivery's code-only sentence. Its one placeholder
/// is `{code}`.
pub const CLAIM_DELIVERY_KEY: &str = "subscriptions.claim_delivery_dm";

/// The catalog key of the delivery's with-link sentence, used when the
/// purchased post is web-published. Its two placeholders are `{code}` and
/// `{url}`.
pub const CLAIM_DELIVERY_LINK_KEY: &str = "subscriptions.claim_delivery_dm_link";

/// The DM's body for `code`, and for the purchased post's public page `url`
/// when it has one: the with-link sentence with both filled in, else the
/// code-only sentence — or `None` while the catalog holds neither usable
/// sentence. A with-link sentence missing from the catalog, or lacking either
/// placeholder, falls back to the code-only one, so a stranger never reads a
/// literal `{…}` and never misses the code.
#[must_use]
pub fn claim_delivery_body(code: &str, url: Option<&str>) -> Option<String> {
    let lookup = fauna_i18n::strings::lookup;
    url.and_then(|url| claim_delivery_link_body_from(lookup(CLAIM_DELIVERY_LINK_KEY)?, code, url))
        .or_else(|| claim_delivery_body_from(lookup(CLAIM_DELIVERY_KEY)?, code))
}

/// The with-link body over a given sentence, or `None` when it lacks either
/// placeholder.
#[must_use]
pub fn claim_delivery_link_body_from(sentence: &str, code: &str, url: &str) -> Option<String> {
    (sentence.contains("{code}") && sentence.contains("{url}"))
        .then(|| sentence.replace("{url}", url).replace("{code}", code))
}

/// The code-only body over a given sentence, or `None` when it lacks its
/// `{code}` placeholder (which would deliver nothing redeemable).
#[must_use]
pub fn claim_delivery_body_from(sentence: &str, code: &str) -> Option<String> {
    sentence
        .contains("{code}")
        .then(|| sentence.replace("{code}", code))
}

/// What one delivery attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimDelivery {
    /// Queued on the leg's outbox as Sent row `.0`.
    Queued(i64),
    /// This zap's code was already queued: nothing sent.
    AlreadyDelivered,
    /// The receipt's sender did not sign its request: nobody to deliver to —
    /// the author's audit stays the recourse.
    NoCertifiedSender,
    /// The catalog does not hold the sentence yet (its public-texts round has
    /// not landed): nothing sent.
    NoWording,
    /// The payee's Nostr key is not on this nest, or its relay list is empty.
    Undeliverable(SendRefused),
    /// The payee's guardian blocked this peer.
    Blocked,
    /// The payee has no recipient seal key: the Sent copy cannot rest sealed.
    NoSealKey,
    /// The payee's room or outbox cap on the leg is reached.
    Full,
}

/// Deliver the claim code `code` that `zap` bought from `payee` — for the post
/// whose hex id is `post_id` — to the receipt's self-certified sender. Called
/// by the purchase leg ([`super::zap_ingest::apply_zap_purchase`]) for an
/// unbound buyer only — a local actor is granted by the waist and needs no
/// message.
///
/// # Errors
/// A database or seal fault.
pub async fn deliver_claim_code(
    state: &Arc<AppState>,
    zap: &TrustedZap,
    payee: &[u8; 32],
    post_id: &str,
    code: &str,
) -> anyhow::Result<ClaimDelivery> {
    let receipt = zap.receipt();
    let Some(buyer) = receipt.sender_pubkey.as_deref() else {
        return Ok(ClaimDelivery::NoCertifiedSender);
    };
    let url = purchased_post_url(state, payee, post_id).await;
    let Some(body) = claim_delivery_body(code, url.as_deref()) else {
        return Ok(ClaimDelivery::NoWording);
    };
    deliver_claim_message(
        state,
        zap.zap_event_id(),
        payee,
        &receipt.target_pubkey,
        buyer,
        &body,
    )
    .await
}

/// The purchased post's public page URL, or `None` when it is not
/// web-published by `payee` or has no public origin. A fault reading it costs
/// the link, never the send: the code-only sentence still goes out.
async fn purchased_post_url(state: &AppState, payee: &[u8; 32], post_id: &str) -> Option<String> {
    let mut post = [0u8; 32];
    hex::decode_to_slice(post_id.trim(), &mut post).ok()?;
    crate::web_content::public_url::public_post_url(
        &state.db,
        payee,
        &post,
        &state.web_serving_domain(),
    )
    .await
    .inspect_err(|e| tracing::warn!(post_id, "claim delivery: the post's URL unread: {e:#}"))
    .ok()
    .flatten()
}

/// The send half of [`deliver_claim_code`], over a rendered `body`: from
/// `payee` (whose Nostr key is `payee_pubkey`) to `buyer_pubkey`, once per
/// `zap_event_id`.
///
/// # Errors
/// A database or seal fault; a `buyer_pubkey` that names no Nostr key.
pub async fn deliver_claim_message(
    state: &Arc<AppState>,
    zap_event_id: &str,
    payee: &[u8; 32],
    payee_pubkey: &str,
    buyer_pubkey: &str,
    body: &str,
) -> anyhow::Result<ClaimDelivery> {
    {
        // The cheap answer for the common redelivery; the send's transaction
        // stays the authority.
        let conn = state.db.conn().await;
        if db::has_claim_delivery(&conn, zap_event_id)? {
            return Ok(ClaimDelivery::AlreadyDelivered);
        }
    }
    let peer = far_room_id(buyer_pubkey)
        .ok_or_else(|| anyhow::anyhow!("the receipt's sender names no Nostr key"))?;
    if let Some(refused) = send_precheck(&state.db, payee).await? {
        return Ok(ClaimDelivery::Undeliverable(refused));
    }
    // The family's outbound rule, as the send handler applies it: only an
    // explicit guardian block refuses.
    if crate::bridge_dm_gate::outbound_blocked(state, payee, NOSTR_LEG_BRIDGE_ID, &peer)
        .await
        .map_err(|e| anyhow::anyhow!("guardian verdict: {}", e.code))?
    {
        return Ok(ClaimDelivery::Blocked);
    }
    let Some(seal_key) = state.db.get_recipient_seal_key(payee).await? else {
        return Ok(ClaimDelivery::NoSealKey);
    };
    let (_, leg_x25519) = state.db.first_party_bridge_key(NOSTR_LEG_BRIDGE_ID).await?;

    // The two blobs the payee's app would seal for a send it composed: the
    // item to the leg's key (the drain opens it), the Sent copy to the payee's
    // own recipient key.
    let kind = fauna_mls::wrapped_blob::RecordKind::Mail;
    let sealed_for_bridge = crate::bridge_routing_handlers::seal_recipient_blob(
        kind,
        body.as_bytes(),
        &leg_x25519,
        None,
        "nostr-claim-delivery",
    )
    .map_err(|e| anyhow::anyhow!("seal for the leg: {}", e.code))?;
    let sealed_for_self = crate::bridge_routing_handlers::seal_recipient_blob(
        kind,
        body.as_bytes(),
        &seal_key.mls_pubkey,
        Some(seal_key.mlkem_ek.as_slice()),
        "nostr-claim-delivery",
    )
    .map_err(|e| anyhow::anyhow!("seal for the payee: {}", e.code))?;

    let block = nostr_leg_block();
    let seat = BridgeSeat {
        principal_id: &NOSTR_LEG_PRINCIPAL_ID,
        bridge: &block,
        bridge_x25519: &leg_x25519,
    };
    let shape = RoomShape {
        label: None,
        participants: vec![peer.clone()],
        self_address: Some(payee_pubkey.to_string()),
    };
    let sent = async {
        let (room, _) = state
            .db
            .upsert_bridged_room(payee, &seat, &peer, &shape)
            .await?;
        state
            .db
            .send_bridged_message_once(
                payee,
                &room,
                &sealed_for_bridge,
                &sealed_for_self,
                |tx, id| db::record_claim_delivery(tx, zap_event_id, id),
            )
            .await
    }
    .await;
    let id = match sent {
        Ok(Some(id)) => id,
        Ok(None) => return Ok(ClaimDelivery::AlreadyDelivered),
        Err(e) if e.downcast_ref::<BridgedRefused>() == Some(&BridgedRefused::Full) => {
            return Ok(ClaimDelivery::Full);
        }
        Err(e) => return Err(e),
    };
    crate::bridge_legs::notify_changed(state, &NOSTR, payee, &peer);
    crate::bridge_legs::nudge_drain(state, &NOSTR_LEG_PRINCIPAL_ID);
    Ok(ClaimDelivery::Queued(id))
}

#[cfg(test)]
mod tests {
    use super::{
        CLAIM_DELIVERY_KEY, CLAIM_DELIVERY_LINK_KEY, claim_delivery_body, claim_delivery_body_from,
        claim_delivery_link_body_from,
    };

    /// The body fills the one placeholder and keeps the rest of the sentence.
    #[test]
    fn the_body_fills_the_code_into_its_sentence() {
        assert_eq!(
            claim_delivery_body_from("Your code is {code}. Redeem it.", "ABCD-1234").as_deref(),
            Some("Your code is ABCD-1234. Redeem it.")
        );
        // A sentence with no placeholder would deliver nothing redeemable.
        assert_eq!(claim_delivery_body_from("Thanks!", "ABCD-1234"), None);
    }

    /// The with-link body fills both placeholders, and a sentence lacking
    /// either is no body — the caller falls back to the code-only one.
    #[test]
    fn the_link_body_fills_the_code_and_the_url() {
        let url = "https://alice.example.org/post/teaser.html";
        assert_eq!(
            claim_delivery_link_body_from("Code {code}, enter it at {url}.", "ABCD-1234", url)
                .as_deref(),
            Some("Code ABCD-1234, enter it at https://alice.example.org/post/teaser.html.")
        );
        assert_eq!(
            claim_delivery_link_body_from("Code {code}.", "ABCD-1234", url),
            None
        );
        assert_eq!(
            claim_delivery_link_body_from("Enter it at {url}.", "ABCD-1234", url),
            None
        );
    }

    /// The catalog's sentences, once their round lands them, name exactly the
    /// placeholders the nest fills — another would reach the stranger as a
    /// literal `{…}`. Until then the nest sends nothing.
    #[test]
    fn the_catalog_sentences_name_exactly_their_placeholders() {
        match fauna_i18n::strings::lookup(CLAIM_DELIVERY_KEY) {
            Some(sentence) => {
                assert!(
                    sentence.contains("{code}"),
                    "{CLAIM_DELIVERY_KEY} lacks {{code}}"
                );
                assert_eq!(sentence.matches('{').count(), 1, "{CLAIM_DELIVERY_KEY}");
                let body = claim_delivery_body("ABCD-1234", None).expect("a body");
                assert!(body.contains("ABCD-1234") && !body.contains('{'));
            }
            None => assert_eq!(claim_delivery_body("ABCD-1234", None), None),
        }
        if let Some(sentence) = fauna_i18n::strings::lookup(CLAIM_DELIVERY_LINK_KEY) {
            assert!(
                sentence.contains("{code}") && sentence.contains("{url}"),
                "{CLAIM_DELIVERY_LINK_KEY} lacks {{code}} or {{url}}"
            );
            assert_eq!(
                sentence.matches('{').count(),
                2,
                "{CLAIM_DELIVERY_LINK_KEY}"
            );
        }
    }

    /// A post with a public page gets the with-link sentence carrying the URL;
    /// one without gets the code-only sentence, which never names a URL.
    #[test]
    fn a_url_picks_the_with_link_sentence() {
        let url = "https://alice.example.org/post/teaser.html";
        let with = claim_delivery_body("ABCD-1234", Some(url));
        let without = claim_delivery_body("ABCD-1234", None);
        if fauna_i18n::strings::lookup(CLAIM_DELIVERY_LINK_KEY).is_some() {
            let with = with.expect("a with-link body");
            assert!(with.contains("ABCD-1234") && with.contains(url) && !with.contains('{'));
        } else {
            assert_eq!(with, without, "no with-link sentence: the code-only one");
        }
        if let Some(without) = without {
            assert!(!without.contains(url));
        }
    }
}
