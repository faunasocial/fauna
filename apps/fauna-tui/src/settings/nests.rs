//! The Settings → Nests sub-page (`ui/nests.md`) — the nests the user's
//! content lives on, plus the v1 **nest-trust** facet on each row.
//!
//! Two halves share one machine. The **linking** half (add / list / unlink) is
//! the long-shipping pairing surface, owned by `behavior/linked-nests.md`. The
//! **trust facet** (`nests.md` § Trust facet) is what each row adds below its
//! identity line: a per-row **Now / History** lens over the client-authoritative
//! signed grant-event log, the content-processing grants that nest is *trusted
//! to read* (Now) with per-grant renew/revoke and the required honest-bound
//! copy, that nest's grant-event timeline (History), the scope-first **mint**
//! flow, and — on the home row only — the **backup trust rows**
//! (`nests.md` § Trust facet — backup rows).
//!
//! A paint shell over the shared `LinkedNestsMachine` (`libs/fauna-client-pair`),
//! consumed directly in Rust like the Devices/Bluesky sub-pages — tui is native
//! Rust, not FFI-mediated, so unlike windows/apple/android it needs no binding
//! hop (priority #2). This layer holds **no** pairing or trust logic: it renders
//! `LinkedNestsSnapshot` and dispatches `LinkedNestsAction`. Every sequencing
//! decision — classifying the entered identity/address, holder discovery, the
//! grant-log folds, the Now/History projection, and **which nest each backup
//! revoke is spoken to** — lives in the shared machine.
//!
//! Vocabulary (`participants.md` § Naming, binding): rendered copy says
//! "trust" / "trusted to read …"; "capability" and "grant" are internal-only and
//! never appear in a string. The `nest-trust-*` element IDs are dev-facing.
//!
//! **Nav id.** The sub-page slug is `nests` fleet-wide — the one id the shared
//! `LinkedNestsActions.navigate()` sends every app (renamed from `linked-nests`
//! 2026-10-02, after the testids and i18n).

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_pair::{
    ForwardQueueStatus, LinkedNestRow, LinkedNestsAction, LinkedNestsMachine, LinkedNestsSnapshot,
    TrustBackupKind, TrustBackupRow, TrustBackupStatus, TrustFolder, TrustGenerationRow,
    TrustGenerationStatus, TrustGrantDuration, TrustGrantRow, TrustHistoryRow, TrustLens,
    TrustLiveness, TrustMintOption, TrustRestoreOutcome, TrustScope,
};
use fauna_i18n::strings::nests as t;

use super::{Action, SettingsField};
use crate::element::{Element, Field, Gesture, SelectTarget};

/// The Nests sub-page's state: the shared machine, its last snapshot, and the
/// page's own local form state (the add-a-nest reveal and the mint form).
///
/// The machine is built once at the post-auth hook (`attach_session`), the same
/// reasoning as [`super::mail::MailState`]: construction is sync and cheap (the
/// RPC is `hydrate()`), and holding it as an `Arc` is what lets an `Op` carry it
/// across a `tokio::spawn`. Session-scoped: `clear_session` drops it, so a stale
/// machine — and with it the config store holding the signed grant-event log —
/// can never outlive the session that built it.
#[derive(Default)]
pub struct NestsState {
    /// The shared machine. `None` pre-login, and also when the trust-enabled
    /// build failed and no plain fallback could be built.
    pub machine: Option<Arc<LinkedNestsMachine>>,
    /// The last snapshot the page painted. `None` until the nav-edge hydrate
    /// folds one — the list then renders empty rather than stale.
    pub snapshot: Option<LinkedNestsSnapshot>,
    /// Holder identities that have stamped an escrow receipt for this
    /// account's generation keys (`fauna.state.escrow-receipt` →
    /// `EscrowReceiptRecord.holder_id`) — the T16
    /// `participant-escrow-holder-badge` renders on the nest row whose
    /// identity matches one (participants.md § Roles). Refreshed on the
    /// nav-edge hydrate; kept across an unreadable pass.
    pub escrow_holders: Vec<[u8; 32]>,
    /// Whether the inline "Link a nest" form is revealed (`nests-add-button` →
    /// `nests-add-input` + submit/cancel). Reset on every nav into the page.
    pub add_form_open: bool,
    /// The `nests-add-input` buffer — a nest address or a 64-hex identity. The
    /// shared `classify_link_input` decides which action it drives, so this
    /// layer never parses it. Committed only on `nests-add-submit-button`.
    pub add_input: String,
    /// The open mint form, if any (`nests.md` § Mint). `Some` only between
    /// `nest-trust-grant-mint-button` and confirm/re-hydrate.
    pub mint: Option<MintForm>,
}

/// The open mint form's local state. Deliberately a single `Option`, not a
/// per-row map: a terminal shows one form at a time, and v1 populates
/// `mint_options` on the home row only (`nests.md:134` — a linked nest's holder
/// roster needs a second authenticated connection, a documented follow-on). It
/// carries its `nest_id` so the confirm dispatch names the row it was opened on
/// rather than assuming which one that was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MintForm {
    /// The row the form was opened on — round-trips into `Mint.nest_id`.
    pub nest_id: String,
    /// The chosen option's index into that row's `mint_options`. `None` until
    /// the user picks — `nest-trust-mint-confirm-button` stays disabled.
    pub scope: Option<usize>,
    /// The picked holder's `bridge_id`, for the ambiguity case only. Unset
    /// otherwise: the holder is *derived* from the scope choice, which is what
    /// stops a user minting e.g. the mail key to the web-serve holder
    /// (`nests.md` § Mint — the scope→holder mapping is 1:1 in practice).
    pub holder: Option<String>,
    /// The `nest-trust-mint-duration-select` pick. `None` until the user picks
    /// — the select then shows, and confirm mints, the row's
    /// `mint_default_duration` (`nests.md` § Expiry / renewal → *Duration and
    /// blessing*).
    pub duration: Option<TrustGrantDuration>,
}

/// The Nests page's blessing door — the account plane's
/// `fauna.state.blessed-nests`, through the shared impl every runtime-hosting
/// app wires, over this app's slot. The page's machine and the onboarding
/// one-tap trust both build it.
pub(crate) fn blessing_door(
    runtime: super::AccountRuntimeSlot,
) -> Arc<dyn fauna_client_pair::BlessedNestsStore> {
    Arc::new(
        fauna_client_account_runtime::blessed_nests::PlaneBlessedNests::new(move || {
            runtime.lock().ok().and_then(|handle| handle.clone())
        }),
    )
}

/// The succession-ledger door — the account plane's
/// `fauna.state.succession-ledger` (the grant log and its marks) over the same
/// slot, resolved at every call: a machine built before the runtime lands
/// answers `fauna_client_config::LEDGER_NOT_READY` (after the bounded wait)
/// until it does, and one that outlives a sign-out holds the retired
/// session's slot, which never fills again.
pub(crate) fn ledger_door(
    runtime: super::AccountRuntimeSlot,
) -> Arc<dyn fauna_client_config::SuccessionLedgerStore> {
    Arc::new(fauna_client_config::ResolvingLedgerStore::new(move || {
        runtime.lock().ok().and_then(|handle| handle.clone())
    }))
}

/// The backup-state door — the account plane's `fauna.state.backup` (each
/// source box's destination list and its unattested marks) over the same slot,
/// resolved at every call exactly like [`ledger_door`]: a caller that runs
/// before the runtime lands gets `fauna_client_config::LEDGER_NOT_READY` after
/// the bounded wait, never a fallback read.
pub(crate) fn backup_door(
    runtime: super::AccountRuntimeSlot,
) -> Arc<dyn fauna_client_config::BackupStateStore> {
    Arc::new(fauna_client_config::ResolvingLedgerStore::new(move || {
        runtime.lock().ok().and_then(|handle| handle.clone())
    }))
}

impl NestsState {
    /// Build the shared trust-enabled machine over `nest`'s authenticated
    /// connection. A build failure (an undecodable secret) is not fatal to the
    /// page: it falls back to the plain pairing machine, which still lists /
    /// links / unlinks — only the trust facet goes missing. Mirrors linux's
    /// `wire_machine` fallback exactly (priority #1).
    ///
    /// `runtime` is `SettingsState::account_runtime`, the slot the blessing
    /// door and the period-key door read ([`blessing_door`],
    /// [`super::period_key_door`]).
    pub fn build(
        nest: Arc<NestClient>,
        secret_hex: &str,
        runtime: super::AccountRuntimeSlot,
        mail: Arc<dyn fauna_client_config::MailStore>,
    ) -> Self {
        let machine = match crate::mail_glue::build_linked_nests_machine_with_trust(
            Arc::clone(&nest),
            secret_hex,
            ledger_door(runtime.clone()),
            backup_door(runtime.clone()),
            blessing_door(runtime.clone()),
            super::period_key_door(runtime.clone()),
            mail,
            super::folder_key_door(runtime),
        ) {
            Ok(machine) => machine,
            Err(e) => {
                tracing::error!("[settings/nests] build trust machine: {e}");
                fauna_client_pair::build_linked_nests_machine(nest)
            }
        };
        NestsState {
            machine: Some(Arc::new(machine)),
            ..Default::default()
        }
    }

    /// Reset the page's local form state — run on every nav into the page so a
    /// half-typed address or a half-picked mint never survives a nav-away (the
    /// `sign_out_pending` / `reset_form` precedent).
    pub fn reset_form(&mut self) {
        self.add_form_open = false;
        self.add_input.clear();
        self.mint = None;
    }

    /// The row a `nest_id` names, across home + pairings — the lookup every
    /// row-scoped action (`SetLens`, mint) resolves through.
    pub(super) fn row_for(&self, nest_id: &str) -> Option<&LinkedNestRow> {
        let snap = self.snapshot.as_ref()?;
        snap.home
            .iter()
            .chain(snap.pairings.iter())
            .find(|r| r.nest_id == nest_id)
    }

    /// Resolve the open mint form into the `Mint` action it dispatches, or
    /// `None` when it isn't confirmable yet (no form, no pick, or a >1-candidate
    /// option whose holder hasn't been picked). Pure — the caller turns it into
    /// an `Op`, so the enable-gate below and the confirm path can't drift.
    pub fn mint_action(&self) -> Option<LinkedNestsAction> {
        let form = self.mint.as_ref()?;
        let option = self.mint_option(form)?;
        let holder = derive_holder(option, form.holder.as_deref())?;
        Some(LinkedNestsAction::Mint {
            nest_id: form.nest_id.clone(),
            holder_bridge_id: holder,
            scope: option.scope.clone(),
            // Named explicitly, so what confirm mints is what the select shows.
            duration: Some(self.mint_duration(form)?),
        })
    }

    /// The duration the open form mints: the user's pick, else the row's
    /// default.
    fn mint_duration(&self, form: &MintForm) -> Option<TrustGrantDuration> {
        let row = self.row_for(&form.nest_id)?;
        Some(form.duration.unwrap_or(row.mint_default_duration))
    }

    /// The catalog entry the open form has selected, if any.
    fn mint_option(&self, form: &MintForm) -> Option<&TrustMintOption> {
        self.row_for(&form.nest_id)?.mint_options.get(form.scope?)
    }
}

/// The holder a mint targets: the single candidate when an option derives
/// exactly one (every option today), else the explicit pick from
/// `nest-trust-mint-holder-select`. `None` means "not confirmable yet" — an
/// ambiguous option with nothing picked, or a catalog entry with no candidate at
/// all (which the shared builder does not emit, but which must not mint blind).
fn derive_holder(option: &TrustMintOption, picked: Option<&str>) -> Option<String> {
    if option.holder_candidates.len() > 1 {
        let picked = picked?;
        return option
            .holder_candidates
            .iter()
            .find(|c| c.as_str() == picked)
            .cloned();
    }
    option.holder_candidates.first().cloned()
}

/// The ordered ui.yaml `nests` element list: the page heading, the add-a-nest
/// affordance (revealed inline — no modal, like every other tui form), then one
/// `nests-item` row per nest, home first (`nests.md` § Layout).
///
/// Row children are registered `.within(ids::NESTS_ITEM, i)`, and each grant /
/// backup / history leaf additionally under its own item container — the exact
/// two-step shape `nests.md:53` documents for a scoped query
/// (`scope="nests-item[i]"` then `nest-trust-grant-item[j]`). They stay readable
/// FLAT as well: an empty scope resolves to the whole frame, so an unscoped
/// `get_text(id, index=j)` walks
/// them in registration (= visual) order — which is how the whole shared suite
/// drives this page today.
pub(super) fn nests_elements(
    state: &NestsState,
    custody: Option<&super::devices::CustodyFacetSnapshot>,
) -> Vec<Element> {
    let mut els = vec![Element::label(ids::PAGE_HEADING, t::TITLE)];

    els.push(Element::gesture_button(
        ids::NESTS_ADD_BUTTON,
        t::ADD_BUTTON,
        true,
        Gesture::Settings(Action::NestsShowAddForm),
    ));
    if state.add_form_open {
        els.push(
            Element::input(
                ids::NESTS_ADD_INPUT,
                state.add_input.clone(),
                Field::Settings(SettingsField::NestsAddInput),
            )
            .labelled(t::NEST_TO_LINK),
        );
        els.push(Element::gesture_button(
            ids::NESTS_ADD_SUBMIT_BUTTON,
            t::ADD_SUBMIT,
            !state.add_input.trim().is_empty(),
            Gesture::Settings(Action::NestsSubmitAdd),
        ));
        els.push(Element::gesture_button(
            ids::NESTS_ADD_CANCEL_BUTTON,
            t::ADD_CANCEL,
            true,
            Gesture::Settings(Action::NestsCancelAdd),
        ));
    }

    // The forward queue (`nests.md` § Forward queue): page-level, before the
    // rows, and only while this nest holds posts of the user's it could not
    // yet hand to its relay. The ruling — whose page, and why the user's —
    // is `private-mode.md` § Post Forwarding; this layer only paints the
    // shared snapshot's `forward_queue`.
    let snapshot = state.snapshot.as_ref();
    if let Some(queue) = snapshot.and_then(|s| s.forward_queue.as_ref())
        && queue.queued > 0
    {
        push_forward_queue(&mut els, queue);
    }

    // Home first, then pairings — the order every trust-facet client renders,
    // and the order `LinkedNestsActions.nest_ids()` assumes when it slices the
    // pairing rows off the end.
    let rows: Vec<&LinkedNestRow> = snapshot
        .map(|s| s.home.iter().chain(s.pairings.iter()).collect())
        .unwrap_or_default();
    for (i, row) in rows.iter().enumerate() {
        push_nest_item(&mut els, state, row, i);
    }
    // Custodian nests (`nests.md` § Trust facet — custody rows): one
    // nests-item per NEST-anchored custody (the nest-custodian identity
    // fact) — a friend's nest holding sealed copies for this account. The
    // item index continues after the linked rows; the revoke gesture carries
    // the row's ORIGINAL fold index (the shared `Action::CustodyRevoke`
    // handler resolves `devices.custody.rows[i]` — one custody source of
    // truth, one revocation path).
    let mut item = rows.len();
    if let Some(facet) = custody {
        for (fold_index, c) in facet.rows.iter().enumerate() {
            if c.custodian_nest_url.is_none() {
                continue;
            }
            push_custody_nest_item(&mut els, c, fold_index, item);
            item += 1;
        }
    }
    // `settings-nav-back` — Esc already returns to the Settings hub
    // (`Action::NavBack => state.sub = SubPage::Root`), but a live user report
    // found it was the ONLY way out on Folders, undiscoverable (user-approved
    // 2026-08-03; matches `account.rs`/`folders.rs`'s existing pattern).
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// The page-level forward-queue block (`nests-forward-*`): the count, the
/// stuck half's what-to-check when any entry is refused past the retry
/// ceiling, the nest's own latest failure when one was recorded, and the two
/// actions. Painted only for a non-empty queue (the caller gates it), so an
/// ordinary Nests page — a public nest, or a home nest whose relay accepts
/// everything — carries none of these ids.
fn push_forward_queue(els: &mut Vec<Element>, queue: &ForwardQueueStatus) {
    let mut summary = t::forward_queue_summary(&queue.queued.to_string());
    if queue.stuck > 0 {
        summary.push(' ');
        summary.push_str(&t::forward_queue_stuck(&queue.stuck.to_string()));
    }
    els.push(Element::label(ids::NESTS_FORWARD_QUEUE, summary));
    if let Some(error) = queue.last_error.as_deref().filter(|e| !e.is_empty()) {
        els.push(Element::label(
            ids::NESTS_FORWARD_QUEUE_REASON,
            t::forward_queue_last_error(error),
        ));
    }
    els.push(Element::gesture_button(
        ids::NESTS_FORWARD_RETRY_BUTTON,
        t::FORWARD_RETRY,
        true,
        Gesture::Settings(Action::NestsRetryForwards),
    ));
    els.push(Element::gesture_button(
        ids::NESTS_FORWARD_DISCARD_BUTTON,
        t::FORWARD_DISCARD,
        true,
        Gesture::Settings(Action::NestsDiscardForwards),
    ));
}

/// One `nests-item` row: the identity line, then the trust facet. The home row
/// carries no Unlink and no sync-caps/expiry lines — it is the user's own
/// connected nest, not a pairing (`nests.md` § Layout).
fn push_nest_item(els: &mut Vec<Element>, state: &NestsState, row: &LinkedNestRow, i: usize) {
    let abbreviated = fauna_core::format::short_id(&row.nest_id);
    let label_text = row
        .label
        .clone()
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| abbreviated.clone());

    els.push(Element::label(ids::NESTS_ITEM, String::new()));
    els.push(Element::label(ids::NESTS_ITEM_LABEL, label_text).within(ids::NESTS_ITEM, i));
    els.push(Element::label(ids::NESTS_ITEM_NEST_ID, abbreviated).within(ids::NESTS_ITEM, i));
    // The escrow-holder role badge (T16; participants.md § Roles): this nest
    // holds the account's R14 (account-data-plane.md § The ratified decisions) generation-key escrow — what makes the
    // participant pages the one "who holds what for me" surface. Derived from
    // recorded escrow receipts, never asserted by the nest itself.
    if let Ok(id) = fauna_core::hex32::decode(&row.nest_id)
        && state.escrow_holders.contains(&id)
    {
        els.push(
            Element::label(ids::PARTICIPANT_ESCROW_HOLDER_BADGE, t::ESCROW_HOLDER_BADGE)
                .within(ids::NESTS_ITEM, i),
        );
    }

    if !row.is_home {
        els.push(
            Element::label(
                ids::NESTS_ITEM_CAPABILITIES,
                row.capability_labels
                    .iter()
                    .map(|l| l.clone().resolve(fauna_i18n::strings::lookup))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .within(ids::NESTS_ITEM, i),
        );
        let expiry = match row.expires_at {
            Some(_) => t::EXPIRY_LABEL,
            None => t::EXPIRY_NEVER,
        };
        els.push(Element::label(ids::NESTS_ITEM_EXPIRY, expiry).within(ids::NESTS_ITEM, i));
        els.push(
            Element::gesture_button(
                ids::NESTS_ITEM_UNLINK_BUTTON,
                t::UNLINK,
                true,
                Gesture::Settings(Action::NestsUnlink(row.nest_id.clone())),
            )
            .within(ids::NESTS_ITEM, i),
        );
    }

    push_trust_facet(els, state, row, i);
}

/// The trust facet for one row: the always-present Now/History lens toggle
/// (both lenses reachable, `nests.md:29`), then the active lens's content.
fn push_trust_facet(els: &mut Vec<Element>, state: &NestsState, row: &LinkedNestRow, i: usize) {
    els.push(
        Element::gesture_button(
            ids::NEST_TRUST_VIEW_NOW,
            t::VIEW_NOW,
            true,
            Gesture::Settings(Action::NestsSetLens {
                nest_id: row.nest_id.clone(),
                history: false,
            }),
        )
        .within(ids::NESTS_ITEM, i),
    );
    els.push(
        Element::gesture_button(
            ids::NEST_TRUST_VIEW_HISTORY,
            t::VIEW_HISTORY,
            true,
            Gesture::Settings(Action::NestsSetLens {
                nest_id: row.nest_id.clone(),
                history: true,
            }),
        )
        .within(ids::NESTS_ITEM, i),
    );

    match row.lens {
        TrustLens::Now => {
            // `nest-trust-empty` says "this nest is trusted with NOTHING", so a
            // backup row suppresses it even with zero content grants
            // (`nests.md:99`): a nest that seals and uploads your messages is
            // plainly trusted, and rendering "not trusted to read anything"
            // directly above "Backs up your messages for you" would state the
            // opposite of the row beneath it. linux shipped that bug and fixed
            // it 2026-07-24 — do not re-derive the condition.
            if row.trust_grants.is_empty() && row.trust_backups.is_empty() {
                els.push(
                    Element::label(ids::NEST_TRUST_EMPTY, t::NOT_TRUSTED)
                        .within(ids::NESTS_ITEM, i),
                );
            } else {
                els.push(
                    Element::label(ids::NEST_TRUST_GRANT_LIST, String::new())
                        .within(ids::NESTS_ITEM, i),
                );
                for (j, grant) in row.trust_grants.iter().enumerate() {
                    push_grant_item(els, grant, i, j);
                }
            }
            // Backup trust rows come AFTER the content-processing grant rows,
            // and outside the branch above because they render alongside
            // *either* arm — next to the grant list when content grants exist,
            // and on their own when none do. The wrapping `nest-trust-backup-list`
            // container (ui.yaml, added 2026-08-25 off windows' shipped shape —
            // `NestsPanel.xaml`'s Border-wrapped `ItemsControl`, "same FlaUI
            // IsOffscreen rationale" per nests.md § Implementation status today)
            // is conditioned on backups alone — independent of grants/generations,
            // matching windows' `BackupListVisibility = backups.Count > 0`.
            if !row.trust_backups.is_empty() {
                els.push(
                    Element::label(ids::NEST_TRUST_BACKUP_LIST, String::new())
                        .within(ids::NESTS_ITEM, i),
                );
            }
            for (j, backup) in row.trust_backups.iter().enumerate() {
                push_backup_item(els, backup, i, j);
            }
            // Retained generations come after the backup rows — one surface, so
            // "who may write here" and "what can I roll back" read together
            // (`nests.md:113`). Same both-arms placement as the backup rows; the
            // wrapping `nest-trust-generation-list` container is likewise
            // conditioned on generations alone, matching windows'
            // `GenerationListVisibility = generations.Count > 0`.
            if !row.trust_generations.is_empty() {
                els.push(
                    Element::label(ids::NEST_TRUST_GENERATION_LIST, String::new())
                        .within(ids::NESTS_ITEM, i),
                );
            }
            for (j, generation) in row.trust_generations.iter().enumerate() {
                push_generation_item(els, generation, i, j);
            }
            // The restore-outcome notice (`nest-trust-generation-notice`,
            // ratified 2026-07-29) — home-row-scoped, NOT per-row: a restore's
            // outcome describes the page's last action, not any one
            // generation row (`LinkedNestsSnapshot::restore_outcome`).
            // Registered whenever the home row's Now lens renders, EMPTY
            // until a restore resolves — the leaf set does not vary.
            // Distinct from `error-message`: only a genuinely failed call
            // (transport refused, unknown destination) reaches that;
            // `PastRecoveryWindow` is a product state, never an error.
            if row.is_home {
                let notice = match state.snapshot.as_ref().and_then(|s| s.restore_outcome) {
                    Some(TrustRestoreOutcome::Restored) => t::GENERATION_RESTORED,
                    Some(TrustRestoreOutcome::PastRecoveryWindow) => t::GENERATION_PAST_WINDOW,
                    None => "",
                };
                els.push(
                    Element::label(ids::NEST_TRUST_GENERATION_NOTICE, notice)
                        .within(ids::NESTS_ITEM, i),
                );
            }
            // The per-nest blessing (`nest-trust-blessed-toggle`, `nests.md`
            // § Expiry / renewal → *Duration and blessing*) — home row only in
            // v1, like the rest of the facet. `state` mirrors the checkbox for
            // a driver (the toggle convention).
            if row.is_home {
                els.push(
                    Element::checkbox_gesture(
                        ids::NEST_TRUST_BLESSED_TOGGLE,
                        t::BLESSED_TOGGLE,
                        row.blessed,
                        Gesture::Settings(Action::NestsSetBlessed {
                            nest_id: row.nest_id.clone(),
                            blessed: !row.blessed,
                        }),
                    )
                    .attr("state", if row.blessed { "on" } else { "off" })
                    .within(ids::NESTS_ITEM, i),
                );
            }
            // The mint flow renders only when the shared option catalog is
            // non-empty — an empty catalog means nothing derivable or no
            // discoverable holder, and a picker that can only error is worse
            // than no picker (`nests.md` § Mint).
            if !row.mint_options.is_empty() {
                push_mint_flow(els, state, row, i);
            }
        }
        TrustLens::History => {
            els.push(
                Element::label(ids::NEST_TRUST_HISTORY_LIST, String::new())
                    .within(ids::NESTS_ITEM, i),
            );
            for (j, h) in row.trust_history.iter().enumerate() {
                els.push(
                    Element::label(ids::NEST_TRUST_HISTORY_ITEM, history_line(h))
                        .within(ids::NEST_TRUST_HISTORY_LIST, j)
                        .within(ids::NESTS_ITEM, i),
                );
            }
        }
    }
}

/// One current-grant row (`nest-trust-grant-item`): the scope line, lasts-until,
/// the liveness status, the REQUIRED honest-bound copy, and renew/revoke. The
/// `grant_id` round-trips unchanged into the Renew/Revoke dispatch.
/// One custodian-NEST `nests-item` row + its `nest-trust-custody-*` child
/// family (T16's nest-shaped half). The row content mirrors the Devices
/// `custody-holder-card` family — the copy constants ARE the shared
/// `devices.custody_*` strings (receipt three-state honesty, held-bytes,
/// revoke beside its REQUIRED honest-bound note), stated once and rendered
/// on both pages. The scope line is trust vocabulary only.
fn push_custody_nest_item(
    els: &mut Vec<Element>,
    row: &fauna_client_capabilities::view_model::CustodyRowView,
    fold_index: usize,
    i: usize,
) {
    use super::devices::{held_bytes_text, receipt_status_text, short_actor};
    use fauna_i18n::strings::devices as td;
    let nest = |e: Element| {
        e.within(ids::NEST_TRUST_CUSTODY_ITEM, 0)
            .within(ids::NESTS_ITEM, i)
    };
    els.push(Element::label(ids::NESTS_ITEM, String::new()));
    els.push(
        Element::label(
            ids::NESTS_ITEM_LABEL,
            t::custody_nest_label(&short_actor(&row.host)),
        )
        .within(ids::NESTS_ITEM, i),
    );
    // The item anchor carries the honest-bound revoke copy, the
    // custody-holder-card shape (the bound stated beside the control).
    els.push(
        Element::label(ids::NEST_TRUST_CUSTODY_ITEM, td::CUSTODY_REVOKE_BOUND_NOTE)
            .within(ids::NESTS_ITEM, i),
    );
    els.push(nest(Element::label(
        ids::NEST_TRUST_CUSTODY_SCOPE,
        td::CUSTODY_HOLDER_SCOPE,
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_CUSTODY_RECEIPT_STATUS,
        receipt_status_text(
            row.receipt_state,
            row.receipt.as_ref().map(|r| r.attested_at_micros),
        ),
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_CUSTODY_HELD_BYTES,
        held_bytes_text(row.receipt.as_ref()),
    )));
    els.push(nest(Element::gesture_button(
        ids::NEST_TRUST_CUSTODY_REVOKE_BUTTON,
        td::CUSTODY_REVOKE,
        // A pending ceremony has minted nothing to revoke yet.
        !row.pending,
        Gesture::Settings(Action::CustodyRevoke(fold_index as u32)),
    )));
}

fn push_grant_item(els: &mut Vec<Element>, grant: &TrustGrantRow, i: usize, j: usize) {
    let nest = |e: Element| {
        e.within(ids::NEST_TRUST_GRANT_ITEM, j)
            .within(ids::NESTS_ITEM, i)
    };

    els.push(Element::label(ids::NEST_TRUST_GRANT_ITEM, String::new()).within(ids::NESTS_ITEM, i));
    els.push(nest(Element::label(
        ids::NEST_TRUST_GRANT_SCOPE,
        format!(
            "{} {}",
            t::TRUSTED_TO_READ,
            scope_line(&grant.scope, grant.folder.clone())
        ),
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_GRANT_LASTS_UNTIL,
        format!(
            "{} {}",
            t::LASTS_UNTIL,
            fauna_core::format::format_unix_local(grant.lasts_until)
        ),
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_GRANT_STATUS,
        status_label(grant.liveness),
    )));
    // A bounded (content-sealing-epochs) mail grant gets the stronger,
    // crypto-bounded wording; every other kind/regime keeps the standing
    // trust-until-revoke wording. Never re-derive the (class, kind, tier) check
    // here — the shared predicate is the single source of truth (priority #2).
    let bound_note = if fauna_client_pair::trust_scope_is_bounded_mail_grant(grant.scope.clone()) {
        t::BOUND_NOTE_BOUNDED_MAIL
    } else {
        t::BOUND_NOTE_STANDING
    };
    els.push(nest(Element::label(
        ids::NEST_TRUST_GRANT_BOUND_NOTE,
        bound_note,
    )));
    els.push(nest(Element::gesture_button(
        ids::NEST_TRUST_GRANT_RENEW,
        t::RENEW,
        true,
        Gesture::Settings(Action::NestsRenewGrant(grant.grant_id.clone())),
    )));
    els.push(nest(Element::gesture_button(
        ids::NEST_TRUST_GRANT_REVOKE,
        t::REVOKE,
        true,
        Gesture::Settings(Action::NestsRevokeGrant(grant.grant_id.clone())),
    )));
    // The post-succession review mark and its Keep half — present only while
    // this row is actually raised (`succession-aftermath.md` § Adjudicating what
    // the aftermath carries across). Absent rather than empty otherwise, for
    // the reason the backups plane's pair is: in a healthy account every grant
    // is the owner's own, and a permanently-rendered mark would train the user
    // straight past the one succession that matters.
    //
    // This plane is the stricter of the two. A thief-added backup destination
    // still only receives segments sealed under a key it lacks; a thief-added
    // grantee is handed live READ capability by the successor's own client, so
    // the mark is mandatory here rather than defense-in-depth.
    //
    // Remove is deliberately NOT re-rendered — `nest-trust-grant-revoke` above
    // already is it, so Keep joins the affordance that exists instead of
    // minting a second revocation path.
    if grant.unattested {
        els.push(nest(Element::label(
            ids::NEST_TRUST_GRANT_UNATTESTED_MARK,
            t::GRANT_UNATTESTED_MARK,
        )));
        els.push(nest(Element::gesture_button(
            ids::NEST_TRUST_GRANT_KEEP_BUTTON,
            t::GRANT_KEEP_BUTTON,
            true,
            Gesture::Settings(Action::NestsKeepGrant(grant.grant_id.clone())),
        )));
    }
}

/// One backup trust row (`nest-trust-backup-item`) in the Now lens on the home
/// nest's row: the scope line, when the trust was given, the row state, the
/// REQUIRED honest-bound copy, and the freeze-the-backup affordance
/// (`nests.md` § Trust facet — backup rows).
///
/// Two row kinds share the component: the seal grant, revoked at the source
/// nest, and one writer row per destination, revoked **at the destination** —
/// the shared machine routes each press to the right nest, so this layer only
/// names which row was pressed. Deliberately no lasts-until / renew / History
/// twin: both grants are standing live nest reads, not folds of the signed
/// grant-event log (`nests.md:99`).
fn push_backup_item(els: &mut Vec<Element>, backup: &TrustBackupRow, i: usize, j: usize) {
    let nest = |e: Element| {
        e.within(ids::NEST_TRUST_BACKUP_ITEM, j)
            .within(ids::NESTS_ITEM, i)
    };

    els.push(Element::label(ids::NEST_TRUST_BACKUP_ITEM, String::new()).within(ids::NESTS_ITEM, i));
    let scope_text = match backup.kind {
        TrustBackupKind::Seal => t::BACKUP_SCOPE_SEAL.to_string(),
        TrustBackupKind::Writer => t::backup_scope_writer(&backup.destination_label),
    };
    els.push(nest(Element::label(
        ids::NEST_TRUST_BACKUP_SCOPE,
        scope_text,
    )));
    // `nest-trust-backup-since` renders EMPTY on the seal row — that grant
    // carries no timestamp on the wire (`nests.md:67`). The element is still
    // registered so the row's leaf set does not vary by kind.
    let since_text = match backup.since {
        Some(at) => format!(
            "{} {}",
            t::BACKUP_SINCE,
            fauna_core::format::format_unix_local(at)
        ),
        None => String::new(),
    };
    els.push(nest(Element::label(
        ids::NEST_TRUST_BACKUP_SINCE,
        since_text,
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_BACKUP_STATUS,
        backup_status_label(backup.status),
    )));
    let bound_note = match backup.kind {
        TrustBackupKind::Seal => t::BACKUP_BOUND_NOTE_SEAL,
        TrustBackupKind::Writer => t::BACKUP_BOUND_NOTE_WRITER,
    };
    els.push(nest(Element::label(
        ids::NEST_TRUST_BACKUP_BOUND_NOTE,
        bound_note,
    )));
    let action = match backup.kind {
        TrustBackupKind::Seal => Action::NestsRevokeBackupSeal,
        TrustBackupKind::Writer => Action::NestsRevokeBackupWriter(backup.destination_id.clone()),
    };
    els.push(nest(Element::gesture_button(
        ids::NEST_TRUST_BACKUP_REVOKE,
        t::BACKUP_REVOKE,
        true,
        Gesture::Settings(action),
    )));
}

/// One retained-generation row (`nest-trust-generation-item`) in the Now lens on
/// the home nest's row: what the owner can roll back to inside the custody grace
/// window `T` (`nests.md` § Trust facet — generation recovery).
///
/// **Every branch here is a ratified honesty requirement, not a style choice:**
///
/// - An `Unreachable` row renders **no** `nest-trust-generation-restore`
///   (`nests.md:122`) — there is no address to restore, and offering the
///   affordance would imply we knew something we do not. It is also why the row
///   exists at all: a destination we could not ask must never render as "nothing
///   to recover", which is the false reassurance a hostile source buys.
/// - A row with no plaintext `path` renders its **hash** rather than being
///   hidden or skipped (`nests.md:123`) — the rows a rogue source produced are
///   exactly the ones a user needs to see.
/// - The expiry leaf carries the REQUIRED quota-bound copy (`nests.md` § Required
///   copy), because a user near their cap sees usage a supersede storm inflated
///   until `T` and this is where that is explicable.
///
/// Ordering is the destination's, preserved by the shared projection — this
/// layer never re-sorts (`nests.md:117`).
fn push_generation_item(
    els: &mut Vec<Element>,
    generation: &TrustGenerationRow,
    i: usize,
    j: usize,
) {
    let nest = |e: Element| {
        e.within(ids::NEST_TRUST_GENERATION_ITEM, j)
            .within(ids::NESTS_ITEM, i)
    };

    els.push(
        Element::label(ids::NEST_TRUST_GENERATION_ITEM, String::new()).within(ids::NESTS_ITEM, i),
    );

    let unreachable = generation.status == TrustGenerationStatus::Unreachable;
    // On an unreachable row the identity leaves describe the DESTINATION that
    // went dark rather than a generation that does not exist — the row's whole
    // job is naming which box could not be asked.
    let path_text = if unreachable {
        generation.destination_label.clone()
    } else {
        match &generation.path {
            Some(path) => t::generation_path(path),
            None => t::generation_path_unknown(&generation.path_hash),
        }
    };
    els.push(nest(Element::label(
        ids::NEST_TRUST_GENERATION_PATH,
        path_text,
    )));
    // The three value leaves render EMPTY on an unreachable row rather than a
    // zero timestamp or "0 B", which would read as fact. The elements stay
    // registered so the row's leaf set does not vary by status — the same
    // shape the sibling backup row uses for its seal-row `since`.
    els.push(nest(Element::label(
        ids::NEST_TRUST_GENERATION_SUPERSEDED,
        if unreachable {
            String::new()
        } else {
            format!(
                "{} {}",
                t::GENERATION_SUPERSEDED,
                fauna_core::format::format_unix_local(generation.superseded_at)
            )
        },
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_GENERATION_EXPIRES,
        if unreachable {
            String::new()
        } else {
            t::generation_expires(&fauna_core::format::format_unix_local(
                generation.expires_at,
            ))
        },
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_GENERATION_SIZE,
        if unreachable {
            String::new()
        } else {
            crate::format::byte_size(generation.size_bytes)
        },
    )));
    els.push(nest(Element::label(
        ids::NEST_TRUST_GENERATION_STATUS,
        if unreachable {
            t::GENERATION_STATUS_UNREACHABLE
        } else {
            t::GENERATION_STATUS_LISTED
        },
    )));
    if !unreachable {
        // The address triple round-trips off the row unchanged — never a row
        // index, which would promote the wrong generation the moment this
        // flattened list is filtered or re-ordered.
        els.push(nest(Element::gesture_button(
            ids::NEST_TRUST_GENERATION_RESTORE,
            t::GENERATION_RESTORE,
            true,
            Gesture::Settings(Action::NestsRestoreGeneration {
                destination_id: generation.destination_id.clone(),
                folder_name: generation.folder_name.clone(),
                path_hash: generation.path_hash.clone(),
                manifest_hash: generation.manifest_hash.clone(),
            }),
        )));
    }
}

/// The scope-first mint flow (`nest-trust-grant-mint-button` →
/// `nest-trust-mint-scope-select` [→ `nest-trust-mint-holder-select`] →
/// `nest-trust-mint-confirm-button`; `nests.md` § Mint, ratified 2026-07-13).
///
/// The scope select's options are the shared `LinkedNestRow.mint_options`
/// catalog verbatim, one use-case option each, labeled shell-side (priority #2 —
/// one catalog, per-app labels). It round-trips the **rendered label**, which
/// is the cross-app `driver.select(id, label)` contract every other app
/// already honours. The holder is derived from the chosen option; the holder
/// select renders only when an option lists more than one candidate (the
/// ambiguity case — every option derives exactly one today).
fn push_mint_flow(els: &mut Vec<Element>, state: &NestsState, row: &LinkedNestRow, i: usize) {
    els.push(
        Element::gesture_button(
            ids::NEST_TRUST_GRANT_MINT_BUTTON,
            t::MINT_BUTTON,
            true,
            Gesture::Settings(Action::NestsOpenMint(row.nest_id.clone())),
        )
        .within(ids::NESTS_ITEM, i),
    );

    let form = match &state.mint {
        Some(f) if f.nest_id == row.nest_id => f,
        _ => return,
    };

    let mut options: Vec<String> = vec![t::MINT_SCOPE_PLACEHOLDER.to_string()];
    options.extend(row.mint_options.iter().map(mint_option_label));
    let selected = form
        .scope
        .and_then(|idx| row.mint_options.get(idx))
        .map(mint_option_label)
        .unwrap_or_else(|| t::MINT_SCOPE_PLACEHOLDER.to_string());
    els.push(
        Element::select(
            ids::NEST_TRUST_MINT_SCOPE_SELECT,
            selected,
            SelectTarget::TrustMintScope,
            options,
        )
        .within(ids::NESTS_ITEM, i),
    );

    // How long the new trust lasts — the shared option list, labeled here; the
    // select round-trips the rendered label like the scope select above.
    let duration = form.duration.unwrap_or(row.mint_default_duration);
    els.push(
        Element::select(
            ids::NEST_TRUST_MINT_DURATION_SELECT,
            duration_label(duration),
            SelectTarget::TrustMintDuration,
            fauna_client_pair::mint_duration_options()
                .into_iter()
                .map(duration_label)
                .collect(),
        )
        .within(ids::NESTS_ITEM, i),
    );

    // Conditional holder select — only for a chosen option with >1 candidate.
    // Candidates are the stable holder names (`bridge_id`, e.g. "web-serve"); a
    // localized holder-label catalog arrives with the first deployment that
    // actually hits this arm.
    if let Some(option) = form.scope.and_then(|idx| row.mint_options.get(idx))
        && option.holder_candidates.len() > 1
    {
        let selected = form
            .holder
            .clone()
            .unwrap_or_else(|| t::MINT_HOLDER_PLACEHOLDER.to_string());
        let mut candidates: Vec<String> = vec![t::MINT_HOLDER_PLACEHOLDER.to_string()];
        candidates.extend(option.holder_candidates.iter().cloned());
        els.push(
            Element::select(
                ids::NEST_TRUST_MINT_HOLDER_SELECT,
                selected,
                SelectTarget::TrustMintHolder,
                candidates,
            )
            .within(ids::NESTS_ITEM, i),
        );
    }

    els.push(
        Element::gesture_button(
            ids::NEST_TRUST_MINT_CONFIRM_BUTTON,
            t::MINT_CONFIRM,
            state.mint_action().is_some(),
            Gesture::Settings(Action::NestsConfirmMint),
        )
        .within(ids::NESTS_ITEM, i),
    );
}

// The four label mappings below (`mint_option_label`, `scope_label`,
// `status_label`, `backup_status_label`) are thin `resolve()` wrappers over
// `fauna_client_pair`'s shared `LocalizedText` mappings — the match-arm logic
// itself is lifted there so tui and linux (and web, once wired) share one
// source of truth (priority #1/#2) instead of hand-rolling identical arms.

/// `nests.mint_option_*`; the paywalled option carries its tier via the
/// `{tier}` named placeholder.
fn mint_option_label(o: &TrustMintOption) -> String {
    fauna_client_pair::mint_option_label(o).resolve(fauna_i18n::strings::lookup)
}

fn duration_label(d: TrustGrantDuration) -> String {
    fauna_client_pair::duration_label(d).resolve(fauna_i18n::strings::lookup)
}

/// Resolve a rendered duration-select label back to its duration — the inverse
/// of [`duration_label`]. `None` for a label no longer offered.
pub(super) fn mint_duration_for_label(label: &str) -> Option<TrustGrantDuration> {
    fauna_client_pair::mint_duration_options()
        .into_iter()
        .find(|d| duration_label(*d) == label)
}

/// Resolve a rendered scope-select label back to its catalog index — the
/// inverse of [`mint_option_label`], used by the `select` fold. `None` for the
/// placeholder (or any label no longer in the catalog), which clears the pick
/// rather than silently keeping a stale one.
pub(super) fn mint_option_index(row: &LinkedNestRow, label: &str) -> Option<usize> {
    row.mint_options
        .iter()
        .position(|o| mint_option_label(o) == label)
}

/// A grant's scope line — the shared [`fauna_client_pair::grant_scope_labels`],
/// which names a folder grant's folder (`folder`, resolved in shared Rust)
/// in place of the bare folder read.
fn scope_line(scope: &[TrustScope], folder: Option<TrustFolder>) -> String {
    fauna_client_pair::grant_scope_labels(scope.to_vec(), folder)
        .into_iter()
        .map(|l| l.resolve(fauna_i18n::strings::lookup))
        .collect::<Vec<_>>()
        .join(", ")
}

fn status_label(l: TrustLiveness) -> String {
    fauna_client_pair::status_label(l).resolve(fauna_i18n::strings::lookup)
}

fn backup_status_label(status: TrustBackupStatus) -> String {
    fauna_client_pair::backup_status_label(status).resolve(fauna_i18n::strings::lookup)
}

/// One History-lens row's self-describing line ("Trusted to read ‹scope› ·
/// ‹when›" etc.) — the shared [`fauna_client_pair::history_line_text`]
/// decision (tui↔linux twin harvest, previously
/// hand-rolled identically here and on linux).
fn history_line(h: &TrustHistoryRow) -> String {
    let scope = scope_line(&h.scope, h.folder.clone());
    let when = fauna_core::format::format_unix_local(h.at);
    fauna_client_pair::history_line_text(h.kind, &scope, &when).resolve(fauna_i18n::strings::lookup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_pair::{LinkedNestStatus, TrustEventKind, TrustMintUseCase};

    fn scope(class: &str, kind: Option<&str>, tier: Option<&str>) -> TrustScope {
        TrustScope {
            class: class.to_string(),
            kind: kind.map(str::to_string),
            tier: tier.map(str::to_string),
        }
    }

    fn home_row() -> LinkedNestRow {
        LinkedNestRow {
            nest_id: "aa".repeat(32),
            capabilities: vec!["mls_pull".to_string()],
            capability_labels: vec![fauna_client_pair::capability_label("mls_pull")],
            expires_at: None,
            created_at: 0,
            label: None,
            nest_url: None,
            is_home: true,
            trust_grants: Vec::new(),
            trust_history: Vec::new(),
            lens: TrustLens::Now,
            available_holders: Vec::new(),
            mint_options: Vec::new(),
            blessed: false,
            mint_default_duration: TrustGrantDuration::OneOff,
            trust_backups: Vec::new(),
            trust_generations: Vec::new(),
        }
    }

    fn state_with(home: LinkedNestRow) -> NestsState {
        NestsState {
            snapshot: Some(LinkedNestsSnapshot {
                home: Some(home),
                pairings: Vec::new(),
                status: LinkedNestStatus::Idle,
                error: None,
                restore_outcome: None,
                forward_queue: None,
            }),
            ..Default::default()
        }
    }

    fn state_with_queue(queue: Option<ForwardQueueStatus>) -> NestsState {
        let mut state = state_with(home_row());
        state.snapshot.as_mut().unwrap().forward_queue = queue;
        state
    }

    // ── forward queue (`nests.md` § Forward queue) ───────────────────

    /// An empty queue (or an absent one) paints none of the
    /// forward ids: the block exists only while something is waiting.
    #[test]
    fn an_empty_or_unreported_forward_queue_paints_nothing() {
        for queue in [
            None,
            Some(ForwardQueueStatus {
                queued: 0,
                stuck: 0,
                last_error: None,
            }),
        ] {
            let els = nests_elements(&state_with_queue(queue), None);
            for id in [
                "nests-forward-queue",
                "nests-forward-queue-reason",
                "nests-forward-retry-button",
                "nests-forward-discard-button",
            ] {
                assert!(!ids(&els).contains(&id), "{id} painted for an empty queue");
            }
        }
    }

    /// A waiting queue paints the count, both actions, and — only once a send
    /// has failed — the nest's own reason. Before the first failure there is
    /// no reason line rather than an empty one.
    #[test]
    fn a_waiting_queue_paints_the_count_the_actions_and_the_reason_once_known() {
        let els = nests_elements(
            &state_with_queue(Some(ForwardQueueStatus {
                queued: 2,
                stuck: 0,
                last_error: None,
            })),
            None,
        );
        let summary = text_of(&els, "nests-forward-queue").expect("summary painted");
        assert!(summary.contains('2'), "{summary}");
        assert!(
            !summary.contains("eight hours"),
            "nothing stuck yet: {summary}"
        );
        assert!(!ids(&els).contains(&"nests-forward-queue-reason"));
        assert!(ids(&els).contains(&"nests-forward-retry-button"));
        assert!(ids(&els).contains(&"nests-forward-discard-button"));
        // The block precedes the nest rows (page-level, `nests.md` § Layout).
        let queue_at = ids(&els)
            .iter()
            .position(|i| *i == "nests-forward-queue")
            .unwrap();
        let first_row_at = ids(&els).iter().position(|i| *i == "nests-item").unwrap();
        assert!(queue_at < first_row_at);

        let els = nests_elements(
            &state_with_queue(Some(ForwardQueueStatus {
                queued: 2,
                stuck: 1,
                last_error: Some("nest not paired for this actor".into()),
            })),
            None,
        );
        let summary = text_of(&els, "nests-forward-queue").unwrap();
        assert!(
            summary.contains("eight hours"),
            "the stuck half names what to check: {summary}"
        );
        assert_eq!(
            text_of(&els, "nests-forward-queue-reason"),
            Some("Last attempt failed: nest not paired for this actor")
        );
    }

    /// The two buttons carry their own actions — a press can never be routed
    /// to the wrong kind.
    #[test]
    fn the_forward_buttons_carry_retry_and_discard() {
        let els = nests_elements(
            &state_with_queue(Some(ForwardQueueStatus {
                queued: 1,
                stuck: 0,
                last_error: None,
            })),
            None,
        );
        let role_of = |id: &str| els.iter().find(|e| e.id == id).map(|e| &e.role);
        assert!(matches!(
            role_of("nests-forward-retry-button"),
            Some(crate::element::Role::Button(Gesture::Settings(
                Action::NestsRetryForwards
            )))
        ));
        assert!(matches!(
            role_of("nests-forward-discard-button"),
            Some(crate::element::Role::Button(Gesture::Settings(
                Action::NestsDiscardForwards
            )))
        ));
    }

    /// A listed retained generation, as the shared projection hands it over.
    fn generation_row(path: Option<&str>) -> TrustGenerationRow {
        TrustGenerationRow {
            status: TrustGenerationStatus::Listed,
            destination_id: "d1".into(),
            destination_label: "Aunt's nest".into(),
            folder_name: "__mail".into(),
            path: path.map(Into::into),
            path_hash: "aa11".into(),
            manifest_hash: "mm11".into(),
            size_bytes: 4096,
            superseded_at: 1_700_000_000,
            expires_at: 1_700_000_000 + 2_592_000,
        }
    }

    fn ids(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    fn text_of<'a>(els: &'a [Element], id: &str) -> Option<&'a str> {
        els.iter().find(|e| e.id == id).map(|e| e.text.as_str())
    }

    #[test]
    fn a_nest_with_no_trust_renders_the_empty_state_not_a_blank_facet() {
        let els = nests_elements(&state_with(home_row()), None);
        assert!(ids(&els).contains(&"nest-trust-view-now"));
        assert!(ids(&els).contains(&"nest-trust-view-history"));
        assert!(ids(&els).contains(&"nest-trust-empty"));
        assert!(!ids(&els).contains(&"nest-trust-grant-list"));
        assert!(!ids(&els).contains(&"nest-trust-backup-list"));
        assert!(!ids(&els).contains(&"nest-trust-generation-list"));
    }

    /// `nests.md:99` — a home nest holding a backup row is plainly trusted with
    /// something, so the "not trusted to read anything" state must NOT render
    /// beside it. linux shipped the opposite and had to be fixed; this is the
    /// unit twin of the e2e assertion that caught it.
    #[test]
    fn a_backup_row_suppresses_the_empty_state_even_with_zero_content_grants() {
        let mut row = home_row();
        row.trust_backups = vec![TrustBackupRow {
            kind: TrustBackupKind::Seal,
            status: TrustBackupStatus::Active,
            destination_id: String::new(),
            destination_label: String::new(),
            since: None,
        }];
        let els = nests_elements(&state_with(row), None);
        assert!(!ids(&els).contains(&"nest-trust-empty"));
        assert!(ids(&els).contains(&"nest-trust-backup-item"));
        // The grant LIST container still renders (empty) — linux does the same,
        // and the e2e counts `nest-trust-grant-item`, never the container. What
        // must be zero is the grant rows themselves.
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "nest-trust-grant-item")
                .count(),
            0
        );
        // `nest-trust-backup-list` is conditioned on backups alone — present
        // here even with zero grants; `nest-trust-generation-list` stays absent
        // since generations are still empty (windows' independent per-list
        // `Count > 0` visibility, not a combined trust-facet condition).
        assert!(ids(&els).contains(&"nest-trust-backup-list"));
        assert!(!ids(&els).contains(&"nest-trust-generation-list"));
    }

    // ── generation recovery (`nest-trust-generation-item`) ──────────

    #[test]
    fn a_retained_generation_renders_all_six_leaves_after_the_backup_rows() {
        let mut row = home_row();
        row.trust_backups = vec![TrustBackupRow {
            kind: TrustBackupKind::Seal,
            status: TrustBackupStatus::Active,
            destination_id: String::new(),
            destination_label: String::new(),
            since: None,
        }];
        row.trust_generations = vec![generation_row(Some("/Mail/2026"))];
        let els = nests_elements(&state_with(row), None);
        let names = ids(&els);

        for leaf in [
            "nest-trust-generation-item",
            "nest-trust-generation-path",
            "nest-trust-generation-superseded",
            "nest-trust-generation-expires",
            "nest-trust-generation-size",
            "nest-trust-generation-status",
            "nest-trust-generation-restore",
        ] {
            assert!(names.contains(&leaf), "missing {leaf}");
        }
        // Placement is ratified (`nests.md:113`): one surface, backup rows then
        // what you can roll back to.
        let backup_at = names
            .iter()
            .position(|n| *n == "nest-trust-backup-item")
            .unwrap();
        let generation_at = names
            .iter()
            .position(|n| *n == "nest-trust-generation-item")
            .unwrap();
        assert!(
            backup_at < generation_at,
            "generation rows render AFTER the backup trust rows"
        );
        assert!(
            text_of(&els, "nest-trust-generation-path")
                .unwrap()
                .contains("/Mail/2026")
        );
        // The REQUIRED quota-bound copy rides the deadline leaf (`nests.md`
        // § Required copy — the quota bound).
        assert!(
            text_of(&els, "nest-trust-generation-expires")
                .unwrap()
                .contains("counts against your storage"),
            "the expiry leaf must carry the required quota-bound copy"
        );
        // Both list containers present — backups and generations are both
        // non-empty here.
        assert!(names.contains(&"nest-trust-backup-list"));
        assert!(names.contains(&"nest-trust-generation-list"));
        let backup_list_at = names
            .iter()
            .position(|n| *n == "nest-trust-backup-list")
            .unwrap();
        let generation_list_at = names
            .iter()
            .position(|n| *n == "nest-trust-generation-list")
            .unwrap();
        assert!(
            backup_list_at < backup_at && generation_list_at < generation_at,
            "each list container renders immediately before its own items"
        );
    }

    /// Invariant 1 (`nests.md:122`) — an unreachable destination renders a row
    /// saying so, with NO restore affordance. Rendering it as an absence is the
    /// false reassurance the whole surface exists to prevent.
    #[test]
    fn an_unreachable_generation_row_says_so_and_offers_no_restore() {
        let mut row = home_row();
        row.trust_generations = vec![TrustGenerationRow {
            status: TrustGenerationStatus::Unreachable,
            destination_id: "d1".into(),
            destination_label: "Aunt's nest".into(),
            folder_name: String::new(),
            path: None,
            path_hash: String::new(),
            manifest_hash: String::new(),
            size_bytes: 0,
            superseded_at: 0,
            expires_at: 0,
        }];
        let els = nests_elements(&state_with(row), None);
        let names = ids(&els);

        assert!(names.contains(&"nest-trust-generation-item"));
        // Generations alone (no backups on this row): the generation-list
        // container renders, the backup-list one does not — proves the two
        // containers are independently conditioned, not a shared "any trust
        // facet row" flag.
        assert!(names.contains(&"nest-trust-generation-list"));
        assert!(!names.contains(&"nest-trust-backup-list"));
        assert_eq!(
            text_of(&els, "nest-trust-generation-status"),
            Some(t::GENERATION_STATUS_UNREACHABLE)
        );
        assert!(
            !names.contains(&"nest-trust-generation-restore"),
            "an unreachable row has no restore address, so it offers no restore"
        );
        // It names WHICH destination went dark, and renders no invented zero
        // timestamp or "0 B" that would read as fact.
        assert_eq!(
            text_of(&els, "nest-trust-generation-path"),
            Some("Aunt's nest")
        );
        assert_eq!(text_of(&els, "nest-trust-generation-size"), Some(""));
        assert_eq!(text_of(&els, "nest-trust-generation-expires"), Some(""));
    }

    /// Invariant 2 (`nests.md:123`) — a path-less row renders its hash and is
    /// never hidden; the rows a rogue source produced are exactly the ones a
    /// user needs to see.
    #[test]
    fn a_path_less_generation_renders_its_hash_and_keeps_its_restore() {
        let mut row = home_row();
        row.trust_generations = vec![generation_row(None)];
        let els = nests_elements(&state_with(row), None);

        assert!(
            text_of(&els, "nest-trust-generation-path")
                .unwrap()
                .contains("aa11"),
            "a path-less row renders the path_hash in the path leaf's place"
        );
        assert!(
            ids(&els).contains(&"nest-trust-generation-restore"),
            "it is still restorable — only the DISPLAY path is missing"
        );
    }

    /// A generation row is trust the nest holds, so like a backup row it must
    /// suppress the "trusted with nothing" empty state.
    #[test]
    fn the_restore_gesture_carries_the_rows_own_address_never_an_index() {
        let mut row = home_row();
        row.trust_generations = vec![generation_row(Some("/Mail/2026"))];
        let els = nests_elements(&state_with(row), None);

        let restore = els
            .iter()
            .find(|e| e.id == "nest-trust-generation-restore")
            .expect("restore affordance");
        match &restore.role {
            crate::element::Role::Button(Gesture::Settings(Action::NestsRestoreGeneration {
                destination_id,
                folder_name,
                path_hash,
                manifest_hash,
            })) => {
                assert_eq!(destination_id, "d1");
                assert_eq!(folder_name, "__mail");
                assert_eq!(path_hash, "aa11");
                assert_eq!(manifest_hash, "mm11");
            }
            other => panic!("expected a RestoreGeneration gesture, got {other:?}"),
        }
    }

    // ── restore-outcome notice (`nest-trust-generation-notice`, ratified
    // 2026-07-29) ──────────────────────────────────────────────────────────

    #[test]
    fn the_generation_notice_registers_empty_with_no_restore_outcome() {
        let els = nests_elements(&state_with(home_row()), None);
        assert_eq!(
            text_of(&els, "nest-trust-generation-notice"),
            Some(""),
            "the leaf is always registered on the home row, empty until a restore resolves"
        );
    }

    #[test]
    fn a_restored_outcome_renders_its_own_notice_text() {
        let mut state = state_with(home_row());
        state.snapshot.as_mut().unwrap().restore_outcome = Some(TrustRestoreOutcome::Restored);
        let els = nests_elements(&state, None);
        assert_eq!(
            text_of(&els, "nest-trust-generation-notice"),
            Some(t::GENERATION_RESTORED)
        );
        // Not an error — `PastRecoveryWindow` is a product state, and neither
        // outcome belongs on error-message any more (`settings/mod.rs`'s
        // `Outcome::NestsSnapshot` fold only bridges `snapshot.error`).
        assert!(state.snapshot.as_ref().unwrap().error.is_none());
    }

    #[test]
    fn a_past_recovery_window_outcome_renders_its_own_notice_text_never_failed() {
        let mut state = state_with(home_row());
        state.snapshot.as_mut().unwrap().restore_outcome =
            Some(TrustRestoreOutcome::PastRecoveryWindow);
        let els = nests_elements(&state, None);
        let text = text_of(&els, "nest-trust-generation-notice").unwrap();
        assert_eq!(text, t::GENERATION_PAST_WINDOW);
        assert!(
            !text.to_lowercase().contains("failed"),
            "past-the-window is a product state, never a failure message (nests.md:124)"
        );
    }

    /// The notice is home-row-scoped: a restore's outcome describes the page's
    /// last action, not any one pairing row, which never renders generation
    /// rows at all.
    #[test]
    fn a_pairing_row_never_renders_the_generation_notice() {
        let mut pairing = home_row();
        pairing.is_home = false;
        let mut state = state_with(pairing);
        state.snapshot.as_mut().unwrap().restore_outcome = Some(TrustRestoreOutcome::Restored);
        let els = nests_elements(&state, None);
        assert!(!ids(&els).contains(&"nest-trust-generation-notice"));
    }

    /// The seal row carries no `granted_at` on the wire (`nests.md:67`), so its
    /// `since` leaf renders EMPTY — but it still registers, so a row's leaf set
    /// does not vary by kind.
    #[test]
    fn the_seal_row_registers_an_empty_since_leaf_and_the_writer_row_a_filled_one() {
        let mut row = home_row();
        row.trust_backups = vec![
            TrustBackupRow {
                kind: TrustBackupKind::Seal,
                status: TrustBackupStatus::Active,
                destination_id: String::new(),
                destination_label: String::new(),
                since: None,
            },
            TrustBackupRow {
                kind: TrustBackupKind::Writer,
                status: TrustBackupStatus::Missing,
                destination_id: "dest-1".to_string(),
                destination_label: "Offsite".to_string(),
                since: Some(1_700_000_000),
            },
        ];
        let els = nests_elements(&state_with(row), None);
        let since: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "nest-trust-backup-since")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(since.len(), 2);
        assert_eq!(since[0], "");
        assert!(since[1].starts_with(t::BACKUP_SINCE));

        let scopes: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "nest-trust-backup-scope")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(scopes[0], t::BACKUP_SCOPE_SEAL);
        assert!(
            scopes[1].contains("Offsite"),
            "the writer row must name the destination it writes to, got {:?}",
            scopes[1]
        );

        let statuses: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "nest-trust-backup-status")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(statuses[0], t::BACKUP_STATUS_ACTIVE);
        // `missing` must never collapse into `unreachable` (or vice versa) — a
        // flaky network reading as a revoked backup is the failure this guards.
        assert_eq!(statuses[1], t::BACKUP_STATUS_MISSING);
    }

    /// The `nest-trust-backup-revoke` press routes by row kind: the seal row to
    /// the source nest, a writer row to ITS destination (which is what keeps the
    /// affordance operable with the source nest hostile).
    #[test]
    fn a_backup_revoke_names_the_row_it_was_pressed_on() {
        let mut row = home_row();
        row.trust_backups = vec![
            TrustBackupRow {
                kind: TrustBackupKind::Seal,
                status: TrustBackupStatus::Active,
                destination_id: String::new(),
                destination_label: String::new(),
                since: None,
            },
            TrustBackupRow {
                kind: TrustBackupKind::Writer,
                status: TrustBackupStatus::Active,
                destination_id: "dest-1".to_string(),
                destination_label: "Offsite".to_string(),
                since: Some(1),
            },
        ];
        let els = nests_elements(&state_with(row), None);
        let gestures: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "nest-trust-backup-revoke")
            .collect();
        assert_eq!(gestures.len(), 2);
        assert!(matches!(
            &gestures[0].role,
            crate::element::Role::Button(Gesture::Settings(Action::NestsRevokeBackupSeal))
        ));
        match &gestures[1].role {
            crate::element::Role::Button(Gesture::Settings(Action::NestsRevokeBackupWriter(
                dest,
            ))) => assert_eq!(dest, "dest-1"),
            other => panic!("writer row must revoke at its destination, got {other:?}"),
        }
    }

    #[test]
    fn the_history_lens_replaces_the_now_content_with_the_timeline() {
        let mut row = home_row();
        row.lens = TrustLens::History;
        row.trust_history = vec![TrustHistoryRow {
            grant_id: vec![1; 16],
            holder: vec![2; 32],
            kind: TrustEventKind::Mint,
            scope: vec![scope("content.read", Some("mail"), None)],
            window_start: 0,
            window_end: 0,
            at: 1_700_000_000,
            folder: None,
        }];
        let els = nests_elements(&state_with(row), None);
        assert!(ids(&els).contains(&"nest-trust-history-list"));
        assert!(ids(&els).contains(&"nest-trust-history-item"));
        // `nest-trust-empty` is a Now-lens affordance — it must be gone here.
        assert!(!ids(&els).contains(&"nest-trust-empty"));
    }

    #[test]
    fn a_grant_row_renders_its_scope_status_and_the_required_honest_bound_copy() {
        let mut row = home_row();
        row.trust_grants = vec![TrustGrantRow {
            grant_id: vec![7; 16],
            holder: vec![2; 32],
            scope: vec![scope("content.read", Some("post"), Some("gold"))],
            lasts_until: 1_700_000_000,
            liveness: TrustLiveness::Active,
            unattested: false,
            folder: None,
        }];
        let els = nests_elements(&state_with(row), None);
        let scope_text = text_of(&els, "nest-trust-grant-scope").unwrap();
        assert!(
            scope_text.contains("gold"),
            "a tier-scoped grant must name WHICH tier in the audit view, got {scope_text:?}"
        );
        assert_eq!(
            text_of(&els, "nest-trust-grant-status"),
            Some(t::STATUS_ACTIVE)
        );
        assert_eq!(
            text_of(&els, "nest-trust-grant-bound-note"),
            Some(t::BOUND_NOTE_STANDING),
            "every grant row carries the honest bound — it is a spec requirement, not flavor"
        );
        assert!(ids(&els).contains(&"nest-trust-grant-renew"));
        assert!(ids(&els).contains(&"nest-trust-grant-revoke"));
        // An ordinary grant carries NO review pair — absent, not empty. This is
        // the half of the mark's contract that a "renders when raised" test
        // cannot check, and the one that keeps the mark meaningful.
        assert!(!ids(&els).contains(&"nest-trust-grant-unattested-mark"));
        assert!(!ids(&els).contains(&"nest-trust-grant-keep-button"));
    }

    /// The web-serve paywall grant names its folder — on the grant row and on
    /// its History `Revoke`, which carries no scope (`nests.md` § Trust facet
    /// — grants; the folder resolved in shared Rust).
    #[test]
    fn a_paywall_grant_and_its_revoke_name_the_folder() {
        let premium = Some(TrustFolder::Named {
            name: "premium".into(),
        });
        let mut row = home_row();
        row.trust_grants = vec![TrustGrantRow {
            grant_id: vec![7; 16],
            holder: vec![9; 32],
            scope: vec![scope("content.read", Some("folder"), None)],
            lasts_until: 1_700_000_000,
            liveness: TrustLiveness::Active,
            unattested: false,
            folder: premium.clone(),
        }];
        let els = nests_elements(&state_with(row.clone()), None);
        assert_eq!(
            text_of(&els, "nest-trust-grant-scope"),
            Some(r#"Trusted to read: Your folder "premium""#)
        );

        row.lens = TrustLens::History;
        row.trust_history = vec![TrustHistoryRow {
            grant_id: vec![6; 16],
            holder: vec![9; 32],
            kind: TrustEventKind::Revoke,
            scope: Vec::new(),
            window_start: 0,
            window_end: 0,
            at: 1_700_000_000,
            folder: premium,
        }];
        let els = nests_elements(&state_with(row), None);
        let line = text_of(&els, "nest-trust-history-item").unwrap();
        assert!(
            line.starts_with(r#"Trust revoked: Your folder "premium" · "#),
            "a Revoke names the folder its grant covered, got {line:?}"
        );
    }

    /// The post-succession review pair on a raised grant row
    /// (`succession-aftermath.md` § Adjudicating what the aftermath carries
    /// across). Asserts all three halves of the ruling at once: the mark
    /// renders, Keep is offered beside it, and **no second revocation control
    /// is minted** — the row's existing `nest-trust-grant-revoke` is Remove.
    #[test]
    fn a_carried_across_grant_renders_the_review_mark_and_keep_but_no_second_revoke() {
        let mut row = home_row();
        row.trust_grants = vec![TrustGrantRow {
            grant_id: vec![7; 16],
            holder: vec![2; 32],
            scope: vec![scope("content.read", Some("mail"), None)],
            lasts_until: 1_700_000_000,
            liveness: TrustLiveness::Active,
            unattested: true,
            folder: None,
        }];
        let els = nests_elements(&state_with(row), None);

        assert!(ids(&els).contains(&"nest-trust-grant-unattested-mark"));
        assert!(ids(&els).contains(&"nest-trust-grant-keep-button"));
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "nest-trust-grant-revoke")
                .count(),
            1,
            "Remove reuses the revoke button the row already has — a second one \
             would be a parallel removal mechanism the ruling forbids"
        );

        // Keep dispatches the adjudication carrying THIS row's handle. Getting
        // the handle wrong would clear some other grant's mark, which no test
        // asserting mere presence would catch.
        let keep = els
            .iter()
            .find(|e| e.id == "nest-trust-grant-keep-button")
            .expect("the Keep button renders on a raised row");
        match &keep.role {
            crate::element::Role::Button(Gesture::Settings(Action::NestsKeepGrant(id))) => {
                assert_eq!(id, &vec![7u8; 16], "Keep must name the row it painted on")
            }
            other => panic!("Keep must dispatch NestsKeepGrant, got {other:?}"),
        }
    }

    /// The mark's copy is a claim about the user's security, so it is pinned on
    /// the **rendered text** rather than the key — and on the two properties the
    /// ruling names: it must read as review rather than accusation (after a
    /// recovery almost every row here is the owner's own), and it must stay in
    /// trust vocabulary, never leaking "capability"/"grant" (`participants.md`
    /// § Naming).
    #[test]
    fn the_review_marks_copy_reviews_rather_than_accuses_and_stays_in_trust_vocabulary() {
        let copy = t::GRANT_UNATTESTED_MARK.to_lowercase();
        assert!(
            copy.contains("keep it"),
            "the mark must offer the benign reading first, got {copy:?}"
        );
        for accusatory in ["stolen", "attacker", "thief", "compromised", "malicious"] {
            assert!(
                !copy.contains(accusatory),
                "the mark must not accuse — found {accusatory:?} in {copy:?}"
            );
        }
        for internal in ["capability", "grant"] {
            assert!(
                !copy.contains(internal),
                "visible trust copy must not say {internal:?} (participants.md § Naming), got {copy:?}"
            );
        }
    }

    /// The mint affordance is hidden when the shared catalog is empty — a picker
    /// that can only error is worse than no picker (`nests.md` § Mint).
    #[test]
    fn the_mint_button_is_absent_without_a_catalog_and_present_with_one() {
        let els = nests_elements(&state_with(home_row()), None);
        assert!(!ids(&els).contains(&"nest-trust-grant-mint-button"));

        let mut row = home_row();
        row.mint_options = vec![TrustMintOption {
            use_case: TrustMintUseCase::Calendar,
            tier: None,
            scope: vec![scope("content.read", Some("calendar"), None)],
            holder_candidates: vec!["mda".to_string()],
        }];
        let els = nests_elements(&state_with(row), None);
        assert!(ids(&els).contains(&"nest-trust-grant-mint-button"));
        // The form itself stays collapsed until the button opens it.
        assert!(!ids(&els).contains(&"nest-trust-mint-scope-select"));
    }

    #[test]
    fn the_mint_form_derives_its_holder_and_gates_confirm_on_a_pick() {
        let mut row = home_row();
        row.mint_options = vec![TrustMintOption {
            use_case: TrustMintUseCase::PaywalledPosts,
            tier: Some("gold".to_string()),
            scope: vec![scope("content.read", Some("post"), Some("gold"))],
            holder_candidates: vec!["web-serve".to_string()],
        }];
        let nest_id = row.nest_id.clone();
        let mut state = state_with(row);
        state.mint = Some(MintForm {
            nest_id: nest_id.clone(),
            ..Default::default()
        });

        // Opened, nothing picked → the select renders, confirm is disabled, and
        // there is no action to dispatch.
        let els = nests_elements(&state, None);
        let confirm = els
            .iter()
            .find(|e| e.id == "nest-trust-mint-confirm-button")
            .unwrap();
        assert!(!confirm.enabled);
        assert!(state.mint_action().is_none());
        // A single candidate ⇒ the holder select never renders (the holder is
        // derived from the scope choice, `nests.md` § Mint).
        assert!(!ids(&els).contains(&"nest-trust-mint-holder-select"));

        // Picking by the RENDERED label is the cross-app select contract.
        let label = t::mint_option_paywalled("gold");
        let idx = mint_option_index(state.row_for(&nest_id).unwrap(), &label);
        assert_eq!(
            idx,
            Some(0),
            "the rendered label must resolve back to its catalog entry"
        );
        state.mint.as_mut().unwrap().scope = idx;

        let els = nests_elements(&state, None);
        let confirm = els
            .iter()
            .find(|e| e.id == "nest-trust-mint-confirm-button")
            .unwrap();
        assert!(confirm.enabled);
        match state.mint_action() {
            Some(LinkedNestsAction::Mint {
                nest_id: n,
                holder_bridge_id,
                scope,
                duration,
            }) => {
                assert_eq!(n, nest_id);
                assert_eq!(holder_bridge_id, "web-serve");
                assert_eq!(scope[0].tier.as_deref(), Some("gold"));
                // Nothing picked: confirm names the row's default explicitly.
                assert_eq!(duration, Some(TrustGrantDuration::OneOff));
            }
            other => panic!("expected a Mint action, got {other:?}"),
        }
    }

    /// The duration select renders in the open form showing the row's default
    /// (a blessed row defaults to 90 days), round-trips its rendered label, and
    /// the pick is what confirm mints (`nests.md` § Expiry / renewal →
    /// *Duration and blessing*).
    #[test]
    fn the_mint_form_offers_both_durations_and_mints_the_pick() {
        let mut row = home_row();
        row.blessed = true;
        row.mint_default_duration = TrustGrantDuration::Standard;
        row.mint_options = vec![TrustMintOption {
            use_case: TrustMintUseCase::Calendar,
            tier: None,
            scope: vec![scope("content.read", Some("calendar"), None)],
            holder_candidates: vec!["mda".to_string()],
        }];
        let nest_id = row.nest_id.clone();
        let mut state = state_with(row);
        state.mint = Some(MintForm {
            nest_id,
            scope: Some(0),
            ..Default::default()
        });

        let els = nests_elements(&state, None);
        let select = els
            .iter()
            .find(|e| e.id == "nest-trust-mint-duration-select")
            .expect("the duration select renders in the open form");
        assert_eq!(select.text, t::MINT_DURATION_STANDARD);

        let picked = mint_duration_for_label(t::MINT_DURATION_ONE_OFF);
        assert_eq!(picked, Some(TrustGrantDuration::OneOff));
        state.mint.as_mut().unwrap().duration = picked;
        match state.mint_action() {
            Some(LinkedNestsAction::Mint { duration, .. }) => {
                assert_eq!(duration, Some(TrustGrantDuration::OneOff))
            }
            other => panic!("expected a Mint action, got {other:?}"),
        }
    }

    /// The blessing toggle renders on the home row only, mirrors the row's
    /// state for a driver, and its press asks for the opposite verdict.
    #[test]
    fn the_blessed_toggle_is_on_the_home_row_and_flips_the_verdict() {
        let els = nests_elements(&state_with(home_row()), None);
        let toggle = els
            .iter()
            .find(|e| e.id == "nest-trust-blessed-toggle")
            .expect("home row carries the blessing toggle");
        assert!(
            toggle
                .attrs
                .contains(&("state".to_string(), "off".to_string()))
        );
        match &toggle.role {
            crate::element::Role::Checkbox {
                gesture: Gesture::Settings(Action::NestsSetBlessed { blessed, .. }),
                checked,
            } => {
                assert!(!checked);
                assert!(*blessed, "an un-blessed row's press blesses it");
            }
            other => panic!("expected the blessing checkbox, got {other:?}"),
        }

        let mut row = home_row();
        row.blessed = true;
        let els = nests_elements(&state_with(row), None);
        let toggle = els
            .iter()
            .find(|e| e.id == "nest-trust-blessed-toggle")
            .unwrap();
        assert!(
            toggle
                .attrs
                .contains(&("state".to_string(), "on".to_string()))
        );
    }

    /// The ambiguity case (>1 candidate): the holder select renders and confirm
    /// stays disabled until a holder is picked — never a blind mint to whichever
    /// candidate happened to be first.
    #[test]
    fn an_ambiguous_option_requires_an_explicit_holder_pick() {
        let mut row = home_row();
        row.mint_options = vec![TrustMintOption {
            use_case: TrustMintUseCase::Mail,
            tier: None,
            scope: vec![scope("content.read", Some("mail"), None)],
            holder_candidates: vec!["mda".to_string(), "other-processor".to_string()],
        }];
        let nest_id = row.nest_id.clone();
        let mut state = state_with(row);
        state.mint = Some(MintForm {
            nest_id,
            scope: Some(0),
            holder: None,
            duration: None,
        });

        let els = nests_elements(&state, None);
        assert!(ids(&els).contains(&"nest-trust-mint-holder-select"));
        assert!(state.mint_action().is_none());

        state.mint.as_mut().unwrap().holder = Some("other-processor".to_string());
        match state.mint_action() {
            Some(LinkedNestsAction::Mint {
                holder_bridge_id, ..
            }) => assert_eq!(holder_bridge_id, "other-processor"),
            other => panic!("expected a Mint action, got {other:?}"),
        }
    }

    /// The home row is not a pairing: no unlink, no sync-caps, no expiry
    /// (`nests.md` § Layout). `LinkedNestsActions.pairing_count()` counts unlink
    /// buttons precisely because of this, so a stray one would inflate it.
    #[test]
    fn the_home_row_carries_no_unlink_and_a_pairing_does() {
        let els = nests_elements(&state_with(home_row()), None);
        assert!(!ids(&els).contains(&"nests-item-unlink-button"));
        assert!(!ids(&els).contains(&"nests-item-capabilities"));

        let mut state = state_with(home_row());
        let mut pairing = home_row();
        pairing.is_home = false;
        pairing.nest_id = "bb".repeat(32);
        state.snapshot.as_mut().unwrap().pairings = vec![pairing];
        let els = nests_elements(&state, None);
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "nests-item-unlink-button")
                .count(),
            1,
            "exactly one unlink button — the pairing's, never the home row's"
        );
        assert_eq!(els.iter().filter(|e| e.id == "nests-item").count(), 2);
    }

    /// The capabilities line paints the row's labels — the shared display
    /// form — never the wire names (`linked-nests.md` § The surface).
    #[test]
    fn a_pairings_capabilities_line_paints_the_shared_labels() {
        let mut state = state_with(home_row());
        let mut pairing = home_row();
        pairing.is_home = false;
        pairing.capabilities = vec!["mls_pull".to_string(), "account_replica".to_string()];
        pairing.capability_labels = pairing
            .capabilities
            .iter()
            .map(|c| fauna_client_pair::capability_label(c))
            .collect();
        state.snapshot.as_mut().unwrap().pairings = vec![pairing];
        let els = nests_elements(&state, None);
        let line = text_of(&els, "nests-item-capabilities").expect("the line renders");
        assert_eq!(
            line,
            format!(
                "mls_pull, {}",
                fauna_i18n::strings::nests::CAPABILITY_ACCOUNT_REPLICA
            )
        );
        assert!(!line.contains("account_replica"));
    }

    #[test]
    fn the_add_form_is_revealed_and_its_submit_gated_on_a_non_empty_value() {
        let mut state = state_with(home_row());
        let els = nests_elements(&state, None);
        assert!(ids(&els).contains(&"nests-add-button"));
        assert!(!ids(&els).contains(&"nests-add-input"));

        state.add_form_open = true;
        let els = nests_elements(&state, None);
        let submit = els
            .iter()
            .find(|e| e.id == "nests-add-submit-button")
            .unwrap();
        assert!(!submit.enabled, "an empty address must not be submittable");

        state.add_input = "  ".to_string();
        let els = nests_elements(&state, None);
        let submit = els
            .iter()
            .find(|e| e.id == "nests-add-submit-button")
            .unwrap();
        assert!(!submit.enabled, "whitespace is not an address");

        state.add_input = "https://nest.example".to_string();
        let els = nests_elements(&state, None);
        let submit = els
            .iter()
            .find(|e| e.id == "nests-add-submit-button")
            .unwrap();
        assert!(submit.enabled);
        assert!(ids(&els).contains(&"nests-add-cancel-button"));
    }

    /// Row children are registered under their `nests-item` occurrence — the
    /// two-step path `nests.md:53` documents for a scoped query — while staying
    /// readable flat (an empty scope resolves to the whole frame, so an
    /// unscoped query walks them in registration order).
    #[test]
    fn row_children_are_scoped_to_their_row_and_leaves_to_their_item() {
        let mut row = home_row();
        row.trust_grants = vec![TrustGrantRow {
            grant_id: vec![7; 16],
            holder: vec![2; 32],
            scope: vec![scope("content.read", Some("calendar"), None)],
            lasts_until: 0,
            liveness: TrustLiveness::Active,
            unattested: false,
            folder: None,
        }];
        let els = nests_elements(&state_with(row), None);
        let path_of = |id: &str| {
            els.iter()
                .find(|e| e.id == id)
                .map(|e| e.path.clone())
                .unwrap()
        };
        assert_eq!(path_of("nests-item"), vec![]);
        assert_eq!(
            path_of("nests-item-label"),
            vec![("nests-item".to_string(), 0)]
        );
        assert_eq!(
            path_of("nest-trust-grant-scope"),
            vec![
                ("nests-item".to_string(), 0),
                ("nest-trust-grant-item".to_string(), 0)
            ],
            "outer row first, then the grant item — the order nests.md:53 documents"
        );
    }

    #[test]
    fn reset_form_drops_a_half_typed_address_and_a_half_picked_mint() {
        let mut state = NestsState {
            add_form_open: true,
            add_input: "https://half-typed".to_string(),
            mint: Some(MintForm::default()),
            ..Default::default()
        };
        state.reset_form();
        assert!(!state.add_form_open);
        assert!(state.add_input.is_empty());
        assert!(state.mint.is_none());
    }

    /// The escrow-holder role badge (T16; participants.md § Roles) renders on
    /// exactly the nest row whose identity has stamped an escrow receipt —
    /// derived, never asserted by the nest — and is absent otherwise.
    #[test]
    fn escrow_holder_badge_renders_on_the_matching_row_only() {
        // The home row's identity holds escrow.
        let mut state = state_with(home_row());
        state.escrow_holders = vec![[0xAA; 32]]; // home_row's nest_id = "aa"*32
        let els = nests_elements(&state, None);
        let badges: Vec<_> = els
            .iter()
            .filter(|e| e.id == "participant-escrow-holder-badge")
            .collect();
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].text, t::ESCROW_HOLDER_BADGE);
        assert!(
            badges[0].path.iter().any(|s| s.0 == "nests-item"),
            "the badge scopes under its nest row"
        );

        // A different holder → no badge (never a false role claim).
        state.escrow_holders = vec![[0xBB; 32]];
        let els = nests_elements(&state, None);
        assert!(
            !els.iter()
                .any(|e| e.id == "participant-escrow-holder-badge"),
            "no receipt from this nest's identity → no badge"
        );
    }

    /// The custodian-NEST family (the nest-custodian identity fact): a
    /// nest-anchored custody renders as its own `nests-item` with the
    /// `nest-trust-custody-*` children — trust-vocabulary scope, the
    /// three-state receipt honesty element, held-bytes, and revoke beside
    /// the REQUIRED honest-bound copy, the gesture carrying the row's
    /// ORIGINAL fold index (the shared CustodyRevoke handler resolves
    /// `devices.custody.rows[i]`). Device-anchored rows stay off this page.
    #[test]
    fn custodian_nest_rows_render_with_the_approved_family() {
        use fauna_i18n::strings::devices as td;
        let state = state_with(home_row());
        let facet = crate::settings::devices::custody_walk_facet();
        let els = nests_elements(&state, Some(&facet));

        // One linked home row + one custodian nest (the fixture's third,
        // nest-anchored custody) = two nests-items; the two device-anchored
        // custodies never paint anything here.
        assert_eq!(ids(&els).iter().filter(|i| **i == "nests-item").count(), 2);
        assert!(
            els.iter().all(|e| !e.id.starts_with("custody-holder")),
            "device-anchored custodies belong to the Devices page"
        );

        // The family, with the shared copy (stated once, both pages).
        let item = els
            .iter()
            .find(|e| e.id == "nest-trust-custody-item")
            .expect("the custody child family renders");
        assert_eq!(item.text, td::CUSTODY_REVOKE_BOUND_NOTE);
        assert!(
            item.path.iter().any(|s| s.0 == "nests-item"),
            "the family scopes under its nests-item"
        );
        assert_eq!(
            text_of(&els, "nest-trust-custody-scope"),
            Some(td::CUSTODY_HOLDER_SCOPE)
        );
        // The fixture's nest custody has no receipt yet — the three-state
        // honesty rule's third state, its own words.
        assert_eq!(
            text_of(&els, "nest-trust-custody-receipt-status"),
            Some(td::CUSTODY_RECEIPT_NONE)
        );
        assert!(text_of(&els, "nest-trust-custody-held-bytes").is_some());

        // Revoke: enabled (the custody is minted + live) and carrying the
        // ORIGINAL fold index — 2, the nest row's position in the shared
        // fixture — never the dense render index.
        let revoke = els
            .iter()
            .find(|e| e.id == "nest-trust-custody-revoke-button")
            .expect("revoke renders");
        assert!(revoke.enabled);
        assert!(
            matches!(
                &revoke.role,
                crate::element::Role::Button(Gesture::Settings(Action::CustodyRevoke(2)))
            ),
            "the gesture must carry the fold index into the shared handler"
        );

        // The item label names the host account in trust vocabulary.
        let labels: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "nests-item-label")
            .map(|e| e.text.as_str())
            .collect();
        assert!(
            labels.iter().any(|l| l.contains("trusted to hold")),
            "the custodian nest's label speaks trust vocabulary: {labels:?}"
        );
    }
}
