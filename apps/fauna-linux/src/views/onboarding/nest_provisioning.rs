//! Page 6 of handle-first onboarding: nest provisioning progress.
//!
//! Snapshot-driven: the page reads `provisioning_snapshot()` on every
//! observer tick and renders the four-step pipeline (Domain, Server, Dns,
//! Online) plus the lifecycle controls (start / cancel / retry / continue
//! / back). The orchestrator owns every state transition; this page is
//! purely presentation. Implementation follows
//! `docs/goal/behavior/onboarding.md` §6 (design tracked internally).
//!
//! ui.yaml IDs (page): `page-heading`, `error-message`,
//! `provisioning-progress` (component containing per-step rows),
//! `provisioning-start-button`, `provisioning-cancel-button`,
//! `provisioning-retry-button`, `provisioning-elapsed`,
//! `provisioning-continue-button`, `provisioning-back-button`,
//! `provisioning-price-bom` (container), `provisioning-bom-domain-line`
//! (optional — only when buying a new domain), `provisioning-bom-vps-line`
//! (always present) — sourced from `OnboardingMachine::bom_domain_line()` /
//! `bom_vps_line()`.
//!
//! ui.yaml IDs (per step row, indexed 0..3): `provisioning-step-row`,
//! `provisioning-step-checkbox`, `provisioning-step-label`,
//! `provisioning-substep`, `provisioning-step-error`.
//!
//! Entry: the wizard transitions to `OnboardingStep::NestProvisioning`
//! after `continue_from_vps()` succeeds. The user lands with `overall ==
//! Idle` and reviews the price summary at the top, then clicks the
//! `provisioning-start-button` ("Buy and set up") to actually kick the
//! orchestrator. Continue is gated until `overall == Succeeded`.

use fauna_ui_ids as ids;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Box, Button, Label, Orientation};

use crate::i18n::resolve_key as resolve;
use crate::testid::set_test_id;
use fauna_onboarding_machine::OnboardingMachine;
use fauna_provisioning::progress::{OverallStatus, ProvisioningSnapshot, StepStatus};

/// A step row's per-tick refresh closure, driven from its `StepSnapshot`.
type StepRefresher = Rc<dyn Fn(&fauna_provisioning::progress::StepSnapshot)>;

/// Renders one snapshot step row and returns the (container, refresh) pair
/// so the parent's refresh closure can update each row in place.
///
/// Each row is identified by the bare `provisioning-step-row` test ID;
/// callers address rows by their tree-order position via the bridge's
/// scope syntax (e.g. `scope="provisioning-step-row[0]"` for Domain).
fn build_step_row(_idx: usize) -> (Box, StepRefresher) {
    let row = Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&row, ids::PROVISIONING_STEP_ROW);

    let checkbox = Label::new(None);
    set_test_id(&checkbox, ids::PROVISIONING_STEP_CHECKBOX);
    row.append(&checkbox);

    let label = Label::new(None);
    label.set_halign(gtk::Align::Start);
    set_test_id(&label, ids::PROVISIONING_STEP_LABEL);
    row.append(&label);

    let substep = Label::new(None);
    substep.set_halign(gtk::Align::Start);
    substep.add_css_class("fauna-muted");
    set_test_id(&substep, ids::PROVISIONING_SUBSTEP);
    substep.set_visible(false);
    row.append(&substep);

    let error = Label::new(None);
    error.set_halign(gtk::Align::Start);
    error.add_css_class("error");
    error.set_wrap(true);
    set_test_id(&error, ids::PROVISIONING_STEP_ERROR);
    error.set_visible(false);
    row.append(&error);

    let refresh: Rc<dyn Fn(&fauna_provisioning::progress::StepSnapshot)> = Rc::new(move |snap| {
        // Glyph + step name are single-sourced in shared Rust
        // (`fauna_provisioning::progress`); the glyph is locale-invariant (use
        // the `String` directly), the step name resolves through linux's i18n.
        checkbox.set_text(&fauna_provisioning::progress::status_glyph(snap.status));
        label.set_text(&crate::i18n::provisioning_step_label(snap.kind));

        // Sub-step text. Visibility is the shared `shows_substep` predicate
        // (`StepSnapshot::recompute_display` — shown on Skipped and on
        // Running/Failed when a substep label or attempt suffix exists); we only
        // resolve the text it gates. The predicate encodes "the resolved text is
        // non-empty", so the row is shown verbatim when it fires.
        if snap.shows_substep {
            let mut text = if matches!(snap.status, StepStatus::Skipped) {
                resolve("onboarding.provision.substep.status_skipped")
            } else {
                // Pass `last_error` as the `{cause}` arg so a `status_retrying`
                // substep substitutes the real cause (shared `substep_label`);
                // every other substep ignores it.
                snap.substep
                    .map(|k| crate::i18n::provisioning_substep_label(k, snap.last_error.clone()))
                    .unwrap_or_default()
            };
            if snap.shows_attempt_suffix {
                let suffix = resolve("onboarding.provision.step_attempt_template")
                    .replace("{attempt}", &snap.attempt.to_string())
                    .replace("{max_attempts}", &snap.max_attempts.to_string());
                text.push_str(&suffix);
            }
            substep.set_text(&text);
            substep.set_visible(true);
        } else {
            substep.set_visible(false);
        }

        // Error line — the shared `shows_error` predicate gates it (Failed with a
        // non-empty last_error; a Running row mid-retry keeps a stale last_error
        // from an earlier attempt that must not surface).
        if snap.shows_error {
            error.set_text(snap.last_error.as_deref().unwrap_or_default());
            error.set_visible(true);
        } else {
            error.set_visible(false);
        }
    });

    (row, refresh)
}

pub fn build(m: Arc<OnboardingMachine>) -> (Box, Rc<dyn Fn()>) {
    let root = Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    // -- Title --
    let title = Label::new(Some(&resolve("onboarding.nest_provisioning.title")));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    // -- Top-region price summary ("Bill of Materials") --
    // Pre-commit recap of up to two priced line items from
    // `bill_of_materials()`: the domain's one-time registration price
    // (only when buying a new domain) and the selected VPS's monthly
    // price (always present once vps_config's Continue has been taken).
    // Pure computation over already-in-state DNS/VPS data — no IO, so the
    // refresh closure below just re-derives it every tick like everything
    // else on this page (onboarding.md §6).
    let price_bom = Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(2)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&price_bom, ids::PROVISIONING_PRICE_BOM);

    let bom_domain_line = Label::new(None);
    bom_domain_line.set_halign(gtk::Align::Start);
    set_test_id(&bom_domain_line, ids::PROVISIONING_BOM_DOMAIN_LINE);
    bom_domain_line.set_visible(false);
    price_bom.append(&bom_domain_line);

    let bom_vps_line = Label::new(None);
    bom_vps_line.set_halign(gtk::Align::Start);
    set_test_id(&bom_vps_line, ids::PROVISIONING_BOM_VPS_LINE);
    bom_vps_line.set_visible(false);
    price_bom.append(&bom_vps_line);

    root.append(&price_bom);

    // -- Top-region "Buy and set up" CTA --
    // Visible only while overall == Idle (per target doc §6 click-handler
    // table).
    let start_btn = Button::with_label(&resolve("onboarding.nest_provisioning.start_button"));
    start_btn.add_css_class("suggested-action");
    set_test_id(&start_btn, ids::PROVISIONING_START_BUTTON);
    {
        let m = m.clone();
        start_btn.connect_clicked(move |_| {
            // No tokio runtime runs on the GTK main thread during onboarding
            // (pre-client), so `start_provisioning`'s `tokio::spawn` would have
            // no runtime to spawn onto (it would panic). Drive the orchestrator
            // to completion on a worker runtime instead — the same mechanism the
            // verify_dns / verify_vps buttons use. Observer ticks keep
            // re-rendering the page, and the cancel button still raises the
            // shared `provisioning_cancel` flag from the GTK thread to stop it.
            let m = m.clone();
            crate::async_helper::run_on_tokio(async move { m.run_provisioning().await }, |_| {});
        });
    }
    root.append(&start_btn);

    // -- Progress region: four step rows --
    let progress = Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&progress, ids::PROVISIONING_PROGRESS);
    root.append(&progress);

    let mut step_refreshers: Vec<StepRefresher> = Vec::with_capacity(4);
    for i in 0..4 {
        let (row, refresh) = build_step_row(i);
        progress.append(&row);
        step_refreshers.push(refresh);
    }

    // -- Elapsed text --
    let elapsed = Label::new(None);
    elapsed.set_halign(gtk::Align::Start);
    elapsed.add_css_class("fauna-muted");
    set_test_id(&elapsed, ids::PROVISIONING_ELAPSED);
    elapsed.set_visible(false);
    root.append(&elapsed);

    // -- Page-level error label --
    let error_label = Label::new(None);
    error_label.add_css_class("error");
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    root.append(&error_label);

    // -- Bottom-row controls --
    let bottom = Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    bottom.set_margin_top(12);

    let back_btn = Button::with_label(&resolve("common.back"));
    set_test_id(&back_btn, ids::PROVISIONING_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| m.back());
    }
    bottom.append(&back_btn);

    let cancel_btn = Button::with_label(&resolve("common.cancel"));
    set_test_id(&cancel_btn, ids::PROVISIONING_CANCEL_BUTTON);
    cancel_btn.set_visible(false);
    {
        let m = m.clone();
        cancel_btn.connect_clicked(move |_| m.cancel_provisioning());
    }
    bottom.append(&cancel_btn);

    let retry_btn = Button::with_label(&resolve("common.retry"));
    retry_btn.add_css_class("suggested-action");
    set_test_id(&retry_btn, ids::PROVISIONING_RETRY_BUTTON);
    retry_btn.set_visible(false);
    {
        let m = m.clone();
        retry_btn.connect_clicked(move |_| {
            // Same runtime constraint as the start button. `run_provisioning`
            // (via `run_provisioning_inner`) resets the snapshot + cancel flag
            // at entry, so it doubles as the retry/resume entry point —
            // idempotency short-circuits already-completed steps.
            let m = m.clone();
            crate::async_helper::run_on_tokio(async move { m.run_provisioning().await }, |_| {});
        });
    }
    bottom.append(&retry_btn);

    let continue_btn = Button::with_label(&resolve("common.continue"));
    continue_btn.add_css_class("suggested-action");
    continue_btn.set_sensitive(false);
    set_test_id(&continue_btn, ids::PROVISIONING_CONTINUE_BUTTON);
    {
        let m = m.clone();
        continue_btn.connect_clicked(move |_| {
            // On `Done` the orchestrator's observer tick routes the outcome
            // (`mod.rs::handle_change`).
            m.continue_from_provisioning();
        });
    }
    bottom.append(&continue_btn);

    root.append(&bottom);

    // Un-id'd chrome beneath the lifecycle row, rule 5 (`ui/README.md` rule
    // 5): the four `○` step glyphs above are a symbol, not a reason. Shape
    // copied from `settings/atproto.rs`'s `depth_reason`.
    let continue_reason = Label::builder()
        .visible(false)
        .wrap(true)
        .halign(gtk::Align::End)
        .css_classes(["dim-label"])
        .build();
    root.append(&continue_reason);

    // -- Refresh closure --
    // The elapsed counter ticks once per second while running so the user
    // sees progress between snapshot updates; it pulls from the snapshot's
    // started_at_ms / finished_at_ms.
    let last_overall: Rc<Cell<Option<OverallStatus>>> = Rc::new(Cell::new(None));
    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let start_btn = start_btn.clone();
        let cancel_btn = cancel_btn.clone();
        let retry_btn = retry_btn.clone();
        let continue_btn = continue_btn.clone();
        let continue_reason = continue_reason.clone();
        let elapsed = elapsed.clone();
        let error_label = error_label.clone();
        let last_overall = last_overall.clone();
        let bom_domain_line = bom_domain_line.clone();
        let bom_vps_line = bom_vps_line.clone();
        move || {
            let snap = m.provisioning_snapshot();

            // Bill-of-materials recap — pre-derived by the shared machine
            // (`resolve_nested`, not `resolve`: `{label}` carries the step's
            // own i18n key). Mirrors tui's `wizard::nest_provisioning`.
            match m.bom_domain_line() {
                Some(text) => {
                    bom_domain_line.set_text(&text.resolve_nested(crate::i18n::strings::lookup));
                    bom_domain_line.set_visible(true);
                }
                None => bom_domain_line.set_visible(false),
            }
            match m.bom_vps_line() {
                Some(text) => {
                    bom_vps_line.set_text(&text.resolve_nested(crate::i18n::strings::lookup));
                    bom_vps_line.set_visible(true);
                }
                None => bom_vps_line.set_visible(false),
            }

            // Per-row updates — the snapshot always carries 4 entries.
            for (i, refresher) in step_refreshers.iter().enumerate() {
                if let Some(step) = snap.steps.get(i) {
                    refresher(step);
                }
            }

            // Lifecycle button visibility / state. The affordance rules
            // (onboarding.md §6) are the typed predicates on OverallStatus, so
            // every app reads the same source — notably `can_retry()` covers
            // both Failed and soft-Cancelled (cancelled runs keep their
            // resources, retry resumes via idempotency).
            let overall = snap.overall;
            start_btn.set_visible(overall.is_idle());
            cancel_btn.set_visible(overall.is_running());
            retry_btn.set_visible(overall.can_retry());
            continue_btn.set_sensitive(overall.can_continue());
            match overall.continue_blocked_reason() {
                Some(text) => {
                    continue_reason.set_text(&text.resolve(crate::i18n::strings::lookup));
                    continue_reason.set_visible(true);
                }
                None => continue_reason.set_visible(false),
            }

            // Elapsed: once started, show "Ns elapsed". When finished,
            // freeze on the finish time. The compute + i18n glue is
            // single-sourced in shared Rust (value-formatting.md).
            match crate::i18n::provisioning_elapsed(
                snap.started_at_ms,
                snap.finished_at_ms,
                fauna_provisioning::progress::now_ms(),
            ) {
                Some(text) => {
                    elapsed.set_text(&text);
                    elapsed.set_visible(true);
                }
                None => elapsed.set_visible(false),
            }

            // Page-level error: prefer the snapshot's final_error (set on
            // Failed/Cancelled); fall back to the machine's general
            // error_message for transitions outside provisioning.
            let err_text = snap
                .final_error
                .as_deref()
                .map(str::to_string)
                .or_else(|| m.error_message().filter(|s| !s.is_empty()));
            crate::settings::render_error_label(&error_label, err_text.as_deref());

            // First entry into Running: kick the 1Hz tick that drives the
            // elapsed counter. We only register the timer once per page
            // build to avoid stacking ticks.
            if last_overall.replace(Some(overall)) != Some(overall) && overall.is_running() {
                let _ = ensure_elapsed_tick(&snap);
            }
        }
    });

    (root, refresh)
}

/// Currently a no-op placeholder — the observer's existing notify() chain
/// fires often enough during Running (after each step transition) that an
/// extra timer isn't needed for parity with the spec. Kept as a hook so a
/// future change can add a 1Hz tick without restructuring the refresh
/// closure. Returns `Ok(())` unconditionally.
fn ensure_elapsed_tick(_snap: &ProvisioningSnapshot) -> Result<(), ()> {
    Ok(())
}
