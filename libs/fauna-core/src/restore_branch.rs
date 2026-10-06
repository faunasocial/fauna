//! The restore branch (`writer-signed-change-records.md` § Writer-signed
//! change records, ruling (10)(b)): what a door about to put the current
//! identity's signature over a version's manifest may do with it.
//!
//! The table lives here, below every restore door, so the Media machine's
//! core (which carries no RPC crate) and the judged-listing doors read ONE
//! function; `fauna_client_sync::restore_branch` re-exports it beside the
//! judged listing and adds the form that takes a row verdict.

/// What a restore door may do with a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreDecision {
    /// Re-point the version verbatim: its manifest, size and stamp recorded
    /// as they were signed.
    Verbatim,
    /// The version was signed by another identity (a predecessor's, another
    /// writer's, an exempt class's) and carries no content-key stamp, so its
    /// bytes may rest under an owner root the current identity must never
    /// re-sign unopened (ruling (8)(d)): a door with a byte seam opens and
    /// re-seals it, a door without one refuses with the reason.
    NeedsReseal,
    /// Not a version: the row did not verify, or cannot be judged yet. Nothing
    /// is recorded.
    Refuse,
}

/// The decision over a version a judged listing kept — an admitted or a
/// history version (ruling (11)(f)), never a refused or held row.
///
/// `content_key_version` is the version's own signed stamp;
/// `current_vouches` is whether the current identity signed this version — or
/// any admitted row of the same set naming the same manifest, since the
/// statement binds the manifest to the set. The caller owns that memory; this
/// function reads only the answer.
///
/// Signed as the current identity → verbatim. Any other stamped version →
/// verbatim: a stamped record opens under its generation whoever re-signs it,
/// never under an owner root (ruling (10)(c)). Any other unstamped version →
/// the re-seal.
pub fn listed_restore_decision(
    content_key_version: Option<u64>,
    current_vouches: bool,
) -> RestoreDecision {
    if current_vouches || content_key_version.is_some() {
        RestoreDecision::Verbatim
    } else {
        RestoreDecision::NeedsReseal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listed_table() {
        assert_eq!(
            listed_restore_decision(None, true),
            RestoreDecision::Verbatim
        );
        assert_eq!(
            listed_restore_decision(Some(2), true),
            RestoreDecision::Verbatim
        );
        assert_eq!(
            listed_restore_decision(Some(2), false),
            RestoreDecision::Verbatim
        );
        assert_eq!(
            listed_restore_decision(None, false),
            RestoreDecision::NeedsReseal
        );
    }
}
