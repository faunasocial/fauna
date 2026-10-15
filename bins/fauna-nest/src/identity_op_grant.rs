//! The oracle's grant check, shared by every nest custodian (TP11 —
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *The oracle*).
//!
//! A custodian performs an operation class only against the owner's LIVE
//! `identity.op` grant to the principal's attested key, re-read per request.
//! The vocabulary and the admission predicate are
//! `fauna_core::identity_op`; this module is the nest's reading of a stored
//! grant blob through them — one function per question, so the Nostr, the
//! ActivityPub and the ATProto custodians cannot drift on what "admits" means.

use fauna_core::identity_op::{Custodian, GrantTupleRef, IdentityOpClass, grant_admits};
use fauna_mls::wrapped_blob::format::GrantBlob;

/// Does the stored grant `blob` admit `class` at `now`: a decodable grant
/// whose window is live and whose declared scope carries the class's
/// `identity.op` tuple? An undecodable blob admits nothing.
#[must_use]
pub fn grant_blob_admits(blob: &[u8], class: IdentityOpClass, now: u64) -> bool {
    let Ok(grant) = GrantBlob::from_canonical_bytes(blob) else {
        return false;
    };
    // Both bounds, through the one window check every authorizing decode
    // names (`state.rs`'s census).
    i64::try_from(now).is_ok_and(|now| fauna_mls::wrapped_blob::grant_window_is_open(&grant, now))
        && grant_admits(
            grant.scope.iter().map(|t| GrantTupleRef {
                class: t.class.as_str(),
                kind: t.kind.as_deref(),
            }),
            class,
        )
}

/// Does any of `blobs` admit a class `custodian` performs?
#[must_use]
pub fn any_grant_admits_custodian<'a>(
    blobs: impl IntoIterator<Item = &'a [u8]>,
    custodian: Custodian,
    now: u64,
) -> bool {
    let classes: Vec<IdentityOpClass> = IdentityOpClass::ALL
        .iter()
        .copied()
        .filter(|c| c.custodian() == custodian)
        .collect();
    blobs
        .into_iter()
        .any(|b| classes.iter().any(|c| grant_blob_admits(b, *c, now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::GrantWindow;

    fn blob(class: IdentityOpClass, window: GrantWindow) -> Vec<u8> {
        fauna_mls::wrapped_blob::build_grant_blob(
            &[0xA1; 32],
            &[0x01; 16],
            &[0x77; 32],
            None,
            window,
            &[(fauna_client_capabilities::identity_op_scope(class), None)],
        )
        .unwrap()
        .to_canonical_bytes()
        .unwrap()
    }

    /// Class-aware: a grant naming one custodian's class never opens
    /// another's, and a closed window admits nothing.
    #[test]
    fn a_grant_admits_its_own_class_its_own_custodian_and_only_while_live() {
        let ap = blob(IdentityOpClass::ActivityPubHttpSig, GrantWindow(100, 200));
        assert!(grant_blob_admits(
            &ap,
            IdentityOpClass::ActivityPubHttpSig,
            150
        ));
        assert!(!grant_blob_admits(
            &ap,
            IdentityOpClass::NostrSignEvent,
            150
        ));
        assert!(!grant_blob_admits(
            &ap,
            IdentityOpClass::ActivityPubHttpSig,
            99
        ));
        assert!(!grant_blob_admits(
            &ap,
            IdentityOpClass::ActivityPubHttpSig,
            201
        ));
        assert!(any_grant_admits_custodian(
            [ap.as_slice()],
            Custodian::ActivityPubActorKey,
            150
        ));
        assert!(!any_grant_admits_custodian(
            [ap.as_slice()],
            Custodian::NostrDepositedKey,
            150
        ));
        assert!(!grant_blob_admits(
            b"garbage",
            IdentityOpClass::ActivityPubHttpSig,
            150
        ));
    }
}
