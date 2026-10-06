//! The offline-affordance gate — W4 (account-data-plane.md § Workstreams) phase 4 on linux.
//!
//! The charter's class-3 sentence ("UI desensitizes these offline",
//! `docs/goal/architecture/account-data-plane.md` § The offline-mutation
//! contract) as one seam. The *decision* is not ours: it is the shared
//! [`fauna_protocol::offline_class::affordance`], which all seven apps read so
//! that none keeps a per-app list of widgets-to-grey (priority #2). What is
//! linux's own is only **how a persistent widget tree obeys it**.
//!
//! # Why linux cannot copy tui's seam verbatim
//!
//! tui rebuilds its element list every frame, so it gates in the one place that
//! list is produced (`App::page_elements`) and a stale gate is impossible by
//! construction. GTK widgets outlive the state that gated them: a control built
//! while connected must desensitize when the link drops, and a control the page
//! re-enables while offline must not escape. So the seam here is a **registry**
//! instead of a pass:
//!
//! * a page declares what its control issues, once, at construction
//!   ([`declare_wire_kind`]) — the whole contribution a page author makes, the
//!   same contract tui's `Action::wire_kind` states;
//! * [`set_connection_state`] re-decides every live declaration when the link
//!   changes (called from the ONE place linux learns the state word, so the
//!   `connection-status` indicator and this gate cannot disagree);
//! * each declaration also watches its own widget's `sensitive` property, so a
//!   page enabling a control while offline is re-gated immediately rather than
//!   at some later repaint that may never come.
//!
//! # What it never does
//!
//! It never *enables* what the page disabled. The page's own reason is stronger
//! and more specific than "no nest" — the same rule tui's early return states —
//! so the effective sensitivity is `the page's own intent AND the gate's
//! verdict`, and a reconnect restores exactly the page's intent, never more.
//! The reason likewise rides the widget's tooltip only when the page left it
//! empty (linux's established disabled-with-a-reason idiom, e.g.
//! `admin-dns-cert-delegate-button`'s "no controlled zone"), and is withdrawn
//! on reconnect only if this gate is what put it there.
//!
//! The reason is per affordance, never a global "you are offline" banner —
//! `account-data-plane.md` § R11, which is also why one mechanism covers the
//! nest-*less* account.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Upgrade a declaration's weak widget reference — the ONE place this module
/// does it, so the cost is countable.
///
/// It is not free: glib's `g_weak_ref_get` takes a **process-global** pointer
/// lock plus a ref/unref pair, and this module used to pay one per live
/// declaration *per new declaration*. That made building a page quadratic in
/// the number of gated controls, which is not a theoretical bound — it burned a
/// core for 25 s+ while rendering feed cards and took the whole GTK main loop
/// with it, so the e2e agent could not answer and the failure surfaced as
/// `agent timeout` on whatever the test happened to be reading.
fn upgrade(weak: &glib::WeakRef<gtk::Widget>) -> Option<gtk::Widget> {
    #[cfg(test)]
    UPGRADES.with(|u| u.set(u.get() + 1));
    weak.upgrade()
}

#[cfg(test)]
thread_local! {
    /// Counts [`upgrade`] calls so the registry's cost can be asserted as a
    /// COUNT rather than a duration — the same rule the e2e suite follows
    /// (`e2e-conventions.md` convention 14: assert latency-independent state,
    /// never wall-clock timing). A stopwatch here would be brittle on a loaded
    /// box; the number of upgrades is exact on any machine.
    static UPGRADES: Cell<u64> = const { Cell::new(0) };
}

/// One declared affordance: the widget, what it issues, and the page's own
/// intent for it.
struct Declaration {
    widget: glib::WeakRef<gtk::Widget>,
    /// The wire kind this control issues — the key
    /// [`fauna_protocol::offline_class::affordance`] reads.
    kind: &'static str,
    /// The sensitivity the *page* asked for, tracked separately from the
    /// widget's live value so a reconnect restores the page's intent rather
    /// than blanket-enabling.
    page_sensitive: Cell<bool>,
    /// `true` while the tooltip on the widget is this gate's reason text (the
    /// page had none), so releasing clears only what we wrote.
    tooltip_is_ours: Cell<bool>,
    /// The sensitivity this gate last WROTE and has not yet seen echoed back.
    ///
    /// ⚠ The obvious design — an "I am writing now" flag held across the write
    /// — does not work, and the failure is silent. GTK does **not** deliver
    /// `notify::sensitive` synchronously inside `set_sensitive`: measured
    /// 2026-08-14, the notification for the gate's own write arrives *after*
    /// the write returns and the flag is gone, so the handler reads the gate's
    /// verdict as the page's intent and the control can never be released
    /// again. A time-scoped guard cannot separate our echo from a real change;
    /// only the VALUE can. One write, one echo: the first notification
    /// carrying exactly what we wrote is consumed as ours, and anything else
    /// is the page changing its mind.
    gate_wrote: Cell<Option<bool>>,
    /// Set when a later [`declare_wire_kind`] on the same widget replaces this
    /// declaration.
    ///
    /// Dropping it from the registry is not enough: its `notify::sensitive`
    /// handler stays connected to the widget and holds an `Rc` to it, so
    /// without this flag the RETIRED kind would keep re-deciding the control
    /// every time the page touched its sensitivity — and for a control that
    /// paints several ceremonies, that is precisely the case re-declaring
    /// exists to serve.
    superseded: Cell<bool>,
}

thread_local! {
    /// The lowercase transport word, the same one
    /// `fauna_core::format::connection_state_label` takes.
    ///
    /// Starts `"disconnected"` because that is what the app itself starts as
    /// (`app.rs`'s `connection-status` label is built reading
    /// `common::DISCONNECTED`), so the indicator and the gate agree from the
    /// first frame rather than from the first WS event.
    static STATE: Cell<&'static str> = const { Cell::new("disconnected") };

    /// Every live declaration, **keyed by its widget's address**, so replacing
    /// a widget's declaration is a hash lookup rather than a scan of everything
    /// declared so far (see [`upgrade`] for what that scan cost).
    ///
    /// The key is only an identity hint: an address is reused once the widget
    /// at it has been finalized, so a hit is confirmed by upgrading that one
    /// entry — a stale entry cannot upgrade, and is replaced rather than
    /// inherited from.
    ///
    /// Entries whose widget has been dropped are pruned on the schedule
    /// [`PRUNE_AT`] sets and by [`set_connection_state`] — never per
    /// declaration. A dead entry is inert until then: it gates nothing
    /// ([`apply`] returns on a failed upgrade) and is invisible to
    /// [`declarations`].
    static DECLARED: RefCell<HashMap<usize, Rc<Declaration>>> =
        RefCell::new(HashMap::new());

    /// Prune when the table reaches this size, then set it to twice what
    /// survived. Doubling is what keeps the amortized cost of a declaration
    /// constant: a page that builds N controls pays O(N) upgrades in total
    /// across all its prunes, not O(N) per control.
    static PRUNE_AT: Cell<usize> = const { Cell::new(PRUNE_FLOOR) };
}

/// The smallest table worth scanning — below it, pruning costs more than the
/// dead entries it reclaims.
const PRUNE_FLOOR: usize = 64;

/// Drop entries whose widget is gone, but only once the table has grown past
/// its threshold.
fn prune_if_grown(declared: &mut HashMap<usize, Rc<Declaration>>) {
    if declared.len() < PRUNE_AT.get() {
        return;
    }
    declared.retain(|_, d| upgrade(&d.widget).is_some());
    PRUNE_AT.set((declared.len() * 2).max(PRUNE_FLOOR));
}

/// Declare that `widget` actuates `kind`, and gate it from this moment on.
///
/// `kind` is the wire kind the gesture issues — the same string the nest's
/// `KindRegistry` and `offline_class` table key on. A control that issues
/// nothing over the wire (a local navigation, an expander) simply does not
/// call this; an unregistered kind stays available by the shared rule's own
/// ruling 2, so a typo shows up as a failing test, never as a dead button.
///
/// Where one control paints several ceremonies whose kinds differ, declare it
/// again after the paint decides — the later declaration wins, which is the
/// persistent-tree form of "the paint decides and the gesture carries it".
///
/// # Adding a declaration can turn an existing widget test red
///
/// Measured 2026-08-20 — `settings::recovery_kit`'s `repaint_follows_the_`
/// `projection_not_a_local_match`, red on `origin/main`: a
/// page's own test that asserts a control is ENABLED reads `is_sensitive()`
/// *after* this gate has had its say. The gate's state starts
/// `"disconnected"`, so declaring an `OnlineOnly` kind on a control that some
/// test enables makes that test fail — the gate doing exactly its job, and a
/// test that never pinned the state it was reading through.
///
/// So when a sweep adds declarations to a surface, the surface's own tests are
/// part of the change: a test asserting the page's intent calls
/// `reset_for_test("connected")` first, and one asserting the gate's verdict
/// drives [`set_connection_state`] itself. Leave the thread on
/// `"disconnected"` either way — `walk.rs`'s I6 is vacuous otherwise, and says
/// so out loud. ⚠ The gate that catches this is `just fauna-linux-test-check`
/// (the whole `--bins` suite); an `offline_gate:: walk::` filter runs neither
/// the page's tests nor this trap.
pub fn declare_wire_kind(widget: &impl IsA<gtk::Widget>, kind: &'static str) {
    let widget: gtk::Widget = widget.clone().upcast();
    debug_assert!(
        fauna_protocol::offline_class::offline_class(kind).is_some(),
        "{kind} is not a registered wire kind — the gate reads an unknown kind as \
         available (ruling 2), so a typo here silently ungates this control"
    );
    // Re-declaring the same widget replaces its kind rather than stacking a
    // second verdict on it. Retiring the old entry also has to silence it (see
    // `Declaration::superseded`) and hand over what it knows: the widget's
    // CURRENT sensitivity may be the old gate's verdict rather than the page's
    // intent, so reading it fresh here would let a retired `NeedsNest` verdict
    // masquerade as "the page wanted this disabled" — permanently.
    let key = widget.as_ptr() as usize;
    let inherited = DECLARED.with_borrow_mut(|declared| {
        let previous = declared.remove(&key)?;
        // Confirm before inheriting: this address may belong to a *different*
        // widget now (the old one finalized, glib handed the memory out again),
        // and handing a dead control's bookkeeping to an unrelated one would
        // make the gate act on an intent no page ever expressed. A stale entry
        // cannot upgrade, so it is simply dropped.
        upgrade(&previous.widget)?;
        previous.superseded.set(true);
        Some((
            previous.page_sensitive.get(),
            previous.tooltip_is_ours.get(),
        ))
    });
    let (page_sensitive, tooltip_is_ours) =
        inherited.unwrap_or_else(|| (own_sensitive(&widget), false));
    let declaration = Rc::new(Declaration {
        widget: widget.downgrade(),
        kind,
        page_sensitive: Cell::new(page_sensitive),
        tooltip_is_ours: Cell::new(tooltip_is_ours),
        gate_wrote: Cell::new(None),
        superseded: Cell::new(false),
    });
    DECLARED.with_borrow_mut(|declared| {
        declared.insert(key, Rc::clone(&declaration));
        prune_if_grown(declared);
    });
    {
        let declaration = Rc::clone(&declaration);
        widget.connect_notify_local(Some("sensitive"), move |w, _| {
            if declaration.superseded.get() {
                return;
            }
            let now = own_sensitive(w);
            // Our own write, coming back late — consume it and change nothing.
            // One write, one echo: see `Declaration::gate_wrote` for why this
            // cannot be a flag held across the write.
            if declaration.gate_wrote.get() == Some(now) {
                declaration.gate_wrote.set(None);
                return;
            }
            // The page changed its mind. Record the new intent, then re-decide:
            // enabling a control while offline must not escape the gate.
            declaration.page_sensitive.set(now);
            apply(&declaration, STATE.get());
        });
    }
    apply(&declaration, STATE.get());
}

/// The link's state changed — re-decide every live declaration.
///
/// Called from `app.rs`'s `update_connection_indicator`, the one place linux
/// turns a `WsEvent` into the lowercase state word, so the indicator a user
/// reads and the gate that greys their controls are driven by the same value.
pub fn set_connection_state(state: &'static str) {
    if STATE.replace(state) == state {
        return;
    }
    let live: Vec<Rc<Declaration>> = DECLARED.with_borrow_mut(|declared| {
        // The one pass that must see every declaration anyway, so it is also
        // where a full prune is free.
        declared.retain(|_, d| upgrade(&d.widget).is_some());
        PRUNE_AT.set((declared.len() * 2).max(PRUNE_FLOOR));
        declared.values().cloned().collect()
    });
    for declaration in &live {
        apply(declaration, state);
    }
}

/// The state word the gate is currently deciding against — the walk invariant's,
/// the tests', and the e2e state provider's window onto it. Never read by
/// production paint: production reads the gate's *effect* on the widgets, never
/// the gate's own bookkeeping.
///
/// Gated to the automation builds rather than `#[cfg(test)]` alone because
/// `update_shared_state` publishes it as `fauna_e2e_agent::CONNECTION_KEY` —
/// the observable the cross-app connection barrier waits on. A plain
/// `Cell::get`, so it stays legal on the state path (convention 11 corollary:
/// the e2e state provider does no blocking I/O).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn connection_state() -> &'static str {
    STATE.get()
}

/// A widget's OWN `sensitive` property — deliberately not `is_sensitive()`,
/// whose value also folds in the ancestors': a control inside a
/// momentarily-insensitive container has not had its own intent changed, and
/// recording that as the page's intent would leave it dead after a reconnect.
/// This is also exactly the property `notify::sensitive` reports on.
fn own_sensitive(widget: &gtk::Widget) -> bool {
    widget.property::<bool>("sensitive")
}

fn apply(declaration: &Declaration, state: &'static str) {
    if declaration.superseded.get() {
        return;
    }
    let Some(widget) = upgrade(&declaration.widget) else {
        return;
    };
    let verdict = fauna_protocol::offline_class::affordance(declaration.kind, state);
    let allowed = declaration.page_sensitive.get() && verdict.is_available();

    if own_sensitive(&widget) != allowed {
        declaration.gate_wrote.set(Some(allowed));
        widget.set_sensitive(allowed);
    }
    // The reason belongs beside the affordance the gate itself withheld. A
    // control the page disabled keeps the page's reason; a control the page
    // never gave a tooltip gets ours, and gets it taken back on reconnect.
    if !verdict.is_available() && declaration.page_sensitive.get() {
        if widget.tooltip_text().is_none()
            && let Some(reason) = verdict.reason()
        {
            widget.set_tooltip_text(Some(&reason.resolve(crate::i18n::strings::lookup)));
            declaration.tooltip_is_ours.set(true);
        }
    } else if declaration.tooltip_is_ours.replace(false) {
        widget.set_tooltip_text(None);
    }
}

/// The outgoing window is being torn down: retire every declaration it made.
///
/// A declaration normally leaves the registry when its widget does, but the
/// authenticated window's tree does **not** finalize on `destroy()` — a
/// GTK-rs signal-closure cycle keeps it alive (`main.rs`'s `"reset"` arm
/// says so at length) — so its controls keep upgrading and nothing is ever
/// pruned. Without this, every [`set_connection_state`] re-decided the
/// controls of every window this process had built: each actor change added
/// a whole window's `set_sensitive` cascades to every later link flip, for
/// the life of the process. Measured 2026-09-21 over a whole-suite sweep: a
/// connection-state message cost 2 s on average in the starved windows, and
/// pump ticks carrying them ran up to 13 s
/// (`apps/linux.md` § Message Flow).
///
/// Each retired declaration is also marked superseded, so its
/// `notify::sensitive` handler — still connected to a widget that is still
/// alive — stops re-deciding a control nobody can see.
///
/// Called from `actor_scope::reset_actor_scoped_state`, the funnel every
/// actor change passes through, before the incoming window is built. The
/// state word is left alone: it is the link's, and the link is not this
/// window's to reset.
pub fn retire_outgoing_window() {
    DECLARED.with_borrow_mut(|declared| {
        for d in declared.values() {
            d.superseded.set(true);
        }
        declared.clear();
    });
    PRUNE_AT.set(PRUNE_FLOOR);
}

/// Forget every declaration and start from `state`.
///
/// Test-only. The process has ONE GTK thread (`testid::run_on_gtk_thread`), so
/// every widget test shares these `thread_local!`s; without an explicit reset a
/// test would inherit whatever its predecessor left, and `walk.rs`'s I6 would
/// pass vacuously over a `"connected"` state some earlier body set.
#[cfg(test)]
pub fn reset_for_test(state: &'static str) {
    retire_outgoing_window();
    STATE.set(state);
}

/// Whether `widget` carries a live declaration — the `declares_enabled` field of
/// the agent's whole-frame `/registry` (a control whose enabled state a call
/// site's predicate decides, versus one that merely defaulted). A map lookup,
/// so legal on the automation path (convention 11 corollary).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn is_declared(widget: &gtk::Widget) -> bool {
    DECLARED.with_borrow(|declared| {
        declared
            .get(&(widget.as_ptr() as usize))
            .and_then(|d| upgrade(&d.widget))
            .is_some_and(|live| &live == widget)
    })
}

/// Every live declaration as `(widget name, wire kind, is sensitive)` — what
/// `walk.rs` asserts its invariants over, and the only reader outside this
/// module. The widget name is the element's test id
/// (`crate::testid::set_test_id`), so a failure names the control a human would
/// recognise. Test-only, for the same reason [`connection_state`] is.
#[cfg(test)]
pub fn declarations() -> Vec<(String, &'static str, bool)> {
    DECLARED.with_borrow(|declared| {
        // Sorted by test id: the table is a hash map, and a walk invariant's
        // failure message should not name a different control run to run.
        let mut out: Vec<(String, &'static str, bool)> = declared
            .values()
            .filter_map(|d| {
                let widget = upgrade(&d.widget)?;
                Some((
                    widget.widget_name().to_string(),
                    d.kind,
                    widget.is_sensitive(),
                ))
            })
            .collect();
        out.sort();
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;

    /// A registered kind of `class`, taken from the shared table rather than
    /// hard-coded, so a later reclassification cannot turn these red for the
    /// wrong reason. `offline_class_keyed` hands back the table's own
    /// `&'static str` key, which is what [`declare_wire_kind`] takes.
    fn a_kind_of(class: fauna_protocol::offline_class::OfflineClass) -> &'static str {
        let registry = fauna_protocol::kind::KindRegistry::full();
        let name = registry
            .iter()
            .map(|(name, _)| name.to_string())
            .find(|name| fauna_protocol::offline_class::offline_class(name) == Some(class))
            .unwrap_or_else(|| panic!("no registered kind is classified {class:?}"));
        fauna_protocol::offline_class::offline_class_keyed(&name)
            .expect("just found it in the table")
            .0
    }

    fn online_only() -> &'static str {
        a_kind_of(fauna_protocol::offline_class::OfflineClass::OnlineOnly)
    }

    fn offline_safe() -> &'static str {
        a_kind_of(fauna_protocol::offline_class::OfflineClass::OfflineSafe)
    }

    /// The whole point: a control that cannot work without a nest is not
    /// offered, and says why where the page said nothing.
    #[test]
    fn an_online_only_control_is_withheld_offline_and_carries_the_reason() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            declare_wire_kind(&button, online_only());
            assert!(!button.is_sensitive());
            assert_eq!(
                button.tooltip_text().as_deref(),
                Some(crate::i18n::strings::common::NEEDS_NEST)
            );
            reset_for_test("disconnected");
        });
    }

    /// Ruling 1 across the seam: the classes the outbox exists to carry stay
    /// live, and get no reason text — greying them would contradict W4 itself.
    #[test]
    fn an_offline_capable_control_stays_live_offline() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            declare_wire_kind(&button, offline_safe());
            assert!(button.is_sensitive());
            assert!(button.tooltip_text().is_none());
            reset_for_test("disconnected");
        });
    }

    /// The persistent-tree half tui never has to solve: the widget outlives the
    /// state that gated it, so the link coming back must release it — and take
    /// back the reason it added.
    #[test]
    fn reconnecting_releases_the_control_and_withdraws_the_reason() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            declare_wire_kind(&button, online_only());
            assert!(!button.is_sensitive());

            set_connection_state("connected");
            assert!(button.is_sensitive(), "the link is back; offer it again");
            assert!(
                button.tooltip_text().is_none(),
                "the gate's reason must not outlive the reason"
            );

            set_connection_state("unreachable");
            assert!(!button.is_sensitive(), "a settled failure gates too");
            reset_for_test("disconnected");
        });
    }

    /// The page's own reason is stronger and more specific than "no nest", so
    /// the gate neither overwrites it nor hands the control back on reconnect.
    /// This is tui's early return, in a tree where the release is a real event.
    #[test]
    fn a_control_the_page_disabled_stays_disabled_with_its_own_reason() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            button.set_sensitive(false);
            button.set_tooltip_text(Some("no controlled zone"));
            declare_wire_kind(&button, online_only());

            assert_eq!(button.tooltip_text().as_deref(), Some("no controlled zone"));
            set_connection_state("connected");
            assert!(
                !button.is_sensitive(),
                "the page disabled it; a reconnect must not enable what the page withheld"
            );
            assert_eq!(button.tooltip_text().as_deref(), Some("no controlled zone"));
            reset_for_test("disconnected");
        });
    }

    /// The failure a one-shot pass would have: the page enables the control
    /// LATER, while the link is still down. A gate that ran only at declaration
    /// time would leave it live, and no repaint would ever come to fix it.
    #[test]
    fn a_control_the_page_enables_while_offline_is_re_gated_at_once() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            button.set_sensitive(false);
            declare_wire_kind(&button, online_only());

            button.set_sensitive(true);
            assert!(
                !button.is_sensitive(),
                "the page re-enabled it while offline — the gate must take it straight back"
            );

            set_connection_state("connected");
            assert!(
                button.is_sensitive(),
                "the page's latest intent was ENABLED, so a reconnect must honour that"
            );
            reset_for_test("disconnected");
        });
    }

    /// A page that already explains itself keeps its words; the gate only fills
    /// a silence.
    #[test]
    fn the_gate_never_overwrites_a_tooltip_the_page_wrote() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            button.set_tooltip_text(Some("set up mail first"));
            declare_wire_kind(&button, online_only());

            assert!(!button.is_sensitive());
            assert_eq!(button.tooltip_text().as_deref(), Some("set up mail first"));
            set_connection_state("connected");
            assert_eq!(
                button.tooltip_text().as_deref(),
                Some("set up mail first"),
                "the gate must not clear a tooltip it never wrote"
            );
            reset_for_test("disconnected");
        });
    }

    /// Ruling 3 across the seam, and the reason its polarity is what it is: an
    /// older app meeting a future state word keeps its controls live rather
    /// than greying them on a guess.
    #[test]
    fn an_unknown_state_word_keeps_controls_live() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            declare_wire_kind(&button, online_only());
            assert!(!button.is_sensitive());

            set_connection_state("reticulating");
            assert!(button.is_sensitive());
            reset_for_test("disconnected");
        });
    }

    /// A widget that has been dropped must not keep its declaration alive — a
    /// page rebuild is the normal case here, not an exception.
    #[test]
    fn a_dropped_widget_prunes_itself() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            {
                let button = gtk::Button::new();
                declare_wire_kind(&button, online_only());
                assert_eq!(declarations().len(), 1);
            }
            set_connection_state("connected");
            assert!(
                declarations().is_empty(),
                "the widget is gone; its declaration must not survive it"
            );
            reset_for_test("disconnected");
        });
    }

    /// **Declaring a control costs a FIXED number of weak-ref upgrades, not one
    /// per control already declared.**
    ///
    /// This is the registry's load-bearing performance contract, and it is
    /// asserted as a count rather than a duration precisely so it is exact on
    /// any machine (`e2e-conventions.md` convention 14 — a stopwatch here would
    /// be the brittleness that convention forbids).
    ///
    /// It exists because the quadratic version shipped and cost the linux suite
    /// dearly: each declaration scanned every live declaration and upgraded its
    /// `WeakRef`, and every upgrade takes glib's process-global pointer lock. A
    /// feed of post cards declares hundreds of controls, so rendering one burned
    /// a core for 25 s+ — with the GTK main loop held the whole time, which made
    /// the app unable to answer the e2e agent at all. In the 2026-09-10
    /// whole-suite sweep that surfaced as **110 of the 205 failures**, each
    /// wearing the clothes of whatever the test was reading when the loop
    /// stopped, and the stall's own captured stack
    /// named `declare_wire_kind` in seven of seven samples.
    #[test]
    fn declaring_a_control_does_not_scan_every_other_declaration() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            const CONTROLS: usize = 200;
            // Held so nothing is pruned: the expensive case is a page whose
            // controls are all still alive, which is every page being built.
            let mut kept: Vec<gtk::Button> = Vec::with_capacity(CONTROLS);
            UPGRADES.with(|u| u.set(0));
            for _ in 0..CONTROLS {
                let button = gtk::Button::new();
                declare_wire_kind(&button, online_only());
                kept.push(button);
            }
            let upgrades = UPGRADES.with(|u| u.get());
            // A generous constant per declaration, deliberately: the exact
            // number is an implementation detail, while the SHAPE is the
            // contract. Quadratic would be ~20_000 here (200²/2) and grow with
            // the square of any page that gets bigger.
            let ceiling = (CONTROLS * 8) as u64;
            // Printed, not just asserted: the number is the evidence a future
            // reader needs to see that the shape is still linear (it was
            // ~20_000 here — CONTROLS²/2 — before the registry was keyed).
            eprintln!("[offline-gate] {CONTROLS} declarations took {upgrades} weak-ref upgrades");
            assert!(
                upgrades <= ceiling,
                "declaring {CONTROLS} controls took {upgrades} weak-ref upgrades \
                 (ceiling {ceiling}): the registry is scanning what it already holds, \
                 which is the quadratic shape that stalled the GTK main loop"
            );
            drop(kept);
            reset_for_test("disconnected");
        });
    }

    /// **A link flip re-decides only the live window's controls** — counted in
    /// weak-ref upgrades, not timed (convention 14).
    ///
    /// The authenticated window's tree survives `destroy()` (a GTK-rs
    /// signal-closure cycle), so its controls stay upgradeable and would never
    /// be pruned: without [`retire_outgoing_window`] at each actor change, every
    /// flip re-decided every window built since launch. Measured 2026-09-21 as
    /// 2 s per connection-state message and pump ticks of up to 13 s in a
    /// whole-suite sweep (`apps/linux.md` § Message Flow). The outgoing
    /// controls are HELD here, exactly as the leak holds them.
    #[test]
    fn an_actor_change_retires_the_outgoing_windows_controls() {
        run_on_gtk_thread(|| {
            reset_for_test("connected");
            let declared = |n: usize| -> Vec<gtk::Button> {
                (0..n)
                    .map(|_| {
                        let button = gtk::Button::new();
                        declare_wire_kind(&button, online_only());
                        button
                    })
                    .collect()
            };
            let outgoing = declared(100);
            retire_outgoing_window();
            let incoming = declared(10);

            UPGRADES.with(|u| u.set(0));
            set_connection_state("disconnected");
            let upgrades = UPGRADES.with(|u| u.get());

            assert!(
                incoming.iter().all(|b| !b.is_sensitive()),
                "the live window's controls are still gated"
            );
            assert!(
                outgoing.iter().all(|b| b.is_sensitive()),
                "the retired window's controls must not be re-decided"
            );
            // The retain and the re-decide each upgrade a live declaration
            // once: two per LIVE control. Unretired, this was 220.
            let ceiling = (incoming.len() * 2) as u64;
            assert!(
                upgrades <= ceiling,
                "one flip took {upgrades} weak-ref upgrades (ceiling {ceiling}): \
                 it is re-deciding a torn-down window's controls"
            );
            // A retired declaration's `notify::sensitive` handler is still
            // connected to its (live) widget; it must not wake up either.
            outgoing[0].set_sensitive(false);
            outgoing[0].set_sensitive(true);
            assert!(outgoing[0].is_sensitive());
            reset_for_test("disconnected");
        });
    }

    /// Re-declaring the same widget replaces its kind — the persistent-tree
    /// form of "the paint decides and the gesture carries it" (the tui sweep's
    /// ruling (iv), for a control that serves several ceremonies).
    #[test]
    fn re_declaring_a_widget_replaces_its_kind() {
        run_on_gtk_thread(|| {
            reset_for_test("disconnected");
            let button = gtk::Button::new();
            declare_wire_kind(&button, online_only());
            assert!(!button.is_sensitive());

            declare_wire_kind(&button, offline_safe());
            assert_eq!(declarations().len(), 1, "one widget, one verdict");
            assert!(
                button.is_sensitive(),
                "the paint chose an offline-capable ceremony; the old verdict must not linger"
            );

            // Dropping the old entry from the registry is not enough — its
            // `notify::sensitive` handler is still connected to this widget.
            // Anything that makes the page touch sensitivity afterwards is what
            // wakes the retired kind up, so drive exactly that.
            button.set_sensitive(false);
            button.set_sensitive(true);
            assert!(
                button.is_sensitive(),
                "the retired OnlineOnly declaration is still deciding this control"
            );
            reset_for_test("disconnected");
        });
    }
}
