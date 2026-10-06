use adw::prelude::*;
#[cfg(test)]
use fauna_ui_ids as ids;

/// Set the GTK widget name to the given test ID — the carrier the in-process
/// automation agent finds widgets by (`automation::find::find_in` matches on
/// `widget_name()` alone; always set, ungated, invisible to the user).
///
/// In a test-capable build (`cfg(any(debug_assertions, feature =
/// "e2e-agent"))`, mirroring `mod test_agent`'s gate) also stamps the AT-SPI
/// accessible description with the same id, as a discovery fallback for widget
/// types that don't reliably expose `Description` to AT-SPI otherwise.
/// **Release builds skip this** — a kebab-case internal ID is not a real
/// accessible description, and a real Flatpak build was stamping every tagged
/// widget's tooltip + screen-reader description with its raw test id (found
/// 2026-08-03, `docs/goal/architecture/apps/linux.md` § E2E automation once
/// claimed this was "real screen-reader a11y"; it wasn't — a human label per
/// widget is separate work this does not attempt).
///
/// # The tooltip half of that stamp is GONE, and it was costing the suite twice
///
/// It used to also `set_tooltip_text(Some(id))`. Two measured reasons not to,
/// both found by the stall-stack capture on 2026-09-11:
///
/// * **It blocks on the X server, once per tagged widget.**
///   `gtk_widget_set_tooltip_text` triggers a tooltip query, which asks the
///   display for the pointer's surface — `XIQueryPointer` → `_XReply` →
///   `poll`. A captured stall sat in exactly that, under `set_test_id` called
///   from the feed's per-post `interaction_button`. Every widget in this app is
///   tagged, so a page build paid a synchronous round trip per control: cheap
///   on an idle box, and not on one running twenty sessions. An automation
///   surface that makes the app slower is measuring itself.
/// * **It silently disabled the offline gate's reason in every test build.**
///   `offline_gate::apply` decides whether a control's tooltip is the *page's*
///   by reading `tooltip_text().is_none()`. Stamping every tagged widget's
///   tooltip with its test id made that read answer "the page spoke" for the
///   whole app, so the gate never wrote a reason anywhere a test could see it —
///   while the gate's own unit tests passed, because their widgets carry no
///   test id.
///
/// Nothing consumed it: `automation::find` matches on `widget_name()` alone,
/// and no driver or test reads a tooltip.
pub fn set_test_id(widget: &(impl IsA<gtk::Accessible> + IsA<gtk::Widget>), id: &str) {
    widget.set_widget_name(id);
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    widget.update_property(&[gtk::accessible::Property::Description(id)]);
    // Every error surface is named here, so this is where the painted-error
    // tally learns of it (`fauna_e2e_agent::PAINTED_ERRORS_KEY`).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    if fauna_e2e_agent::is_error_surface(id) {
        crate::automation::observables::register_error_surface(widget.upcast_ref());
    }
}

/// Wrap `page` in a vertical `gtk::Box` with a real, visible page heading
/// (Adwaita `title-2`) painted above it, carrying test id `id`.
///
/// For a sub-page hosted in a plain `gtk::Stack` outside an
/// `adw::PreferencesWindow`, `PreferencesPage::title()` is inert chrome
/// metadata the container never paints — nothing on screen says which page
/// this is (`docs/goal/ui/README.md` § Navigation model, "a shell sub-page
/// must paint a real, visible heading", ratified 2026-08-13). Mirrors
/// `views/admin.rs`'s `build_bridges_pending_page`/`build_dns_page` heading,
/// generalized to wrap an already-built page rather than being inlined per
/// builder. `id` is usually `"page-heading"`; a page whose own ui.yaml entry
/// designates a different landmark id (`muted-words`, `settings-logs`) passes
/// that instead — `check_surface_has_a_heading` (`walk.rs`) knows both.
pub fn wrap_page_with_heading(title: &str, id: &str, page: &impl IsA<gtk::Widget>) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("title-2");
    heading.set_margin_top(16);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_halign(gtk::Align::Start);
    set_test_id(&heading, id);
    outer.append(&heading);
    outer.append(page);
    outer
}

/// The GObject data key carrying an explicit automation text — see
/// [`set_test_text`]. Read back by `automation::find::text_of`.
pub const TEST_TEXT_KEY: &str = "fauna-e2e-text";

/// Declare, explicitly, what `get_text(id)` reads back for this widget —
/// overriding the widget-kind inference in `automation::find::text_of`.
///
/// **Why an explicit override exists.** linux is the only app whose agent
/// *infers* a row's text from its widget kind; tui carries the text on the
/// `Element` itself (`Element::gesture_button("folder-row", fs.name, …)`) and
/// apple declares it per view (`.automationValue("folder-row", text: {
/// folder.name })`). Inference cannot separate the two shapes an
/// `adw::ActionRow` is used for: a **caption/value** row, whose value is the
/// subtitle (the identity actor-id row — what the subtitle branch of `text_of`
/// exists for), and a **content** row, whose identity is the *title* and whose
/// subtitle is a secondary detail. The second shape read back as its detail and
/// silently lost its title: a read-only member `folder-row`
/// (`views/devices_folders/folders.rs`) is `.title(name).subtitle(mode)`, so
/// `get_text` returned the mode string `"sync"` for every such row — including
/// a cross-nest foreign row, where it presented as the shared set's name having
/// arrived empty from the nest, and cost a session's hunt through the whole
/// server-side `set_name` resolution chain before the read was suspected
/// (2026-08-11; `docs/goal/ui/folders.md` § Implementation status today).
/// The sibling owner/writer rows are `adw::ExpanderRow` — not an `ActionRow`
/// subclass — so they fell through to the descendant-label join and kept their
/// title by accident of widget kind, which is exactly why no earlier test
/// caught it.
///
/// Prefer this on any row whose title is its identity. It is checked *first*,
/// so it also settles kinds whose inference is merely awkward.
///
/// Free-form by construction: unlike [`set_test_attr`]'s CSS-class carrier this
/// holds arbitrary text (a user-chosen set name is not a CSS-safe token).
pub fn set_test_text(widget: &impl IsA<gtk::Widget>, text: &str) {
    // SAFETY: `set_data`/`data` are unsafe only in that the reader must name
    // the same type the writer stored. Both sides are in this file's contract
    // — written here as `String`, read as `String` by `find::test_text_of`,
    // which is the ONLY reader — and the value is dropped with the widget.
    unsafe {
        widget
            .as_ref()
            .set_data::<String>(TEST_TEXT_KEY, text.to_owned());
    }
}

/// Read back what [`set_test_text`] declared, if anything.
pub fn test_text(widget: &impl IsA<gtk::Widget>) -> Option<String> {
    // SAFETY: see `set_test_text` — `String` is the only type ever stored
    // under this key, and the borrow ends before the widget can be dropped.
    unsafe {
        widget
            .as_ref()
            .data::<String>(TEST_TEXT_KEY)
            .map(|p| p.as_ref().clone())
    }
}

/// Expose a readable string test attribute (`get_attr(id, name)`) as a CSS
/// class `test-attr-{name}-{value}`.
///
/// The AT-SPI bridge read these via the widget's accessible *Description*, but
/// GTK4 exposes no getter for that, so the in-process automation agent can't
/// read it. A CSS class is the one piece of widget state the agent *can* read
/// (`widget.css_classes()`), so it's the migration-friendly carrier. Removes
/// any prior value for `name` first, so exactly the current value is present.
/// `value` must be a CSS-identifier-safe token (kebab-case like our test ids).
pub fn set_test_attr(widget: &impl IsA<gtk::Widget>, name: &str, value: &str) {
    let prefix = format!("test-attr-{name}-");
    for cls in widget.css_classes() {
        if cls.starts_with(&prefix) {
            widget.remove_css_class(&cls);
        }
    }
    widget.add_css_class(&format!("{prefix}{value}"));
}

/// Declare `container` the automation **scope** of the `id`-carrying widget
/// inside it — for a control whose id must sit on a child (the widget a driver
/// presses and a role greys) while ui.yaml renders other elements *inside* that
/// control, as the child's siblings. tui states the same relation as
/// `Element::within`; a GTK tree states only nesting, and a leaf has no
/// children, so without this `scope="id[i]"` resolves to the leaf and finds
/// nothing beside it.
///
/// Carried as a CSS class `test-scope-{id}`, the readable-state carrier
/// [`set_test_attr`] uses; `automation::find::scope_container` is its only
/// reader.
pub fn set_test_scope(container: &impl IsA<gtk::Widget>, id: &str) {
    container.add_css_class(&format!("test-scope-{id}"));
}

/// Stamp a test ID onto an `adw::AlertDialog`/`adw::MessageDialog` response
/// button. Neither exposes its response-button widgets directly, so walk the
/// dialog's widget tree and tag the first descendant `gtk::Button` whose label
/// matches the localized response text. Must run after the dialog is
/// presented/mapped (the response buttons are materialized at present-time) —
/// call it from a `glib::idle_add_local_once` queued right before `present()`.
///
/// Shared by every feature that confirms a destructive/suggested action
/// through one of the two: `crate::confirm_dialog`'s nine `AlertDialog` sites
/// (mail-disable, account re-auth, sign-out, factory reset, bridges rotate,
/// snapshot delete, the three folder confirms), plus the
/// handful of sites not yet migrated still on
/// `MessageDialog` (conversation rename + add-participant, the feed
/// train-target sheet). The `set_test_id`-on-a-response-button shape is
/// uniform across them.
pub fn tag_response_button(root: &gtk::Widget, label: &str, id: &str) {
    if let Some(button) = root.downcast_ref::<gtk::Button>()
        && button.label().as_deref() == Some(label)
    {
        set_test_id(button, id);
        return;
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        tag_response_button(&c, label, id);
        child = c.next_sibling();
    }
}

/// Stamp a test ID onto an `adw::ActionRow`'s own **title label** — the text
/// the user actually reads on the row — for an element ui.yaml scopes *inside*
/// the row (e.g. `account-item-handle` within `account-switcher-item`).
///
/// The row exposes no getter for that label, so this walks the row's subtree
/// for the `gtk::Label` carrying the `title` CSS class — libadwaita's
/// documented CSS node for a row title (`box.title > label.title`), the same
/// public styling contract every theme targets. Returns whether it tagged one.
///
/// ⚠ The id must never sit on a hidden stand-in label instead: the automation
/// walk prunes every non-showing widget (`automation::find::is_showing`), so a
/// `set_visible(false)` probe is simply absent — `count` 0, indexed reads 404.
/// That is how the switcher's handle read `[]` on every linux run.
pub fn tag_row_title(row: &impl IsA<gtk::Widget>, id: &str) -> bool {
    fn walk(w: &gtk::Widget, id: &str) -> bool {
        if let Some(label) = w.downcast_ref::<gtk::Label>()
            && label.has_css_class("title")
        {
            set_test_id(label, id);
            return true;
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if walk(&child, id) {
                return true;
            }
            c = child.next_sibling();
        }
        false
    }
    walk(row.upcast_ref(), id)
}

/// Smoke guard that `set_test_id` keeps a widget discoverable after the AT-SPI
/// bridge removal (a follow-up cleanup). The in-process agent finds widgets by
/// `widget_name`; we also keep the accessible description for real
/// screen-reader a11y (deleting the *test* bridge doesn't touch the session
/// a11y bus, but a future edit must not silently drop the tag). GTK4 exposes no
/// getter for the accessible description, so the readable carrier this asserts
/// is the widget name — the agent's find key.
///
/// **It also asserts what `set_test_id` must NOT write: a tooltip, in any
/// build.** Until 2026-09-11 this test required one in test-capable builds, and
/// that requirement was itself the defect (see [`set_test_id`]): the stamp cost
/// a blocking X11 round trip per tagged widget, and it made
/// `offline_gate::apply` read every tagged control as "the page already wrote a
/// tooltip", so the gate's reason never appeared where a test could see it. The
/// assertion is now unconditional, because there is no build in which stamping
/// one is right.
#[cfg(test)]
#[test]
fn set_test_id_tags_widget_for_discovery() {
    run_on_gtk_thread(|| {
        let button = gtk::Button::new();
        set_test_id(&button, ids::SMOKE_TARGET);
        assert_eq!(button.widget_name(), "smoke-target");
        // The accessible role is still a real, screen-reader-visible role (not
        // overwritten by the test tagging) — Button stays Button.
        assert_eq!(button.accessible_role(), gtk::AccessibleRole::Button);
        assert_eq!(
            button.tooltip_text(),
            None,
            "tagging a widget must leave its tooltip alone: the tooltip is the \
             offline gate's channel for the reason it withheld a control, and \
             writing one here blocks on the X server per widget"
        );
    });
}

/// Budget for a single GTK test body. A widget test builds a few widgets and
/// asserts on them, so a green run finishes in milliseconds and pays nothing
/// for this ceiling — it exists only so a body that blocks forever fails
/// loudly instead of hanging the whole suite behind it (`testing.md`
/// § Cross-app e2e conventions point 14: a named generous budget and a
/// deadline, never a wall-clock assertion).
#[cfg(test)]
const GTK_TEST_BUDGET_SECS: u64 = 120;

/// Start a throwaway X server for this test process and return it plus its
/// display name.
///
/// **Why.** A widget test is not a pure computation: it `present()`s real
/// windows, and an autohide `gtk::Popover` takes a *display-wide* keyboard and
/// pointer grab. Run against the ambient `$DISPLAY` those windows land on a
/// desktop every other process on the box shares, which cost this project two
/// distinct intermittent failures in `automation::find::tests` (Track 14,
/// diagnosed 2026-08-06 on the primary dev VM, whose `:0` is the live
/// GNOME/Xwayland session):
///
/// 1. **A dismissed popover.** Six concurrent test processes each popping an
///    autohide popover fought over the one display grab; the losers had their
///    popover popped straight back down (`is_visible()=false`) and the walk
///    found nothing. Measured 4 failures / 150 runs concurrent, 0 / 60 alone.
/// 2. **A rewritten entry.** A popped-up popover leaves its `gtk::Entry`
///    focused with its whole value *selected* (`selection_bounds() ==
///    (0, len)`), so a single key event delivered anywhere on that shared
///    desktop replaces the entire value. That is exactly the shape of the
///    corruption observed: payloads with no trace of the value the test set,
///    of differing lengths, several being long runs of one character — key
///    auto-repeat, not heap garbage. It was mistaken for an uninitialized-read
///    / use-after-free through `gtk_editable_get_text()` for three sessions.
///
/// This is `e2e-conventions.md` **point 10** — an app launch is isolated from
/// the box it runs on — applied one tier down, and the same throwaway-Xvfb
/// answer `drivers/linux.py::launch` already gives for the e2e tier. Deleting
/// the shared dependency is the fix; serialising on it or widening a timeout
/// is not.
///
/// `-displayfd` has the server pick a free display number and report it back,
/// so concurrent test processes never race for one. The child dies with this
/// process via `PR_SET_PDEATHSIG`, which is why it is spawned *from the GTK
/// thread* — that signal fires when the spawning **thread** exits, and every
/// other thread here is a libtest thread that outlives nothing.
#[cfg(test)]
fn start_private_display() -> (std::process::Child, String) {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("Xvfb");
    cmd.args([
        "-displayfd",
        "1",
        "-screen",
        "0",
        "1280x1024x24",
        "-nolisten",
        "tcp",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    // SAFETY: `prctl` is async-signal-safe and touches only this child's own
    // process state, which is all `pre_exec` permits between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().expect(
        "could not spawn `Xvfb` for the private GTK test display — install it \
         (Debian/Ubuntu: `xvfb`). The widget tests deliberately refuse to run \
         against the ambient $DISPLAY; see `start_private_display`",
    );
    let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("could not read the display number from Xvfb");
    let number = line.trim().to_owned();
    assert!(
        !number.is_empty(),
        "Xvfb exited without reporting a display number — it could not start a server"
    );

    // `e2e-conventions.md` point 13: a redirected pipe is drained for as long
    // as the child lives. Xvfb says little after the display number, but a
    // full pipe would block it mid-run rather than fail loudly.
    std::thread::Builder::new()
        .name("xvfb-drain".to_owned())
        .spawn(move || {
            let mut sink = String::new();
            while reader.read_line(&mut sink).unwrap_or(0) > 0 {
                sink.clear();
            }
        })
        .expect("could not spawn the Xvfb drain thread");

    (child, format!(":{number}"))
}

/// Run `f` on the process-wide GTK test thread and return what it returns.
///
/// **Why this exists.** GTK4 binds to whichever thread called `gtk::init()`,
/// but libtest gives every `#[test]` its own thread — it spawns one per test
/// even under `--test-threads=1`, for panic isolation. The shape this replaced
/// was a `gtk_test_init_or_skip() -> bool` guard that every widget test called
/// and that returned `false` on any thread but the first, so the test body was
/// skipped, the test returned normally, and libtest printed **`ok`**. Measured
/// on the primary dev VM 2026-08-02, `cargo test -p fauna-linux --bins`: **64 of the 65**
/// guarded tests skipped in a single run that reported `250 passed; 0 failed`.
/// About five widget assertions actually executed, and *which* five depended on
/// thread scheduling. That is precisely the failure mode
/// `docs/goal/architecture/testing.md` § Cross-app e2e conventions **point 7**
/// forbids — a skip is not coverage — one tier below the pytest suite whose
/// ratchet enforces it.
///
/// So rather than skip work that lands off-thread, we *move* it: one dedicated
/// thread owns `gtk::init()` for the whole process, and every test body is
/// shipped to it and awaited. Every assertion runs, whichever thread libtest
/// picks, and a panic inside `f` is re-raised on the calling test thread so
/// libtest reports a real failure carrying the original assertion message.
///
/// There is deliberately **no skip path**: this either runs `f` or panics.
/// That is the enforcement — the mechanism that let a widget assertion
/// silently not run no longer exists to be called.
///
/// **The display is private, always** — see [`start_private_display`]. Widget
/// tests present real windows and take real grabs, so running them against the
/// developer's ambient `$DISPLAY` puts them on a desktop other apps share.
#[cfg(test)]
pub fn run_on_gtk_thread<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Mutex, OnceLock};
    use std::thread::ThreadId;
    use std::time::Duration;

    type Job = Box<dyn FnOnce() + Send + 'static>;
    struct GtkTestThread {
        jobs: Mutex<Sender<Job>>,
        id: ThreadId,
    }

    static GTK: OnceLock<GtkTestThread> = OnceLock::new();
    /// Set when a body overruns its budget. The GTK thread is still stuck on
    /// that body, so every later test would queue behind it and time out too;
    /// fail them immediately naming the real cause instead of burning the
    /// budget once per remaining test.
    static WEDGED: AtomicBool = AtomicBool::new(false);

    let gtk_thread = GTK.get_or_init(|| {
        let (jobs_tx, jobs_rx) = channel::<Job>();
        let (ready_tx, ready_rx) = channel::<ThreadId>();
        std::thread::Builder::new()
            .name("gtk-test-main".to_owned())
            .spawn(move || {
                gtk::init().expect("gtk::init() failed on the dedicated GTK test thread");
                // Only *after* `gtk_init` — GDK aborts a `gdk_display_open()`
                // that precedes it. `gtk_init` therefore still opens whatever
                // `$DISPLAY` names, but nothing is ever built on it: every
                // widget below is created against the default display, which
                // from here on is the private one. Bound to a named local
                // because the server must outlive every job, and
                // `PR_SET_PDEATHSIG` keys on *this* thread, which runs until
                // the process exits.
                let _xvfb = start_private_display();
                let display = gtk::gdk::Display::open(Some(&_xvfb.1)).unwrap_or_else(|| {
                    panic!("could not open the private GTK test display {}", _xvfb.1)
                });
                gtk::gdk::DisplayManager::get().set_default_display(&display);
                // Guard, not decoration: if a future GTK stops honouring
                // `set_default_display`, the widget tests must fail loudly
                // here rather than quietly go back to sharing the developer's
                // desktop — the exact silent regression this whole function
                // exists to prevent.
                let in_use = gtk::gdk::Display::default()
                    .map(|d| d.name().to_string())
                    .unwrap_or_default();
                assert_eq!(
                    in_use, _xvfb.1,
                    "the GTK test thread must run on its private display, not the ambient one"
                );
                ready_tx
                    .send(std::thread::current().id())
                    .expect("the GTK test thread could not report readiness");
                drop(ready_tx);
                while let Ok(job) = jobs_rx.recv() {
                    job();
                }
            })
            .expect("could not spawn the dedicated GTK test thread");
        let id = ready_rx
            .recv()
            .expect("the dedicated GTK test thread died during gtk::init()");
        GtkTestThread {
            jobs: Mutex::new(jobs_tx),
            id,
        }
    });

    // A body that calls back in would otherwise deadlock waiting on the thread
    // it is already running on. Run it inline instead.
    if std::thread::current().id() == gtk_thread.id {
        return f();
    }

    assert!(
        !WEDGED.load(Ordering::SeqCst),
        "the dedicated GTK test thread is wedged by an earlier test that overran its \
         {GTK_TEST_BUDGET_SECS}s budget, so this test never ran — find that earlier \
         timeout in this run's output, it names the body that hung"
    );

    // `FAUNA_GTK_TEST_TRACE=1` prints one line per body that is actually
    // entered. This exists because the bug this helper replaced was invisible
    // precisely in a green run: the only honest way to claim "every widget test
    // executes" is to be able to enumerate them from a real run rather than
    // trust a number in a doc. libtest names each test's thread after the test,
    // so the *caller's* thread name is the test's own name.
    let traced = std::env::var_os("FAUNA_GTK_TEST_TRACE").is_some();
    let caller = std::thread::current().name().map(str::to_owned);

    let (done_tx, done_rx) = channel::<std::thread::Result<T>>();
    let job: Job = Box::new(move || {
        if traced {
            eprintln!(
                "[gtk-body-ran] {}",
                caller.as_deref().unwrap_or("<unnamed>")
            );
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // A send error means the waiting test already gave up on its budget;
        // nothing to report to.
        let _ = done_tx.send(outcome);
    });
    gtk_thread
        .jobs
        .lock()
        .expect("the GTK test job queue is poisoned")
        .send(job)
        .expect("the dedicated GTK test thread died");

    match done_rx.recv_timeout(Duration::from_secs(GTK_TEST_BUDGET_SECS)) {
        Ok(Ok(value)) => value,
        // The body panicked on the GTK thread. Re-raise the *same* payload here
        // so libtest fails this test with the original assertion message; the
        // default hook already printed file:line from the GTK thread.
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            WEDGED.store(true, Ordering::SeqCst);
            panic!(
                "this test body did not finish within its {GTK_TEST_BUDGET_SECS}s budget \
                 on the GTK test thread"
            )
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the dedicated GTK test thread died while running this test")
        }
    }
}

/// Pins the isolation [`start_private_display`] exists to provide: widget tests
/// never run on the display the rest of the box shares.
///
/// Without this the regression is silent — the suite goes on passing, and only
/// pays for the shared desktop intermittently, which is exactly how the two
/// Track 14 failures survived three sessions and one wrong "memory corruption"
/// diagnosis. `e2e-conventions.md` point 10 asks every isolation carve-out to
/// be pinned by a test; this pins the default.
#[cfg(test)]
#[test]
fn widget_tests_run_on_a_private_display() {
    let (in_use, ambient) = run_on_gtk_thread(|| {
        (
            gtk::gdk::Display::default()
                .map(|d| d.name().to_string())
                .unwrap_or_default(),
            std::env::var("DISPLAY").unwrap_or_default(),
        )
    });
    assert!(
        !in_use.is_empty(),
        "the GTK test thread reported no default display at all"
    );
    assert_ne!(
        in_use, ambient,
        "widget tests are running on the ambient display ({ambient}) — they present real \
         windows and take display-wide grabs, so every assertion here is at the mercy of \
         whatever else is on that desktop"
    );
}

/// Proves the property every other GTK test in this crate now depends on: a
/// body dispatched from a thread that is *not* the GTK thread still runs.
///
/// This is the regression test for the silent-skip bug. Under the guard
/// `run_on_gtk_thread` replaced, seven of these eight bodies would have been
/// skipped and this test would still have reported `ok` — which is precisely
/// how 64 of 65 widget assertions stopped running with nobody noticing. Eight
/// threads is far more than libtest needs to reproduce it: one thread other
/// than the claiming one was always enough.
#[cfg(test)]
#[test]
fn a_body_dispatched_from_any_thread_actually_runs() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let ran = Arc::new(AtomicUsize::new(0));
    let dispatchers: Vec<_> = (0..8)
        .map(|_| {
            let ran = Arc::clone(&ran);
            std::thread::spawn(move || {
                run_on_gtk_thread(move || {
                    // A real GTK call, so this also proves the body reaches a
                    // usable GTK — not merely that a closure was invoked.
                    let button = gtk::Button::new();
                    set_test_id(&button, ids::DISPATCH_PROBE);
                    assert_eq!(button.widget_name(), "dispatch-probe");
                    ran.fetch_add(1, Ordering::SeqCst);
                });
            })
        })
        .collect();
    for d in dispatchers {
        d.join().expect("a dispatching thread panicked");
    }
    assert_eq!(
        ran.load(Ordering::SeqCst),
        8,
        "every dispatched body must run — a body that does not run is the bug this helper exists to kill"
    );
}

/// The other half of "a skip is not coverage": a body that fails must fail the
/// *test*, carrying its original message. If a panic on the GTK thread were
/// swallowed, every assertion in this crate would be decorative.
///
/// Expect one panic line on stderr during this test — it is the deliberate one
/// raised below, not a failure.
#[cfg(test)]
#[test]
fn a_failing_body_fails_the_calling_test_with_its_own_message() {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_on_gtk_thread(|| panic!("expected panic: proving failures propagate"));
    }));
    let payload = outcome.expect_err("a panicking body must not report success");
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(
        message.contains("expected panic: proving failures propagate"),
        "the original panic message must survive the hop back to the test thread, got {message:?}"
    );
}

/// Collect every non-empty `widget_name` in `root`'s subtree, **including
/// `root` itself**.
///
/// Inclusivity is load-bearing: a page's landmark id (`muted-words`,
/// `task-delegation`, …) is stamped with `set_test_id` on the *root*
/// `adw::PreferencesPage`/`gtk::Box`, and the real automation agent reaches it
/// by walking down from the window — so a `*_exposes_static_ui_yaml_ids` test
/// must check the root's own name too. Earlier per-module copies of this helper
/// started the walk at `root.first_child()`, silently never checking the page
/// landmark; the bug hid because the guard these tests used to call skipped
/// them on every thread but the one that first claimed GTK, so the landmark
/// assertion almost never ran (see `run_on_gtk_thread`, which replaced that
/// guard and is why they all run now). This single canonical helper walks
/// `root` inclusively so the landmark id is always asserted.
#[cfg(test)]
pub fn widget_names(root: &impl IsA<gtk::Widget>) -> Vec<String> {
    fn walk(w: &gtk::Widget, out: &mut Vec<String>) {
        let name = w.widget_name();
        if !name.is_empty() {
            out.push(name.to_string());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            walk(&child, out);
            c = child.next_sibling();
        }
    }
    let mut out = Vec::new();
    walk(root.upcast_ref(), &mut out);
    out
}

/// Find the first widget in `root`'s subtree (root included) whose
/// `widget_name()` equals `id` — the read half of [`set_test_id`], which is
/// what writes that name. `recovery_kit.rs` and `identity_export.rs` each
/// hand-rolled this exact walk before this lift (round 71 of the shared-Rust
/// lift sweep).
///
/// **Not `#[cfg(test)]` any more (2026-09-02).** It was, and the cost was a
/// third hand-rolled copy: the folders view needed the same walk at *runtime*
/// — to reach a response button it had just tagged, so the offline gate could
/// declare a wire kind on it — and could not call a test-only function. That
/// copy is gone, and [`crate::confirm_dialog`] uses this one for the same
/// reason on behalf of every confirm dialog in the app.
pub fn find_by_test_id(root: &impl IsA<gtk::Widget>, id: &str) -> Option<gtk::Widget> {
    fn walk(w: &gtk::Widget, id: &str) -> Option<gtk::Widget> {
        if w.widget_name() == id {
            return Some(w.clone());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(found) = walk(&child, id) {
                return Some(found);
            }
            c = child.next_sibling();
        }
        None
    }
    walk(root.upcast_ref(), id)
}
