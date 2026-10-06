//! The Nostr key-succession aftermath's **npub check** — leg 3 of the ratified
//! succession row (`docs/goal/ui/nostr.md` § Key succession and rotation:
//! "the successor confirms the page shows their npub; a wrong or missing one
//! routes into the new-key path").
//!
//! # Why this plane needs none of `filter_marks`' classification machinery
//!
//! [`crate::raise_succession_filter_marks`] classifies a *growing list* of
//! rows against a succession bound, because `email_filters` carries no era
//! marker of its own. This plane tracks a single scalar: has the account
//! confirmed its current npub since its most recent succession? One stamp
//! comparable to the nest's own — the account plane's
//! `fauna.state.nostr-confirmation` row
//! ([`fauna_core::nostr_confirmation::NostrConfirmation`], read and written
//! through the account-store handle's `npub_confirmed_at` /
//! `confirm_nostr_npub` doors) — is enough: no roster, no per-row verdict, no
//! raise, no adjudication (nothing here is a row a thief could plant: the
//! sole writer is the owner's own "yes, that's my npub" gesture). Because
//! that gesture is the only writer, the stamp needs no post-auth pass and no
//! store-ready edge of its own: the confirm happens with the runtime up.
//!
//! This crate cannot see the handle (the plane depends on this crate), so the
//! read takes the stamp as a future the host builds from its handle — `None`
//! when the host has no account runtime yet — and every app shares the one
//! degrade policy below.
//!
//! # Reuses the succession-status fetch, not its millis conversion
//!
//! [`crate::filter_marks::fetch_succession_status_secs`] already owns the
//! `fauna.recovery.succession.status` round trip; this plane consumes the raw
//! epoch-**seconds** answer directly rather than through
//! [`crate::filter_marks::SuccessionTime`], whose millis rounding exists only
//! because `email_filters.created_at` is stored in milliseconds — irrelevant
//! here, where both sides of the comparison are already seconds.

use fauna_protocol::RpcRequester;
use fauna_protocol::requester::RpcErrorClass;

use crate::filter_marks::fetch_succession_status_secs;

/// Pure classifier: is the owner owed a confirmation right now?
///
/// `succeeded_at` is `None` for an account that has never succeeded — never
/// owed, the honest answer for the ordinary case. Otherwise owed when
/// `confirmed_at` is absent, or predates the succession — the thief-unlink-
/// and-relink case nostr.md:73 names, where a different npub was left linked.
///
/// Pure and total, so no app re-derives it and no nest round trip is needed
/// to test it.
pub fn npub_confirmation_owed(succeeded_at: Option<i64>, confirmed_at: Option<i64>) -> bool {
    match succeeded_at {
        None => false,
        Some(at) => confirmed_at.is_none_or(|c| c < at),
    }
}

/// Ask the nest + the account plane whether the caller's own npub
/// confirmation is owed right now — the read the Nostr page's load calls.
/// `confirmed_at` is the host's read of the stored stamp
/// (`AccountStoreHandle::npub_confirmed_at`), `None` when the host has
/// no account runtime; it is awaited only once the nest says a succession
/// happened.
///
/// Best-effort, mirroring [`crate::filter_marks::fetch_succession_time`]:
/// any unhappy answer on either leg (a refusal, a dropped connection, no
/// account runtime, an unreadable stamp) degrades to `false` rather than
/// failing the page. `false` is the safe direction here — a page that misses a
/// due confirmation this load is re-asked on the next one (harmless), where
/// showing a banner it cannot actually back up would be the false positive.
pub async fn npub_confirmation_owed_for<R, F, E>(nest: &R, confirmed_at: Option<F>) -> bool
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    F: Future<Output = Result<Option<i64>, E>>,
{
    let Some(confirmed_at) = confirmed_at else {
        return false;
    };
    let Some(succeeded_at) = fetch_succession_status_secs(nest).await else {
        return false;
    };
    match confirmed_at.await {
        Ok(confirmed_at) => npub_confirmation_owed(Some(succeeded_at), confirmed_at),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_succeeded_is_never_owed() {
        assert!(!npub_confirmation_owed(None, None));
        assert!(!npub_confirmation_owed(None, Some(100)));
    }

    #[test]
    fn succeeded_with_no_confirmation_on_file_is_owed() {
        assert!(npub_confirmation_owed(Some(100), None));
    }

    #[test]
    fn confirmation_older_than_the_succession_is_owed() {
        assert!(npub_confirmation_owed(Some(100), Some(99)));
    }

    #[test]
    fn confirmation_at_or_after_the_succession_is_not_owed() {
        assert!(!npub_confirmation_owed(Some(100), Some(100)));
        assert!(!npub_confirmation_owed(Some(100), Some(101)));
    }
}
