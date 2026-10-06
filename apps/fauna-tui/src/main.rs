//! `fauna-tui` — the seventh fauna app: a full-screen terminal UI.
//!
//! Target architecture: `docs/goal/architecture/apps/tui.md` — a single
//! monolithic binary (ratatui + crossterm + tokio) with the `fauna-*` crates
//! as direct Cargo dependencies, the Linux-monolith pattern minus GTK. Shell
//! (M0) + in-process e2e automation agent (M1) are built; login (M2) and the
//! pages follow in later milestones (tracked internally).

mod account_scope;
mod address_book;
mod admin;
mod app;
mod archive_glue;
mod automation;
mod backup_audit;
mod backups;
mod bridges;
mod contacts;
mod content_policy;
mod conversations;
mod critical_alerts;
mod custody_glue;
mod document;
mod drafts_autosave;
mod element;
mod events;
mod family;
mod feature_editor;
mod feed;
mod format;
mod graphics;
mod image_cache;
mod launch;
mod locked;
mod mail_glue;
mod media;
mod media_handoff;
mod moderation;
mod nostr;
mod notifications;
/// The T1 body-rendered browse trigger's app half: which records this frame
/// actually showed a body for (`account-data-plane.md` § The replica boundary).
mod observation;
/// The co-present offline share-initiation ceremony's tui shell
/// (`p2p.md` § Offline share initiation — tui leads the affordance).
#[cfg(feature = "p2p-share")]
mod offline_share;
mod os_notify;
mod os_open;
mod pages;
mod press;
mod profile;
mod push;
mod recovery;
mod region;
mod remote_image;
mod report;
mod routes;
mod screen_lock;
mod search;
mod session;
mod settings;
/// The share plane's app glue (`p2p.md` § Cross-user shared-set transfer,
/// row 58 — tui leads): the store-ready bind, the pump loop, the durable
/// endpoints sink, and the transfer surface's state cell.
#[cfg(feature = "p2p-share")]
mod share_glue;
mod subscriptions_author;
mod sync_agent;
#[cfg(test)]
mod test_support;
mod thumbnail;
mod ui;
mod unlock;
#[cfg(test)]
mod walk;
mod wizard;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyEventKind, MouseButton,
    MouseEventKind,
};
use fauna_onboarding_machine::OnboardingStep;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::app::{App, DataMessage, UiMessage};
use crate::automation::{Agent, AgentRequest, Registry};

/// Is this process under e2e automation? tui's one spelling of the question,
/// the twin of `fauna-desktop`'s `e2e_mode_enabled` (`apps/fauna-linux/src/main.rs`).
///
/// This is convention 15's **inner** switch — it picks agent-on vs agent-off
/// *within* a test-capable build — and never the boundary. The boundary is the
/// `#[cfg]` every caller also carries, or the production twin below where a
/// caller is plumbing the app compiles unconditionally
/// (`e2e-automation-surface-gating.md` § The convention, the Rust-apps bullet).
/// Two gates, both required, exactly as `conversations::init` already spells it.
///
/// **Why one crate-level fn and not a local copy per module.** tui grew three
/// spellings of this question — `sync_agent::e2e_gated`, `conversations::e2e_mode`,
/// and the ungated env read in `backups::download_dir` that shipped the seed into
/// the release binary. Three spellings is how the
/// third one came to have no gate at all: nothing named the rule in one place,
/// so a new reader was written from scratch instead of from the pattern.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn e2e_mode_enabled() -> bool {
    std::env::var_os("FAUNA_E2E_AGENT_PORT").is_some()
}

/// Production twin: a shipped app is never under e2e automation.
///
/// It exists for the caller that cannot carry a `#[cfg]` of its own —
/// `sync_agent`'s spawner selection, which is plumbing every flavor compiles —
/// and it is what keeps `FAUNA_E2E_AGENT_PORT` from being *named* in a release
/// binary at all.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub fn e2e_mode_enabled() -> bool {
    false
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Install the shared `fauna_log` subscriber: the in-memory ring (which the
    // Settings → Logs page renders — observability.md § Surfaces) + a rolling
    // on-disk file + stderr. The ring is always installed; stderr is suppressed
    // while a live tui owns the alternate screen (painting over it corrupts the
    // display) and kept under the e2e driver / any `2>file` launch, where stderr
    // is `app.err` — the one debugging surface this client has. Before
    // `ratatui::init()`, so the stderr decision sees the real terminal. Without
    // it the whole client's `tracing::*` calls would go nowhere.
    session::install_logging();
    // The external-media handoff dir is swept at app start AND app exit
    // (`apps/tui.md` § External media handoff — a crash leaves plaintext
    // only until the next launch; the exit sweep bounds the normal case).
    media_handoff::sweep();
    // Raw mode + alternate screen; the ratatui init helpers install a panic
    // hook that restores the terminal before the panic message prints.
    let terminal = ratatui::init();
    // Which graphics protocol paints thumbnails (`apps/tui.md` § Rendering —
    // auto-detected, never configured).
    //
    // **Here, and only here.** Detection asks the terminal a question and reads
    // its answer back off the terminal (a `/dev/tty` descriptor on unix, the
    // console API on windows — one probe, two transports), so it needs raw mode
    // already on (`ratatui::init()`, just above) and it must happen before
    // anything else reads stdin — the crossterm `EventStream` inside `run`
    // swallows exactly this reply. See `graphics::detect::probe_da1`.
    let protocol = graphics::detect();
    // Mouse reporting, for real-terminal click support (`apps/tui.md` §
    // Architecture: "mouse support where the terminal offers it") — after
    // `graphics::detect()`, which needs an untouched tty to read the DA1 reply
    // off. `ratatui::init()`'s own panic hook restores raw mode/alt-screen but
    // knows nothing about a mode this fn turns on afterward, so a panic mid-click
    // would otherwise leave the terminal reporting mouse escapes at the next
    // shell prompt; chaining onto the existing hook closes that gap.
    let previous_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        previous_panic_hook(info);
    }));
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    // Convention 11's UI-thread heartbeat: the loop's stamp is beaten whenever
    // the loop yields, so an agent timeout can tell a waiting loop from a held
    // one (`automation::beat_while_pending` — wrapped around the loop, not an
    // arm inside it, because tui's arms await their ops inline). Only with an
    // agent; a real user's loop is awaited untouched.
    let result =
        automation::beat_while_pending(automation::loop_heartbeat(), run(terminal, protocol)).await;
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    // Erase protocol graphics while the alternate screen is still up — they
    // live outside its buffer, so `restore()` alone does not take them and the
    // last thumbnail can survive onto the returning shell prompt. Ordering is
    // the whole contract; see `graphics::Painter::scrub`.
    let _ = graphics::Painter::scrub(protocol, &mut std::io::stdout());
    ratatui::restore();
    media_handoff::sweep();
    result
}

async fn run(
    mut terminal: ratatui::DefaultTerminal,
    protocol: graphics::Protocol,
) -> std::io::Result<()> {
    // Backend → UI channel (linux.md § Message Flow). `tx` stays alive here
    // as the prototype every backend task clones; if it ever fully drops,
    // `recv` would yield `None` forever and spin the select loop.
    let (tx, mut rx) = mpsc::unbounded_channel::<UiMessage>();

    // Signed-out steady state, through the real channel path; a restored or
    // patched-in session replaces it via the supervisor watch pump.
    tx.send(UiMessage::Data(DataMessage::ConnectionState(
        fauna_ws_substrate::supervisor::ConnectionState::Disconnected,
    )))
    .expect("receiver is alive");

    // In-process e2e automation agent, gated on FAUNA_E2E_AGENT_PORT.
    // `start_if_enabled` is a compiled-out no-op in release builds without
    // `e2e-agent` — it never binds a port there (testing.md convention 15).
    let mut agent = automation::start_if_enabled();
    let mut registry = Registry::default();

    let mut events = EventStream::new();
    let mut app = App::new(&tx);
    // A `fauna://` route on the command line (`apps/tui.md` § System
    // integration → *In-app routes*), parsed once and held until the session
    // is signed in — the loop below applies it.
    app.pending_route = routes::first_route_arg(std::env::args());

    // Install the disk-backed nest-identity pin store before the first
    // authenticated connect, so TOFU pins survive restarts (`security.md`
    // § Transport trust) rather than living in the volatile
    // `MemoryPinStore` default. Must precede `launch::start`, which runs the
    // silent challenge that consults the pin.
    session::install_disk_pin_store();

    // App-launch routing (`onboarding.md` § App-launch routing): hand the
    // long-term store to the shared LaunchMachine and paint the "Signing you
    // in…" surface until its snapshot lands on the UI channel. It decides
    // between the authenticated shell, a seeded wizard entry, and the
    // `launch_retry` surface — this client never re-derives that branch.
    //
    // One gate first: when the credential store resolved to the sealed
    // headless backend (`tui.md` § Credential storage) the unlock/create
    // surface owns the screen instead, and `launch::start` runs on its
    // successful submit — routing before the store can serve reads would
    // classify a stored identity as "no identity" and land on onboarding.
    app.route_locked_store(&tx);

    // Emits inline images after each frame, on whichever arm the terminal
    // supports; a no-op on the half-block arm, which paints as ordinary text.
    let mut painter = graphics::Painter::new(protocol);
    let mut stdout = std::io::stdout();
    // Synthesizes a double press out of the plain `Down`s a terminal delivers,
    // and arbitrates it against the single press for the one element family
    // that defines both (`press` — the month day cell).
    let mut presses = press::Arbiter::new(press::DOUBLE_PRESS_WINDOW);

    // Built ONCE, outside the select loop: `periodic_tick`'s doc comment above
    // is the "why" (a period rebuilt fresh inside the loop restarts
    // on every unrelated event). `MissedTickBehavior::Delay` means a period
    // this app was too busy to poll on time schedules its NEXT fire from now,
    // rather than bursting through every tick it missed.
    let mut dns_poll = tokio::time::interval(wizard::awaiting_manual_dns::POLL_INTERVAL);
    dns_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut invite_poll = tokio::time::interval(std::time::Duration::from_millis(
        fauna_onboarding_machine::INVITE_RECHECK_POLL_MS,
    ));
    invite_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut lock_tick = tokio::time::interval(screen_lock::LOCK_TICK);
    lock_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The mail-import Progress screen's live-refresh poll
    // (`settings::mail_import_progress_active`'s doc comment): `run_import`
    // mutates the machine's own snapshot in a background task, and nothing
    // else in this loop repaints while the user sits idle watching it.
    let mut mail_import_poll = tokio::time::interval(std::time::Duration::from_secs(1));
    mail_import_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The mail-export Progress screen's live-refresh poll — the mail-import
    // poll's twin, and for the same reason: `run_export` mutates the machine's
    // own snapshot in a background task while the user sits watching it.
    let mut mail_export_poll = tokio::time::interval(std::time::Duration::from_secs(1));
    mail_export_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The archive-import Progress screen's live-refresh poll — the mail-import
    // poll's twin, and for the same reason: `run_import` mutates the machine's
    // own snapshot in a background task while the user sits watching it.
    let mut archive_import_poll = tokio::time::interval(std::time::Duration::from_secs(1));
    archive_import_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The engagement-cue sampling tick (`feed::cues`): dwell must keep accruing
    // while the user sits still on a card, and nothing else redraws then.
    let mut cue_tick = tokio::time::interval(feed::cues::SAMPLE_INTERVAL);
    cue_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The blessed-grant auto-renew loop's tick (`ui/nests.md` § Expiry /
    // renewal → *Duration and blessing*): app-wide, not page-scoped — a blessed
    // box's trust must renew whichever page the user sits on. An `Interval`'s
    // first tick is immediate, so the loop also runs as the app comes up.
    let mut nests_renew_tick = tokio::time::interval(std::time::Duration::from_secs(
        fauna_client_capabilities::view_model::AUTO_RENEW_CHECK_SECS,
    ));
    nests_renew_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A command whose apply started a teardown (`reset`, `logout`, a `session`
    // patch that signs out or switches): its ack waits here until the stop and
    // everything queued behind it have run (`App::teardown_pending`), so the
    // next test's login can never race the erase — linux's `reset_after_stop`.
    let mut deferred_ack: Option<(String, Option<String>)> = None;

    while !app.should_quit {
        // Backstop for the persist-failure-message nav-edge guard
        // (`App::sync_recovery_message_nav_edge`'s doc comment): `handle_key`
        // and `gesture_work` already call this on every keyboard/mouse/agent
        // actuation, but a few programmatic transitions (an account switch's
        // teardown, `clear_session`) move `(page, sub)` with no shared door at
        // all. Running it once more here, before every draw, means nothing
        // reaches the screen between a genuine nav-away and its discharge.
        app.sync_recovery_message_nav_edge();
        // The held `fauna://` route, applied the first pass a signed-in session
        // owns the screen (`App::apply_route` is the one door; the loop is its
        // spawn half).
        if let Some(op) = app.apply_pending_route() {
            app.spawn_page_op(op);
        }
        // `draw` hands the callback a `Frame` and keeps nothing afterwards, so
        // the placements and hit regions come back out through the closure —
        // they are the only record of where the images (and this frame's
        // clickable rows) landed (`ui::render`).
        let mut drawn: Option<ui::DrawnFrame> = None;
        terminal.draw(|frame| {
            drawn = Some(ui::render(frame, &app));
        })?;
        let ui::DrawnFrame {
            placements,
            hits,
            page,
        } = drawn.expect("terminal.draw runs its closure exactly once");
        // The post-paint seam (`apps/tui.md` § Rendering): protocol graphics
        // are escape sequences at a cell rect, which the `Line` model cannot
        // carry — so they go straight to stdout, after ratatui has finished with
        // the frame and flushed it.
        painter.paint(placements, &mut stdout)?;
        // The engagement-cue probe, from the page this frame painted — the only
        // place that knows where each feed card landed in the viewport
        // (`feed::cues` owns why). Every draw is a sample: a focus move always
        // redraws, which is the extra sample on scroll the capture contract asks
        // for, and `cue_tick` below keeps dwell accruing while the user holds
        // still.
        feed::cues::sample_frame(&mut app, &page, &tx);
        // The T1 report, from the one place that knows what is on screen: the
        // frame's own hit regions, which exist only for rows the viewport
        // painted (`crate::observation` owns why this cannot live anywhere
        // else). Cheap on the steady state — a re-render of an already-recorded
        // body publishes nothing — so it runs every frame rather than trying to
        // remember what it already sent.
        if let Some(store) = app.settings.account_store.as_ref() {
            observation::report(store, observation::observed_this_frame(&app, &hits));
        }
        if let Some(agent) = &agent {
            // Rebuild the per-frame registry + publish e2e state after every
            // draw, so the agent always answers from the current frame — the
            // page half exactly as it painted, geometry and all.
            ui::register_frame(&app, &mut registry, &page);
            // The frame just painted is the one the painted-error tally reads —
            // registered and painted through one door, so a tallied error is an
            // error the user saw (`fauna_e2e_agent::PAINTED_ERRORS_KEY`).
            app.painted_errors.observe(registry.texts());
            agent.publish(&app, None);
        }

        tokio::select! {
            event = events.next() => match event {
                // Key-press only: on Windows-style terminals crossterm also
                // reports Release/Repeat, which would double-navigate.
                // Refused while a teardown's stop runs (`App::teardown_pending`):
                // the outgoing session is being torn down under the frame, and
                // a quit now would leave a sign-out that never erased. Dropped,
                // not buffered — a keystroke meant for the old screen must not
                // land on the next one.
                Some(Ok(Event::Key(_))) if app.teardown_pending() => {}
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                    app.handle_key(key);
                }
                // A left click actuates whatever this frame's hit-test map says
                // is under the cursor — the mouse counterpart of `Enter` on the
                // focus ring (`App::click_sidebar`/`click_page_element`, which
                // both route through the same `actuate_focused` the key handler
                // uses). Anything else (drag, release, scroll wheel, a click that
                // misses every painted row) is a no-op.
                Some(Ok(Event::Mouse(m)))
                    if m.kind == MouseEventKind::Down(MouseButton::Left)
                        && !app.teardown_pending() =>
                {
                    // **The wall clock stops here.** The automation agent's
                    // `double_click` runs the element's second gesture directly
                    // and never comes through this path, so no test is ever
                    // asked to beat this threshold (`testing.md` convention 14)
                    // — the decision logic itself is pinned by `press`'s own
                    // tier_1 tests with synthetic instants.
                    press::left_press(
                        &mut app,
                        &mut presses,
                        &hits,
                        m.column,
                        m.row,
                        std::time::Instant::now(),
                    );
                }
                // Resize/other-mouse/focus need no state change — the redraw at
                // the top of the loop handles them.
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err),
                // Terminal input closed under us: nothing can ever reach the
                // UI again, so exit rather than spin.
                None => break,
            },
            msg = rx.recv() => match msg {
                Some(msg) => app.handle_message(msg),
                None => unreachable!("prototype sender lives for the whole loop"),
            },
            // The client owns the poll cadence; the machine's `recheck_manual_dns`
            // is single-shot (`onboarding.md` § "Almost ready" surface). Spawned,
            // not awaited: a probe against a nest whose DNS hasn't propagated can
            // hang for the connect timeout, and the TUI must stay responsive —
            // the observer tick repaints when it lands. The surface also paints
            // an explicit `awaiting-dns-recheck-button` — the button is the
            // deterministic probe, this tick is the unattended one.
            _ = periodic_tick(&mut dns_poll, app.wizard.is_awaiting_manual_dns()) => {
                wizard::spawn_action(
                    std::sync::Arc::clone(&app.wizard.machine),
                    session::confirm_identity_sink(&app),
                    wizard::Action::RecheckManualDns,
                    String::new(),
                );
            }
            // The pending-invite poll — the same shape, and for the same reason:
            // approval reaches an unregistered actor through no push channel, so
            // the client asks (`onboarding.md` § The pending-invite surface —
            // "Poll is the channel, structurally"). The recheck resolves an
            // approval into `LoggedIn` by itself, and the `Done` handler below
            // then lands the user in the app with no user action at all.
            _ = periodic_tick(&mut invite_poll, app.wizard.is_pending_invite_review()) => {
                wizard::spawn_action(
                    std::sync::Arc::clone(&app.wizard.machine),
                    session::confirm_identity_sink(&app),
                    wizard::Action::RecheckInviteStatus,
                    String::new(),
                );
            }
            // A held first press whose double-press window closed: it was a
            // single press after all, so its own gesture runs now (`press`).
            _ = press_hold(&presses) => {
                press::expire(&mut app, &mut presses, std::time::Instant::now());
            }
            // The ward's screen-time tick (`family-safety.md` § Screen time).
            // Time passes continuously while the policy only changes on a status
            // read, so without this a ward already in the app would sail past
            // their bedtime — the redraw at the top of the loop re-asks
            // `page_elements`, which re-evaluates the lock.
            //
            // The same tick drives the usage heartbeat, which is why the
            // interval must stay at or under `MAX_ACCRUAL_STEP_SECS`: the shared
            // engine credits at most one step per call, so a slower tick would
            // silently under-count. That argument survives this fix unchanged —
            // the engine accrues from the measured gap between calls, not a tick
            // count — but the direction "slower" could drift no longer includes
            // "arbitrarily, under constant activity": before row 244, `lock_tick`
            // rebuilt its `sleep()` fresh every spin and could starve
            // indefinitely while the app stayed busy; it is now a persistent
            // `Interval`, so the measured gap can no longer exceed `LOCK_TICK`
            // no matter how busy the app is. Spawned rather than awaited, like
            // the DNS recheck above — a heartbeat against an unreachable nest
            // must never make the TUI stop redrawing.
            _ = periodic_tick(&mut lock_tick, app.authenticated() && !app.showing_launch_surface()) => {
                // The same tick also flushes Guardian Notify (`family-safety.md`
                // § Guardian Notify). Both legs are asked every minute and both
                // gate themselves — the heartbeat on the shared engine's cadence,
                // Notify on its ≤hourly batch — so this arm never needs to know
                // which pillar is armed.
                // …and the region relay refresh, on the shared cadence
                // (`fauna_core::region_authority::REFRESH_INTERVAL_SECS`), which
                // gates itself the same way.
                region::refresh_if_due(&mut app, &tx);
                let due = [
                    family::due_usage_report(&mut app, true),
                    family::due_notify_report(&mut app),
                ];
                for op in due.into_iter().flatten() {
                    let tx = tx.clone();
                    let generation = app.session_generation;
                    tokio::spawn(async move {
                        let outcome = op.run().await;
                        let _ = tx.send(app::UiMessage::Data(app::DataMessage::Page(
                            generation,
                            app::PageOutcome::Family(outcome),
                        )));
                    });
                }
            }
            // The mail-import Progress screen's live-refresh poll — see
            // `settings::mail_import_progress_active`'s doc comment for why a
            // page-scoped tick is the only thing that can repaint this screen
            // while the user sits idle watching a run in progress.
            _ = periodic_tick(
                &mut mail_import_poll,
                crate::settings::mail_import_progress_active(&app.settings),
            ) => {
                if let Some(machine) = crate::settings::mail_import_machine(&app.settings) {
                    crate::settings::spawn_mail_import_progress_refresh(
                        machine,
                        &tx,
                        app.session_generation,
                    );
                }
            }
            // The mail-export Progress screen's live-refresh poll — the arm
            // directly above, over the export wizard's own machine.
            _ = periodic_tick(
                &mut mail_export_poll,
                crate::settings::mail_export_progress_active(&app.settings),
            ) => {
                if let Some(machine) = crate::settings::mail_export_machine(&app.settings) {
                    crate::settings::spawn_mail_export_progress_refresh(
                        machine,
                        &tx,
                        app.session_generation,
                    );
                }
            }
            // The engagement-cue tick — wakes the loop so the next draw samples.
            // Nothing to do in the arm itself: the sample rides the redraw at
            // the top of the loop, which is also what keeps it measuring the
            // frame the user actually sees. Gated on the post list having been
            // on screen at the last sample, so no other page is ever woken.
            _ = periodic_tick(&mut cue_tick, app.feed.cue_capture.showing()) => {}
            // The auto-renew tick — spawned, like the heartbeat above, so an
            // unreachable nest never stalls a redraw.
            _ = periodic_tick(
                &mut nests_renew_tick,
                app.authenticated()
                    && !app.showing_launch_surface()
                    && crate::settings::nests_machine(&app.settings).is_some(),
            ) => {
                if let Some(machine) = crate::settings::nests_machine(&app.settings) {
                    crate::settings::spawn_nests_auto_renew(machine, &tx, app.session_generation);
                }
            }
            // The archive-import Progress screen's live-refresh poll — the arm
            // directly above, over the archive wizard's own machine.
            _ = periodic_tick(
                &mut archive_import_poll,
                crate::settings::archive_import_progress_active(&app.settings),
            ) => {
                if let Some(machine) = crate::settings::archive_import_machine(&app.settings) {
                    crate::settings::spawn_archive_import_progress_refresh(
                        machine,
                        &tx,
                        app.session_generation,
                    );
                }
            }
            // Not even received while a teardown is under way: the driver's
            // next request waits in the channel, so nothing it does can act
            // on the outgoing session or overtake the erase — the automation
            // half of the input the key arm refuses.
            req = recv_agent(&mut agent), if !app.teardown_pending() => match req {
                AgentRequest::Element(op) => {
                    // Awaits the machine call behind a wizard click, so the
                    // driver's next (single-shot, un-retried) element read
                    // already observes the new step.
                    let reply = automation::perform(&mut app, &registry, &op.req).await;
                    // A wizard gesture may have finished the whole wizard —
                    // including an append-mode "Add account" over a live session
                    // (`showing_launch_surface()` covers both the no-session onboard
                    // and `adding_account`).
                    // Never while the retire page is up: it is hosted by the
                    // wizard but is not the wizard's journey.
                    if app.showing_launch_surface()
                        && app.wizard.retire.is_none()
                        && app.wizard.machine.step() == OnboardingStep::Done
                    {
                        wizard::handle_wizard_done(&mut app, &tx);
                    }
                    // The pending-invite journey never reaches `Done` (it stays
                    // on `invite_request` and polls), so its resume slot is
                    // written here rather than by the handler above.
                    wizard::persist_pending_invite_slot(&mut app);
                    if let Some(agent) = &agent {
                        // Republish BEFORE replying: an actuation may have
                        // mutated the app, and the driver reads /app/state the
                        // moment the reply lands — publishing only on the next
                        // draw would serve it the pre-click snapshot.
                        agent.publish(&app, None);
                    }
                    // A dropped reply just means the server timed the op out.
                    let _ = op.reply.send(reply);
                }
                AgentRequest::Command { id, action, state, method, json_arg, posts, payload } => {
                    let result = automation::apply_command(
                        &mut app, &tx, &action, &state, &method, &json_arg, &posts, &payload,
                    )
                    .await;
                    // `barrier`'s actual work, and it must happen BEFORE the ack
                    // below: drain everything already queued on the UI channel,
                    // so the ack means "nothing enqueued before this command is
                    // still pending" rather than merely "this command applied".
                    // It lives here and not in `apply_command` because `rx` is
                    // owned by this loop. See `automation::drain_pending_ui_messages`
                    // for why `select!`'s random branch order makes this necessary.
                    // Also true for a FUSED `barrier_probe`, which enqueues its
                    // batch inside `apply_command` just above and then barriers
                    // here — one round trip, so no inter-command gap can drain
                    // the channel on the barrier's behalf
                    // (`fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD`).
                    if fauna_e2e_agent::command_needs_barrier(&action, &payload) {
                        let applied = automation::drain_pending_ui_messages(&mut app, &mut rx);
                        tracing::debug!("[agent] barrier drained {applied} queued ui message(s)");
                        // Freeze what the barrier saw, AFTER the drain and before
                        // the ack below. The live `barrier_probe` keeps moving as
                        // the loop redraws and republishes, so only this frozen
                        // copy can tell the driver what was true at the ack —
                        // see `fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`.
                        app.barrier_ack_probe = app.barrier_probe.clone();
                    }
                    if !result.recognized {
                        // Acked but not understood. `apply_command` has already
                        // put the refusal on the app's own `error-message`
                        // (testing.md convention 11) — a log line alone is silent
                        // to a driver, which reads a green ack as "the app did
                        // what I asked". This warn is the human-facing half.
                        tracing::warn!("[agent] command {action:?} not fully recognized");
                    }
                    // Ack AFTER the apply — same ordering discipline as
                    // linux's SharedState / apple's ready=false window — and,
                    // when the apply started a teardown, after the stop too.
                    deferred_ack = Some((id, result.machine_result));
                }
            },
        }

        if !app.teardown_pending()
            && let Some((id, machine_result)) = deferred_ack.take()
            && let Some(agent) = &agent
        {
            // Stash the reader value before the ack, so the driver sees the
            // result for *this* command.
            agent.set_machine_result(machine_result);
            agent.publish(&app, Some(&id));
        }

        // The keyboard path spawns wizard actions, so the machine reaches
        // `Done` on an observer tick rather than inside an arm. `handle_wizard_done`
        // is idempotent — after it runs, the wizard resets (step != Done) and, in
        // append mode, `adding_account` clears, so `showing_launch_surface()` +
        // step==Done makes the Element arm's earlier call a no-op here.
        if app.showing_launch_surface()
            && app.wizard.retire.is_none()
            && app.wizard.machine.step() == OnboardingStep::Done
        {
            wizard::handle_wizard_done(&mut app, &tx);
        }
        // Same reason as the Element arm above: `PendingReview` is not a `Done`
        // state, so the slot write cannot ride the outcome handler. Idempotent
        // per request id.
        wizard::persist_pending_invite_slot(&mut app);
    }

    flush_drafts_on_quit(&app).await;
    // The engagement-cue rollup's close flush (`engagement-cues.md` § At rest:
    // put "on background/close"): the card on screen at quit is an exposure in
    // progress, and the debounce may be holding back the latest verdicts.
    // Bounded like the drafts rails above.
    feed::cues::flush_on_quit(&mut app, std::time::Duration::from_secs(3)).await;

    Ok(())
}

/// The leave-flush leg of `drafts-survive` outcome 5
/// (`reserved-folders.md` § The leave-flush promise, row 481): force an
/// immediate save of every rail's current compose, bypassing the debounce,
/// right before the process exits. tui runs natively inside the tokio
/// runtime for its whole life, so this is a direct `.await` — no
/// thread-spinning the way linux's GTK main thread needs
/// (`blocking_flush`-equivalent has no reason to exist here).
///
/// Each rail is independently bounded: a slow/unreachable nest must not hang
/// quitting the app, and one rail's timeout must not cost another rail its
/// own flush.
async fn flush_drafts_on_quit(app: &App) {
    const LEAVE_FLUSH_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

    if let (Some(manager), Some(sync)) =
        (&app.conversations.manager, &app.conversations.drafts_sync)
    {
        let _ = tokio::time::timeout(
            LEAVE_FLUSH_BUDGET,
            drafts_autosave::flush_now(manager.as_ref(), sync, "drafts"),
        )
        .await;
    }
    if let (Some(manager), Some(sync)) = (&app.feed.manager, &app.feed.drafts_sync) {
        let _ = tokio::time::timeout(
            LEAVE_FLUSH_BUDGET,
            drafts_autosave::flush_now(manager.as_ref(), sync, "feed drafts"),
        )
        .await;
    }
    if let Some(sync) = &app.events.drafts_sync {
        let draft = app.events.event_draft();
        let _ =
            tokio::time::timeout(LEAVE_FLUSH_BUDGET, events::drafts::flush_now(sync, &draft)).await;
    }
}

/// Wake when a held first press's double-press window closes; pend forever with
/// nothing held, so the arm never wakes an idle TUI.
///
/// An **absolute** deadline: this arm is re-created on every loop spin (a
/// fresh call to `press_hold`), so a *relative* sleep would restart the window
/// each time an unrelated event arrived — a user pressing a day cell and then
/// resizing their terminal would never see the drill-in. `arbiter.deadline()`
/// reads a value stored on `presses`, owned outside this fn, which is what
/// makes re-creating the call safe every spin — the same shape `periodic_tick`
/// below uses via `Interval` instead of a hand-rolled deadline (the
/// three ticks below it used to `sleep()` a *relative* duration rebuilt fresh
/// each spin, which — unlike this fn — really did restart on every event and
/// could starve indefinitely under constant activity).
async fn press_hold(arbiter: &press::Arbiter) {
    match arbiter.deadline() {
        Some(due) => tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await,
        None => std::future::pending::<()>().await,
    }
}

/// Fire once per `tick`'s period while `active`; pend forever otherwise, so a
/// page-scoped tick never wakes an idle TUI sitting on an unrelated screen.
///
/// `tick: &mut Interval` is owned by the caller *outside* the select loop
/// (`run`'s `dns_poll`/`invite_poll`/`lock_tick`), which is the whole fix: the
/// next deadline lives inside `Interval`, not inside this fn's future, so
/// `select!` racing this arm against a faster one and dropping the loser each
/// spin does not restart the countdown (before this, each of the
/// three pages below called `sleep(PERIOD).await` fresh inside its own arm,
/// so any other arm winning — a keypress, a mouse click, an agent request —
/// reset that sleep to zero; the real cadence was "PERIOD of UI inactivity",
/// not the interval the doc and the shared constants promise).
///
/// A side effect worth knowing, not fixing: leaving `active` for a while and
/// then returning finds `tick`'s deadline in the past, so the very next check
/// fires immediately rather than waiting a full period. That is desirable
/// here — `onboarding.md` § The pending-invite surface already asks for
/// "first poll fires immediately when the page shows a hydrated
/// `PendingReview`", which tui did not implement before this fix.
async fn periodic_tick(tick: &mut tokio::time::Interval, active: bool) {
    if active {
        tick.tick().await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Receive the next agent request, or pend forever when the agent is off
/// (production) so the select arm never fires.
async fn recv_agent(agent: &mut Option<Agent>) -> AgentRequest {
    match agent {
        Some(agent) => match agent.rx.recv().await {
            Some(req) => req,
            // Server thread gone (bind failure): behave like production.
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod periodic_tick_tests {
    use super::periodic_tick;
    use std::time::Duration;
    use tokio::time::{Instant, MissedTickBehavior, interval, sleep};

    /// The exact race `run`'s select loop puts every page-scoped tick through:
    /// this arm racing against something that resolves far more often. Before
    /// the fix, `pending_invite_tick`/`awaiting_dns_tick`/
    /// `screen_lock_tick` called `sleep(period).await` fresh inside the arm
    /// every time `select!` re-polled it, so the sleep never won this race —
    /// the deadline lived nowhere but the losing future, which `select!`
    /// drops on every spin a different arm wins. `periodic_tick` fixes that
    /// by reading its deadline off `tick: &mut Interval`, owned outside the
    /// loop, so it survives being raced and re-constructed every spin.
    #[tokio::test(start_paused = true)]
    async fn fires_on_schedule_under_continuous_faster_activity() {
        let period = Duration::from_secs(30);
        let mut tick = interval(period);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut ticks = 0u32;
        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            tokio::select! {
                _ = periodic_tick(&mut tick, true) => { ticks += 1; }
                _ = sleep(Duration::from_millis(100)) => {}
            }
        }
        assert!(
            ticks >= 9,
            "periodic_tick must keep firing roughly every {period:?} despite \
             constant faster activity; got {ticks} ticks in 300s of virtual \
             time (expected ~10) — a re-created sleep() would starve here \
             entirely, which is exactly the bug this test guards against",
        );
    }

    /// `active: false` must never fire — the page-off-screen case (`main`'s
    /// three ticks each pend forever while their surface isn't shown, so an
    /// idle TUI on an unrelated page is never woken).
    #[tokio::test(start_paused = true)]
    async fn inactive_never_fires() {
        let period = Duration::from_secs(30);
        let mut tick = interval(period);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut ticks = 0u32;
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            tokio::select! {
                _ = periodic_tick(&mut tick, false) => { ticks += 1; }
                _ = sleep(Duration::from_millis(100)) => {}
            }
        }
        assert_eq!(ticks, 0, "an inactive tick must never fire");
    }
}
