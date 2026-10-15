//! The SPA's door to **the let-go** — a dead generation's rows are retired by
//! the user, and by nothing else (`fleet-scope-reclamation.md`
//! § *Fleet-scope reclamation*, clause (3)(j); the surface is
//! `docs/goal/ui/settings.md` § Recovery kit, the fifth act).
//!
//! The twin of `fauna-ffi`'s `generation_let_go` door, export for export: the
//! read and the act are shared Rust (`fauna_account_plane::generation_let_go`),
//! reached through this tab's account-runtime handle
//! (`crate::account_runtime::handle`); this module decides nothing — which
//! generations are dead, what the line says and what the act retired all
//! arrive from the shared crate. **No export takes an identity**: the handle
//! knows its own.
//!
//! The one web difference is the clock face: the line's mint date is rendered
//! by `format_date(ms) → string`, the SPA's own, because
//! `format_unix_local_date_ms` needs the OS timezone database wasm lacks —
//! the `adminForceRotateIssuer` precedent. The sentence around it is still the
//! shared projection's.

use std::collections::BTreeSet;

use fauna_account_plane::generation_let_go::{
    DeadGeneration, LET_GO_CONFIRM_WORD, unreadable_status,
};
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::rpc::{from_js, to_js};

/// The account runtime's dead read, as the section renders it — the FFI's
/// `FfiDeadGenerations`.
///
/// **The render gate is `unreadable_status`**: the line, its confirm field and
/// its button render only while it is non-null — the shared projection's own
/// rule, so the SPA never re-derives it from the id list.
#[derive(Serialize, Default)]
struct DeadGenerationsJs {
    /// The dead generations' 32-byte ids, hex — what
    /// `recovery-kit-let-go-button` hands back to [`recovery_let_go`]. Opaque
    /// to the SPA; never rendered.
    generation_ids: Vec<String>,
    /// `recovery-kit-unreadable-status`, a `LocalizedText` the SPA resolves
    /// through `resolveLocalized`; `None` (→ `null`) when nothing is dead.
    unreadable_status: Option<fauna_core::localized::LocalizedText>,
}

impl DeadGenerationsJs {
    fn from_dead(dead: &[DeadGeneration], format_date: &js_sys::Function) -> Self {
        DeadGenerationsJs {
            generation_ids: dead.iter().map(|d| hex::encode(d.generation_id)).collect(),
            unreadable_status: unreadable_status(dead, |ms| {
                format_date
                    .call1(&JsValue::NULL, &JsValue::from_f64(ms as f64))
                    .ok()
                    .and_then(|v| v.as_string())
                    // A clock face that threw still owes the user a date: the
                    // raw milliseconds, never an empty gap.
                    .unwrap_or_else(|| ms.to_string())
            }),
        }
    }
}

/// What one press of `recovery-kit-let-go-button` did — the FFI's
/// `FfiLetGoOutcome`.
#[derive(Serialize)]
struct LetGoOutcomeJs {
    /// Rows the nest retired — `settings.recovery_kit.let_go_done`'s count.
    retired: u64,
    /// A requested generation no longer read dead when the act fired and was
    /// left untouched — the SPA appends `settings.recovery_kit.let_go_kept`.
    kept: bool,
    /// The dead read taken after the act. It **replaces** the one the section
    /// rendered from, so the trio leaves the screen once nothing is dead.
    dead: DeadGenerationsJs,
}

/// The confirm word `recovery-kit-let-go-confirm-field` must hold before
/// `recovery-kit-let-go-button` arms — shared Rust's one constant, never
/// localized and never re-spelled in the SPA, which re-checks it when the act
/// fires.
#[wasm_bindgen(js_name = recoveryLetGoConfirmWord)]
pub fn recovery_let_go_confirm_word() -> String {
    LET_GO_CONFIRM_WORD.to_string()
}

/// The dead generations of this account's fleet scope →
/// `{ generation_ids, unreadable_status }`, read at the Settings Account
/// page's hydrate.
///
/// **Never rejects.** With no runtime in this tab, or on a failed read
/// (logged), the answer is the empty one: a let-go that cannot be shown is not
/// offered — the fold tui's hydrate and the FFI door apply.
#[wasm_bindgen(js_name = recoveryDeadGenerations)]
pub fn recovery_dead_generations(format_date: js_sys::Function) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let Some(store) = crate::account_runtime::handle() else {
            return to_js(&DeadGenerationsJs::default());
        };
        match store.dead_generations().await {
            Ok(dead) => to_js(&DeadGenerationsJs::from_dead(&dead, &format_date)),
            Err(e) => {
                tracing::warn!("[settings] dead-generation read failed: {e:#}");
                to_js(&DeadGenerationsJs::default())
            }
        }
    })
}

/// `recovery-kit-let-go-button` — let go of `generation_ids` (hex), the ids
/// the section's last [`recovery_dead_generations`] read listed, then re-read
/// → `{ retired, kept, dead }`.
///
/// The runtime re-reads each generation dead before it retires anything; one
/// that no longer reads dead is left untouched and reported through `kept`.
/// Idempotent. The caller must have gated this behind the confirm field
/// reading [`recovery_let_go_confirm_word`].
///
/// Rejects — rendered through `settings.recovery_kit.let_go_failed` — with no
/// runtime in this tab, a malformed id, or the act or the re-read failing.
#[wasm_bindgen(js_name = recoveryLetGo)]
pub fn recovery_let_go(generation_ids: JsValue, format_date: js_sys::Function) -> js_sys::Promise {
    wasm_bindgen_futures::future_to_promise(async move {
        let ids: Vec<String> = from_js(generation_ids)?;
        let generations = generation_set(&ids).map_err(|e| JsValue::from_str(&e))?;
        let store = crate::account_runtime::handle()
            .ok_or_else(|| JsValue::from_str("the account store is not ready yet"))?;
        let failed = |e: anyhow::Error| JsValue::from_str(&format!("{e:#}"));
        let report = store.let_go(generations).await.map_err(failed)?;
        let dead = store.dead_generations().await.map_err(failed)?;
        to_js(&LetGoOutcomeJs {
            retired: report.retired as u64,
            kept: !report.refused.is_empty(),
            dead: DeadGenerationsJs::from_dead(&dead, &format_date),
        })
    })
}

/// The ids the SPA handed back, as the set the handle takes. A malformed one
/// refuses the whole act rather than being dropped: the user confirmed a
/// specific list.
fn generation_set(generation_ids: &[String]) -> Result<BTreeSet<[u8; 32]>, String> {
    generation_ids
        .iter()
        .map(|id| {
            let bytes = hex::decode(id).map_err(|e| format!("a generation id is hex: {e}"))?;
            <[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| format!("a generation id is 32 bytes, got {}", bytes.len()))
        })
        .collect()
}
