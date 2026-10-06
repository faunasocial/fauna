//! UniFFI façade for **the let-go** — a dead generation's rows are retired by
//! the user, and by nothing else (`account-data-taxonomy.md` § The generation
//! machinery → *Fleet-scope reclamation*, clause (3)(j); the surface is
//! `docs/goal/ui/settings.md` § Recovery kit, the fifth act).
//!
//! The read and the act are shared Rust
//! (`fauna_account_plane::generation_let_go`), reached by tui and linux through
//! the account runtime's handle directly; the UniFFI apps need a door to the
//! same two handle methods, the copy projection and the confirm word. This
//! module is that door and decides nothing: which generations are dead, what
//! the line says and what the act retired all arrive from the shared crate.
//!
//! **No export takes an identity.** Both ride this process's account-store
//! handle (`crate::account_runtime::handle`), which knows its own.
//!
//! Gated behind `account-runtime` (default-on via `store-safe`): the handle is
//! the whole surface, and the Go mail-bridge `--no-default-features` build
//! hosts no runtime, so its checked-in bindings stay byte-identical.

use std::collections::BTreeSet;

use fauna_core::localized::LocalizedText;
use fauna_sync_engine::generation_let_go::{
    DeadGeneration, LET_GO_CONFIRM_WORD, unreadable_status,
};

use crate::FfiError;

/// The account runtime's dead read, as the section renders it.
///
/// **The render gate is [`Self::unreadable_status`]**: the line, its confirm
/// field and its button render only while it is `Some` — the shared
/// projection's own rule, so no app re-derives it from the id list.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct FfiDeadGenerations {
    /// The dead generations' raw 32-byte ids — what `recovery-kit-let-go-button`
    /// hands back to [`recovery_let_go`]. Opaque to the app; never rendered.
    pub generation_ids: Vec<Vec<u8>>,
    /// `recovery-kit-unreadable-status` — the shared `unreadable_status`
    /// projection (row count, earliest readable mint date, the sign-in-first
    /// warning), as a `LocalizedText` the app resolves through its own
    /// pipeline. `None` when nothing is dead.
    pub unreadable_status: Option<LocalizedText>,
}

impl From<&[DeadGeneration]> for FfiDeadGenerations {
    fn from(dead: &[DeadGeneration]) -> Self {
        FfiDeadGenerations {
            generation_ids: dead.iter().map(|d| d.generation_id.to_vec()).collect(),
            unreadable_status: unreadable_status(
                dead,
                fauna_core::format::format_unix_local_date_ms,
            ),
        }
    }
}

/// What one press of `recovery-kit-let-go-button` did.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiLetGoOutcome {
    /// Rows the nest retired — `settings.recovery_kit.let_go_done`'s count.
    pub retired: u64,
    /// Whether a requested generation no longer read dead when the act fired
    /// and was left untouched — the app appends
    /// `settings.recovery_kit.let_go_kept`, so data that became readable again
    /// is named as kept, never silently skipped.
    pub kept: bool,
    /// The dead read taken after the act. It **replaces** the one the section
    /// rendered from, so the trio leaves the screen once nothing is dead.
    pub dead: FfiDeadGenerations,
}

/// The confirm word `recovery-kit-let-go-confirm-field` must hold before
/// `recovery-kit-let-go-button` arms — shared Rust's one constant
/// (`generation_let_go::LET_GO_CONFIRM_WORD`), never localized and never
/// re-spelled per app. The app re-checks it when the act fires.
#[uniffi::export]
pub fn recovery_let_go_confirm_word() -> String {
    LET_GO_CONFIRM_WORD.to_string()
}

/// The dead generations of this account's fleet scope — what the Settings
/// recovery-kit section renders its let-go from, read at the Account hydrate.
///
/// **Never `Err`.** With no runtime yet, or on a failed read (logged), the
/// answer is the empty one: a let-go that cannot be shown is not offered —
/// the same fold tui's hydrate applies.
#[fauna_uniffi_async::export]
pub async fn recovery_dead_generations() -> FfiDeadGenerations {
    let Some(store) = crate::account_runtime::handle() else {
        return FfiDeadGenerations::default();
    };
    match store.dead_generations().await {
        Ok(dead) => FfiDeadGenerations::from(dead.as_slice()),
        Err(e) => {
            tracing::warn!("[settings] dead-generation read failed: {e:#}");
            FfiDeadGenerations::default()
        }
    }
}

/// `recovery-kit-let-go-button` — let go of `generation_ids`, the ids the
/// section's last [`recovery_dead_generations`] read listed, then re-read.
///
/// The runtime re-reads each generation dead before it retires anything
/// (`AccountStoreHandle::let_go`); one that no longer reads dead is left
/// untouched and reported through [`FfiLetGoOutcome::kept`]. Idempotent: a
/// repeat finishes what a deferred retire left. The caller must have gated
/// this behind the confirm field reading [`recovery_let_go_confirm_word`].
///
/// # Errors
///
/// No account runtime, a malformed id, or the act or the re-read failing —
/// rendered through `settings.recovery_kit.let_go_failed`.
#[fauna_uniffi_async::export]
pub async fn recovery_let_go(generation_ids: Vec<Vec<u8>>) -> Result<FfiLetGoOutcome, FfiError> {
    let generations = generation_set(&generation_ids)?;
    let store = crate::account_runtime::handle().ok_or_else(|| FfiError::General {
        msg: "the account store is not ready yet".into(),
    })?;
    let failed = |e: anyhow::Error| FfiError::General {
        msg: format!("{e:#}"),
    };
    let report = store.let_go(generations).await.map_err(failed)?;
    let dead = store.dead_generations().await.map_err(failed)?;
    Ok(FfiLetGoOutcome {
        retired: report.retired as u64,
        kept: !report.refused.is_empty(),
        dead: FfiDeadGenerations::from(dead.as_slice()),
    })
}

/// The ids an app handed back, as the set the handle takes. A malformed one
/// refuses the whole act rather than being dropped: the user confirmed a
/// specific list.
fn generation_set(generation_ids: &[Vec<u8>]) -> Result<BTreeSet<[u8; 32]>, FfiError> {
    generation_ids
        .iter()
        .map(|id| {
            <[u8; 32]>::try_from(id.as_slice()).map_err(|_| FfiError::General {
                msg: format!("a generation id is 32 bytes, got {}", id.len()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_sync_engine::generation_let_go::KEY_UNREADABLE_STATUS;

    /// The record's render gate is the SHARED projection: no line while
    /// nothing is dead, and the ids cross in the read's own order beside the
    /// line once something is.
    #[test]
    fn the_dead_read_crosses_with_the_shared_line_as_its_render_gate() {
        let none = FfiDeadGenerations::from([].as_slice());
        assert_eq!(none, FfiDeadGenerations::default());
        assert!(none.unreadable_status.is_none());

        let dead = [
            DeadGeneration {
                generation_id: [1u8; 32],
                minted_at_ms: Some(1_609_459_200_000),
                live_rows: 3,
            },
            DeadGeneration {
                generation_id: [2u8; 32],
                minted_at_ms: None,
                live_rows: 8,
            },
        ];
        let crossed = FfiDeadGenerations::from(dead.as_slice());
        assert_eq!(crossed.generation_ids, vec![vec![1u8; 32], vec![2u8; 32]]);
        assert_eq!(
            crossed.unreadable_status,
            unreadable_status(&dead, fauna_core::format::format_unix_local_date_ms)
        );
        let line = crossed.unreadable_status.expect("something is dead");
        assert_eq!(line.key, KEY_UNREADABLE_STATUS);
        assert_eq!(line.args["rows"], "11");
    }

    /// The ids round-trip into the handle's set, and a malformed one refuses
    /// the act instead of narrowing the list the user confirmed.
    #[test]
    fn a_malformed_generation_id_refuses_the_act() {
        let set = generation_set(&[vec![1u8; 32], vec![2u8; 32]]).expect("two 32-byte ids");
        assert_eq!(set, BTreeSet::from([[1u8; 32], [2u8; 32]]));
        assert!(generation_set(&[vec![1u8; 32], vec![2u8; 31]]).is_err());
    }

    /// The gate word is shared Rust's constant, not a second spelling.
    #[test]
    fn the_confirm_word_is_the_shared_constant() {
        assert_eq!(recovery_let_go_confirm_word(), LET_GO_CONFIRM_WORD);
    }
}
