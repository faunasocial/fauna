//! UniFFI entry point for the MDA server-side `calendar-auto-schedule` gateway's
//! **mailbox-less** leg (`docs/goal/behavior/caldav-server.md` § Server-side
//! auto-schedule).
//!
//! When a stock CalDAV organizer (e.g. Apple Calendar, no Fauna app present)
//! invites a Fauna attendee with CalDAV enabled but **email disabled**, the
//! invite cannot ride email. The Go MDA instead seals it here — **end-to-end
//! encrypted from the nest** (the encryption invariant) — and ships the three
//! byte-blobs over the caller-scoped WS-RPC scheduling rail (`welcome.deliver`
//! tagged `Scheduling` + `channel.send`).
//!
//! The MDA never holds the organizer's Ed25519 secret (CalDAV auth is
//! password→capability), so the one-off MLS group is signed by a fresh
//! **ephemeral** identity minted per delivery, so on this rail the MLS creator
//! credential identifies nobody and the recipient ignores it (the tier_3
//! `conformance_caldav_scheduling_mailbox_less::ephemeral_mls_sender_scheduling_delivery_is_received_and_applied`
//! test pins that such a delivery is still received and applied). The iMIP
//! `ORGANIZER` line authenticates nothing either — it is sender-written text.
//! What identifies the deliverer is the **home-nest-attested record author**:
//! the nest posts this record *as the organizer* it caller-scoped the MDA to
//! (`caldav-server.md` § Who may mutate an existing event over the inbound
//! rail). Single-sourced through
//! [`fauna_mls::engine::MlsEngine::build_scheduling_delivery`] — the SAME sealing
//! a Fauna app's rail uses, so a server-fanned invite is byte-identical to a
//! client-fanned one (priority #2).

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;

/// Failure building a sealed scheduling delivery. UniFFI mirror in the
/// `fauna_mail` namespace (the cross-namespace `fauna_mls` Go-binding footgun —
/// same reason the iCalendar writer records live in `icalendar.rs`).
#[derive(Debug, thiserror::Error, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Error))]
pub enum SchedulingDeliveryError {
    /// `sender_actor_id` was not exactly 32 bytes.
    #[error("sender_actor_id must be 32 bytes, got {0}")]
    BadSenderActorId(u32),
    /// The recipient key package was malformed, or the MLS sealing failed.
    #[error("seal scheduling delivery: {0}")]
    Seal(String),
}

/// The sealed bytes the Go MDA ships over the caller-scoped WS-RPC scheduling
/// rail to deliver one iMIP to a mailbox-less Fauna attendee. UniFFI record in
/// the `fauna_mail` namespace; the Go MDA passes each field straight to the
/// matching RPC.
#[derive(Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SealedSchedulingDelivery {
    /// The MLS Welcome → `welcome.deliver` (tagged `Scheduling` so the recipient
    /// routes the channel to calendar-apply, never the chat UI).
    pub welcome_bytes: Vec<u8>,
    /// The one-off channel id (hex) → both `welcome.deliver` and `channel.send`.
    pub channel_id_hex: String,
    /// The first (and only) application-message envelope → `channel.send`.
    pub app_envelope: Vec<u8>,
}

/// Seal a one-off MLS scheduling delivery of `imip_rfc5322` to the recipient
/// whose **consumed** key package is `recipient_kp` (fetched via
/// `keypackage.fetch`), stamping `sender_actor_id` (the real organizer's 32-byte
/// actor id) as the app-level sender. The MLS signing identity is a fresh
/// ephemeral keypair (see the module docs). The Go MDA reaches it as
/// `mailfauna.BuildSchedulingDelivery` and then delivers the result over the
/// caller-scoped scheduling rail.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn build_scheduling_delivery(
    recipient_kp: Vec<u8>,
    sender_actor_id: Vec<u8>,
    imip_rfc5322: Vec<u8>,
) -> Result<SealedSchedulingDelivery, SchedulingDeliveryError> {
    let sender_bytes: [u8; 32] = sender_actor_id
        .as_slice()
        .try_into()
        .map_err(|_| SchedulingDeliveryError::BadSenderActorId(sender_actor_id.len() as u32))?;
    let sender = ActorId(sender_bytes);

    let engine = MlsEngine::new_in_memory(ActorKeypair::generate())
        .map_err(|e| SchedulingDeliveryError::Seal(e.to_string()))?;
    let delivery = engine
        .build_scheduling_delivery(&recipient_kp, sender, imip_rfc5322)
        .map_err(|e| SchedulingDeliveryError::Seal(e.to_string()))?;

    Ok(SealedSchedulingDelivery {
        welcome_bytes: delivery.welcome_bytes,
        channel_id_hex: delivery.channel_id.to_string(),
        app_envelope: delivery.app_envelope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A garbage key package surfaces a `Seal` error rather than panicking, so
    /// the Go MDA's best-effort gateway can log + swallow it.
    #[test]
    fn bad_key_package_is_a_seal_error() {
        let err =
            build_scheduling_delivery(vec![0u8; 8], vec![7u8; 32], b"BEGIN:VCALENDAR".to_vec())
                .unwrap_err();
        assert!(matches!(err, SchedulingDeliveryError::Seal(_)));
    }

    /// A non-32-byte sender actor id is rejected before any MLS work.
    #[test]
    fn bad_sender_actor_id_is_rejected() {
        let err = build_scheduling_delivery(vec![0u8; 8], vec![1u8; 16], Vec::new()).unwrap_err();
        assert_eq!(err, SchedulingDeliveryError::BadSenderActorId(16));
    }
}
