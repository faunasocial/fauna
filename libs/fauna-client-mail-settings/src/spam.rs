//! Shared orchestration for the user-facing `mail-spam` page (the per-account
//! spam-classifier management surface): reset the per-user Bayesian model,
//! opt in/out of the deployment-baseline contribution, and the training-history
//! list with per-row Undo.
//!
//! Authority for behavior: `docs/goal/behavior/mail-spam.md` § Reset
//! (`reset_spam_model`), § Cold start Path 2 (the deployment-baseline opt-in
//! toggle), § Training-sample retention / § Undo (`list_spam_training_history`;
//! the undo itself is client-side — every row's delta is sealed to the actor,
//! so [`MailSpamMachine`] inverts it through the [`SealedModelWriter`] and
//! deletes the row atomically via `put_spam_model`). Authority for UX/IDs:
//! `tests/e2e-unified/ui.yaml` `mail-spam` page +
//! `mail-spam-training-history-list` component.
//!
//! Mirrors `forwarders.rs` / `aliases.rs`: a [`MailSpamSnapshot`] the per-app
//! UI renders + a [`MailSpamAction`] surface it dispatches, over one WS-RPC seam
//! ([`MailSpamNest`]). linux is the lead app; the other apps lift this shape
//! (priority #2/#4). The seam returns the [`SpamTrainingView`] the snapshot
//! renders directly; the shared `rpc_glue` projects the wire row into it.
//!
//! [`SealedModelWriter`]: crate::machine::SealedModelWriter
//!
//! # Honest gap surfaced, not faked
//!
//! `mail-spam.md` § Cold start Path 2 names the
//! `mail-spam-contribute-baseline-toggle` opt-in but the doc's § Wire shapes
//! table does **not** name a setter RPC for it (it names the admin
//! `publish_spam_baseline`, not a per-user contribution flag). The seam's
//! [`MailSpamNest::set_baseline_contribution`] is the inferred per-user setter;
//! the consuming nest track ratifies the actual wire kind + persistence home
//! (likely the `fauna.state.mail` custody). Carried as a noted gap, not silently invented
//! into the goal doc.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_core::localized::LocalizedText;
use fauna_protocol::MaybeSendSync;
use serde::{Deserialize, Serialize};

use crate::error::{DispatchError, NestError};

/// The label a training event applied — `mail-spam-training-history-list-item-label`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrainingLabel {
    /// The message was trained as spam.
    Spam,
    /// The message was trained as ham (not spam).
    Ham,
    /// A label a newer nest wrote that this build does not know. Shown as a
    /// neutral badge; the row cannot be undone (the label decides which class
    /// an undo decrements). Never produced by a gesture of this app.
    Unknown,
}

/// The signal source a training event came from
/// (`mail-spam-training-history-list-item-source`), per `mail-spam.md`
/// § Training signal sources — one arm per wire `TrainingSource` (the wire set
/// is complete; the dormant `ColdStartBaseline` and `Other` arms left
/// 2026-10-01).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TrainingSource {
    /// The Fauna app's first-party "Mark as spam" / "Not spam" button.
    ExplicitButton,
    /// A third-party MUA set/cleared the IMAP `\Junk` flag.
    ImapJunkFlag,
    /// A third-party MUA moved the message into/out of the Junk mailbox.
    ImapJunkMove,
    /// A source a newer nest wrote that this build does not know. Shown as a
    /// neutral badge.
    Unknown,
}

/// The refusal for an undo of a row whose label this build does not know: the
/// label picks the class the inverse decrements, so a guess could corrupt the
/// model.
fn unknown_label_undo_refusal() -> DispatchError {
    DispatchError::InvalidState(
        "this training row's label is from a newer version — it cannot be undone here".into(),
    )
}

/// Canonical label for a [`TrainingLabel`], returned as [`LocalizedText`] so each
/// app resolves it through its own i18n runtime (mirrors
/// [`member_status_label`](crate::member_status_label)). Lifts the identical
/// two-arm map that linux/windows/apple/android each hard-coded for the
/// `mail-spam-training-history-list-item-label` badge (priority #1/#2).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn training_label_badge(label: TrainingLabel) -> LocalizedText {
    match label {
        TrainingLabel::Spam => LocalizedText::key("mail_spam.label_spam"),
        TrainingLabel::Ham => LocalizedText::key("mail_spam.label_ham"),
        TrainingLabel::Unknown => LocalizedText::key("mail_spam.label_unknown"),
    }
}

/// Canonical label for a [`TrainingSource`], returned as [`LocalizedText`] so each
/// app resolves it through its own i18n runtime. Lifts the identical
/// per-source map that linux/windows/apple/android each hard-coded for the
/// `mail-spam-training-history-list-item-source` badge (priority #1/#2).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn training_source_badge(source: TrainingSource) -> LocalizedText {
    match source {
        TrainingSource::ExplicitButton => LocalizedText::key("mail_spam.source_explicit_button"),
        TrainingSource::ImapJunkFlag => LocalizedText::key("mail_spam.source_imap_junk_flag"),
        TrainingSource::ImapJunkMove => LocalizedText::key("mail_spam.source_imap_junk_move"),
        TrainingSource::Unknown => LocalizedText::key("mail_spam.source_unknown"),
    }
}

/// One training-history row as the `mail-spam-training-history-list` renders it.
/// Projected by the seam from the (unbuilt) `spam_training_history` wire row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SpamTrainingView {
    /// Lowercase hex of the 16-byte `history_id` (UUID). Carried verbatim into
    /// the [`MailSpamAction::UndoTraining`] action — the UI never needs the raw
    /// bytes (mirrors `ForwarderView::alias_id_hex`).
    pub history_id_hex: String,
    /// The trained message's display string (subject / `Message-ID:`) plus the
    /// mailbox it was in at train time — `…-list-item-message`.
    pub message: String,
    /// `…-list-item-label`.
    pub label: TrainingLabel,
    /// `…-list-item-source`.
    pub source: TrainingSource,
    /// Epoch-millis the training event was applied — `…-list-item-created-at`.
    pub created_at_ms: i64,
    /// The row's stored `model_delta_applied` verbatim: always an **opaque
    /// sealed** `wrapped_blob` (sealed to the actor by the capability holder that
    /// trained). Not rendered — the undo unwraps it client-side; a row whose delta
    /// does not content-detect as sealed
    /// (`fauna_mail::spam::model_write::decode_history_delta`) fails the undo
    /// closed.
    #[serde(default)]
    pub model_delta_applied: Vec<u8>,
    /// The trained message's subject **sealed to the actor's own recipient key**
    /// (every row carries one; empty only on a malformed reply, where the
    /// nest's mailbox-only `message` is shown as is). The **explicit
    /// discriminator** for a sealed row (`!sealed_subject.is_empty()`, superseding
    /// content-detecting the delta):
    /// when non-empty the machine unwraps it under the actor's own key and
    /// composes `message` = `{unwrapped subject} · {mailbox}` (build-item 3 write
    /// side, `mail-spam.md` § Training-sample retention). Internal to the display
    /// compose — a client renders `message`, not this.
    #[serde(default)]
    pub sealed_subject: Vec<u8>,
    /// The mailbox the row was trained in (`INBOX` / `Junk` / …). For a sealed row
    /// the nest can't fold it into `message`, so the machine composes
    /// `{unwrapped subject} · {mailbox}` from this + [`Self::sealed_subject`]. Empty
    /// from a pre-build-item-3 nest (the machine then leaves the nest's `message`).
    #[serde(default)]
    pub mailbox: String,
}

impl SpamTrainingView {
    /// Build a row from its parts, hex-encoding the 16-byte history id. The test
    /// `FakeNest` and (once it lands) the real glue both project the wire row
    /// through here — keeping the hex projection in one place.
    // One arg per wire field: a projection constructor grows with the wire row,
    // and a params struct would just duplicate SpamTrainingView minus one field.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        history_id: &[u8],
        message: impl Into<String>,
        label: TrainingLabel,
        source: TrainingSource,
        created_at_ms: i64,
        model_delta_applied: Vec<u8>,
        sealed_subject: Vec<u8>,
        mailbox: impl Into<String>,
    ) -> Self {
        Self {
            history_id_hex: hex::encode(history_id),
            message: message.into(),
            label,
            source,
            created_at_ms,
            model_delta_applied,
            sealed_subject,
            mailbox: mailbox.into(),
        }
    }
}

/// What `list_spam_training_history` returns: the rows plus the per-user
/// baseline-contribution opt-in flag (the `mail-spam` page reads both on load).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SpamHistory {
    pub events: Vec<SpamTrainingView>,
    /// Whether this account opts its training into the deployment baseline
    /// (`mail-spam-contribute-baseline-toggle`); default off per
    /// `mail-spam.md` § Cold start Path 2.
    pub contribute_baseline: bool,
}

/// Coarse machine status for spinner / disabled-control rendering. Mirrors
/// `ForwarderStatus`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SpamStatus {
    Idle,
    Loading,
    Working,
}

/// Read-only snapshot the per-app UI renders for `mail-spam`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailSpamSnapshot {
    /// The most recent training-history rows (the nest paginates; the seed renders
    /// what it returns) — each one `mail-spam-training-history-list` row.
    pub events: Vec<SpamTrainingView>,
    /// `mail-spam-contribute-baseline-toggle` state.
    pub contribute_baseline: bool,
    pub status: SpamStatus,
    /// Last action's error, surfaced via `error-message`. While the backend is
    /// unbuilt every action surfaces the seam's `"unimplemented"` rejection here.
    pub error: Option<String>,
}

impl MailSpamSnapshot {
    fn empty() -> Self {
        Self {
            events: Vec::new(),
            contribute_baseline: false,
            status: SpamStatus::Idle,
            error: None,
        }
    }
}

/// Actions the per-app UI dispatches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum MailSpamAction {
    /// Re-read the training history + the contribution flag (page load / after a
    /// mutation).
    Refresh,
    /// Reset the per-user classifier: delete the model file + all training
    /// history (`reset_spam_model`). Irreversible — the UI confirms first.
    ResetModel,
    /// Opt the account's training in/out of the deployment baseline
    /// (`mail-spam.md` § Cold start Path 2).
    SetContributeBaseline { contribute: bool },
    /// Undo one training event by its hex `history_id` — client-side: unwraps the
    /// row's sealed delta, applies its inverse to the re-sealed model and deletes
    /// the row atomically (`put_spam_model` `history_op: Delete`).
    UndoTraining { history_id_hex: String },
}

/// WS-RPC seam to nest. Per-app glue implements this over the (unbuilt)
/// user-tier spam RPCs. Dual `async_trait` arm + `MaybeSendSync` supertrait so
/// the one seam serves native + wasm (mirrors `ForwarderNest`).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait MailSpamNest: MaybeSendSync {
    /// `fauna.bridges.list_spam_training_history` — the recent rows + the
    /// per-user baseline-contribution flag.
    async fn list_spam_training_history(&self) -> Result<SpamHistory, NestError>;
    /// `fauna.bridges.reset_spam_model` — delete the model + history for the
    /// authenticated actor.
    async fn reset_spam_model(&self) -> Result<(), NestError>;
    /// Inferred per-user setter for the baseline-contribution opt-in (see the
    /// module-level *Honest gap* note — `mail-spam.md` § Wire shapes names the
    /// admin `publish_spam_baseline` but not this per-user flag).
    async fn set_baseline_contribution(&self, contribute: bool) -> Result<(), NestError>;
}

/// Decode a row's hex history id to the 16-byte wire form. A corrupted snapshot
/// shouldn't crash the dispatch — a malformed / wrong-length string surfaces as
/// a user-visible error instead of panicking.
fn decode_history_id(history_id_hex: &str) -> Result<Vec<u8>, DispatchError> {
    crate::error::decode_hex_id16("spam history id", history_id_hex)
}

/// One instance per user client. Holds the rendered snapshot; drives the seam.
/// Mirrors `ForwarderMachine`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MailSpamMachine {
    nest: Arc<dyn MailSpamNest>,
    /// The sealed spam-model write capability (built from the actor's keypair, so
    /// it derives the same MSEK). Present ⇒ a **client-written** (sealed) row's
    /// undo runs fully client-side (unwrap the sealed delta → `ModelWriteOp::Undo`
    /// → atomic model-write + `history_op: Delete`); `None` ⇒ the undo fails
    /// closed (a build site that hasn't wired the writer — no server-side undo
    /// exists, the nest cannot invert a sealed delta).
    writer: Option<Arc<dyn crate::machine::SealedModelWriter>>,
    inner: Mutex<MailSpamSnapshot>,
    /// Orders the list re-reads, so a page re-read that started before a
    /// mutation can never land after the mutation's own and restore old rows.
    reads: crate::read_order::ReadOrder,
}

impl MailSpamMachine {
    pub fn new(
        nest: Arc<dyn MailSpamNest>,
        writer: Option<Arc<dyn crate::machine::SealedModelWriter>>,
    ) -> Self {
        Self {
            nest,
            writer,
            inner: Mutex::new(MailSpamSnapshot::empty()),
            reads: Default::default(),
        }
    }

    fn set_status(&self, status: SpamStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    async fn refresh(&self) -> Result<(), DispatchError> {
        let ticket = self.reads.begin();
        self.set_status(SpamStatus::Loading);
        // Read before taking the lock (no .await while holding it).
        let mut history = self.nest.list_spam_training_history().await?;
        // Compose the `{subject} · {mailbox}` display for client-written (sealed)
        // rows, which the nest can't format (it degrades `message` to the mailbox
        // alone). Runs before the lock — it awaits per sealed row.
        self.compose_sealed_displays(&mut history.events).await;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        if !self.reads.admit(ticket) {
            // A read that started after this one has already landed: this
            // reply describes an older list (a reset's rows, say).
            return Ok(());
        }
        snap.events = history.events;
        snap.contribute_baseline = history.contribute_baseline;
        snap.status = SpamStatus::Idle;
        Ok(())
    }

    /// Fill in the display `message` for **client-written** (sealed) rows: the
    /// nest can't read a sealed subject, so it returns the opaque
    /// `sealed_subject` + `mailbox` separately and degrades `message` to the mailbox alone. The
    /// client unwraps the subject under its own key and renders
    /// `{subject} · {mailbox}` (the same separator the nest uses for a plaintext
    /// row). The **explicit `!sealed_subject.is_empty()` discriminator** (superseding
    /// content-detecting the delta). Best-effort: a row whose subject won't unwrap,
    /// or a client with no reseal writer / no mail, keeps the nest's degraded
    /// `message` — never fails the whole list.
    async fn compose_sealed_displays(&self, events: &mut [SpamTrainingView]) {
        let Some(writer) = &self.writer else { return };
        for e in events.iter_mut() {
            if e.sealed_subject.is_empty() {
                continue;
            }
            if let Ok(Some(subject)) = writer.unwrap_history_subject(&e.sealed_subject).await {
                e.message = format!("{subject} · {}", e.mailbox);
            }
        }
    }

    async fn reset_model(&self) -> Result<(), DispatchError> {
        self.set_status(SpamStatus::Working);
        self.nest.reset_spam_model().await?;
        // Re-read so the now-empty history list renders.
        self.refresh().await
    }

    /// Opt this actor into (or out of) the deployment baseline — a bit + a
    /// **grant** gesture (`mail-spam.md` § Encrypted-mode interaction, piece (b)).
    /// The ordering is **forced** by b1-nest's gating: `fetch_spam_model` volunteers
    /// the box's aggregation `holder_seal_target` **only once the opt-in bit is
    /// set**, so ON must set the bit *before* the writer fetches to mint + seal.
    ///
    /// - **ON**: `set_baseline_contribution(true)` → the writer mints the keyless
    ///   `content.read{spam-model}` grant to the volunteered holder + seals the
    ///   initial holder copy of the current model.
    /// - **OFF**: the writer revokes the standing grant (the nest then drops the
    ///   paired copy on the clear) → `set_baseline_contribution(false)`.
    ///
    /// A build site with no `writer` wired (or no holder enrolled)
    /// degrades to bit-only (no copy, so the publish does not count this
    /// actor until a write attaches one). Crash-safe: an orphan grant with no copy (or copy with
    /// no grant) is harmless — publish skips an absent copy and the worklist gates
    /// on the standing grant.
    async fn set_contribute_baseline(&self, contribute: bool) -> Result<(), DispatchError> {
        self.set_status(SpamStatus::Working);
        if contribute {
            self.nest.set_baseline_contribution(true).await?;
            if let Some(writer) = &self.writer {
                writer.attach_and_mint_baseline_grant().await?;
            }
        } else {
            if let Some(writer) = &self.writer {
                writer.revoke_baseline_grant().await?;
            }
            self.nest.set_baseline_contribution(false).await?;
        }
        // Re-read so the toggle reflects the persisted value.
        self.refresh().await
    }

    async fn undo_training(&self, history_id_hex: String) -> Result<(), DispatchError> {
        self.set_status(SpamStatus::Working);
        let history_id = decode_history_id(&history_id_hex)?;
        // Every training row is written by a capability holder (the client or
        // the AUTH'd mail agent) with its forward delta SEALED to the actor's own
        // key — the nest can neither read nor invert it, so the undo runs here:
        // the atomic `{model-write + history-DELETE}` on `put_spam_model`
        // (`history_op`). Applying the model undo without the row delete would
        // leave the row undoable twice (a double-invert corrupts the model), so
        // the two commit atomically on one kind. A row not in the current list,
        // or one whose stored delta is not sealed, cannot be undone — fail
        // closed rather than guess (no server-side undo exists).
        let row = {
            let snap = self.inner.lock().expect("snapshot mutex");
            snap.events
                .iter()
                .find(|e| e.history_id_hex == history_id_hex)
                .map(|e| (e.model_delta_applied.clone(), e.label))
        };
        let Some((delta, label)) = row else {
            return Err(DispatchError::InvalidState(
                "training row is not in the current list — refresh and retry".into(),
            ));
        };
        if label == TrainingLabel::Unknown {
            return Err(unknown_label_undo_refusal());
        }
        let fauna_mail::spam::model_write::HistoryDelta::Sealed(sealed) =
            fauna_mail::spam::model_write::decode_history_delta(&delta)
        else {
            return Err(DispatchError::InvalidState(
                "training row carries no sealed delta — it cannot be undone".into(),
            ));
        };
        self.undo_sealed_row(sealed, label, history_id).await?;
        // Re-read so the undone event drops out of the list.
        self.refresh().await
    }

    /// Undo a **client-written** (sealed) training row entirely client-side: unwrap
    /// the row's sealed forward delta under the actor's own key, apply its inverse
    /// to the re-sealed model, and delete the consumed audit row **atomically**
    /// (`ModelWriteOp::Undo` + `history_op: Delete` on one `put_spam_model` write).
    /// The `TrainingLabel` picks the class the inverse decrements.
    async fn undo_sealed_row(
        &self,
        sealed_delta: Vec<u8>,
        label: TrainingLabel,
        history_id: Vec<u8>,
    ) -> Result<(), DispatchError> {
        let Some(writer) = &self.writer else {
            // A sealed row exists but no reseal capability is wired (a build site
            // that hasn't threaded the writer) — fail closed; only a holder of
            // the actor's key can invert a sealed delta.
            return Err(DispatchError::InvalidState(
                "client-written training row: this client has no sealed-model write \
                 capability wired to undo it"
                    .into(),
            ));
        };
        let Some(delta) = writer.unwrap_history_delta(&sealed_delta).await? else {
            // No MSEK — but a sealed row can only exist for a mail-enabled actor,
            // so this is an inconsistent state, not a normal degrade.
            return Err(DispatchError::InvalidState(
                "sealed training row but mail is not enabled — cannot unwrap it".into(),
            ));
        };
        let op = fauna_mail::spam::model_write::ModelWriteOp::Undo {
            delta,
            label: match label {
                TrainingLabel::Spam => fauna_mail::spam::SpamLabel::Spam,
                TrainingLabel::Ham => fauna_mail::spam::SpamLabel::Ham,
                // Unreachable through `undo_training`, which refuses first; kept
                // as a refusal so no other caller can decrement a guessed class.
                TrainingLabel::Unknown => return Err(unknown_label_undo_refusal()),
            },
        };
        match writer
            .write_spam_model(
                op,
                Some(crate::machine::HistoryWrite::Delete {
                    history_id: history_id.clone(),
                }),
            )
            .await?
        {
            // The model was re-sealed and the row deleted atomically nest-side.
            crate::machine::SpamModelWriteOutcome::Sealed { .. } => Ok(()),
            // Unreachable by construction — the one-lesson rule gates only a
            // history `Insert`, and an undo rides a `Delete`; the row is gone.
            crate::machine::SpamModelWriteOutcome::Duplicate { .. } => Ok(()),
            // No sealed write is possible (mail disabled since the row was
            // written, or a nest without the sealed-at-rest token): nothing was
            // written and no server-side undo exists — fail closed, the row stays.
            crate::machine::SpamModelWriteOutcome::ServerPath => Err(DispatchError::InvalidState(
                "no sealed spam-model write is possible (mail not enabled) — the \
                 training row cannot be undone"
                    .into(),
            )),
        }
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl MailSpamMachine {
    pub fn snapshot(&self) -> MailSpamSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl MailSpamMachine {
    /// Initial page load.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        self.refresh().await
    }

    pub async fn dispatch(&self, action: MailSpamAction) -> Result<(), DispatchError> {
        // Clear any prior error before the new action runs.
        self.inner.lock().expect("snapshot mutex").error = None;
        crate::dispatch_capturing_error!(
            self,
            SpamStatus,
            match action {
                MailSpamAction::Refresh => self.refresh().await,
                MailSpamAction::ResetModel => self.reset_model().await,
                MailSpamAction::SetContributeBaseline { contribute } => {
                    self.set_contribute_baseline(contribute).await
                }
                MailSpamAction::UndoTraining { history_id_hex } => {
                    self.undo_training(history_id_hex).await
                }
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// In-memory nest modelling the per-user spam state: `list` returns the
    /// rows and flag; `reset` clears the rows; `set_baseline_contribution`
    /// flips the flag. (No undo seam: undo runs client-side via the writer.)
    #[derive(Default)]
    struct FakeNest {
        events: StdMutex<Vec<SpamTrainingView>>,
        contribute: StdMutex<bool>,
        /// When set, the next list read takes its copy of the rows and then
        /// yields once before replying — the window a concurrent mutation
        /// lands in.
        hold_next_list: std::sync::atomic::AtomicBool,
    }

    fn event(
        id: u8,
        message: &str,
        label: TrainingLabel,
        source: TrainingSource,
    ) -> SpamTrainingView {
        SpamTrainingView::from_parts(
            &[id; 16],
            message,
            label,
            source,
            1_700_000_000_000,
            Vec::new(),
            Vec::new(),
            "",
        )
    }

    #[async_trait]
    impl MailSpamNest for FakeNest {
        async fn list_spam_training_history(&self) -> Result<SpamHistory, NestError> {
            let history = SpamHistory {
                events: self.events.lock().unwrap().clone(),
                contribute_baseline: *self.contribute.lock().unwrap(),
            };
            if self
                .hold_next_list
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                tokio::task::yield_now().await;
            }
            Ok(history)
        }

        async fn reset_spam_model(&self) -> Result<(), NestError> {
            self.events.lock().unwrap().clear();
            Ok(())
        }

        async fn set_baseline_contribution(&self, contribute: bool) -> Result<(), NestError> {
            *self.contribute.lock().unwrap() = contribute;
            Ok(())
        }
    }

    fn machine_with(events: Vec<SpamTrainingView>, contribute: bool) -> MailSpamMachine {
        MailSpamMachine::new(
            Arc::new(FakeNest {
                events: StdMutex::new(events),
                contribute: StdMutex::new(contribute),
                ..Default::default()
            }),
            None,
        )
    }

    /// A spy [`SealedModelWriter`] that records the write calls and returns canned
    /// unwrapped values — lets the sealed-row undo + display be unit-tested without
    /// a real reseal loop / keys.
    struct SpyWriter {
        unwrapped: std::collections::BTreeSet<String>,
        subject: String,
        calls: StdMutex<
            Vec<(
                fauna_mail::spam::model_write::ModelWriteOp,
                Option<crate::machine::HistoryWrite>,
            )>,
        >,
        /// Records the baseline seam calls in order — `true` = `attach_and_mint`
        /// (toggle ON), `false` = `revoke` (toggle OFF) — so the seam's ON/OFF
        /// routing is unit-testable without a real mint/config store.
        baseline_calls: StdMutex<Vec<bool>>,
    }

    #[async_trait]
    impl crate::machine::SealedModelWriter for SpyWriter {
        async fn unwrap_history_delta(
            &self,
            _sealed_delta: &[u8],
        ) -> Result<Option<std::collections::BTreeSet<String>>, DispatchError> {
            Ok(Some(self.unwrapped.clone()))
        }
        async fn unwrap_history_subject(
            &self,
            _sealed_subject: &[u8],
        ) -> Result<Option<String>, DispatchError> {
            Ok(Some(self.subject.clone()))
        }
        async fn write_spam_model(
            &self,
            op: fauna_mail::spam::model_write::ModelWriteOp,
            history: Option<crate::machine::HistoryWrite>,
        ) -> Result<crate::machine::SpamModelWriteOutcome, DispatchError> {
            self.calls.lock().unwrap().push((op, history));
            Ok(crate::machine::SpamModelWriteOutcome::Sealed {
                sample_count: 0,
                delta: Default::default(),
            })
        }
        async fn attach_and_mint_baseline_grant(&self) -> Result<(), DispatchError> {
            self.baseline_calls.lock().unwrap().push(true);
            Ok(())
        }
        async fn revoke_baseline_grant(&self) -> Result<(), DispatchError> {
            self.baseline_calls.lock().unwrap().push(false);
            Ok(())
        }
    }

    fn machine_with_writer(
        events: Vec<SpamTrainingView>,
        writer: Arc<SpyWriter>,
    ) -> MailSpamMachine {
        MailSpamMachine::new(
            Arc::new(FakeNest {
                events: StdMutex::new(events),
                contribute: StdMutex::new(false),
                ..Default::default()
            }),
            Some(writer),
        )
    }

    /// A re-read that took its copy of the list BEFORE a reset must not land
    /// AFTER the reset's own re-read and put the reset rows back. The page
    /// re-reads on its own (mount, becoming visible) while a gesture's
    /// mutation re-reads too, and the two run concurrently. A linux whole-suite
    /// sweep (2026-09-22) ended a reset with "after reset, list shows 2".
    #[tokio::test]
    async fn a_read_that_started_before_a_reset_cannot_restore_the_reset_rows() {
        let nest = Arc::new(FakeNest {
            events: StdMutex::new(vec![
                event(
                    0x01,
                    "Cheap pills — INBOX",
                    TrainingLabel::Spam,
                    TrainingSource::ExplicitButton,
                ),
                event(
                    0x02,
                    "Lunch? — INBOX",
                    TrainingLabel::Ham,
                    TrainingSource::ImapJunkMove,
                ),
            ]),
            ..Default::default()
        });
        nest.hold_next_list
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let m = MailSpamMachine::new(nest.clone(), None);

        let (hydrated, reset) = tokio::join!(m.hydrate(), m.dispatch(MailSpamAction::ResetModel));
        hydrated.unwrap();
        reset.unwrap();

        assert!(
            nest.events.lock().unwrap().is_empty(),
            "precondition: the reset reached the nest"
        );
        assert!(
            m.snapshot().events.is_empty(),
            "the page re-read that started before the reset landed after it and \
             restored the reset rows: {:?}",
            m.snapshot().events
        );
    }

    #[tokio::test]
    async fn refresh_projects_events_and_flag() {
        let m = machine_with(
            vec![
                event(
                    0x01,
                    "Cheap pills — INBOX",
                    TrainingLabel::Spam,
                    TrainingSource::ExplicitButton,
                ),
                event(
                    0x02,
                    "Lunch? — INBOX",
                    TrainingLabel::Ham,
                    TrainingSource::ImapJunkMove,
                ),
            ],
            true,
        );
        m.hydrate().await.unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.events.len(), 2);
        assert_eq!(snap.events[0].label, TrainingLabel::Spam);
        assert_eq!(snap.events[0].source, TrainingSource::ExplicitButton);
        assert_eq!(snap.events[0].history_id_hex, hex::encode([0x01u8; 16]));
        assert!(snap.contribute_baseline);
        assert_eq!(snap.status, SpamStatus::Idle);
        assert!(snap.error.is_none());
    }

    #[tokio::test]
    async fn reset_clears_history() {
        let m = machine_with(
            vec![event(
                0x05,
                "x",
                TrainingLabel::Spam,
                TrainingSource::ExplicitButton,
            )],
            false,
        );
        m.hydrate().await.unwrap();
        m.dispatch(MailSpamAction::ResetModel).await.unwrap();
        assert!(m.snapshot().events.is_empty());
        assert!(m.snapshot().error.is_none());
    }

    #[tokio::test]
    async fn set_contribute_baseline_persists() {
        let m = machine_with(vec![], false);
        m.hydrate().await.unwrap();
        assert!(!m.snapshot().contribute_baseline);
        m.dispatch(MailSpamAction::SetContributeBaseline { contribute: true })
            .await
            .unwrap();
        assert!(m.snapshot().contribute_baseline);
    }

    /// With a writer wired, the toggle drives the grant seam: ON routes to
    /// `attach_and_mint_baseline_grant`, OFF to `revoke_baseline_grant` — the
    /// bit + grant gesture (`mail-spam.md` § Encrypted-mode interaction, piece (b)).
    /// The deeper mint/copy/grant-log behavior is proven against the real
    /// `MailSettingsMachine` writer in `tests/spam_model_write.rs`.
    #[tokio::test]
    async fn set_contribute_baseline_drives_the_grant_seam() {
        let spy = Arc::new(SpyWriter {
            unwrapped: std::collections::BTreeSet::new(),
            subject: String::new(),
            calls: StdMutex::new(Vec::new()),
            baseline_calls: StdMutex::new(Vec::new()),
        });
        let m = machine_with_writer(vec![], Arc::clone(&spy));
        m.hydrate().await.unwrap();

        m.dispatch(MailSpamAction::SetContributeBaseline { contribute: true })
            .await
            .unwrap();
        assert_eq!(
            *spy.baseline_calls.lock().unwrap(),
            vec![true],
            "ON ⇒ attach_and_mint_baseline_grant"
        );

        m.dispatch(MailSpamAction::SetContributeBaseline { contribute: false })
            .await
            .unwrap();
        assert_eq!(
            *spy.baseline_calls.lock().unwrap(),
            vec![true, false],
            "OFF ⇒ revoke_baseline_grant"
        );
        assert!(m.snapshot().error.is_none());
    }

    /// A row that does not carry a SEALED delta (a plaintext JSON n-gram array,
    /// or no delta at all) cannot come from a current nest and has no undo — no
    /// server-side undo exists. The machine fails closed: an honest error, no
    /// write, the row stays. Even with a writer wired.
    #[tokio::test]
    async fn undo_of_a_row_without_a_sealed_delta_fails_closed() {
        for delta in [br#"["win","prize"]"#.to_vec(), Vec::new()] {
            let mut row = event(
                0x07,
                "a",
                TrainingLabel::Spam,
                TrainingSource::ExplicitButton,
            );
            row.model_delta_applied = delta.clone();
            let spy = Arc::new(SpyWriter {
                unwrapped: std::collections::BTreeSet::new(),
                subject: String::new(),
                calls: StdMutex::new(Vec::new()),
                baseline_calls: StdMutex::new(Vec::new()),
            });
            let m = machine_with_writer(vec![row], Arc::clone(&spy));
            m.hydrate().await.unwrap();
            let err = m
                .dispatch(MailSpamAction::UndoTraining {
                    history_id_hex: hex::encode([0x07u8; 16]),
                })
                .await
                .unwrap_err();
            assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
            assert!(
                spy.calls.lock().unwrap().is_empty(),
                "no write for {delta:?}"
            );
            assert_eq!(m.snapshot().events.len(), 1, "the row stays");
        }
    }

    /// An undo of a row the current list does not hold fails closed (the
    /// machine needs the row's sealed delta to invert it).
    #[tokio::test]
    async fn undo_of_a_row_not_in_the_list_fails_closed() {
        let m = machine_with(vec![], false);
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(MailSpamAction::UndoTraining {
                history_id_hex: hex::encode([0x07u8; 16]),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    }

    /// A row whose stored delta is an **opaque sealed** blob, with **no** reseal
    /// writer wired (a build site that hasn't threaded it): the undo fails closed
    /// with an honest error and the row stays.
    #[tokio::test]
    async fn undo_sealed_row_without_writer_fails_closed() {
        let mut row = event(
            0x07,
            "a",
            TrainingLabel::Spam,
            TrainingSource::ExplicitButton,
        );
        // Any non-JSON byte shape reads as sealed (`decode_history_delta`).
        row.model_delta_applied = vec![0xA2, 0x01, 0x02, 0x03];
        let m = machine_with(vec![row], false);
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(MailSpamAction::UndoTraining {
                history_id_hex: hex::encode([0x07u8; 16]),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
        assert_eq!(m.snapshot().events.len(), 1, "the row must survive");
    }

    /// A row whose label a newer nest wrote (`Unknown`) cannot be undone: the
    /// label picks the class the inverse decrements, so a guess could corrupt
    /// the model. The refusal comes before anything is unwrapped or written, and
    /// the row stays.
    #[tokio::test]
    async fn undo_of_a_row_with_an_unknown_label_is_refused() {
        let mut row = event(
            0x0C,
            "a",
            TrainingLabel::Unknown,
            TrainingSource::ExplicitButton,
        );
        row.model_delta_applied = vec![0xA2, 0x01, 0x02, 0x03]; // non-JSON ⇒ sealed
        let spy = Arc::new(SpyWriter {
            unwrapped: std::collections::BTreeSet::from(["win".to_string()]),
            subject: String::new(),
            calls: StdMutex::new(Vec::new()),
            baseline_calls: StdMutex::new(Vec::new()),
        });
        let m = machine_with_writer(vec![row], Arc::clone(&spy));
        m.hydrate().await.unwrap();
        let err = m
            .dispatch(MailSpamAction::UndoTraining {
                history_id_hex: hex::encode([0x0Cu8; 16]),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
        assert!(
            spy.calls.lock().unwrap().is_empty(),
            "no model write may follow a refused undo"
        );
        assert_eq!(m.snapshot().events.len(), 1, "the row must survive");
    }

    /// Leg 1c: a **client-written** (sealed) row's undo, with the reseal writer
    /// wired, unwraps the sealed delta and rides the atomic model-undo +
    /// history-DELETE on the writer. Proves the op the machine composes.
    #[tokio::test]
    async fn undo_sealed_row_runs_client_side_via_writer() {
        let mut row = event(
            0x09,
            "Cheap meds — INBOX",
            TrainingLabel::Spam,
            TrainingSource::ExplicitButton,
        );
        row.model_delta_applied = vec![0xA2, 0x01, 0x02, 0x03]; // non-JSON ⇒ sealed
        let spy = Arc::new(SpyWriter {
            unwrapped: std::collections::BTreeSet::from(["win".to_string(), "prize".to_string()]),
            subject: String::new(),
            calls: StdMutex::new(Vec::new()),
            baseline_calls: StdMutex::new(Vec::new()),
        });
        let m = machine_with_writer(vec![row], Arc::clone(&spy));
        m.hydrate().await.unwrap();
        m.dispatch(MailSpamAction::UndoTraining {
            history_id_hex: hex::encode([0x09u8; 16]),
        })
        .await
        .expect("client-side undo succeeds");

        let calls = spy.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly one write");
        let (op, history) = &calls[0];
        // The inverse of the unwrapped delta, on the row's own label.
        match op {
            fauna_mail::spam::model_write::ModelWriteOp::Undo { delta, label } => {
                assert_eq!(*label, fauna_mail::spam::SpamLabel::Spam);
                assert_eq!(
                    *delta,
                    std::collections::BTreeSet::from(["win".to_string(), "prize".to_string()])
                );
            }
            other => panic!("expected Undo, got {other:?}"),
        }
        // The consumed audit row is deleted atomically on the same write.
        assert_eq!(
            history,
            &Some(crate::machine::HistoryWrite::Delete {
                history_id: [0x09u8; 16].to_vec(),
            })
        );
        assert!(m.snapshot().error.is_none());
    }

    /// A **client-written** (sealed) row: the nest degrades `message` to the
    /// mailbox alone and returns the opaque `sealed_subject`; the machine unwraps
    /// it under the actor's key and composes `{subject} · {mailbox}` on refresh.
    #[tokio::test]
    async fn sealed_row_message_is_composed_from_unwrapped_subject_and_mailbox() {
        let mut row = event(
            0x0A,
            "Junk", // nest's degraded message (mailbox alone) for a sealed row
            TrainingLabel::Spam,
            TrainingSource::ImapJunkFlag,
        );
        row.sealed_subject = vec![0xB1, 0x02, 0x03]; // non-empty ⇒ sealed discriminator
        row.mailbox = "Junk".to_string();
        let spy = Arc::new(SpyWriter {
            unwrapped: std::collections::BTreeSet::new(),
            subject: "Cheap meds 90% off".to_string(),
            calls: StdMutex::new(Vec::new()),
            baseline_calls: StdMutex::new(Vec::new()),
        });
        let m = machine_with_writer(vec![row], Arc::clone(&spy));
        m.hydrate().await.unwrap();
        assert_eq!(
            m.snapshot().events[0].message,
            "Cheap meds 90% off · Junk",
            "sealed row renders {{subject}} · {{mailbox}}"
        );
    }

    /// A row with an empty `sealed_subject` (a malformed reply — every current
    /// row carries one) keeps the nest's `message` untouched: the compose is
    /// best-effort, never a failed list.
    #[tokio::test]
    async fn plaintext_row_message_is_left_as_the_nest_formatted_it() {
        let row = event(
            0x0B,
            "Weekly digest · INBOX",
            TrainingLabel::Ham,
            TrainingSource::ExplicitButton,
        );
        let spy = Arc::new(SpyWriter {
            unwrapped: std::collections::BTreeSet::new(),
            subject: "SHOULD NOT BE USED".to_string(),
            calls: StdMutex::new(Vec::new()),
            baseline_calls: StdMutex::new(Vec::new()),
        });
        let m = machine_with_writer(vec![row], spy);
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().events[0].message, "Weekly digest · INBOX");
    }

    #[tokio::test]
    async fn undo_malformed_hex_surfaces_wrap_without_calling_nest() {
        let m = machine_with(vec![], false);
        let err = m
            .dispatch(MailSpamAction::UndoTraining {
                history_id_hex: "zz-not-hex".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Wrap(_)), "got {err:?}");
        assert!(m.snapshot().error.is_some());
    }

    #[tokio::test]
    async fn undo_wrong_length_hex_surfaces_invalid_state() {
        let m = machine_with(vec![], false);
        let err = m
            .dispatch(MailSpamAction::UndoTraining {
                history_id_hex: hex::encode([0x66u8; 32]),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    }

    // ── training badges ─────────────────────────────────────────────────

    #[test]
    fn training_label_badge_maps_every_variant() {
        for (label, key) in [
            (TrainingLabel::Spam, "mail_spam.label_spam"),
            (TrainingLabel::Ham, "mail_spam.label_ham"),
            (TrainingLabel::Unknown, "mail_spam.label_unknown"),
        ] {
            assert_eq!(training_label_badge(label).key, key, "{label:?}");
        }
    }

    #[test]
    fn training_source_badge_maps_every_variant() {
        for (source, key) in [
            (
                TrainingSource::ExplicitButton,
                "mail_spam.source_explicit_button",
            ),
            (
                TrainingSource::ImapJunkFlag,
                "mail_spam.source_imap_junk_flag",
            ),
            (
                TrainingSource::ImapJunkMove,
                "mail_spam.source_imap_junk_move",
            ),
            (TrainingSource::Unknown, "mail_spam.source_unknown"),
        ] {
            assert_eq!(training_source_badge(source).key, key, "{source:?}");
        }
    }
}
