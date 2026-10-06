//! Terminal paint layer: the unauthenticated screen (the launch flow's own
//! surface, or the onboarding wizard) or the sidebar + page shell
//! (authenticated).
//!
//! Desktop form factor per `apps/tui.md` § Form factor: a sidebar of
//! `{page}-tab` rows with the global status header pinned above it (same
//! placement as the linux sidebar) — `connection-status`, and on builds that
//! drive a local sync agent the `sync-agent-status` reading plus its dim
//! version/uptime subtitle — and the current page in the main pane with its
//! `error-message` line. Navigation is hidden while
//! unauthenticated (ui.yaml `navigation.hidden_when`) — pre-login the app
//! paints [`crate::launch::LaunchSurface`] until the launch flow yields the
//! screen to the wizard (`onboarding.md` § App-launch routing).
//!
//! Paint and the automation registry both consume the screen's single
//! [`crate::element::Element`] list ([`App::screen_elements`]) — the
//! authenticated shell included, sidebar and all — so a painted element is
//! automatable by construction and no element can be registered without pixels
//! behind it.
//!
//! The one asymmetry, stated in [`crate::element`] and enforced here: **the
//! element list is the registry; the viewport clips paint only.** A page hands
//! over every element its snapshot implies; [`scroll_offset`] decides which of
//! them get pixels this frame.

use fauna_ws_substrate::supervisor::ConnectionState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, Zone};

/// ui.yaml element IDs painted by this module (beyond the per-row
/// `Page::tab_id` and the wizard's own), served to the automation agent by
/// [`register_frame`].
pub const CONNECTION_STATUS_ID: &str = fauna_ui_ids::CONNECTION_STATUS;
/// The global local-agent health indicator and its two optional children
/// (`sync-agent.md` § Local agent health). Conditionally present: painted only
/// on builds that drive a local `fauna-sync-agent`, which
/// [`crate::sync_agent::SyncAgentState::rendered_status`] answers for.
pub const SYNC_AGENT_STATUS_ID: &str = fauna_ui_ids::SYNC_AGENT_STATUS;
pub const SYNC_AGENT_STATUS_VERSION_ID: &str = fauna_ui_ids::SYNC_AGENT_STATUS_VERSION;
pub const SYNC_AGENT_STATUS_UPTIME_ID: &str = fauna_ui_ids::SYNC_AGENT_STATUS_UPTIME;
/// The global "This account is supervised by X" chrome (ui.yaml `global`,
/// `family-safety.md` § App surface). **Conditionally present**: painted and
/// registered only while `fauna.family.status` reports a guardian for this
/// account — supervision is never silent, and never dismissable.
pub const SUPERVISED_INDICATOR_ID: &str = fauna_ui_ids::SUPERVISED_INDICATOR;
pub const ERROR_MESSAGE_ID: &str = fauna_ui_ids::ERROR_MESSAGE;
pub const WARNING_MESSAGE_ID: &str = fauna_ui_ids::WARNING_MESSAGE;
pub const INFO_MESSAGE_ID: &str = fauna_ui_ids::INFO_MESSAGE;
/// The every-page critical-alerts banner (ui.yaml `global`,
/// `behavior/critical-alerts.md` § Mechanism → *Rendering contract*).
/// **Conditionally present**: the landmark and its indexed rows exist iff ≥1
/// alert is active, so an absent banner is literal absence. Non-dismissable —
/// there is no gesture on either element; an alert leaves only when the
/// condition that raised it re-checks clean.
pub const CRITICAL_ALERTS_ID: &str = fauna_ui_ids::CRITICAL_ALERTS;
pub const CRITICAL_ALERT_ID: &str = fauna_ui_ids::CRITICAL_ALERT;
/// The persistent key-hint footer (ui.yaml `global.platform_elements`,
/// `tui: [nav-key-hints]`; `apps/tui.md` § Key-hint footer). **tui-only**: the
/// six pointer apps advertise an affordance by drawing a control the user can
/// see, and a keyboard-only client has no such channel — so it names its keys.
/// Present on the whole authenticated shell and nowhere else, which is exactly
/// the span on which its bindings are live.
pub const NAV_KEY_HINTS_ID: &str = fauna_ui_ids::NAV_KEY_HINTS;
/// The glyphs the hints name. Not i18n strings — they are the keys themselves,
/// and a translator places them rather than translating them (`{keys}`).
const PANE_KEYS: &str = "←/→";
const MOVE_KEYS: &str = "↑/↓";
const ENTER_KEY: &str = "⏎";
const QUIT_KEY: &str = "q";
/// Middle dot with hair spaces — the terminal convention (tig, mc), and narrow
/// enough that the whole row still fits an 80-column terminal.
const HINT_SEPARATOR: &str = " · ";

/// Whether the key-hint footer belongs on this frame at all.
///
/// **`authenticated()` alone is the wrong test**, and wrongly in the direction
/// that looks right: the append-mode "Add account" wizard runs *over a live
/// session*, so `authenticated()` is true while the wizard owns the screen and
/// [`App::screen_elements`] deliberately emits **no sidebar**. A footer gated on
/// `authenticated()` would there advertise "←/→ pane" on a screen with a single
/// pane — a hint for a gesture with nowhere to go. So this is the same gate the
/// rest of the shell chrome uses ([`critical_alert_lines`], the
/// `supervised-indicator` registration): authenticated **and** not showing a
/// launch/wizard surface.
pub(crate) fn nav_key_hints_visible(app: &App) -> bool {
    app.authenticated() && !app.showing_launch_surface()
}

/// The footer's text for the frame being painted.
///
/// **Every hint names a binding that would fire on the CURRENT focus.** That is
/// the contract, not a nicety: `q` quits only while no text input holds the ring
/// (`App::handle_key`'s `KeyCode::Char` arm — with an input focused the same key
/// types a literal `q`), so a fixed string would tell the user something false
/// exactly when they are most likely to try it. Both arms read
/// [`App::focused_input`], the same predicate the key handler branches on, so
/// the two cannot drift apart.
pub(crate) fn nav_key_hints_text(app: &App) -> String {
    use fauna_i18n::strings::tui_nav_hints as strings;
    let mut hints = vec![strings::pane(PANE_KEYS), strings::move_focus(MOVE_KEYS)];
    if app.focused_input().is_some() {
        // Enter over an editable field advances the ring (or commits, for an
        // `InputCommit`); it does not "open" anything. And `q` is a character
        // here, so it is deliberately absent — the one hint whose omission is
        // load-bearing.
        hints.push(strings::next(ENTER_KEY));
    } else {
        hints.push(strings::open(ENTER_KEY));
        hints.push(strings::quit(QUIT_KEY));
    }
    hints.join(HINT_SEPARATOR)
}
/// Register everything this frame paints into the automation registry — the
/// single source the agent answers `/element/*` from. Kept in lockstep with
/// the paint functions below: every element that gets pixels registers here,
/// and (apple's `error-message` lesson) an empty error does NOT register, so
/// `is_visible("error-message")` is false on a clean page even though the
/// paint reserves the line.
///
/// The page half registers the page exactly as `page` — the draw's own
/// [`PaintedPage`] — painted it: each element carries its position in that
/// list and whether it landed in view ([`PaintedPage::in_view`]), which is what
/// the agent's `in-viewport` attribute and its targeted scroll-into-view read.
pub fn register_frame(app: &App, registry: &mut crate::automation::Registry, page: &PaintedPage) {
    registry.clear();
    registry.text(CONNECTION_STATUS_ID, connection_status_text(app.connection));
    // nav-key-hints — registered exactly where `render_shell` paints it, through
    // the same predicate, so a hint can never be registered without pixels behind
    // it. Registered as plain text and never given a `Role`, so the focus ring
    // cannot walk onto the very row explaining what the focus keys do.
    if nav_key_hints_visible(app) {
        registry.text(NAV_KEY_HINTS_ID, nav_key_hints_text(app));
    }
    // critical-alerts — registered exactly where `render_shell` paints it: the
    // band above the authenticated shell, and only while ≥1 alert is active. The
    // landmark carries no text of its own (the `atproto-page` idiom); each alert
    // is one indexed `critical-alert[N]` row in the registry's push order, which
    // is the shared registry's deterministic BTreeMap order.
    for (index, line) in critical_alert_lines(app).into_iter().enumerate() {
        if index == 0 {
            registry.text(CRITICAL_ALERTS_ID, String::new());
        }
        registry.text(CRITICAL_ALERT_ID, line);
    }
    // sync-agent-status — registered exactly where it paints: the authenticated
    // sidebar, and only on a build that drives a local agent. version/uptime are
    // empty unless the agent answered, and an empty text does not register, so
    // "empty/absent while sync-agent-status reads 'Not running'" (ui.yaml) is
    // literal absence rather than an empty string.
    if let Some(status) = app
        .sync_agent
        .rendered_status()
        .filter(|_| app.authenticated())
    {
        registry.text(SYNC_AGENT_STATUS_ID, sync_agent_status_text(status.health));
        if !status.version.is_empty() {
            registry.text(SYNC_AGENT_STATUS_VERSION_ID, &status.version);
        }
        if !status.uptime.is_empty() {
            registry.text(SYNC_AGENT_STATUS_UPTIME_ID, &status.uptime);
        }
    }
    // supervised-indicator — registered exactly where it paints: the
    // authenticated sidebar's status header, and only for a supervised account
    // (`crate::family::supervised_indicator_text` answers `None` otherwise, so an
    // ordinary account gets literal absence, not an empty string). ui.yaml types
    // it a BUTTON that navigates to the family page, so it registers as a real
    // `Gesture::Nav` element rather than a text line — a driver click reaches the
    // page the same way it does on linux's header button.
    if let Some(text) = crate::family::supervised_indicator_text(&app.family)
        .filter(|_| app.authenticated() && !app.showing_launch_surface())
    {
        registry.element(crate::element::Element::gesture_button(
            SUPERVISED_INDICATOR_ID,
            text,
            true,
            crate::element::Gesture::Nav(crate::pages::Page::Family),
        ));
    }
    // One door for every screen: the sidebar's tab rows are `Element`s like any
    // other, so the authenticated shell has no hand-maintained registration path
    // that could drift from what it paints — the two halves of
    // `App::screen_elements`, the page half as drawn.
    //
    // Chrome (an empty id) is painted but never automatable — real UI that
    // ui.yaml gives no element ID, like the feed's "No posts yet." line.
    for element in app.sidebar_elements() {
        if element.id.is_empty() {
            continue;
        }
        registry.element(element);
    }
    for (index, element) in page.elements.iter().enumerate() {
        if element.id.is_empty() {
            continue;
        }
        registry.page_element(element.clone(), index, page.in_view(index));
    }
    if let Some(error) = app.error_line_text() {
        registry.text(ERROR_MESSAGE_ID, error);
    }
    // The global warning/info lines (ui.yaml `global`) — today fed only by the
    // test-agent `messages` state patch, like the error line they register
    // exactly when they paint (empty ⇒ absent, apple's lesson above).
    if let Some(warning) = app.injected_warning.clone().filter(|w| !w.is_empty()) {
        registry.text(WARNING_MESSAGE_ID, warning);
    }
    if let Some(info) = app.injected_info.clone().filter(|i| !i.is_empty()) {
        registry.text(INFO_MESSAGE_ID, info);
    }
}

/// [`register_frame`] for a registry built outside a draw (tier_1 tests): the
/// page as listed, with no geometry, so every `in-viewport` read answers `null`.
#[cfg(test)]
pub fn register_listed(app: &App, registry: &mut crate::automation::Registry) {
    register_frame(app, registry, &PaintedPage::unpainted(app.page_elements()));
}

/// The active critical alerts as display lines, one per `critical-alert[N]` row
/// — empty unless the shell is actually showing an authenticated page.
///
/// The gate matters both ways: the contract is *every authenticated page*, and
/// the launch/wizard surfaces are pre-identity, where an alert keyed to a DID has
/// no user to warn and would paint over the sign-in flow. Paint and
/// [`register_frame`] both call this, so a row can never be registered without
/// pixels behind it (or vice versa).
fn critical_alert_lines(app: &App) -> Vec<String> {
    if !app.authenticated() || app.showing_launch_surface() {
        return Vec::new();
    }
    app.alerts.active_lines(fauna_i18n::strings::lookup)
}

/// Display text for the global `connection-status` element — the state → label
/// decision is the shared `fauna_core::format::connection_state_label`
/// (transport.md § Connection-status indicator), so this is a thin resolve
/// rather than a hand-rolled match (mirrors `apps/fauna-linux/src/i18n::connection_state_label`).
/// The word itself comes from `ConnectionState::as_wire_word` — same one mapping
/// `App::connection_state_word` delegates to (`app.rs`'s doc comment on that fn
/// names this function as the mapping's other consumer; this used to be its own
/// hand-rolled copy of the match).
pub fn connection_status_text(state: ConnectionState) -> String {
    fauna_core::format::connection_state_label(state.as_wire_word())
        .resolve(fauna_i18n::strings::lookup)
}

/// Display text for the global `sync-agent-status` element — like
/// `connection-status`, ui.yaml pins the five readings to the shared string
/// table, so the five-state derivation stays shared Rust and only the lookup is
/// per-app.
pub fn sync_agent_status_text(health: crate::sync_agent::AgentHealth) -> &'static str {
    use crate::sync_agent::AgentHealth;
    use fauna_i18n::strings::status::sync_agent as strings;
    match health {
        AgentHealth::Running => strings::RUNNING,
        AgentHealth::RestartPending => strings::RESTART_PENDING,
        AgentHealth::KeysPending => strings::KEYS_PENDING,
        AgentHealth::NotEnrolled => strings::NOT_ENROLLED,
        AgentHealth::NotRunning => strings::NOT_RUNNING,
    }
}

/// Paint a frame, and report where any images landed and what a mouse click on
/// each row would hit.
///
/// The return value is the *only* record of either: a `Frame`'s `Rect`s are
/// closure locals and the painted `Line`s do not remember which element made
/// them, so once this returns, where a thumbnail (or a clickable row) is on
/// screen is knowable nowhere else. [`crate::graphics::Painter`] consumes the
/// placements after `terminal.draw` (`apps/tui.md` § Rendering: an image is
/// an escape at a cell rect and a scrolled `Paragraph` cannot carry one); the
/// main loop consumes the hit regions the same way, on the next mouse event,
/// and the painted page for everything that asks what is on screen
/// ([`PaintedPage`]).
pub fn render(frame: &mut Frame, app: &App) -> DrawnFrame {
    if app.authenticated() {
        render_shell(frame, app)
    } else {
        render_screen(frame, app)
    }
}

/// The painted text of one control — **the whole control vocabulary, in one
/// place** (`apps/tui.md` § Rendering → *Control vocabulary*, whose table this
/// function is the implementation of).
///
/// A terminal has no widget chrome, so this text is the only thing that says
/// what kind of control the user is looking at and what state it is in. It is
/// shared by both painting paths — an element on its own line and an element
/// as a cell in an inline row — because the vocabulary is a property of the
/// *control*, not of where it happens to sit.
///
/// **That sharing is the point.** The inline path used to carry its own partial
/// copy of this match, which dropped `Role::Button`'s brackets for *every*
/// inline element. The grids that motivated it really do need raw text — a
/// day/slot cell is `Element::label(..).clickable(..)`, and `clickable` makes it
/// a `Role::Button` whose body is nonetheless canvas — but the blanket rule also
/// caught the two inline elements that are genuinely controls: the Events
/// sidebar's `calendar-item` and the agenda's RSVP row, which painted as bare
/// text and so read as labels. That is the same "is this even a control?"
/// illegibility the vocabulary exists to end, and it was invisible because
/// nothing asserts that a button *looks* like one.
///
/// So the canvas case is now the marked one ([`Element::cell`]) and everything
/// else inherits the vocabulary by default — one function, no second copy to
/// fall behind it.
fn control_text(element: &crate::element::Element) -> String {
    use crate::element::Role;
    match &element.role {
        // A read-only value with a human label paints as "prompt: value" (a MUA
        // row's "IMAP host: mail.example.org"), the same shape the Input arm
        // below gives a field. The label is paint-only: the registry keeps
        // `text` as the BARE value, because that is what every app's
        // `get_text` contract returns and what the cross-app assertions read
        // (`mua_webdav_url()` must start with "https://", not with a prompt). A
        // label-less value paints alone — which is also what makes a grid's
        // fixed-width cell come through untouched.
        Role::Label => match element.label.as_deref() {
            Some(prompt) => format!("{}: {}", prompt, element.text),
            None => element.text.clone(),
        },
        // A NAV button goes somewhere; a plain one acts now. Brackets are the
        // "acts now" promise, so a destination drops them and takes the trailing
        // ▸ instead (`Element::nav`) — the paint answer to the user's
        // "navigational?" question about a rail that painted its destinations
        // and its actions identically.
        //
        // A way OUT is still a destination, so it drops the brackets too — but
        // it takes the MIRRORED glyph, leading (`Element::nav_back`): `Back ▸`
        // would drop the brackets correctly and then promise the opposite
        // direction, which is worse than the paint it replaced. This arm must
        // precede the forward one — `nav_back` implies `nav`.
        // A canvas cell keeps its raw body: it is a painted box a click acts on,
        // not a control, and its grid aligns on the exact width (`Element::cell`).
        Role::Button(_) if element.cell => element.text.clone(),
        Role::Button(_) if element.nav_back => format!("◂ {}", element.text),
        Role::Button(_) if element.nav => format!("{} ▸", element.text),
        Role::Button(_) => format!("[ {} ]", element.text),
        // Prompt with the human label where the page gave one (a credential
        // field's "API token", a contact field's "Postal code"); fall back to
        // the element id, which is all a page without labels can offer. A
        // committing input paints identically to a plain one — the only
        // difference is what Enter does, and the caret already says "typing
        // happens here".
        Role::Input(_) | Role::InputCommit { .. } => {
            let prompt = element.label.as_deref().unwrap_or(&element.id);
            format!("{}: {}_", prompt, element.text)
        }
        // The box IS the body when there is no text: an empty-labelled checkbox
        // painting "[x] " would trail a space into a grid column, which is how
        // the Events sidebar's `calendar-visibility` sits beside its
        // `calendar-item`. `checked` stays the single source of truth for both
        // the paint and the automation surface.
        Role::Checkbox { checked, .. } => {
            let box_ = if *checked { "[x]" } else { "[ ]" };
            marker_with_text(box_, &element.text)
        }
        // The terminal radio idiom: the marker says which option of the group is
        // CURRENT, something neither `[ label ]` (a plain button — no state) nor
        // `[x] label` (a checkbox — independent on/off) can express. Never
        // `REVERSED` — that stays the focus affordance.
        Role::Radio { selected, .. } => {
            let dot = if *selected { "(*)" } else { "( )" };
            marker_with_text(dot, &element.text)
        }
        // The selected option between cycle arrows. A picker's `text` is the
        // value that round-trips through `/element/select` (for
        // `feed-rule-type-select` that is the raw variant key), so the human
        // reads the paint-only `display` where the page supplied one — and the
        // `label` PROMPT paints in front, exactly like an input's. The
        // pre-2026-08-03 arm painted the label *instead of* the value, so every
        // prompt-labelled select hid its current state (the audit's
        // dropdown-state-unknowable class).
        Role::Select { display, .. } => {
            let value = display.as_deref().unwrap_or(&element.text);
            match element.label.as_deref() {
                Some(prompt) => format!("{prompt}: < {value} >"),
                None => format!("< {value} >"),
            }
        }
    }
}

/// A state marker plus its label, or the bare marker when there is no label.
fn marker_with_text(marker: &str, text: &str) -> String {
    if text.is_empty() {
        marker.to_string()
    } else {
        format!("{marker} {text}")
    }
}

/// Paint one element list to lines, and report which line the focused element
/// starts on so the caller can scroll it into view.
///
/// Focus is shown with a `>` gutter; disabled controls are dimmed; inputs render
/// their live value. An element carrying a [`RenderDocument`](fauna_core::render::RenderDocument)
/// is **walked** ([`crate::document::render_document`]) instead of painted as
/// one flat line — that is how a post body gets real emphasis, lists and code
/// while the registry still reads the single plaintext `text`.
fn element_lines(elements: &[crate::element::Element], focused: Option<usize>) -> PaintedLines {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut focused_line = 0;
    let mut art_spans: Vec<ArtSpan> = Vec::new();
    // Which input-slice index painted each output line — the hit-test
    // counterpart of `focused_line`/`art_spans`: this loop is the only code
    // that knows which lines an element produced, so a click's row → element
    // lookup reads it here rather than re-deriving it (and risking drift) at
    // the mouse-handling call site.
    let mut element_at_line: Vec<usize> = Vec::new();
    // Where each inline cell landed within its shared line. `element_at_line`
    // can only say "this line belongs to element N", which is exactly the
    // resolution a row of day cells does not have.
    let mut inline_bands: Vec<InlineBand> = Vec::new();
    // The currently open inline run: (its output line, the column its next cell
    // starts at). Cleared by any non-inline element, so a run never spans a
    // paragraph break.
    let mut run: Option<(usize, u16)> = None;
    for (element_index, element) in elements.iter().enumerate() {
        if !element.inline {
            run = None;
        }
        let is_focused = focused == Some(element_index);
        if is_focused {
            // A cell joining an open run is on that run's line, which is
            // already pushed — `lines.len()` would point one line past it and
            // scroll the row off screen.
            focused_line = match (element.inline, run) {
                (true, Some((line, _))) => line,
                _ => lines.len(),
            };
        }
        let gutter = if is_focused { "> " } else { "  " };
        // An element nested under an indexed container (a post card's children,
        // a provisioning step row's substep lines) paints indented beneath it,
        // so the visual nesting matches the scope path the registry records.
        let indent = "  ".repeat(element.path.len());
        let style = if element.enabled {
            Style::default()
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        // An explicit colour pair (`Element::colors`) overrides theme
        // inheritance outright — the identity-export QR's need: a QR's
        // module contrast is spec, not a theme choice, so it must paint
        // dark-on-light regardless of the terminal's light/dark theme.
        let style = if let Some((fg, bg)) = element.colors {
            style
                .fg(Color::Rgb(fg[0], fg[1], fg[2]))
                .bg(Color::Rgb(bg[0], bg[1], bg[2]))
        } else {
            style
        };

        // Half-block art: one `Line` per art row, one `▀` span per cell —
        // fg = the cell's top pixel, bg = its bottom (`crate::thumbnail`). This
        // is the one body that cannot ride `text`: the styled-row arm below
        // gives a whole row a single `Style`, and an image needs a colour pair
        // per cell. Same gutter+indent prefix as every other element, so a
        // thumbnail still reads as one row of the item it belongs to.
        if let Some(art) = &element.art {
            let first_line = lines.len();
            for (row_index, row) in art.rows.iter().enumerate() {
                let prefix = if row_index == 0 {
                    format!("{gutter}{indent}")
                } else {
                    format!("{}{indent}", " ".repeat(gutter.len()))
                };
                let mut spans = vec![Span::raw(prefix)];
                spans.extend(row.iter().map(|cell| {
                    Span::styled(
                        crate::thumbnail::HALF_BLOCK,
                        Style::default()
                            .fg(Color::Rgb(cell.top[0], cell.top[1], cell.top[2]))
                            .bg(Color::Rgb(cell.bottom[0], cell.bottom[1], cell.bottom[2])),
                    )
                }));
                lines.push(Line::from(spans));
                element_at_line.push(element_index);
            }
            // Record where the art landed, for the post-paint protocol pass.
            // **Here and nowhere else**: this loop is the only code that knows
            // which lines an element produced, and a second implementation of it
            // would be free to drift from where the art actually painted.
            if let Some(pixels) = &element.pixels {
                art_spans.push(ArtSpan {
                    pixels: pixels.clone(),
                    line: first_line,
                    cols: art.rows.first().map_or(0, |row| row.len()) as u16,
                    rows: art.rows.len() as u16,
                    // Gutter and indent are ASCII, so their byte length is their
                    // cell width.
                    prefix: (gutter.len() + indent.len()) as u16,
                });
            }
            continue;
        }

        // A rich body: walk the document, prefixing each line with this
        // element's gutter + indent so the block still reads as one element.
        if let Some(doc) = &element.doc {
            for mut line in crate::document::render_document(doc) {
                line.spans.insert(0, Span::raw(format!("{gutter}{indent}")));
                lines.push(line);
                element_at_line.push(element_index);
            }
            continue;
        }

        // An inline cell joins the open run's line instead of taking one of its
        // own (`Element::inline`) — seven `events-day-cell-*` elements painting
        // as one week row, each still individually addressable.
        //
        // It paints its `text` RAW, skipping the `[ ... ]` decoration the
        // `Role::Button` arm below gives a standalone button: brackets would
        // break the fixed cell width the month grid aligns on, and they already
        // mean something else in that grid — `[15]` is how it marks *today*.
        //
        // A **checkbox** is the one exception, because the box IS its body: an
        // inline checkbox painting raw text would render as nothing at all when
        // its text is empty, which is exactly how the Events sidebar's
        // `calendar-visibility` box sits beside its `calendar-item` name.
        // `checked` stays the single source of truth for both the paint and the
        // automation surface.
        if element.inline {
            // `starts_row` opens a fresh line even though a run is already
            // open — the only way a multi-ROW grid can express its row
            // boundary, since ending a run with a non-inline element would
            // spend a whole line on the separator (see
            // `Element::starts_row`).
            let (line_index, col) = match run.filter(|_| !element.starts_row) {
                Some(open) => open,
                None => {
                    let prefix = format!("{gutter}{indent}");
                    let col = prefix.len() as u16;
                    lines.push(Line::from(vec![Span::raw(prefix)]));
                    // One line, many elements: the band list below is what
                    // resolves a click precisely. This keeps `element_at_line`
                    // total (every line has an entry) and makes the run's first
                    // cell the fallback for a hit that lands past the last band.
                    element_at_line.push(element_index);
                    (lines.len() - 1, col)
                }
            };
            // The SAME vocabulary as a standalone element — one function, so an
            // idiom can never be present on its own line and missing in a row.
            let cell_text = control_text(element);
            let width = cell_text.chars().count() as u16;
            // Focus cannot use the `>` gutter mid-row, so a focused cell is
            // shown in reverse video — the one focus affordance that reads
            // correctly inside a grid.
            let cell_style = if is_focused {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            lines[line_index]
                .spans
                .push(Span::styled(cell_text, cell_style));
            inline_bands.push(InlineBand {
                line: line_index,
                col_start: col,
                col_end: col + width,
                element: element_index,
            });
            run = Some((line_index, col + width));
            continue;
        }

        let body = control_text(element);
        // A multi-line body is N rows, not one. A ratatui `Line` is a single
        // row, and building one from a string containing `\n` silently turns
        // each newline into a *span* boundary — concatenating every row onto
        // one line rather than stacking them (the identity-export QR's block
        // art painted as a single unscannable strip this way). Split first, so
        // one text row is one `Line`; continuation rows keep the element's
        // indent and align under its gutter.
        for (row_index, row) in body.split('\n').enumerate() {
            let prefix = if row_index == 0 {
                format!("{gutter}{indent}")
            } else {
                format!("{}{indent}", " ".repeat(gutter.len()))
            };
            // The focused row reads as focused, not merely marked. The `> `
            // gutter alone loses against rows that carry their own state
            // styling — a live user reported every option looking "equally
            // selected" and could not tell which one the arrows drove.
            // `REVERSED` dominates
            // because nothing else in this pane uses it: the row-level
            // vocabulary is otherwise only `DIM` (disabled) and default.
            //
            // This is the sidebar's own affordance, lifted rather than invented
            // (priority #4) — see `render_sidebar`, which has paired `REVERSED`
            // with the `> ` caret since it was written, for the same reason.
            //
            // It cannot produce the two-focus-rings problem that sidebar warns
            // about: the sidebar-bearing path passes `focused` only while
            // `app.zone == Zone::Page`, so at most one pane ever has a focused
            // element to paint, and `render_screen` (the other caller) has no
            // sidebar to compete with.
            // Row 0 only, exactly like the gutter: a focused multi-row element
            // still reads as one block instead of a wall of reverse video.
            let row_style = if is_focused && row_index == 0 {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            lines.push(Line::styled(format!("{prefix}{row}"), row_style));
            element_at_line.push(element_index);
        }
    }
    // The gate. Strip control characters from every painted line, HERE and
    // nowhere else: this is the single funnel all three arms above converge on
    // (walked document, flat text/label body, half-block art), so every present
    // and future content source is covered structurally.
    //
    // Content reaching a tui cell grid is remote-authored in the general case — a
    // post body, a remote-chosen display name, third-party OpenGraph
    // `og:title`/`og:description` (chosen by whoever controls a linked page, with
    // no Fauna account at all; the nest's link-preview parser applies only
    // `str::trim` plus a char-count cap). ratatui preserves ESC/BEL verbatim into
    // the grid, so an unstripped escape is a terminal injection into the reader's
    // screen. `tui.md` § Rendering owns the claim.
    //
    // **Deliberately not fixed at the sources.** The source list is open-ended —
    // the og class was found one commit after the first two were — so a
    // per-source filter is wrong by construction, and a strip at the sink costs
    // nothing legitimate: styling is applied by the renderer, never by content
    // bytes. Runs after the `'\n'` split above, which consumes newlines as row
    // *structure*; a newline surviving to here could only forge that structure.
    for line in &mut lines {
        sanitize_line(line);
    }
    PaintedLines {
        lines,
        focused_line,
        art: art_spans,
        element_at_line,
        inline_bands,
    }
}

/// The absolute focus position [`render_page`] paints against — `None`
/// outside the page zone (the sidebar has its own ring, painted through
/// [`App::sidebar_index`]). Factored out so [`focused_line_count`] computes
/// focus identically to what the terminal actually shows, rather than
/// re-deriving its own copy of this check.
fn page_focused_index(app: &App) -> Option<usize> {
    (app.zone == Zone::Page)
        .then(|| app.focused_index())
        .flatten()
}

/// How many lines the current page paints with the focus highlight
/// (`Modifier::REVERSED`) — the e2e-visible twin of the unit-layer invariant
/// `elements_sharing_an_id_do_not_all_paint_focused` pins (below). No e2e
/// driver reads pixels (`tui.md` § Rendering), and the rows this most matters
/// for are DELIBERATELY id-less (`settings/root.rs`'s rail), so no
/// per-element registry query can see them — this reuses the EXACT
/// `element_lines` call [`render_page`] paints through (via
/// [`page_focused_index`]), so automation state can never drift from what a
/// human's terminal shows.
/// A focused element on its own line carries `REVERSED` on the **line**; a
/// focused inline CELL carries it on its **span**, because the rest of the row
/// belongs to its siblings. Both count — reading only the line style made every
/// focused cell in a grid or a control row invisible here, so the invariant
/// would have reported "nothing is focused" for a screen that plainly shows
/// which cell is (and, being an equality check, red rather than merely blind).
///
/// Counting spans is safe precisely because rule 2 of the control vocabulary
/// reserves `REVERSED` for focus and nothing else (`apps/tui.md` § Rendering).
pub(crate) fn focused_line_count(app: &App) -> usize {
    element_lines(&app.page_elements(), page_focused_index(app))
        .lines
        .iter()
        .filter(|l| {
            l.style.add_modifier.contains(Modifier::REVERSED)
                || l.spans
                    .iter()
                    .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        })
        .count()
}

/// The literal text rows the current page paints, one `String` per painted
/// line — the corpus reader for the copy-comprehensibility audit
/// (`walk.rs::dump_screen_text_corpus`). Reuses the EXACT `element_lines`
/// call [`render_page`] paints through, for the same no-drift reason as
/// [`focused_line_count`]: the corpus must be what a human's terminal shows,
/// not a parallel rendering.
#[cfg(test)]
pub(crate) fn painted_page_text(app: &App) -> Vec<String> {
    element_lines(&app.page_elements(), page_focused_index(app))
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// Strip control characters from every span of one painted line, in place.
///
/// Spans are left untouched when already clean (the overwhelmingly common case),
/// so a frame pays one scan per span and no allocation. The style is never
/// touched — colour and emphasis are the renderer's, and stripping content bytes
/// is precisely what stops content from impersonating them.
fn sanitize_line(line: &mut Line<'_>) {
    for span in &mut line.spans {
        if let std::borrow::Cow::Owned(clean) =
            fauna_core::control_chars::strip_control_chars(span.content.as_ref())
        {
            span.content = std::borrow::Cow::Owned(clean);
        }
    }
}

/// [`sanitize_line`] over a whole line vec, at a widget boundary.
///
/// Every `Paragraph`/`List` in this module is built through this, so the
/// property is auditable by inspection: *no line reaches a ratatui widget
/// un-sanitized.* Re-scanning lines that [`element_lines`] already cleaned is
/// deliberate and nearly free (a clean span borrows, so there is no allocation) —
/// the alternative is a caller having to know which of its lines came from where,
/// which is exactly the per-source reasoning this gate rejects. It also covers
/// the non-element text these screens paint alongside elements: an
/// `error-message` line is **server-supplied**, so it is remote-influenced too.
fn sanitized<'a>(mut lines: Vec<Line<'a>>) -> Vec<Line<'a>> {
    for line in &mut lines {
        sanitize_line(line);
    }
    lines
}

/// What one [`element_lines`] pass produced.
struct PaintedLines {
    lines: Vec<Line<'static>>,
    /// Index of the focused element's first line, for [`scroll_offset`].
    focused_line: usize,
    /// Where each image element's art landed in `lines`.
    art: Vec<ArtSpan>,
    /// Parallel to `lines`: which index of the elements slice passed to
    /// [`element_lines`] painted that line — the hit-test counterpart of
    /// `art`/`focused_line`, consumed by [`page_hit_regions`].
    element_at_line: Vec<usize>,
    /// Where each [`inline`](crate::element::Element::inline) cell landed
    /// within the line it shares. Empty for every page that paints one element
    /// per line, which is all of them but the month grid today.
    inline_bands: Vec<InlineBand>,
}

/// One inline cell's column span on the line it shares with its run — the
/// sub-line resolution `element_at_line` cannot express.
///
/// Line-relative like [`ArtSpan`], for the same reason: [`element_lines`] knows
/// neither the viewport nor the scroll, so [`page_hit_regions`] projects these
/// onto the screen once, using the same origin/offset/lead as every other row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InlineBand {
    /// Index of the shared line within the pass.
    line: usize,
    /// First column of this cell, relative to the page area's left edge.
    col_start: u16,
    /// One past this cell's last column.
    col_end: u16,
    /// Index into the elements slice.
    element: usize,
}

/// An image element's art, located in the line list — the raw material for a
/// [`crate::graphics::Placement`].
///
/// Line-relative, not screen-relative: [`element_lines`] does not know the
/// viewport or the scroll. [`placements_of`] projects these onto the screen once
/// the caller that *does* know both has applied them.
struct ArtSpan {
    pixels: crate::thumbnail::Pixels,
    /// Index of the art's first line within the pass.
    line: usize,
    /// The art's size in cells.
    cols: u16,
    rows: u16,
    /// Cells of gutter + indent before the art on each of its lines.
    prefix: u16,
}

/// Project art spans onto the screen.
///
/// `origin` is the top-left cell *inside* the scrolled block's border, `height`
/// its visible line count, `offset` the scroll the `Paragraph` was given, and
/// `lead` the lines painted ahead of the element list (the unauthenticated
/// screen's description header; zero for a page).
///
/// **A partially-visible span is cropped, never spilled.** A protocol image is
/// not clipped by the block that contains it — the escape paints where it is
/// told, so emitting the full picture while only part of it should show would
/// spill over the border into whatever is beyond it, corrupting the frame; that
/// is never acceptable. Instead both the on-screen rect and the source's `crop`
/// window ([`crate::graphics::Placement::crop`]) shrink to exactly the visible
/// band, so a scrolled thumbnail keeps showing a crisp partial picture instead
/// of degrading to the half-block fallback for as long as it straddles the edge
/// (that fallback stays painted underneath regardless, per [`crate::graphics`]).
/// A span with no overlap with the viewport at all is dropped.
fn placements_of(
    art: Vec<ArtSpan>,
    origin: (u16, u16),
    height: usize,
    offset: u16,
    lead: usize,
) -> Vec<crate::graphics::Placement> {
    let (x, y) = origin;
    let top = offset as usize;
    let bottom = top + height;
    art.into_iter()
        .filter_map(|span| {
            let first = lead + span.line;
            let last = first + span.rows as usize;
            let visible_top = first.max(top);
            let visible_bottom = last.min(bottom);
            if visible_top >= visible_bottom {
                return None;
            }
            let rows_before = (visible_top - first) as u16;
            let visible_rows = (visible_bottom - visible_top) as u16;
            // Only carry a crop window when something was actually clipped —
            // the ordinary fully-visible case stays `None`, matching every
            // existing placement built before cropping existed.
            let crop = (visible_rows != span.rows).then_some((rows_before, span.rows));
            Some(crate::graphics::Placement {
                pixels: span.pixels,
                col: x + span.prefix,
                row: y + (visible_top - top) as u16,
                cols: span.cols,
                rows: visible_rows,
                crop,
            })
        })
        .collect()
}

/// What a mouse click on a screen row should hit — the click counterpart of a
/// keyboard [`crate::element::Gesture`]'s focus target. Both variants carry a
/// *position*, not an id: [`crate::app::App::click_sidebar`] /
/// [`crate::app::App::click_page_element`] take the same index this frame's
/// [`App::sidebar_pages`](crate::app::App::sidebar_pages) /
/// [`App::page_elements`](crate::app::App::page_elements) would give it, so a
/// click never has to re-resolve an id against a registry that may have
/// several entries sharing one (an indexed `post-card`, say).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// A position in `App::sidebar_pages()`.
    Sidebar(usize),
    /// A position in `App::page_elements()`.
    Page(usize),
}

/// One screen row this frame painted, and what a click there hits. Built fresh
/// every frame, like [`crate::graphics::Placement`] — a `Frame`'s `Rect`s are
/// closure locals, so a returned `Vec<RowHit>` is the only record of where a
/// row landed once `terminal.draw` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowHit {
    row: u16,
    col_start: u16,
    col_end: u16,
    target: HitTarget,
}

impl RowHit {
    /// What this painted row belongs to. The row exists *because* it painted
    /// inside the viewport — [`page_hit_regions`] iterates the visible band
    /// only — which is what makes the hit list a truthful answer to "what is on
    /// screen right now", the question [`crate::observation`] asks of it.
    pub fn target(&self) -> HitTarget {
        self.target
    }

    fn contains(&self, col: u16, row: u16) -> bool {
        row == self.row && col >= self.col_start && col < self.col_end
    }
}

/// Find what a click at `(col, row)` hits, or `None` off every painted row —
/// the border, a title, or genuinely outside the frame.
pub fn hit_test(hits: &[RowHit], col: u16, row: u16) -> Option<HitTarget> {
    hits.iter().find(|h| h.contains(col, row)).map(|h| h.target)
}

/// Everything one draw produced that outlives the `Frame` — a `Frame`'s `Rect`s
/// are closure locals, so these are the only record of where things landed once
/// `terminal.draw` returns.
pub struct DrawnFrame {
    /// Where the images went, for the post-paint protocol pass.
    pub placements: Vec<crate::graphics::Placement>,
    /// What a click on each painted row hits.
    pub hits: Vec<RowHit>,
    /// The page as this frame painted it.
    pub page: PaintedPage,
}

/// One element's lines in its page's scrolled line space: `first` counts from
/// the top of the scrolled content (a `lead` header included), `count` is how
/// many lines it painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    pub first: usize,
    pub count: usize,
}

impl LineSpan {
    /// The smallest span covering both — a container's extent from its parts.
    pub fn union(self, other: LineSpan) -> LineSpan {
        let first = self.first.min(other.first);
        let end = (self.first + self.count).max(other.first + other.count);
        LineSpan {
            first,
            count: end - first,
        }
    }
}

/// The page **exactly as one frame painted it**: the element list the draw
/// used, where each element's lines sit in the scrolled line space, and the
/// band of that space the viewport showed.
///
/// This is the one place tui answers *what is on screen* geometrically, rather
/// than only row-by-row like [`RowHit`]: an element whose lines straddle the
/// viewport edge is known to be partly visible, and by how much. Two readers
/// need exactly that — the automation registry's `in-viewport` attribute and
/// the engagement-cue capture ([`crate::feed::cues`]) — and both read it from
/// here, so neither re-derives the scroll (**the viewport clips paint only**;
/// the element list is the registry, see [`crate::element`]).
///
/// The element list is carried rather than re-read from the app because a
/// manager's snapshot can move on a worker thread between the draw and a later
/// read, and a span is only meaningful against the list it was measured on.
#[derive(Debug, Clone, Default)]
pub struct PaintedPage {
    pub elements: Vec<crate::element::Element>,
    /// Parallel to `elements`; `None` for an element that painted no line.
    spans: Vec<Option<LineSpan>>,
    /// First line of the painted band.
    top: usize,
    /// How many lines the band shows.
    height: usize,
}

impl PaintedPage {
    /// A page listed but not drawn — no geometry at all, so every
    /// [`in_view`](Self::in_view) answers `None`. The registry built outside a
    /// draw (tier_1 tests) uses this.
    #[cfg(test)]
    pub fn unpainted(elements: Vec<crate::element::Element>) -> Self {
        let spans = vec![None; elements.len()];
        PaintedPage {
            elements,
            spans,
            top: 0,
            height: 0,
        }
    }

    /// Measure a painted element list, from the same `element_at_line` map and
    /// origin/offset/lead every other per-frame projection here uses
    /// ([`page_hit_regions`], [`placements_of`]).
    fn measure(
        elements: Vec<crate::element::Element>,
        element_at_line: &[usize],
        inline_bands: &[InlineBand],
        lead: usize,
        offset: u16,
        height: usize,
    ) -> Self {
        let mut spans: Vec<Option<LineSpan>> = vec![None; elements.len()];
        let mut extend = |element: usize, line: usize| {
            let line = LineSpan {
                first: lead + line,
                count: 1,
            };
            if let Some(slot) = spans.get_mut(element) {
                *slot = Some(slot.map_or(line, |span| span.union(line)));
            }
        };
        for (line, &element) in element_at_line.iter().enumerate() {
            extend(element, line);
        }
        // An inline cell shares its run's line, which `element_at_line` credits
        // to the run's first cell only.
        for band in inline_bands {
            extend(band.element, band.line);
        }
        PaintedPage {
            elements,
            spans,
            top: offset as usize,
            height,
        }
    }

    /// Where element `index`'s lines landed, or `None` if it painted none.
    pub fn span(&self, index: usize) -> Option<LineSpan> {
        self.spans.get(index).copied().flatten()
    }

    /// The painted band, `[top, bottom)`, in the same line space as
    /// [`span`](Self::span).
    pub fn band(&self) -> (usize, usize) {
        (self.top, self.top + self.height)
    }

    /// Whether element `index` is in view: its middle line lies inside the
    /// painted band — linux's rule (a widget's vertical centre inside its
    /// scroller's visible band), in lines. `None` when it painted no line, so a
    /// caller can tell "out of view" from "cannot say".
    pub fn in_view(&self, index: usize) -> Option<bool> {
        let span = self.span(index)?;
        let middle = span.first + (span.count - 1) / 2;
        let (top, bottom) = self.band();
        Some(middle >= top && middle < bottom)
    }

    /// Measure `elements` painted with no lead into a `height`-line viewport
    /// scrolled to `top` — the real [`element_lines`] pass, so a test's spans
    /// are the ones a draw would produce.
    #[cfg(test)]
    pub(crate) fn for_test(
        elements: Vec<crate::element::Element>,
        top: u16,
        height: usize,
    ) -> Self {
        let painted = element_lines(&elements, None);
        Self::measure(
            elements,
            &painted.element_at_line,
            &painted.inline_bands,
            0,
            top,
            height,
        )
    }
}

/// Project a page's `element_at_line` map onto the screen — the hit-test
/// counterpart of [`placements_of`], built from the exact same origin/height/
/// offset/lead so a click resolves without a second geometry pass that could
/// drift from where the `Paragraph` actually scrolled to.
fn page_hit_regions(
    element_at_line: &[usize],
    inline_bands: &[InlineBand],
    origin: (u16, u16),
    width: u16,
    height: usize,
    offset: u16,
    lead: usize,
) -> Vec<RowHit> {
    let (x, y) = origin;
    let top = offset as usize;
    (0..height)
        .flat_map(|row_in_view| {
            let mut row_hits: Vec<RowHit> = Vec::new();
            let Some(element_line) = (top + row_in_view).checked_sub(lead) else {
                return row_hits;
            };
            let row = y + row_in_view as u16;
            // A line carrying inline cells resolves per column band, so a click
            // hits the day cell under the cursor rather than the whole week.
            // Pushed FIRST because `hit_test` takes the first containing
            // region, making the narrow bands win over the full-width fallback
            // below them.
            row_hits.extend(
                inline_bands
                    .iter()
                    .filter(|band| band.line == element_line)
                    .map(|band| RowHit {
                        row,
                        col_start: x + band.col_start,
                        col_end: x + band.col_end,
                        target: HitTarget::Page(band.element),
                    }),
            );
            if let Some(&element_index) = element_at_line.get(element_line) {
                row_hits.push(RowHit {
                    row,
                    col_start: x,
                    col_end: x + width,
                    target: HitTarget::Page(element_index),
                });
            }
            row_hits
        })
        .collect()
}

/// The sidebar's rows, one per visible page — no scroll to account for (the
/// gated 12/13-row list always fits a real terminal), so this is a direct
/// row → position map rather than mirroring `page_hit_regions`' offset math.
fn sidebar_hit_regions(row_count: usize, area: ratatui::layout::Rect) -> Vec<RowHit> {
    (0..row_count)
        .filter(|&i| (i as u16) < area.height)
        .map(|i| RowHit {
            row: area.y + i as u16,
            col_start: area.x,
            col_end: area.x + area.width,
            target: HitTarget::Sidebar(i),
        })
        .collect()
}

/// The scroll offset that keeps `focused_line` inside a `height`-tall viewport.
///
/// **The viewport clips paint only** — never the element list, which *is* the
/// automation registry (see [`crate::element`]). A feed of 40 posts registers 40
/// `post-card`s however few of them fit; this only decides which rows get
/// pixels. Without it a focused element below the fold would simply be
/// invisible: fine for wizard pages that fit, broken for a feed.
fn scroll_offset(focused_line: usize, total: usize, height: usize) -> u16 {
    if height == 0 || total <= height {
        return 0;
    }
    let max_offset = total - height;
    // Keep the focused line on screen, biased to leave context above it.
    let offset = focused_line.saturating_sub(height / 2);
    offset.min(max_offset) as u16
}

/// The unauthenticated surface — the launch flow's own screen (`launch_retry`,
/// the "Signing you in…" spinner) or the onboarding wizard's current page,
/// painted from whichever owns the screen.
fn render_screen(frame: &mut Frame, app: &App) -> DrawnFrame {
    let [status_area, body_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .areas(frame.area());
    frame.render_widget(
        Paragraph::new(connection_status_text(app.connection)),
        status_area,
    );

    let mut lines: Vec<Line> = app
        .screen_description()
        .into_iter()
        .map(|d| Line::styled(d, Style::default().add_modifier(Modifier::DIM)))
        .collect();
    lines.push(Line::from(""));
    let header = lines.len();

    let elements = app.page_elements();
    let painted = element_lines(&elements, app.focused_index());
    lines.extend(painted.lines);

    // error-message (+ the injected warning/info lines): a line only exists
    // when there is a message to show, so paint and registry agree on
    // visibility.
    if let Some(error) = app.error_line_text() {
        lines.push(Line::from(""));
        lines.push(Line::from(error));
    }
    for msg in [&app.injected_warning, &app.injected_info] {
        if let Some(text) = msg.clone().filter(|t| !t.is_empty()) {
            lines.push(Line::from(text));
        }
    }

    let height = body_area.height.saturating_sub(2) as usize; // the block's borders
    let offset = scroll_offset(header + painted.focused_line, lines.len(), height);
    frame.render_widget(
        Paragraph::new(sanitized(lines)).scroll((offset, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.screen_title()),
        ),
        body_area,
    );

    // No wizard or launch page paints art today, so this is all but always
    // empty — but it is derived from the same offsets as every other surface
    // rather than assumed away, so the day one does, it simply works.
    let placements = placements_of(
        painted.art,
        (body_area.x + 1, body_area.y + 1),
        height,
        offset,
        header,
    );
    let hits = page_hit_regions(
        &painted.element_at_line,
        &painted.inline_bands,
        (body_area.x + 1, body_area.y + 1),
        body_area.width.saturating_sub(2),
        height,
        offset,
        header,
    );
    let page = PaintedPage::measure(
        elements,
        &painted.element_at_line,
        &painted.inline_bands,
        header,
        offset,
        height,
    );
    DrawnFrame {
        placements,
        hits,
        page,
    }
}

/// The authenticated shell: sidebar + current page.
fn render_shell(frame: &mut Frame, app: &App) -> DrawnFrame {
    // The critical-alerts band spans the FULL width above both panes — the
    // terminal analogue of linux mounting its banner above the content stack
    // (`apps/fauna-linux/src/app.rs`), and the only placement that is at once
    // prominent and wide enough for a multi-sentence alert. The 24-column
    // sidebar, where the other permanent chrome (`supervised-indicator`) lives,
    // would wrap this into an unreadable column. Zero-height when no alert is
    // active, so the everyday layout is byte-identical to before.
    let alerts =
        crate::critical_alerts::wrapped_lines(&critical_alert_lines(app), frame.area().width);
    let [alerts_area, shell_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(alerts.len() as u16), Constraint::Min(1)])
        .areas(frame.area());
    if !alerts.is_empty() {
        // Deliberately loud, and deliberately not dismissable: possible
        // compromise or imminent unrecoverable loss only
        // (`critical-alerts.md` § the severity bar).
        frame.render_widget(
            Paragraph::new(sanitized(
                alerts.into_iter().map(Line::from).collect::<Vec<_>>(),
            ))
            .style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            alerts_area,
        );
    }

    // The key-hint footer spans the FULL width beneath both panes — it describes
    // the shell's keys, not one pane's, and the 24-column sidebar could not hold
    // the row without wrapping it. Split before the sidebar/page split so both
    // panes shorten by the one row rather than the footer overpainting content.
    // `Min(1)` keeps the body ahead of the footer when the terminal is too short
    // for both: a one-row terminal shows content and drops the hint, never the
    // reverse. Zero-height when the footer is absent (the add-account wizard
    // over a live session), so that screen's layout is byte-identical to before.
    let hints = nav_key_hints_visible(app);
    let [body_area, hints_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(u16::from(hints))])
        .areas(shell_area);

    let [sidebar_area, page_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(1)])
        .areas(body_area);

    let mut hits = render_sidebar(frame, app, sidebar_area);
    let mut drawn = render_page(frame, app, page_area);
    hits.append(&mut drawn.hits);
    drawn.hits = hits;

    // Dimmed: permanently-visible chrome that must never compete with content
    // for the eye. Through `sanitized` like every other line this module paints
    // (`apps/tui.md` § Rendering — the strip is at the funnel, not the sources),
    // even though this text is composed from i18n copy and key glyphs only.
    if hints {
        frame.render_widget(
            Paragraph::new(sanitized(vec![Line::from(nav_key_hints_text(app))]))
                .style(Style::default().add_modifier(Modifier::DIM)),
            hints_area,
        );
    }

    drawn
}

fn render_sidebar(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) -> Vec<RowHit> {
    // The status header is connection-status plus — on builds that drive a local
    // agent — the sync-agent-status line and its dim version/uptime subtitle,
    // the same stack linux pins above its sidebar tabs. A supervised account
    // adds the permanent `supervised-indicator` line beneath them.
    let agent_status = app.sync_agent.rendered_status();
    let supervised = crate::family::supervised_indicator_text(&app.family);
    let status_height =
        if agent_status.is_some() { 3 } else { 1 } + u16::from(supervised.is_some());
    let [status_area, tabs_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(status_height), Constraint::Min(1)])
        .areas(area);

    // connection-status — pinned above the tab rows, like the linux sidebar.
    let mut status_lines: Vec<Line<'static>> =
        vec![Line::from(connection_status_text(app.connection))];
    if let Some(status) = agent_status {
        // Distinct concept from connection-status directly above it: that is the
        // nest WS-RPC link, this is whether the LOCAL agent process is running
        // and current (`sync-agent.md` § Local agent health).
        status_lines.push(Line::from(sync_agent_status_text(status.health)));
        // Both children are empty unless the agent answered, so a "Not running"
        // reading leaves this subtitle blank rather than showing stale values.
        let subtitle = [status.version, status.uptime]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  ");
        status_lines.push(Line::from(subtitle).style(Style::default().add_modifier(Modifier::DIM)));
    }
    // `supervised-indicator` (ui.yaml `global`) — permanent, non-dismissable
    // chrome on every authenticated page (`family-safety.md` § The trust shape
    // invariant 4: supervision is never silent). The sidebar header is the tui
    // analogue of linux's header-bar placement: it is outside the page pane, so
    // it survives every page switch including the Settings and Admin shells.
    if let Some(text) = supervised {
        status_lines.push(Line::from(text).style(Style::default().add_modifier(Modifier::BOLD)));
    }
    frame.render_widget(Paragraph::new(sanitized(status_lines)), status_area);

    // The gate-aware visible rows (`App::sidebar_pages`): the 12 always-on
    // pages plus the gated `admin-tab` when the user is an admin. Paint reads
    // the SAME list the registry and focus ring do, so the highlight index can
    // never drift from what `screen_elements` registered.
    let rows = app.sidebar_pages();
    let items: Vec<ListItem> = rows.iter().map(|p| ListItem::new(p.label())).collect();
    // The selected row is always highlighted (it *is* the painted page), but it
    // only reads as *focused* — reversed, with the `>` caret — while the
    // keyboard is actually in this pane. Otherwise the user sees two focus rings
    // and cannot tell which one their arrows drive.
    let focused = app.zone == Zone::Sidebar;
    let highlight = if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::RIGHT))
        .highlight_style(highlight)
        .highlight_symbol(if focused { "> " } else { "  " });
    let mut state = ListState::default().with_selected(app.sidebar_index());
    frame.render_stateful_widget(list, tabs_area, &mut state);
    sidebar_hit_regions(rows.len(), tabs_area)
}

fn render_page(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) -> DrawnFrame {
    // error-message — every page has one (ui.yaml); blank when no error. The
    // injected warning/info lines (the test-agent `messages` patch) extend the
    // banner band below it only while set, so the everyday layout is unchanged.
    let mut banner_lines = vec![Line::from(app.error_line_text().unwrap_or_default())];
    for msg in [&app.injected_warning, &app.injected_info] {
        if let Some(text) = msg.clone().filter(|t| !t.is_empty()) {
            banner_lines.push(Line::from(text));
        }
    }
    let [error_area, body_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(banner_lines.len() as u16),
            Constraint::Min(1),
        ])
        .areas(area);
    frame.render_widget(Paragraph::new(sanitized(banner_lines)), error_area);

    // The page's own elements, from the same list the registry reads. Pages
    // still building out (M4+) contribute none and paint an empty titled pane.
    let elements = app.page_elements();
    let painted = element_lines(&elements, page_focused_index(app));
    let height = body_area.height.saturating_sub(2) as usize; // the block's borders
    let offset = scroll_offset(painted.focused_line, painted.lines.len(), height);
    frame.render_widget(
        Paragraph::new(sanitized(painted.lines))
            .scroll((offset, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(app.page.label()),
            ),
        body_area,
    );

    // The Media page's thumbnails. `offset` is the *same* value the `Paragraph`
    // above just scrolled by — which is the whole reason this is computed here
    // and not by a post-paint pass reading the app again.
    let placements = placements_of(
        painted.art,
        (body_area.x + 1, body_area.y + 1),
        height,
        offset,
        0,
    );
    let hits = page_hit_regions(
        &painted.element_at_line,
        &painted.inline_bands,
        (body_area.x + 1, body_area.y + 1),
        body_area.width.saturating_sub(2),
        height,
        offset,
        0,
    );
    let page = PaintedPage::measure(
        elements,
        &painted.element_at_line,
        &painted.inline_bands,
        0,
        offset,
        height,
    );
    DrawnFrame {
        placements,
        hits,
        page,
    }
}

/// The texts one element slice actually **paints**, one entry per painted line —
/// the test-only seam a page's paint assertions read.
///
/// It exists because the registry cannot answer this question. `crate::element`'s
/// invariant is "the element list IS the registry; the viewport clips paint
/// only", and the inverse is a blind spot: anything registered is reachable by
/// `is_visible`/`click`/`get_text` whether or not a pixel of it painted where a
/// human could see it. The month grid shipped for a day painting all 42 day
/// cells on ONE clipped line while every id assertion passed
/// ([`crate::element::Element::starts_row`] tells that story). A page whose
/// layout is load-bearing asserts through here as well as through its ids.
#[cfg(test)]
pub(crate) fn painted_line_texts(elements: &[crate::element::Element]) -> Vec<String> {
    element_lines(elements, None)
        .lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Element;
    use fauna_ui_ids as ids;

    /// Each of the five `sync-agent-status` readings paints its own shared
    /// string — *Keys pending* and *Not enrolled* included, which must never
    /// fall back onto another reading's words (`sync-agent.md` § Local agent
    /// health).
    #[test]
    fn sync_agent_status_paints_a_distinct_string_per_reading() {
        use crate::sync_agent::AgentHealth;
        let texts = [
            AgentHealth::Running,
            AgentHealth::RestartPending,
            AgentHealth::KeysPending,
            AgentHealth::NotRunning,
            AgentHealth::NotEnrolled,
        ]
        .map(sync_agent_status_text);
        assert_eq!(texts[2], "Keys pending");
        assert_eq!(texts[4], "Not enrolled");
        let distinct: std::collections::BTreeSet<_> = texts.iter().collect();
        assert_eq!(distinct.len(), 5, "{texts:?}");
    }

    /// The rendered text of one painted `Line`, spans concatenated.
    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// A multi-line element body paints one `Line` **per text row**.
    ///
    /// Regression: a `Line` built from a string containing `\n` turns each
    /// newline into a span boundary and concatenates every row onto a single
    /// row of the terminal. That silently flattened the `identity-export-qr`
    /// block art — a QR painted as one strip is unscannable — and would flatten
    /// any half-block art the same way.
    #[test]
    fn multiline_body_paints_one_line_per_row() {
        let lines = element_lines(&[Element::label("probe", "AAA\nBBB\nCCC")], None).lines;
        assert_eq!(lines.len(), 3, "3 text rows must paint as 3 Lines");
        assert_eq!(line_text(&lines[0]), "  AAA");
        // Continuation rows align under the first row's gutter, so a grid of
        // block art stays square rather than stair-stepping.
        assert_eq!(line_text(&lines[1]), "  BBB");
        assert_eq!(line_text(&lines[2]), "  CCC");
    }

    /// An ANSI/OSC payload of the shape a hostile author actually sends: clear
    /// the screen, home the cursor, then set the terminal's window title —
    /// followed by innocuous-looking text so the row still reads as content.
    ///
    /// `\x1b]0;…\x07` is the interesting half: an OSC window-title set is
    /// something ratatui itself never emits, so its absence downstream is a
    /// discriminating observation rather than a vacuous one.
    const ESCAPE_PAYLOAD: &str = "\x1b[2J\x1b[H\x1b]0;pwned\x07Innocent Headline";

    /// No control character from remote-authored content survives into a painted
    /// `Line` — asserted across an og-metadata sink, a remote-chosen display
    /// name, and a walked document body **in one assertion**.
    ///
    /// This is the gate (`tui.md` § Rendering). Every one of these three
    /// strings is chosen by someone other than the user reading them: an
    /// `og:title` is chosen by whoever controls a linked web page (no Fauna
    /// account needed at all — the nest's link-preview parser applies only
    /// `str::trim` and a char-count cap), `post-author` is a remote-chosen
    /// display name, and the document body is the post text. `ui.rs` is the one
    /// funnel all three reach the cell grid through, and a probe proved ratatui
    /// preserves ESC/BEL verbatim into that grid, so an unstripped escape is a
    /// terminal injection into the reader's screen.
    ///
    /// **Deliberately one assertion over all three sinks**: the source list is
    /// open-ended (the og class was found one commit after the first two), so a
    /// per-source fix must fail this test. Sanitizing only the sink you happen
    /// to be looking at is the failure mode this pin exists to reject.
    #[test]
    fn no_control_character_from_remote_content_reaches_a_painted_line() {
        use fauna_core::render::{Inline, RenderBlock, RenderDocument};

        let body = RenderDocument {
            blocks: vec![RenderBlock::Paragraph {
                inlines: vec![Inline::Text {
                    text: ESCAPE_PAYLOAD.to_string(),
                }],
            }],
        };
        let elements = [
            // The og-metadata sink, as `link_preview_bubble_elements` builds it.
            Element::label(ids::LINK_PREVIEW_TITLE, ESCAPE_PAYLOAD),
            // A non-og sink: the remote-chosen display name.
            Element::label(ids::POST_AUTHOR, ESCAPE_PAYLOAD),
            // The walked-document arm — a different code path to the same grid.
            Element::document(ids::FEED_POST_TEXT, body),
        ];

        let lines = element_lines(&elements, None).lines;

        for line in &lines {
            let text = line_text(line);
            assert!(
                !text.chars().any(char::is_control),
                "a painted line carries control characters from remote-authored \
                 content — terminal injection into the reader's screen: {text:?}"
            );
        }
        // The strip must remove the escapes, not the content: a fix that painted
        // nothing at all would satisfy the loop above vacuously.
        let all: String = lines.iter().map(line_text).collect();
        assert!(
            all.contains("Innocent Headline"),
            "the legitimate text must still paint; got {all:?}"
        );
    }

    /// A two-row thumbnail: a red cell over a blue one, for the paint tests.
    /// Carries both arms' representations, as the real rasterizer always does.
    fn probe_art() -> crate::thumbnail::Thumbnail {
        use crate::thumbnail::{HalfBlockArt, HalfBlockCell, Thumbnail};
        Thumbnail {
            art: HalfBlockArt {
                rows: vec![
                    vec![HalfBlockCell {
                        top: [255, 0, 0],
                        bottom: [0, 255, 0],
                    }],
                    vec![HalfBlockCell {
                        top: [0, 0, 255],
                        bottom: [255, 255, 0],
                    }],
                ],
            },
            pixels: std::sync::Arc::new(image::RgbImage::from_pixel(1, 4, image::Rgb([255, 0, 0]))),
        }
    }

    /// Half-block art paints one `Line` per art row, each cell carrying its OWN
    /// colour pair: fg = the top pixel, bg = the bottom.
    ///
    /// This is the whole reason `Element::art` exists as a third arm. The
    /// styled-text arm gives an entire row one `Style`, so an image forced
    /// through it would paint as a single flat colour — a "thumbnail" that
    /// proves only that N glyphs were emitted.
    #[test]
    fn art_paints_per_cell_colour_pairs() {
        let lines = element_lines(
            &[Element::thumbnail(ids::MEDIA_THUMBNAIL, probe_art())],
            None,
        )
        .lines;
        assert_eq!(lines.len(), 2, "2 art rows must paint as 2 Lines");

        // Row 0: the gutter span, then one cell span.
        let cell = &lines[0].spans[1];
        assert_eq!(cell.content.as_ref(), crate::thumbnail::HALF_BLOCK);
        assert_eq!(
            cell.style.fg,
            Some(Color::Rgb(255, 0, 0)),
            "fg is the cell's TOP pixel"
        );
        assert_eq!(
            cell.style.bg,
            Some(Color::Rgb(0, 255, 0)),
            "bg is the cell's BOTTOM pixel"
        );

        // Row 1 carries different colours — proving each cell is styled from its
        // own pixels rather than the whole element sharing one style.
        let next = &lines[1].spans[1];
        assert_eq!(next.style.fg, Some(Color::Rgb(0, 0, 255)));
        assert_eq!(next.style.bg, Some(Color::Rgb(255, 255, 0)));
    }

    /// Art rows carry the same gutter/indent as every other element, so a
    /// thumbnail nested under an indexed `media-item` still lines up beneath it.
    #[test]
    fn art_rows_keep_the_element_gutter_and_indent() {
        let painted = element_lines(
            &[Element::thumbnail(ids::MEDIA_THUMBNAIL, probe_art()).within(ids::MEDIA_ITEM, 0)],
            Some(0),
        );
        let (lines, focused_line) = (painted.lines, painted.focused_line);
        assert_eq!(focused_line, 0, "focus points at the art's first row");
        assert_eq!(lines[0].spans[0].content.as_ref(), ">   ");
        assert_eq!(
            lines[1].spans[0].content.as_ref(),
            "    ",
            "continuation rows align under the gutter, keeping the block square"
        );
    }

    /// The focus gutter marks the element's FIRST row, and later rows align
    /// under it — so a focused multi-row element still reads as one block.
    #[test]
    fn multiline_body_gutter_marks_only_the_first_row() {
        let painted = element_lines(&[Element::label("probe", "AAA\nBBB")], Some(0));
        let (lines, focused_line) = (painted.lines, painted.focused_line);
        assert_eq!(focused_line, 0, "focus points at the element's first row");
        assert_eq!(line_text(&lines[0]), "> AAA");
        assert_eq!(line_text(&lines[1]), "  BBB");
    }

    /// **Exactly one** painted line wears the focus affordance, and that
    /// affordance is distinct from every *state* style in the pane's
    /// vocabulary. The field report this pins came from a live user who could
    /// not tell focus from state — "all options equally selected" — against a
    /// page where enabled and disabled rows sit together, which is precisely
    /// the shape below.
    ///
    /// The `> ` gutter is deliberately NOT the assertion: it was already there
    /// when the user reported the failure. Styles are data in the `Line` model,
    /// so the affordance a human actually sees is headlessly assertable, and
    /// this is that assertion (`tui.md` § Rendering).
    #[test]
    fn exactly_one_line_wears_the_focus_affordance_and_it_beats_state_styling() {
        let painted = element_lines(
            &[
                Element::label("plain", "AAA"),
                // A focused element whose body spans rows: only its first row
                // may wear the affordance, or "exactly one" is a lie the moment
                // any multi-row element takes focus.
                Element::label("focused", "BBB\nCCC"),
                Element::label("disabled", "DDD").enabled(false),
            ],
            Some(1),
        );
        let lines = painted.lines;

        let focused: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.style.add_modifier.contains(Modifier::REVERSED))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            focused,
            vec![1],
            "exactly one painted line may wear the focus affordance, and it is \
             the focused element's FIRST row — got {}",
            lines
                .iter()
                .map(|l| format!("{:?}:{:?}", line_text(l), l.style.add_modifier))
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Focus must not be confusable with any state style. The pane's whole
        // row-level state vocabulary is default (enabled) and DIM (disabled);
        // a future state style that reached for REVERSED would fail here, which
        // is the point.
        let focus_style = lines[1].style.add_modifier;
        for (i, line) in lines.iter().enumerate() {
            if i == 1 {
                continue;
            }
            assert_ne!(
                line.style.add_modifier,
                focus_style,
                "line {i} ({:?}) wears the focus style while unfocused — focus \
                 and state must stay visually distinct",
                line_text(line)
            );
        }
        assert!(
            lines[3].style.add_modifier.contains(Modifier::DIM),
            "the disabled row must keep its own state styling — the focus fix \
             must not flatten state into invisibility"
        );
    }

    /// A pathological-but-real case the test above cannot see: several elements
    /// sharing the SAME id. `settings/root.rs`'s rail deliberately gives most rows
    /// a blank id (no ui.yaml rail-row id is scoped for them — the e2e reaches a
    /// sub-page via the `nav` patch, not a rail click), so the Settings page alone
    /// paints ~19 rows with `id == ""`. Comparing focus by id string therefore
    /// marked EVERY blank-id row focused the moment focus landed on any one of
    /// them — exactly the live-user report "all menu choices except the top 2-3
    /// are highlighted together." Focus is keyed by POSITION now
    /// ([`crate::app::App::focused_index`]), which stays unique even when ids
    /// collide (or are all blank).
    #[test]
    fn elements_sharing_an_id_do_not_all_paint_focused() {
        let painted = element_lines(
            &[
                Element::label("plain", "AAA"),
                Element::label("", "Privacy"),
                Element::label("", "Muted words"),
                Element::label("", "Devices"),
            ],
            Some(2),
        );
        let lines = painted.lines;

        let focused: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.style.add_modifier.contains(Modifier::REVERSED))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            focused,
            vec![2],
            "exactly one row may paint focused even when several rows share an \
             id — got REVERSED lines {focused:?} of {}",
            lines.len()
        );
    }

    /// A single-line body is unaffected — the common case keeps its exact shape.
    #[test]
    fn single_line_body_is_unchanged() {
        let lines = element_lines(&[Element::label("probe", "hello")], None).lines;
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  hello");
    }

    /// A read-only value given a human label paints as "prompt: value" — a MUA
    /// row's "IMAP host: mail.example.org". Without it the connection details
    /// are a column of bare hostnames and ports with nothing saying which is
    /// which. The label is paint-only: [`Element::text`] stays the bare value,
    /// because that is what the registry hands `get_text` and what the
    /// cross-app assertions read.
    #[test]
    fn a_labelled_value_paints_its_prompt_but_registers_the_bare_value() {
        let el = Element::label(ids::MAIL_SETTINGS_MUA_IMAP_HOST, "mail.example.org")
            .labelled("IMAP host");
        assert_eq!(el.text, "mail.example.org");

        let lines = element_lines(std::slice::from_ref(&el), None).lines;
        assert_eq!(line_text(&lines[0]), "  IMAP host: mail.example.org");
    }

    /// **A destination and an action must not paint alike.** Brackets are the
    /// "acts now" promise (`apps/tui.md` § Rendering → *Control vocabulary*), so
    /// a nav button drops them and takes the trailing ▸ — the fix for a Settings
    /// rail on which `[ Copy ]` and `[ Account ]` were indistinguishable, which
    /// a live user reported as not knowing what was "navigational?".
    #[test]
    fn a_nav_button_paints_as_a_destination_and_a_plain_one_as_an_action() {
        let gesture = crate::element::Gesture::Settings(crate::settings::Action::OpenAccount);
        let nav = Element::gesture_button(ids::PROBE_NAV, "Account", true, gesture.clone()).nav();
        let action = Element::gesture_button(ids::PROBE_ACTION, "Copy", true, gesture);

        assert_eq!(
            line_text(&element_lines(std::slice::from_ref(&nav), None).lines[0]),
            "  Account ▸",
            "a destination drops the brackets"
        );
        assert_eq!(
            line_text(&element_lines(std::slice::from_ref(&action), None).lines[0]),
            "  [ Copy ]",
            "an action keeps them"
        );
    }

    /// **A way out points the other way.** Rule 4 takes the brackets off every
    /// destination, and a Back button is one — but the forward glyph would then
    /// have the screen promising *onward* for a control that goes back, which is
    /// worse than the `[ Back ]` it replaced. So `nav_back` paints the mirrored
    /// glyph, LEADING (`apps/tui.md` § Rendering → *Control vocabulary*, rule 5).
    ///
    /// The arm ordering is the load-bearing half: `nav_back` implies `nav`, so a
    /// paint that tested `nav` first would swallow every back button and this
    /// assertion is what holds the two arms in order.
    #[test]
    fn a_back_button_paints_the_mirrored_glyph_and_never_the_forward_one() {
        let gesture = crate::element::Gesture::Settings(crate::settings::Action::NavBack);
        let back = Element::gesture_button(ids::PROBE_BACK, "Back", true, gesture).nav_back();

        assert!(back.nav, "a way out is still a destination");
        assert_eq!(
            line_text(&element_lines(std::slice::from_ref(&back), None).lines[0]),
            "  ◂ Back",
            "the mirrored glyph, leading — never `[ Back ]` and never `Back ▸`"
        );
        assert_eq!(back.text, "Back", "the flag is paint-only, like `nav`");
    }

    /// The flag is **paint-only**: a nav row stays a button to the registry and
    /// the automation surface, so `get_text` reports the bare label and every
    /// driver keeps reaching it exactly as before. A nav idiom that leaked into
    /// `text` would rewrite what every cross-app assertion reads.
    #[test]
    fn nav_ness_never_reaches_the_registered_text() {
        let el = Element::gesture_button(
            ids::PROBE_NAV,
            "Account",
            true,
            crate::element::Gesture::Settings(crate::settings::Action::OpenAccount),
        )
        .nav();
        assert_eq!(el.text, "Account");
        assert!(el.nav);
        assert!(el.enabled);
    }

    /// An element carrying [`crate::element::Element::colors`] paints with that
    /// EXPLICIT (fg, bg) pair rather than inheriting the terminal theme — the
    /// identity-export QR's need (`settings.md` § Identity export: "a
    /// theme-inverted QR does not scan"; apple hit exactly this bug).
    ///
    /// Every row of a multi-row body carries the pair, not just the first — a
    /// QR is many text rows and every one must stay dark-on-light.
    #[test]
    fn element_with_explicit_colors_overrides_theme_inheritance() {
        let el = Element::label("probe", "AAA\nBBB").colors([0, 0, 0], [255, 255, 255]);
        let lines = element_lines(&[el], None).lines;
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert_eq!(
                line.style.fg,
                Some(Color::Rgb(0, 0, 0)),
                "fg must be the explicit colour, not theme-inherited"
            );
            assert_eq!(
                line.style.bg,
                Some(Color::Rgb(255, 255, 255)),
                "bg must be the explicit colour, not theme-inherited"
            );
        }
    }

    /// Without an explicit pair, an element still paints with NO fg/bg set — the
    /// theme-inheriting default every element had before this field existed.
    #[test]
    fn element_without_explicit_colors_inherits_theme() {
        let lines = element_lines(&[Element::label("probe", "hello")], None).lines;
        assert_eq!(lines[0].style.fg, None);
        assert_eq!(lines[0].style.bg, None);
    }

    /// A span with no art content of its own — just enough to drive
    /// [`placements_of`], which never looks at the pixels themselves.
    fn art_span(line: usize, rows: u16) -> ArtSpan {
        ArtSpan {
            pixels: std::sync::Arc::new(image::RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 0]))),
            line,
            cols: 4,
            rows,
            prefix: 0,
        }
    }

    /// A fully-visible span carries no crop — the ordinary case, unaffected by
    /// cropping's introduction.
    #[test]
    fn a_fully_visible_span_is_placed_uncropped() {
        // Viewport: origin (0,0), 5 lines tall, no scroll, no lead.
        let placements = placements_of(vec![art_span(0, 5)], (0, 0), 5, 0, 0);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].row, 0);
        assert_eq!(placements[0].rows, 5);
        assert_eq!(placements[0].crop, None);
    }

    /// A span clipped at the BOTTOM of the viewport is placed at the top of its
    /// visible band, shrunk to just the rows the viewport still shows, and
    /// cropped starting at row 0 of its own original extent.
    #[test]
    fn a_span_clipped_at_the_bottom_is_cropped_not_dropped() {
        // Viewport shows lines [0, 5); the span spans lines [3, 8) — only its
        // first 2 rows are visible.
        let placements = placements_of(vec![art_span(3, 5)], (0, 0), 5, 0, 0);
        assert_eq!(placements.len(), 1, "must not be dropped");
        let p = &placements[0];
        assert_eq!(p.row, 3, "starts where the span first becomes visible");
        assert_eq!(p.rows, 2, "shrunk to the 2 rows still on screen");
        assert_eq!(
            p.crop,
            Some((0, 5)),
            "crop starts at the span's own row 0, out of its original 5"
        );
    }

    /// A span clipped at the TOP (scrolled partway past) lands at the
    /// viewport's own top row, shrunk to what remains, cropped starting partway
    /// into its original extent — the mirror of the bottom-clip case.
    #[test]
    fn a_span_clipped_at_the_top_is_cropped_not_dropped() {
        // Scrolled so the viewport shows lines [3, 8); the span spans [0, 5) —
        // only its last 2 rows are still visible.
        let placements = placements_of(vec![art_span(0, 5)], (0, 0), 5, 3, 0);
        assert_eq!(placements.len(), 1, "must not be dropped");
        let p = &placements[0];
        assert_eq!(p.row, 0, "pinned to the viewport's own top");
        assert_eq!(p.rows, 2, "shrunk to the 2 rows still on screen");
        assert_eq!(
            p.crop,
            Some((3, 5)),
            "crop skips the 3 rows that scrolled off, out of the original 5"
        );
    }

    /// A span with NO overlap with the viewport at all is still dropped — only
    /// a genuinely partial overlap gets cropped.
    #[test]
    fn a_span_entirely_off_screen_is_dropped() {
        let placements = placements_of(vec![art_span(10, 2)], (0, 0), 5, 0, 0);
        assert!(placements.is_empty());
    }

    /// `element_at_line` is parallel to `lines`: a multi-row element (the
    /// two-row thumbnail every other art test already uses) maps BOTH of its
    /// rows back to its own index, not just its first — the hit-test
    /// counterpart of `art_spans` recording only where art landed.
    #[test]
    fn element_at_line_maps_every_output_line_back_to_its_source_element() {
        let elements = vec![
            Element::label("a", "one"),
            Element::thumbnail("b", probe_art()),
            Element::label("c", "three"),
        ];
        let painted = element_lines(&elements, None);
        assert_eq!(painted.lines.len(), 4, "1 + 2 (art) + 1 output lines");
        assert_eq!(painted.element_at_line, vec![0, 1, 1, 2]);
    }

    /// A grid is ROWS of cells, and
    /// [`starts_row`](crate::element::Element::starts_row) is what says where
    /// one row ends and the next begins.
    ///
    /// Regression pin for a paint bug the whole e2e suite was blind to: the
    /// month grid pushed all 42 day cells as one unbroken inline run, so the
    /// six week rows painted end to end on ONE line and every cell past the
    /// terminal's width was invisible. Every id was still *registered*, so
    /// `has_day_cell`/`click_day_cell` passed against a grid a human could not
    /// see — the element-list-is-the-registry invariant cuts both ways, and
    /// paint therefore needs its own assertions.
    #[test]
    fn starts_row_breaks_an_inline_run_into_grid_rows() {
        let elements = vec![
            Element::label("a1", " 1 ").starts_row(),
            Element::label("a2", " 2 ").inline(),
            Element::label("b1", " 3 ").starts_row(),
            Element::label("b2", " 4 ").inline(),
        ];
        let painted = element_lines(&elements, None);
        assert_eq!(
            painted.lines.len(),
            2,
            "two rows of two cells, not one line"
        );
        assert_eq!(line_text(&painted.lines[0]), "   1  2 ");
        assert_eq!(line_text(&painted.lines[1]), "   3  4 ");
        // Each row's second cell sits after its own row's first — the bands
        // restart per row rather than marching along one line.
        let starts: Vec<(usize, u16)> = painted
            .inline_bands
            .iter()
            .map(|b| (b.line, b.col_start))
            .collect();
        assert_eq!(starts, vec![(0, 2), (0, 5), (1, 2), (1, 5)]);
    }

    /// A run of [`inline`](crate::element::Element::inline) elements paints as
    /// ONE line — one week row of the month grid — while each cell keeps its
    /// own column band, which is the resolution `element_at_line` cannot
    /// express.
    #[test]
    fn an_inline_run_paints_one_line_with_a_band_per_cell() {
        let elements = vec![
            Element::label("head", "hdr"),
            Element::label("a", "  1  ").inline(),
            Element::label("b", "  2  ").inline(),
            Element::label("c", "  3  ").inline(),
            Element::label("tail", "after"),
        ];
        let painted = element_lines(&elements, None);
        assert_eq!(
            painted.lines.len(),
            3,
            "header + ONE line for the whole run + tail"
        );
        // The run's line falls back to its first cell; the bands below carry
        // the per-cell truth.
        assert_eq!(painted.element_at_line, vec![0, 1, 4]);

        // Gutter+indent is 2 cells, then three 5-wide cells end to end.
        assert_eq!(
            painted.inline_bands,
            vec![
                InlineBand {
                    line: 1,
                    col_start: 2,
                    col_end: 7,
                    element: 1
                },
                InlineBand {
                    line: 1,
                    col_start: 7,
                    col_end: 12,
                    element: 2
                },
                InlineBand {
                    line: 1,
                    col_start: 12,
                    col_end: 17,
                    element: 3
                },
            ]
        );
    }

    /// The whole point of the bands: a click lands on the CELL under the
    /// cursor, not on the row. Without this projection every day cell in a week
    /// would resolve to the same element and the Outlook drill-in could not
    /// exist (`ui/events.md` § Layout & flow).
    ///
    /// The narrow bands must also beat the full-width fallback that shares the
    /// row — `hit_test` takes the first containing region, so ordering is load
    /// bearing, not incidental.
    #[test]
    fn a_click_resolves_to_the_inline_cell_under_the_cursor() {
        let element_at_line = vec![7, 7];
        let bands = vec![
            InlineBand {
                line: 1,
                col_start: 2,
                col_end: 7,
                element: 10,
            },
            InlineBand {
                line: 1,
                col_start: 7,
                col_end: 12,
                element: 11,
            },
        ];
        // Origin (0,0), no lead, no scroll: element line 1 paints on screen row 1.
        let hits = page_hit_regions(&element_at_line, &bands, (0, 0), 40, 2, 0, 0);

        assert_eq!(hit_test(&hits, 3, 1), Some(HitTarget::Page(10)));
        assert_eq!(hit_test(&hits, 6, 1), Some(HitTarget::Page(10)));
        // The band boundary is exclusive at `col_end`, so column 7 is the NEXT
        // cell — an off-by-one here would silently misroute every second cell.
        assert_eq!(hit_test(&hits, 7, 1), Some(HitTarget::Page(11)));
        assert_eq!(hit_test(&hits, 11, 1), Some(HitTarget::Page(11)));
        // Past the last band the row's own element still answers, so a click in
        // the empty tail of a week row is not a dead zone.
        assert_eq!(hit_test(&hits, 30, 1), Some(HitTarget::Page(7)));
        // A line with no bands is unaffected.
        assert_eq!(hit_test(&hits, 3, 0), Some(HitTarget::Page(7)));
    }

    /// [`page_hit_regions`] is the hit-test mirror of [`placements_of`]: same
    /// origin/height/offset/lead inputs, projecting `element_at_line` instead
    /// of `art`. A multi-row element occupies multiple hit rows, all pointing
    /// at the same element index — proven here with the exact scroll/lead
    /// combination `render_screen` uses (a header `lead` before the element
    /// list, and a `Paragraph` `offset` scrolled past part of it).
    #[test]
    fn page_hit_regions_project_element_at_line_through_lead_and_scroll() {
        // 4 element-only lines: line0→element0, line1..=2→element1 (its 2-row
        // art), line3→element2. A 3-line header sits ahead of them in the full
        // frame, and the Paragraph is scrolled to full-frame line 4 (i.e. past
        // the header and past element-only line 0).
        let element_at_line = vec![0, 1, 1, 2];
        let hits = page_hit_regions(&element_at_line, &[], (2, 5), 20, 2, 4, 3);
        assert_eq!(
            hits,
            vec![
                RowHit {
                    row: 5,
                    col_start: 2,
                    col_end: 22,
                    target: HitTarget::Page(1)
                },
                RowHit {
                    row: 6,
                    col_start: 2,
                    col_end: 22,
                    target: HitTarget::Page(1)
                },
            ],
            "both visible rows land on element 1 — the thumbnail's two art rows"
        );
    }

    /// A viewport still partly over the `lead` header yields no hit for those
    /// rows — the header isn't clickable to any element — rather than
    /// underflowing or panicking on `line_index - lead`.
    #[test]
    fn page_hit_regions_skips_rows_still_inside_the_lead_header() {
        let element_at_line = vec![0, 1, 1, 2];
        // offset=1, height=3, lead=3: the first two viewport rows (full-frame
        // lines 1, 2) are still inside the 3-line header; only the third
        // (full-frame line 3 → element-only line 0) hits anything.
        let hits = page_hit_regions(&element_at_line, &[], (0, 0), 10, 3, 1, 3);
        assert_eq!(hits.len(), 1, "the two header rows must not hit anything");
        assert_eq!(hits[0].target, HitTarget::Page(0));
    }

    /// [`hit_test`] finds the row containing `(col, row)`, respects the
    /// column band, and misses cleanly off every hit region.
    #[test]
    fn hit_test_resolves_by_row_and_column_band() {
        let hits = vec![RowHit {
            row: 5,
            col_start: 2,
            col_end: 22,
            target: HitTarget::Page(3),
        }];
        assert_eq!(hit_test(&hits, 10, 5), Some(HitTarget::Page(3)));
        assert_eq!(
            hit_test(&hits, 2, 5),
            Some(HitTarget::Page(3)),
            "col_start is inclusive"
        );
        assert_eq!(hit_test(&hits, 22, 5), None, "col_end is exclusive");
        assert_eq!(hit_test(&hits, 10, 6), None, "wrong row misses entirely");
    }

    /// The sidebar's rows never scroll in practice (the gated list always fits
    /// a real terminal), so this is a direct row → position map — proven here
    /// for both the ordinary case and a too-short area, which drops the rows
    /// that don't fit rather than panicking.
    #[test]
    fn sidebar_hit_regions_map_rows_and_drop_what_the_area_cannot_fit() {
        let area = ratatui::layout::Rect {
            x: 0,
            y: 1,
            width: 20,
            height: 5,
        };
        let hits = sidebar_hit_regions(3, area);
        assert_eq!(
            hits,
            vec![
                RowHit {
                    row: 1,
                    col_start: 0,
                    col_end: 20,
                    target: HitTarget::Sidebar(0)
                },
                RowHit {
                    row: 2,
                    col_start: 0,
                    col_end: 20,
                    target: HitTarget::Sidebar(1)
                },
                RowHit {
                    row: 3,
                    col_start: 0,
                    col_end: 20,
                    target: HitTarget::Sidebar(2)
                },
            ]
        );

        let short_area = ratatui::layout::Rect { height: 2, ..area };
        let hits = sidebar_hit_regions(3, short_area);
        assert_eq!(
            hits.len(),
            2,
            "a row the area is too short to paint gets no hit region"
        );
    }

    // ---- nav-key-hints: the key-hint footer -------------------------------
    //
    // `apps/tui.md` § Key-hint footer. The e2e leg
    // (`tests/e2e-unified/tests/test_tui_nav_key_hints.py`) can only prove the
    // element exists and is visible: the agent's `type` command writes a field
    // WITHOUT moving the focus ring, so no driver can put an input under the
    // ring, and no driver reads pixels. Both of the footer's load-bearing
    // properties therefore live here — the contextual content, and where it
    // lands on screen.

    /// Paint a whole frame at a known size and return its rows as text.
    ///
    /// The footer is painted straight to the frame (like the critical-alerts
    /// band), not through the element list, so [`painted_line_texts`] cannot see
    /// it — only a real buffer can answer "which row did it land on".
    fn painted_rows(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                render(frame, app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The quit hint is present with the ring on a control, and ABSENT with the
    /// ring on a text input — where `q` types a literal `q`.
    ///
    /// This is the footer's whole contract in one test: it names bindings that
    /// would fire *right now*. A fixed hint string would be a lie precisely when
    /// the user is typing, which is when they are most likely to try the key.
    #[test]
    fn the_quit_hint_is_absent_while_a_text_input_holds_focus() {
        use crate::app::tests::authed_app;

        let mut app = authed_app();
        // Ring on a control: Enter opens, `q` quits.
        let on_control = nav_key_hints_text(&app);
        assert!(
            on_control.contains("q quit"),
            "with no input focused `q` quits, so the footer must say so: {on_control:?}"
        );
        assert!(
            on_control.contains("⏎ open"),
            "Enter actuates the focused control: {on_control:?}"
        );

        // Put a real text input under the ring: Search always carries its query
        // box, so this needs no snapshot seeding.
        let _ = app.apply(crate::pages::Page::Search);
        // `App::focused` indexes the FOCUSABLE elements, not every element, so the
        // ring position must be computed over the same filtered list.
        let idx = app
            .page_elements()
            .iter()
            .filter(|e| e.focusable())
            .position(|e| {
                matches!(
                    e.role,
                    crate::element::Role::Input(_) | crate::element::Role::InputCommit { .. }
                )
            })
            .expect("the search page carries a query input");
        app.zone = Zone::Page;
        app.focus = idx;
        assert!(
            app.focused_input().is_some(),
            "the ring must actually be on the query input for this test to mean anything"
        );

        let typing = nav_key_hints_text(&app);
        assert!(
            !typing.contains("quit"),
            "`q` types a literal q while an input has focus — the footer must NOT \
             offer it as quit: {typing:?}"
        );
        assert!(
            typing.contains("⏎ next"),
            "Enter advances the ring from an input: {typing:?}"
        );
        // The two always-live hints survive either arm.
        assert!(
            typing.contains("pane") && typing.contains("move"),
            "pane/move are live regardless of what holds focus: {typing:?}"
        );
    }

    /// The footer paints on the LAST row, and the shell body shortens by exactly
    /// one row rather than the footer overpainting content.
    ///
    /// The registry cannot answer either question (`painted_line_texts`'s doc
    /// tells the month-grid story). Without this, a footer rendered into the
    /// wrong `Rect` — or into the same `Rect` as the page pane — passes every id
    /// assertion while sitting on top of the last line of content.
    #[test]
    fn the_footer_paints_on_the_last_row_and_does_not_overpaint_the_body() {
        use crate::app::tests::authed_app;

        let app = authed_app();
        let rows = painted_rows(&app, 80, 12);
        let last = rows.last().expect("12 rows painted");
        assert!(
            last.contains("←/→") && last.contains("pane"),
            "the key-hint footer must occupy the bottom row: {rows:#?}"
        );
        // The sidebar's own chrome must still be on screen and must NOT have been
        // pushed off or overpainted — the footer took a row from the body, so the
        // body is one row shorter, not one row misplaced.
        assert!(
            rows.iter().any(|r| r.contains("Disconnected")),
            "the sidebar status header survives the footer's row: {rows:#?}"
        );
        // And nothing but the footer names the keys — a second copy would mean
        // the page pane is painting its own.
        let hint_rows = rows.iter().filter(|r| r.contains("←/→")).count();
        assert_eq!(hint_rows, 1, "exactly one key-hint row: {rows:#?}");
    }

    /// Registry and paint agree, in both directions, including the one state
    /// where `authenticated()` is true but the shell is NOT on screen.
    ///
    /// The append-mode "Add account" wizard runs over a live session
    /// (`App::showing_launch_surface`), where `screen_elements` emits no sidebar
    /// — so "←/→ pane" would name a gesture with nowhere to go. Gating on
    /// `authenticated()` alone looks right and is wrong; this pins the gate.
    #[test]
    fn the_footer_is_absent_from_the_wizard_over_a_live_session() {
        use crate::app::tests::{authed_app, test_app};

        let signed_out = test_app();
        assert!(
            !nav_key_hints_visible(&signed_out),
            "no footer on the signed-out launch surface"
        );

        let authed = authed_app();
        assert!(
            nav_key_hints_visible(&authed),
            "the authenticated shell carries the footer"
        );

        let mut adding = authed_app();
        adding.adding_account = true;
        assert!(
            !nav_key_hints_visible(&adding),
            "the add-account wizard owns the screen though the session is live — \
             a pane hint there names a gesture with no second pane"
        );
        let rows = painted_rows(&adding, 80, 12);
        assert!(
            !rows.iter().any(|r| r.contains("←/→")),
            "and the paint must agree with the gate: {rows:#?}"
        );

        // Registry follows the same predicate — no hint registered without pixels.
        let mut registry = crate::automation::Registry::default();
        register_listed(&adding, &mut registry);
        assert_eq!(
            registry.text_of(NAV_KEY_HINTS_ID),
            None,
            "an absent footer must not be registered"
        );
        let mut registry = crate::automation::Registry::default();
        register_listed(&authed, &mut registry);
        assert_eq!(
            registry.text_of(NAV_KEY_HINTS_ID),
            Some(nav_key_hints_text(&authed).as_str()),
            "a painted footer must be registered, with the text it painted"
        );
    }
}
