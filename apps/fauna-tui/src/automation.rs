//! In-process e2e automation for the TUI (`apps/tui.md` § E2E automation).
//!
//! The shared `fauna-e2e-agent` crate hosts the HTTP contract; this module
//! supplies the tui half. Ratatui is immediate-mode — there is no persistent
//! widget tree to walk (the linux approach), so the automatable surface is a
//! **per-frame element registry** built during `ui::render`, the TUI analogue
//! of the apple `AutomationRegistry` (`clients/apple-e2e-automation.md`): each
//! painted element records its ui.yaml id, live text, and — for actuable
//! elements — an [`Action`] that invokes the same state change the key
//! handler performs. Requests marshal into the main event loop as
//! [`AgentRequest`] messages (the TUI analogue of "forward to the GTK main
//! thread"); the loop redraws (rebuilding the registry) before it takes the
//! next request, so every reply reflects the current frame.
//!
//! Registration is gated on `FAUNA_E2E_AGENT_PORT` — production runs build no
//! registry at all.

use std::sync::{Arc, Mutex};

use fauna_e2e_agent::{ElementOp, ElementReq, ScopeStep};
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
use fauna_e2e_agent::{AgentHooks, ElementKind};

use crate::app::App;
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
use crate::pages::Page;

/// An actuation recorded at render time: applying it performs the same state
/// change the corresponding key handler would (the apple "activate closure
/// invokes the same method" convention, in enum form).
///
/// One variant, because there is one door: every actuable element on every
/// screen — a wizard mutator, a launch CTA, a sidebar tab — is a
/// [`Gesture`](crate::element::Gesture). The agent's click and the keyboard's
/// Enter therefore cannot diverge; they dispatch the same value.
#[derive(Debug, Clone)]
pub enum Action {
    Gesture(crate::element::Gesture),
}

/// One element painted this frame.
struct Entry {
    id: String,
    text: String,
    enabled: bool,
    action: Option<Action>,
    /// Set for text inputs: which field `/element/type` and `/element/clear`
    /// write. `None` = not editable.
    field: Option<crate::element::Field>,
    /// Set for pickers: what `/element/select` actuates, and **the options this
    /// frame actually painted**. `None` = not selectable.
    ///
    /// The options are load-bearing, not diagnostics: they are what makes the
    /// select op refusable. A GUI app refuses an unoffered value for free —
    /// it has to *find* the `ComboBoxItem` / `StringObject` / `By.text` node
    /// before it can actuate it — but a value-writeback registry like this one
    /// has nothing to look up, so it would hand any string straight to the
    /// mutator and ack. Keeping the list here is what lets [`element_op`] check
    /// membership before dispatching (e2e-conventions.md § convention 11).
    select: Option<(crate::element::SelectTarget, Vec<String>)>,
    /// Ancestor scope path (container-id, occurrence-index), for scoped
    /// queries — apple's `Slot.scopePath` shape. Top-level elements have an
    /// empty path, so any scoped query correctly misses them (never the
    /// scope-dropping collapse apple had to fix).
    path: Vec<ScopeStep>,
    /// Automation attributes read by `/element/attr` (`get_attr(id, key)`) —
    /// the inline tui twin of the GUI apps' AT-SPI Description / UIA HelpText
    /// / `test-attr-*` class. Empty for all but the handful of elements that
    /// carry state beyond their text (`recipient-resolve-status`'s `state`).
    attrs: Vec<(String, String)>,
    /// What `/element/double_click` actuates
    /// ([`Element::dbl`](crate::element::Element::dbl)). `None` = this element
    /// has no second gesture, and the agent says so rather than silently
    /// running `action`.
    dbl: Option<Action>,
    /// Where this entry sits in the page's own element list — `None` for the
    /// sidebar and the shell chrome, which never scroll. What the targeted
    /// scroll-into-view moves the focus ring to.
    page_index: Option<usize>,
    /// Whether the frame that registered this entry painted it in view
    /// ([`crate::ui::PaintedPage::in_view`]) — the derived `in-viewport`
    /// attribute. `None` = no geometry to say (a registry built outside a draw,
    /// or an element that painted no line), which the attribute reports as
    /// `null` rather than guessing.
    in_view: Option<bool>,
}

/// The per-frame element registry, rebuilt by `ui::render` on every draw.
#[derive(Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

impl Registry {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The registered text for `id`, or `None` if nothing registered it.
    ///
    /// Test-only read seam: [`Self::matches`] is private (scope resolution is
    /// the agent's business), but a paint test needs to assert the other half of
    /// the registry↔paint agreement — that an element the frame did *not* paint
    /// was also not registered.
    #[cfg(test)]
    pub(crate) fn text_of(&self, id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.text.as_str())
    }

    /// Test-only: the registry a frame's elements would build, so a paint test
    /// can ask the **real** scope rule rather than hand-copying it.
    ///
    /// Three modules' `count_scoped` helpers used to re-implement the rule as
    /// `e.path.first() == Some(&scope)` while their doc comments claimed to
    /// "prove the scoped e2e query resolves". That copy stopped being the rule
    /// the moment scope resolution became descendant matching
    /// (e2e-conventions.md § convention 1, ruled 2026-08-14) — and a copy of a
    /// rule proves nothing about the rule.
    #[cfg(test)]
    pub(crate) fn of(elements: impl IntoIterator<Item = crate::element::Element>) -> Self {
        let mut registry = Self::default();
        for element in elements {
            registry.element(element);
        }
        registry
    }

    /// Test-only: how many entries the agent's own scoped query would return.
    #[cfg(test)]
    pub(crate) fn count_scoped(&self, id: &str, scope: &[ScopeStep]) -> usize {
        self.matches(id, scope).count()
    }

    /// The whole frame as `GET /registry` records (`fauna_e2e_agent::
    /// ElementKind::Registry`), field for field with the apple, windows and web
    /// bridges (`drivers/base.py::registry_snapshot` owns the contract).
    ///
    /// `index` is the id's GLOBAL occurrence in registration order — the order
    /// paint registers in, and exactly what an unscoped query addresses
    /// ([`Self::matches`] with no scope), so every record re-drives verbatim, as
    /// web's does. `scope` is the ancestor path in the wire DSL
    /// (`post-card[1]/quoted-post`), descriptive only. `declares_enabled` is
    /// true for every interactive element: tui's constructors take `enabled` as
    /// an argument, so an interactive element's value is always a declaration,
    /// while a label's `true` is a default.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub fn snapshot_json(&self) -> Value {
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let elements: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                let slot = seen.entry(e.id.as_str()).or_insert(0);
                let index = *slot;
                *slot += 1;
                let scope = e
                    .path
                    .iter()
                    .map(|(id, i)| format!("{id}[{i}]"))
                    .collect::<Vec<_>>()
                    .join("/");
                json!({
                    "id": e.id,
                    "index": index,
                    "enabled": e.enabled,
                    "declares_enabled": e.action.is_some() || e.field.is_some() || e.select.is_some(),
                    "actuable": e.action.is_some(),
                    "editable": e.field.is_some(),
                    "scope": scope,
                    "text": e.text,
                })
            })
            .collect();
        json!({ "elements": elements })
    }

    /// Every registered element as `(id, text)`, in registration order — what
    /// the render loop hands `fauna_e2e_agent::PaintedErrorTally` per frame.
    pub fn texts(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|e| (e.id.as_str(), e.text.as_str()))
    }

    /// Record a read-only element (labels, status lines).
    pub fn text(&mut self, id: &str, text: impl Into<String>) {
        self.entries.push(Entry {
            id: id.to_string(),
            text: text.into(),
            enabled: true,
            action: None,
            field: None,
            select: None,
            path: Vec::new(),
            attrs: Vec::new(),
            dbl: None,
            page_index: None,
            in_view: None,
        });
    }

    /// Record one painted [`Element`](crate::element::Element) — role and
    /// ancestor scope path and all. This is the single door **every** screen
    /// registers through, so a page that nests an element under an indexed
    /// container (`post-card[2]`, `provisioning-step-row[2]`) gets a scoped
    /// registry entry without the page knowing the registry exists.
    pub fn element(&mut self, element: crate::element::Element) {
        use crate::element::Role;
        // A checkbox's state is DERIVED into its `checked` attr, the way the
        // agent derives `disabled` from `enabled`: `checked` is the single
        // source of truth for the paint's `[x]` / `[ ]`, and without this the
        // automation surface had no reading of it at all — `get_attr(id,
        // "checked")` answered `Null` for both states. An inline `checked`
        // still wins, so a page that ever needs to say something else can.
        let checked = match &element.role {
            Role::Checkbox { checked, .. } => Some(*checked),
            _ => None,
        };
        let mut attrs = element.attrs;
        if let Some(checked) = checked
            && !attrs.iter().any(|(k, _)| k == "checked")
        {
            attrs.push(("checked".to_string(), checked.to_string()));
        }
        let (action, field, select) = match element.role {
            Role::Label => (None, None, None),
            Role::Button(gesture)
            | Role::Checkbox { gesture, .. }
            | Role::Radio { gesture, .. } => (Some(Action::Gesture(gesture)), None, None),
            Role::Input(field) => (None, Some(field), None),
            // Editable AND actuable: `type` writes the buffer, `click` fires the
            // commit gesture — the type-then-click idiom the shared action layer
            // uses for a commit-on-Enter entry.
            Role::InputCommit { field, gesture } => {
                (Some(Action::Gesture(gesture)), Some(field), None)
            }
            Role::Select {
                target, options, ..
            } => (None, None, Some((target, options))),
        };
        self.entries.push(Entry {
            id: element.id.to_string(),
            text: element.text,
            enabled: element.enabled,
            action,
            field,
            select,
            path: element.path,
            attrs,
            dbl: element.dbl.map(Action::Gesture),
            page_index: None,
            in_view: None,
        });
    }

    /// [`Self::element`] for one of the page's own elements, as a draw painted
    /// it: `index` is its position in that page list, `in_view` what the frame
    /// measured ([`crate::ui::register_frame`]).
    pub fn page_element(
        &mut self,
        element: crate::element::Element,
        index: usize,
        in_view: Option<bool>,
    ) {
        self.element(element);
        if let Some(entry) = self.entries.last_mut() {
            entry.page_index = Some(index);
            entry.in_view = in_view;
        }
    }

    /// Resolve a query scope to the concrete ancestor path of the container it
    /// names — the flat-registry emulation of the **descendant** walk every
    /// other app performs (e2e-conventions.md § convention 1, *Scope resolution
    /// is descendant matching*, ruled 2026-08-14).
    ///
    /// Each step is looked up among the container instances of that id living
    /// **anywhere below the previously resolved step** (below the root for the
    /// first step), in document order, and the step's index selects among
    /// *those* — exactly what web's `root.locator(id).nth(i)`, linux's
    /// `scope_root`'s `collect_in` and windows' `WalkScope` do. So a scope may
    /// name only the containers it cares about (`quoted-post`), leaving the
    /// ones between it and the root implicit; the old root-anchored prefix
    /// required every one of them and silently matched nothing otherwise.
    ///
    /// `None` = a step names a container this frame did not paint, which the
    /// callers turn into an empty match set (the same "absent scope → no
    /// results" contract as linux's `scope_root`).
    ///
    /// A container has **two** witnesses in a flat registry, and the walk needs
    /// both, because the index counts container *instances* and a real widget
    /// tree counts them whether or not they hold anything:
    ///
    /// 1. **Its own entry** — a container registers under its own id like any
    ///    element, carrying its *parent's* path (an ancestor path excludes the
    ///    element itself, exactly as a DOM node is not its own descendant). Its
    ///    ordinal among same-id declarations under that same parent is the
    ///    occurrence index the paint code baked into its children, because
    ///    `.within(container, i)` numbers them 0-based in paint order.
    /// 2. **The paths of the elements inside it** — the only witness for a
    ///    container that carries no id of its own.
    ///
    /// Dropping witness 1 is not a shortcut, it is a bug: a childless container
    /// would vanish from the ordinal sequence and shift every later sibling up,
    /// so `restore-history-item[0]` would resolve to row *1* whenever row 0
    /// painted no banner — and answer with row 1's banner for a query that must
    /// find nothing (`the_banner_renders_only_when_diverged_and_inside_its_own_row`
    /// is the pin).
    fn scope_root(&self, scope: &[ScopeStep]) -> Option<Vec<ScopeStep>> {
        let mut root: Vec<ScopeStep> = Vec::new();
        for (id, index) in scope {
            // The distinct instances of `id` below `root`, in document order.
            // Registration order is paint order, and within one entry's path
            // outer containers precede inner ones, so pushing every
            // `id`-terminated prefix in that order yields pre-order document
            // order — the order `nth` indexes into.
            let mut instances: Vec<Vec<ScopeStep>> = Vec::new();
            // How many times each parent path has already declared this id.
            let mut declared: Vec<(Vec<ScopeStep>, usize)> = Vec::new();
            for e in &self.entries {
                if !e.path.starts_with(&root) {
                    continue;
                }
                // Witness 1: the container's own registration — but only in the
                // idiom where it carries its PARENT's path (`backups`'
                // `restore-history-item`, feed's `link-preview-card`). Pages also
                // write the self-scoping idiom, `Element::label(row, "")
                // .within(row, i)`, where the entry already names itself: there
                // the path IS the instance and witness 2 below records it, so
                // minting `path + (id, n)` here would invent a bogus second level
                // and steal the ordinal (`provisioning-step-row`, the tiers rows).
                if e.id == *id && e.path.last().map(|(step, _)| step != id).unwrap_or(true) {
                    let occurrence = match declared.iter_mut().find(|(p, _)| *p == e.path) {
                        Some((_, seen)) => {
                            *seen += 1;
                            *seen - 1
                        }
                        None => {
                            declared.push((e.path.clone(), 1));
                            0
                        }
                    };
                    let mut instance = e.path.clone();
                    instance.push((id.clone(), occurrence));
                    if !instances.contains(&instance) {
                        instances.push(instance);
                    }
                }
                // Witness 2: an element painted inside one.
                for depth in root.len()..e.path.len() {
                    if e.path[depth].0 == *id {
                        let instance = e.path[..=depth].to_vec();
                        if !instances.contains(&instance) {
                            instances.push(instance);
                        }
                    }
                }
            }
            root = instances.into_iter().nth(*index)?;
        }
        Some(root)
    }

    /// All entries matching `id` under `scope`, in registration (= visual)
    /// order. An entry is inside a scope iff the container [`Self::scope_root`]
    /// resolved is one of its ancestors — so an element nested deeper still
    /// matches, and a top-level element (empty path) matches no scoped query.
    fn matches<'a>(&'a self, id: &str, scope: &'a [ScopeStep]) -> impl Iterator<Item = &'a Entry> {
        let root = self.scope_root(scope);
        self.entries.iter().filter(move |e| match &root {
            Some(root) => e.id == id && e.path.starts_with(root),
            None => false,
        })
    }
}

/// A request from the agent server thread to the main event loop.
pub enum AgentRequest {
    Element(ElementOp),
    /// A `/app/commands` state-protocol command. The loop applies it, then
    /// acks by setting `last_command_id`/`ready` in the shared state.
    Command {
        id: String,
        action: String,
        state: Value,
        /// `call_machine_method`'s two top-level fields (`onboarding.md`
        /// § E2E bridge contract).
        method: String,
        json_arg: String,
        /// `feed_inject_posts`'s top-level field — a `TestPostSpec[]`, the same
        /// shape linux's `handle_feed_inject_posts` deserializes. Like `method`/
        /// `json_arg`, this rides `call_command`'s payload merged straight into
        /// the request body (never nested under `state`, which only `patch`
        /// populates).
        posts: Value,
        /// The **whole** request body — the tui analogue of linux's
        /// `RawCommand.payload`. The conversations inject commands
        /// (`conversations_inject_inbound` / `_create_mls_group` /
        /// `_inject_send_failure`) carry their fields flat at the top level
        /// (`body.update(payload)` in the action layer), so they read
        /// `payload.get("rail")` etc. rather than a nested envelope.
        // Boxed: it's a full request-body clone (the largest field by far), and
        // this variant sits next to the much lighter `Element` in the same enum.
        payload: Box<Value>,
    },
}

/// What the loop learned from applying one command.
pub struct CommandResult {
    /// False for a shape the client doesn't understand — logged loudly rather
    /// than silently greened. The ack fires either way (the driver polls).
    pub recognized: bool,
    /// A reader method's JSON-serialized return, surfaced to the driver as
    /// `state.machine_method_result`.
    pub machine_result: Option<String>,
}

/// State served to `GET /app/state`, written by the main loop.
struct SharedState {
    last_command_id: String,
    ready: bool,
    state: Value,
    machine_result: Option<String>,
}

/// The main loop's handle to the running agent.
pub struct Agent {
    pub rx: mpsc::UnboundedReceiver<AgentRequest>,
    shared: Arc<Mutex<SharedState>>,
}

/// Release builds without `e2e-agent` never bind a port — the automation
/// surface is compiled out entirely (testing.md convention 15).
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub fn start_if_enabled() -> Option<Agent> {
    None
}

/// No agent in a release build, so no main-loop stamp either: the loop is
/// awaited as it always was, with no ticker waking it (convention 15).
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub fn loop_heartbeat() -> Option<Arc<fauna_e2e_agent::UiThreadHeartbeat>> {
    None
}

/// The port the agent binds, when `FAUNA_E2E_AGENT_PORT` names one — the one
/// reading both [`start_if_enabled`] and [`loop_heartbeat`] make, so the stamp
/// exists exactly when the agent that reads it does.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn agent_port() -> Option<u16> {
    std::env::var("FAUNA_E2E_AGENT_PORT").ok()?.parse().ok()
}

/// This process's main-loop liveness stamp — convention 11's UI-thread
/// heartbeat — when the agent will run: the stamp [`start_if_enabled`] hands
/// the agent server, which reads its age when an op times out, and the one
/// `main` beats by wrapping the loop in [`beat_while_pending`]. `None` with no
/// agent port, so a debug build run by hand carries no ticker either.
///
/// Process-wide because `main` wraps the loop before `run` has started the
/// agent, and both must name the same stamp.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn loop_heartbeat() -> Option<Arc<fauna_e2e_agent::UiThreadHeartbeat>> {
    agent_port()?;
    static STAMP: std::sync::OnceLock<Arc<fauna_e2e_agent::UiThreadHeartbeat>> =
        std::sync::OnceLock::new();
    Some(Arc::clone(
        STAMP.get_or_init(fauna_e2e_agent::UiThreadHeartbeat::new),
    ))
}

/// Drive `fut` to completion, beating `heartbeat` every
/// [`fauna_e2e_agent::HEARTBEAT_CADENCE`] for as long as it is pending — tui's
/// half of convention 11's UI-thread heartbeat (`e2e-conventions.md`).
///
/// **Why it wraps the whole loop rather than joining its `select!` as an arm.**
/// tui's Element and Command arms await their op INLINE (`automation::perform`,
/// `apply_command`), and `select!` runs a winning arm to completion before it
/// polls any other — so a beat arm is starved during exactly the waits it
/// exists to classify, and a slow op that is only waiting on the network would
/// read as a blocked loop. Wrapped around the loop's future instead, this
/// future's own ticker is polled every time the loop yields — idle between
/// events, or parked inside an arm on an op's await — so the stamp stays
/// fresh; when the loop's thread is held by synchronous work, nothing can poll
/// the ticker, the stamp goes stale, and the agent's verdict names synchronous
/// work on the UI thread. That split is the stamp's whole reason to exist.
///
/// **It forces no repaint.** Re-polling a suspended future resumes it at its
/// await point; it never re-runs the loop body, so `terminal.draw()` is not
/// called by a beat — the four-a-second full render a beat arm in tui's loop
/// would have cost, and the reason a 2026-09-02 attempt was reverted.
///
/// `None` (every release build; a debug build with no agent port) awaits `fut`
/// directly: no ticker, no wake.
pub async fn beat_while_pending<F: std::future::Future>(
    heartbeat: Option<Arc<fauna_e2e_agent::UiThreadHeartbeat>>,
    fut: F,
) -> F::Output {
    match heartbeat {
        Some(stamp) => beat_while_pending_with(move || stamp.beat(), fut).await,
        None => fut.await,
    }
}

/// [`beat_while_pending`]'s mechanism, over any beat — so its two properties
/// (a waiting loop beats at the cadence, a held one beats nothing) can be
/// pinned by counting and timing beats rather than reading a stamp whose clock
/// a paused test runtime cannot move.
pub(crate) async fn beat_while_pending_with<F: std::future::Future>(
    mut beat: impl FnMut(),
    fut: F,
) -> F::Output {
    let mut ticker = tokio::time::interval(fauna_e2e_agent::HEARTBEAT_CADENCE);
    // A loop too busy to beat on time beats once when it gets back, not in a
    // burst: the stamp records the latest moment the loop ran, and a burst of
    // catch-up beats would record nothing more.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            // The loop first: the ticker exists to observe it, never to delay it.
            biased;
            out = &mut fut => return out,
            _ = ticker.tick() => beat(),
        }
    }
}

/// Start the agent when `FAUNA_E2E_AGENT_PORT` is set (the uniform e2e gate
/// all in-process clients key on). Returns `None` in production.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn start_if_enabled() -> Option<Agent> {
    let port: u16 = agent_port()?;
    let (tx, rx) = mpsc::unbounded_channel::<AgentRequest>();
    let shared = Arc::new(Mutex::new(SharedState {
        last_command_id: String::new(),
        ready: true,
        state: Value::Null,
        machine_result: None,
    }));

    let dispatch_tx = tx.clone();
    let state_shared = Arc::clone(&shared);
    let cmd_shared = Arc::clone(&shared);
    fauna_e2e_agent::start(
        port,
        AgentHooks {
            dispatch: Box::new(move |op| {
                dispatch_tx.send(AgentRequest::Element(op)).map_err(|_| ())
            }),
            app_state: Box::new(move || {
                let s = state_shared.lock().unwrap_or_else(|e| e.into_inner());
                json!({
                    "last_command_id": s.last_command_id,
                    "ready": s.ready,
                    "state": s.state,
                })
            }),
            inject_command: Box::new(move |body| {
                let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let action = body
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("patch");
                let state = body.get("state").cloned().unwrap_or(Value::Null);
                let method = body
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                // The driver sends `json_arg` as a JSON *string*; accept a raw
                // object too, so a caller that skips the double-encode works.
                let json_arg = match body.get("json_arg") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => serde_json::to_string(other).unwrap_or_default(),
                    None => String::new(),
                };
                let posts = body.get("posts").cloned().unwrap_or(Value::Null);
                let payload = Box::new(body.clone());
                // Not-ready until the loop acks — the driver polls /app/state
                // for last_command_id==id && ready (the render-settle
                // discipline apple had to retrofit; here the ack IS ordered
                // after the apply because both run on the main loop).
                cmd_shared.lock().unwrap_or_else(|e| e.into_inner()).ready = false;
                match tx.send(AgentRequest::Command {
                    id: id.to_string(),
                    action: action.to_string(),
                    state,
                    method: method.to_string(),
                    json_arg,
                    posts,
                    payload,
                }) {
                    Ok(()) => json!({ "ok": true }),
                    Err(_) => json!({ "error": "app loop closed" }),
                }
            }),
            // The loop's own stamp, beaten by `main` wrapping the loop in
            // `beat_while_pending` — so a timed-out op's verdict says whether
            // the loop was waiting (the op is slow) or held (synchronous work
            // on the UI thread), instead of UNMEASURED.
            heartbeat: loop_heartbeat(),
        },
    );
    Some(Agent { rx, shared })
}

impl Agent {
    /// Publish the app's e2e-visible state (called by the loop after every
    /// apply; also the command ack when `command_id` is set).
    pub fn publish(&self, app: &App, command_id: Option<&str>) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let machine_result = s.machine_result.clone();
        s.state = state_json(app, machine_result);
        if let Some(id) = command_id {
            s.last_command_id = id.to_string();
        }
        s.ready = true;
    }

    /// Stash a reader method's return **before** the ack, so the driver reads
    /// the result of *this* command (clearing any prior reader's value).
    pub fn set_machine_result(&self, result: Option<String>) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .machine_result = result;
    }
}

/// The `state` object the driver reads (`get_state`) and `reset()` polls
/// (`session.authenticated == false`).
///
/// Gated, unlike the per-page `crate::*::state_json` mirrors it aggregates: two of
/// its callees are themselves automation-only and already gated
/// (`crate::sync_agent::state_json`, `SettingsState::caldav_mailbox_reply`), so an
/// ungated aggregator simply does not compile in a release build without
/// `e2e-agent` — a latent break, since nothing had ever built tui that way. Its
/// only caller (`start_if_enabled`) carries the same cfg, so this needs no no-op
/// twin. `docs/goal/architecture/testing.md` convention 15.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn state_json(app: &App, machine_result: Option<String>) -> Value {
    let session = match &app.session {
        Some(s) => json!({
            "authenticated": true,
            "handle": s.handle,
            "actor_id": s.actor_id,
        }),
        None => json!({ "authenticated": false }),
    };
    // The nav projection mirrors the SCREEN, not the page ring: while a
    // launch/wizard surface owns the screen the view reads `welcome` — the
    // same reading linux reports when no authenticated stack is mounted — so
    // the cross-app onboarding assertions ("reset/logout land on welcome")
    // hold on tui without a native welcome Page existing in the ring.
    let nav_view = if app.showing_launch_surface() {
        "welcome"
    } else if app.page == Page::Settings && app.settings.sub == crate::settings::SubPage::Devices {
        // `devices` predates the 2026-06-28 sync/folder UI unification as a
        // top-level page; the legacy single-element nav + the generic
        // cross-app nav smoke tests still expect the state report to echo
        // that name back, not the enclosing `settings` shell — mirroring
        // linux's `main.rs` state report, which walks its settings sub-stack
        // the same way (`test_agent.rs::gtk_to_canonical`'s doc comment).
        "devices"
    } else {
        app.page.view_name()
    };
    let mut state = json!({
        "session": session,
        // The launch clock this process signs in on — `{"offset_secs", "now_secs"}`
        // (`fauna_launch_machine::launch_clock`). The wrong-clock launch witness
        // reads it as the in-app control that the `FAUNA_E2E_CLOCK_OFFSET_SECS`
        // seed reached the app's own clock, so a green silent challenge is not
        // vacuous. Shared getters, no per-app bookkeeping
        // (`fauna_e2e_agent::CLOCK_KEY` owns the contract). TOP-level for the
        // same cross-app-one-depth reason as the keys below.
        fauna_e2e_agent::CLOCK_KEY: fauna_e2e_agent::clock_json(
            fauna_launch_machine::launch_clock::clock_offset_secs(),
            fauna_launch_machine::launch_clock::now_secs_or_zero(),
        ),
        // The launch machine's bearer schedule on this app's own clock —
        // `{"expires_in_secs", "own_session_ids"}` (`fauna_e2e_agent::
        // LAUNCH_TOKEN_KEY`, which owns the contract). The wrong-clock REFRESH
        // witness reads it: a non-positive `expires_in_secs` right after a
        // launch is the hot re-mint loop's signature. Plain field reads off the
        // machine's snapshot (convention 11 corollary); TOP-level for the same
        // cross-app-one-depth reason as `clock` above.
        fauna_e2e_agent::LAUNCH_TOKEN_KEY: match &app.launch_machine {
            Some(machine) => {
                let expires_at_secs = match machine.snapshot().token {
                    fauna_launch_machine::TokenStatus::Valid { expires_at_secs } => {
                        Some(expires_at_secs)
                    }
                    _ => None,
                };
                fauna_e2e_agent::launch_token_json(
                    expires_at_secs,
                    fauna_launch_machine::launch_clock::now_secs_or_zero(),
                    &machine.own_token_ids(),
                )
            }
            None => fauna_e2e_agent::launch_token_json(None, 0, &[]),
        },
        // This app's transport connection — `{"state": word, "online": bool}`
        // (`fauna_e2e_agent::CONNECTION_KEY`, which owns the contract). TOP-level
        // for the same cross-app-one-depth reason as the barrier keys, and the
        // same one-line-per-app shape: the word comes from the shared supervisor
        // (`App::connection_state_word` → `ConnectionState::as_wire_word`) and
        // the online verdict from shared Rust, so no app decides the polarity.
        // A plain field read, legal on the state path (convention 11 corollary).
        "connection": fauna_e2e_agent::connection_json(app.connection_state_word()),
        // The indicator's report/transition counts — the stickiness observable
        // (`fauna_e2e_agent::CONNECTION_REPORTS_KEY`, which owns the contract);
        // counting and JSON both shared. TOP-level, like `connection`.
        fauna_e2e_agent::CONNECTION_REPORTS_KEY: app.connection_reports.json(),
        // Every error surface a painted frame has shown — the "no error
        // anywhere" observable (`fauna_e2e_agent::PAINTED_ERRORS_KEY`). TOP-level.
        fauna_e2e_agent::PAINTED_ERRORS_KEY: app.painted_errors.json(),
        "nav": { "stack": [ { "view": nav_view } ] },
        // The `barrier` self-test's only observable — the value of the last
        // applied `barrier_probe` item, or `null`. TOP-level, not under `data`:
        // the cross-app test reads `get_state("barrier_probe")`, and linux/web
        // publish it at the same depth (`fauna_e2e_agent::BARRIER`).
        "barrier_probe": app.barrier_probe,
        // What the barrier saw at its OWN ack, frozen — the only key the
        // self-test asserts. See `fauna_e2e_agent::BARRIER_ACK_PROBE_KEY` for
        // why the live key above cannot carry this proof.
        "barrier_ack_probe": app.barrier_ack_probe,
        // How many authenticated-session teardowns this app has initiated —
        // convention 14's negative-assert observable. TOP-level for the same
        // reason as the two barrier keys: the cross-app helper reads
        // `get_state("session_generation")` at one depth on every app
        // (`fauna_e2e_agent::SESSION_GENERATION_KEY`). Unlike them it is LIVE by
        // design and needs no frozen twin — a monotonic counter can only reveal
        // more teardowns to a late read, never fewer.
        "session_generation": app.session_generation,
        // The critical-alert sweep's pass counters, read straight off the
        // shared registry — convention 14's causal barrier for the *sweep's*
        // negative asserts, the way `session_generation` above is for
        // teardowns. TOP-level for the same cross-app-one-depth reason
        // (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`, which owns the contract).
        //
        // No per-app bookkeeping exists here on purpose: the counting lives in
        // the shared sweep crate, so the other six apps publish this key by
        // reading the same two getters rather than re-deriving anything.
        "alert_sweep_passes": {
            "started": app.alerts.sweep_passes_started(),
            "completed": app.alerts.sweep_passes_completed(),
        },
        // Per-channel counts of inbound MLS commits this device has folded in —
        // the twin-device barrier (`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`,
        // which owns the contract). TOP-level for the same cross-app-one-depth
        // reason as the keys above.
        //
        // Like `alert_sweep_passes` there is no per-app bookkeeping here on
        // purpose: the counting AND the JSON shape are both shared
        // (`FaunaMlsBackend::folded_commits` →
        // `fauna_conversations::state_json::mls_folded_commits_json_for_session`),
        // so every other app's leg is this same one-line call.
        "mls_folded_commits": fauna_conversations::state_json::mls_folded_commits_json_for_session(
            app.conversations.real_session.as_ref(),
        ),
        // How many full receive-loop cycles this session has begun and finished
        // — the completion observable beside the `conv_receive_now` poke
        // (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`, which owns the contract).
        // Same one-line-per-app shape as the two keys above: the counting and
        // the JSON both live in shared Rust.
        "conv_receive_cycles": fauna_conversations::state_json::conv_receive_cycles_json(
            app.conversations.real_session.as_ref(),
        ),
        // What the inbound poll did with peer share-endpoint advertisements —
        // the peer-transfer plane's ingest tally
        // (`fauna_e2e_agent::SHARE_ENDPOINTS_COUNTS_KEY`, which owns the
        // contract). Same one-line-per-app shape: four atomics read through
        // the shared derivation, so it is legal on the state path
        // (convention 11 corollary) and identical on linux.
        "share_endpoints_counts": fauna_conversations::state_json::share_endpoints_counts_json(
            app.conversations.real_session.as_ref(),
        ),
        // What this process's share plane has SERVED, per path, plus the
        // serve hold's state — the sender-side witness that an interrupted
        // transfer re-sends nothing that arrived
        // (`fauna_e2e_agent::SHARE_SERVE_TALLY_KEY`, which owns the contract).
        // A lock-and-clone, legal on the state path (convention 11 corollary).
        fauna_e2e_agent::SHARE_SERVE_TALLY_KEY: share_serve_tally_json(),
        // The account-plane pump's full-pass cycle counters — the
        // `conv_receive_cycles` twin for the account runtime
        // (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`, which owns the
        // contract, beside the `account_pump_now` poke). Counting lives in
        // shared Rust (`fauna_sync_engine::account_runtime::PumpCycles`);
        // this is a plain atomic read, never a channel round trip, so it is
        // legal on the state path (convention 11 corollary). Zeros pre-auth,
        // the `conv_receive_cycles` no-session shape.
        "account_pump_cycles": account_pump_cycles_json(app),
        // Every new-message banner this process actually fired, plus the
        // diff-tick counters that make a NEGATIVE read of that list sound — the
        // witness for `conversations` outcome 11
        // (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`, which owns the contract).
        // Recording and JSON both live in shared Rust, so this is a getter read
        // like the keys above, not a tui tally — identical on linux.
        //
        // Published UNCONDITIONALLY, pre-auth included: the key's contract makes
        // absent (`null`) mean "this app has no firing leg at all" and distinct
        // from `{"started": 0, …}`, so publishing only once a manager exists
        // would tell a reader tui was unbuilt for the whole pre-login window.
        "message_banners": fauna_conversations::notification::message_banners_json(),
        // The shared feed manager's reload counters — the `conv_receive_cycles`
        // twin for the feed's re-query funnel
        // (`fauna_e2e_agent::FEED_RELOADS_KEY`, which owns the contract).
        // Counting and JSON both live in shared Rust; a plain atomic read, so
        // legal on the state path. Zeros pre-auth (no manager built yet).
        "feed_reloads": fauna_feed::feed_reloads_json(
            app.feed.manager.as_ref().map(|m| m.reload_counts()),
        ),
        // The post-claim serving-enablement step's completion anchor
        // (`fauna_e2e_agent::SERVING_ENABLEMENT_KEY`, which owns the contract).
        // Recording and JSON both live in shared Rust; a lock-and-clone of a
        // handful of records. Published unconditionally — the empty run list
        // before the wizard's `LoggedIn` is the legitimate "not run yet",
        // distinct from an absent leg.
        fauna_e2e_agent::SERVING_ENABLEMENT_KEY:
            fauna_client_mail_settings::serving_enablement::serving_enablement_json(),
        // How many lines the CURRENT page paints with the focus highlight —
        // `ui::focused_line_count`, the automation-visible twin of the
        // paint-time invariant `ui::tests::elements_sharing_an_id_do_not_all_paint_focused`
        // pins. Exists because the Settings rail's rows are DELIBERATELY
        // id-less, so no per-element registry query (`get_attr`/`is_visible`)
        // can see them; this is a minimal additive state field, not a new
        // ui.yaml id, reusing the exact paint-time computation so it can
        // never drift from what a real terminal shows.
        "focused_line_count": crate::ui::focused_line_count(app),
        // Convention 17's "a region Block never renders silent" — the region
        // verdicts the current surface's own walk counts against the block
        // placeholders its paint registered (`crate::region::block_render_json`).
        "region_block_render": crate::region::block_render_json(app),
        // ui.yaml's declared `settings.state_fields` (`settings.inbox_mode`,
        // `settings.spam_threshold`) — a TOP-LEVEL sibling of `data`, matching
        // linux's own `main.rs` state_json exactly (`"settings": {"inbox_mode":
        // ...}` beside `"data"`, not nested inside it) — the action layer's
        // `SettingsActions.set_inbox_mode`/`get_inbox_mode` read
        // `get_state("settings")` with no `data.` prefix.
        "settings": crate::settings::state_json(&app.settings),
        // `data.feed.posts[]` is ui.yaml's declared `feed.state_fields`. It is
        // not decoration: the shared harness resolves a post's uploaded
        // `media_hash` from here and nowhere else — there is no element behind
        // it — so the image tests' 64-hex-char assertion is answered by this
        // serializer. `data.conversation_threads[]` is the conversations page's
        // declared `state_fields`, read by the action layer's `list_threads()`.
        "data": {
            "feed": crate::feed::state_json(&app.feed),
            "conversation_threads": crate::conversations::state_json(&app.conversations),
            "conversation_sort": crate::conversations::sort_state_json(&app.conversations),
            "selected_thread_id": crate::conversations::selected_state_json(&app.conversations),
            // ui.yaml's declared `contacts.state_fields` (peer_id/status/handle).
            "contacts": crate::contacts::state_json(&app.contacts),
            // ui.yaml's declared `notifications.state_fields` (unread_count).
            "notifications": crate::notifications::state_json(&app.notifications),
            // ui.yaml's declared `profile.state_fields` (is_self/actor_id).
            "profile": crate::profile::state_json(&app.profile),
            // ui.yaml's declared `events.state_fields`
            // (id/summary/start/end/rsvp_status per row).
            "events": crate::events::state_json(&app.events),
            // The Media explorer's cross-set aggregate — the items, the filter
            // options, and the active view state, straight off the shared
            // `MediaPageSnapshot`.
            "media": crate::media::state_json(&app.media),
            // The Nostr bridge's link state + its relay/follow lists. Not a
            // ui.yaml `state_fields` block (the `nostr` page declares none) —
            // it mirrors the element surface so a driver can assert link state
            // without inferring it from which elements happen to render.
            "nostr": crate::nostr::state_json(&app.nostr),
            // The real-wire readiness flag `enable_real_faunamls` polls before
            // driving a real send (the linux `ready` twin) — true once the
            // login-built `ConversationsSession` is live.
            "conv_real_backend_active":
                crate::conversations::conv_backend::is_e2e_real_active(&app.conversations),
            // The last succession's group sweep — `null` until one runs. The
            // ceremony renders this as ID-less chrome (prose, not an
            // affordance), so this is the ONLY way a journey can assert that a
            // real succession actually re-pointed the user's groups. It rides
            // here rather than under `settings` because it deliberately
            // outlives `settings.clear_session()` at the account switch.
            "succession_sweep":
                crate::settings::recovery::sweep_state_json(app.succession_sweep.as_ref()),
            // The MEMBER side of a succession — `succession_sweep`'s twin for
            // the seat that *receives* the statement. The sweep above is read
            // on the succeeding client; nothing rendered anywhere reports what
            // the audience's own poll and witness did with what it published,
            // and every failure there leaves the participant row looking
            // untouched. See `conv_backend::witness_state_json`.
            "succession_witness":
                crate::conversations::conv_backend::witness_state_json(&app.conversations),
            // The external sync agent's live `{running, locations}` — the linux
            // `sync_state_json` twin, polled by the shared
            // `conftest.py::_wait_for_engine`. See `sync_agent::state_json` for
            // why the pair is exactly this and why there is no `files` key.
            "sync": crate::sync_agent::state_json(&app.sync_agent),
        },
        // The cross-app `messages` object — the state-protocol read the
        // shared `ActionLayer` tries *before* falling back to the
        // `error-message`/`warning-message`/`info-message` elements. `error`
        // reads the SAME `error_line_text()` the element paints (state-vs-UI
        // honesty); `warning`/`info` carry the test-agent injected lines, the
        // linux SharedState twin. All three keys are always present so the
        // action layer treats the state as authoritative.
        "messages": {
            "error": app.error_line_text(),
            "warning": app.injected_warning,
            "info": app.injected_info,
        },
        "machine_method_result": machine_method_result_json(machine_result),
        // The `enable_caldav_mailbox` test command's outcome, in linux's exact
        // wire shape (`{"ok": true}` / `{"ok": false, "error": ".."}`) — the key
        // `helpers/mail_dedicated_nest.py` polls. Absent until a run completes,
        // which is how the helper distinguishes "not finished" from "failed".
        "caldav_mailbox_reply": match app.settings.caldav_mailbox_reply() {
            None => Value::Null,
            Some(Ok(())) => json!({ "ok": true }),
            Some(Err(e)) => json!({ "ok": false, "error": e }),
        },
        // The `serve_enable_folder` test command's outcome, in linux's exact
        // wire shape — the key `helpers/webdav_roundtrip.py` polls, absent until
        // a run completes.
        "webdav_serve_reply": match app.settings.webdav_serve_reply() {
            None => Value::Null,
            Some(Ok(served_sets)) => json!({ "ok": true, "served_sets": served_sets }),
            Some(Err(e)) => json!({ "ok": false, "error": e }),
        },
    });
    // Set after the literal: the object above is at `json!`'s macro recursion
    // limit, so further keys are inserted rather than listed.
    //
    // The Devices/Folders machine's refresh triple — the `feed_reloads` twin
    // for `DevicesMachine::refresh` (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`,
    // which owns the contract). A plain atomic read; zeros before the machine
    // is built.
    state[fauna_e2e_agent::DEVICES_REFRESHES_KEY] =
        fauna_devices_machine::devices_refreshes_json(app.settings.devices_refresh_counts());
    state
}

/// No-op twin of [`state_json`] for a release build without `e2e-agent`, so
/// `Agent::publish` — part of the plumbing tui keeps unconditionally compiled
/// (`docs/goal/architecture/testing.md` convention 15's tui bullet) — still has a
/// same-signature callee. Unreachable in such a build: `start_if_enabled` returns
/// `None` there, so no `Agent` is ever constructed to publish from.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
fn state_json(_app: &App, _machine_result: Option<String>) -> Value {
    Value::Null
}

/// A reader method hands back its **JSON-serialized** value, so it must be
/// parsed before it goes into the state object — publishing the `String` as-is
/// double-encodes it, and the driver (which returns `machine_method_result`
/// verbatim, no decode) then reads `"\"http://…\""` where the test expects
/// `http://…`, or a JSON *string* where it expects the provisioning-snapshot
/// *object*. Linux parses in exactly the same place; this is the one wire shape
/// `HttpBridgeDriver.call_machine_method` is specified against.
///
/// Unparseable JSON degrades to `Null` rather than smuggling a raw string
/// through — a reader that can't serialize has no value to report.
/// The `account_pump_cycles` state value, extracted because a bare block in
/// `json!` reads as an object literal. A plain atomic read (convention 11
/// corollary: no blocking I/O on the state path).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn account_pump_cycles_json(app: &App) -> Value {
    // The shape is shared with every other hosting app (linux today, the
    // `fauna-ffi` seat next) so one cross-app contract cannot grow two
    // spellings — the `fauna_conversations::state_json` pattern, for the
    // account pump. `None` here still means "no store yet", which the shared
    // builder publishes as `(0, 0)`; a convention-11 refusal is the key being
    // absent, never a zero.
    fauna_client_account_runtime::account_pump_cycles_json(app.settings.account_store.as_ref())
}

/// [`fauna_e2e_agent::SHARE_SERVE_TALLY_KEY`]'s body, or `null` on a build
/// without the share plane (an absent leg, never an empty tally).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn share_serve_tally_json() -> Value {
    #[cfg(feature = "p2p-share")]
    {
        serde_json::to_value(fauna_sync_engine::share_serve_tally::snapshot())
            .unwrap_or(Value::Null)
    }
    #[cfg(not(feature = "p2p-share"))]
    {
        Value::Null
    }
}

fn machine_method_result_json(machine_result: Option<String>) -> Value {
    machine_result
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null)
}

/// Run a gesture the **agent** way: await the network half and fold its outcome
/// before the single-shot reply.
///
/// This is the await half of the actuation duality; `App::spawn_gesture` is the
/// spawn half. Both call [`crate::app::gesture_work`] — the one door — so the
/// per-page dispatch lives in exactly one place and this fn only decides *how*
/// to run what the door hands back.
///
/// Awaiting is not a style choice: element reads are single-shot (no implicit
/// retry in `HttpBridgeDriver`), so a click's effect must have landed **and**
/// been folded before `/element/click` replies, or the driver's very next read
/// sees the pre-click frame.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
async fn run_gesture(app: &mut App, gesture: crate::element::Gesture) {
    match crate::app::gesture_work(app, gesture) {
        crate::app::GestureWork::None => {}
        crate::app::GestureWork::Page(op) => {
            // The one exception to "the agent awaits a click's op": work that
            // deliberately outlives the click (see `PageOp::outlives_click`).
            // Awaiting those would hold the HTTP reply hostage to a nest the
            // journey may have killed on purpose, and no consumer reads the
            // result off the click anyway — they poll the resulting surface.
            // The second exception: a feed op while a test holds the next feed
            // reload (see `feed_reload_held`).
            if op.outlives_click()
                || (matches!(*op, crate::app::PageOp::Feed(_)) && feed_reload_held(app))
            {
                app.spawn_page_op(*op);
            } else {
                let outcome = op.run().await;
                crate::app::apply_page_outcome(app, outcome);
            }
        }
        crate::app::GestureWork::Wizard(machine, action, payload) => {
            let sink = crate::session::confirm_identity_sink(app);
            crate::wizard::run_action(machine, sink, action, payload).await;
        }
        crate::app::GestureWork::Launch(action) => {
            let tx = app.tx.clone();
            crate::launch::perform(app, &tx, action).await;
        }
        // Awaited, so the driver's next read sees the machine's settled state.
        crate::app::GestureWork::Retire(work) => work.run().await,
    }
}

/// Navigate the **agent** way: apply the nav, then **await** the nav-edge
/// refresh it implies before returning.
///
/// The await is the point. `App::apply` hands the refresh back rather than
/// firing it, and the driver's ready-ack contract (a nav doesn't return until
/// the triggering command is acked AND ready) then guarantees the fetch has
/// landed before any subsequent read — with zero races. A fire-and-forget spawn
/// here would let `navigate_to("contacts")` return before the roster arrived, so
/// the driver's very next `count("contact-row")` could read the pre-fetch frame.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
async fn run_nav_enter(app: &mut App, page: crate::pages::Page) {
    if let Some(op) = app.apply(page) {
        // Except while a test holds the next feed reload (`feed_reload_held`).
        if matches!(op, crate::app::PageOp::Feed(_)) && feed_reload_held(app) {
            app.spawn_page_op(op);
            return;
        }
        let outcome = op.run().await;
        crate::app::apply_page_outcome(app, outcome);
    }
}

/// Whether a test holds the NEXT feed reload (`FeedManager::hold_next_reload_for_test`,
/// armed by the `feed_hold_next_reload` command). While it does, the agent STARTS a
/// feed op rather than await it: the held reload cannot land until the test releases
/// it, and the release is a command this same loop must run — so an awaited feed op
/// would park the agent, and with it every read and the release itself. What the test
/// is holding the reload open for is to read the page while it is in flight
/// (`feed.md` § The read model), which only a free loop can answer.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn feed_reload_held(app: &App) -> bool {
    app.feed
        .manager
        .as_ref()
        .is_some_and(|m| m.reload_hold_armed_for_test())
}

/// Perform one element op against the current frame's registry — the tui
/// analogue of linux's `agent::perform`. Reads default-safe (no `error` key →
/// HTTP 200); actuation errors map to 404 in the shared front-end.
///
/// Async because a wizard click **awaits** its machine call: element reads are
/// single-shot (no implicit retry in `HttpBridgeDriver`), so the driver's next
/// `is_visible` must already see the new step. Awaiting here — on the main
/// loop, which dequeues the next agent request only after the following
/// redraw rebuilds the registry — makes `click(); is_visible()` deterministic.
/// The keyboard path spawns the same action instead, so a slow probe never
/// freezes the render loop.
///
/// Unreachable in a release build without `e2e-agent`: `start_if_enabled`
/// never returns `Some`, so nothing ever drives this call — but its body
/// still compiles out entirely (testing.md convention 15) rather than rely
/// on unreachability alone.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub async fn perform(_app: &mut App, _registry: &Registry, _req: &ElementReq) -> Value {
    json!({ "error": "e2e automation not built into this artifact" })
}

/// Is refusal tui's DEFAULT? **Yes, and it must stay yes.**
///
/// Unlike linux — which landed the same gate mid-2026-08 in the staging posture
/// convention 11 prescribes for an app that has never refused — tui has refused
/// a disabled `click` since its automation surface existed. `default_strict` is
/// a per-HOST parameter of the shared gate precisely so the two can differ:
/// making tui permissive to "match linux" would be a regression, not a
/// harmonization.
///
/// Permissive mode is still reachable here (`--permissive-actuation` /
/// `FAUNA_E2E_PERMISSIVE_ACTUATION`), and that is deliberate: it is the
/// measuring instrument any later broad change to this app re-measures itself
/// with — an enumerating sweep that turns nothing red — not staging scaffolding
/// to delete.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
const TUI_REFUSES_DISABLED_ACTUATION_BY_DEFAULT: bool = true;

/// Consult the element's LIVE enabled state before an actuation route drives it.
///
/// `Some(refusal)` → reply it verbatim (it carries its own 409); `None` → drive
/// the control. The decision, the refusal shape, the `DISABLED-ACTUATION`
/// marker and both staging flags live in shared Rust
/// (`fauna_e2e_agent::gate_actuation`), which linux hosts too — so the two
/// direct-Rust agents cannot drift, and one grep spans both apps' sweep logs
/// (priority #2).
///
/// tui needs no `folding`/ancestor analogue: `Element::enabled` is already the
/// effective, post-gate value, because `App::page_elements` applies the offline
/// gate and the screen-time lock before the registry ever sees an element — the
/// same one door paint and the focus ring read.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn gate(route: &str, id: &str, index: usize, enabled: bool) -> Option<Value> {
    fauna_e2e_agent::gate_actuation(
        route,
        id,
        index,
        enabled,
        TUI_REFUSES_DISABLED_ACTUATION_BY_DEFAULT,
    )
}

#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub async fn perform(app: &mut App, registry: &Registry, req: &ElementReq) -> Value {
    let id = req.id.as_str();
    let scope = &req.scope;
    match req.kind {
        ElementKind::Count => json!({ "count": registry.matches(id, scope).count() }),
        // Painted this frame == visible: the render loop only draws the
        // current page + chrome, so absence from the registry is invisibility.
        ElementKind::Visible => {
            json!({ "visible": registry.matches(id, scope).next().is_some() })
        }
        ElementKind::Enabled => json!({
            "enabled": registry.matches(id, scope).next().map(|e| e.enabled).unwrap_or(false)
        }),
        // A missing widget reports `{"error": "not found"}` instead of an
        // empty `text`, so "not on screen" is distinguishable from "on screen
        // but empty" — matching linux's `agent.rs` and
        // apple/android/windows, which already raise on a missing text read.
        // `Visible`/`Enabled` above stay `unwrap_or(false)`: missing=false is
        // the documented cross-app predicate contract every driver relies on;
        // only `Text` needs the split.
        ElementKind::Text => match registry.matches(id, scope).nth(req.index) {
            Some(e) => json!({ "text": e.text.clone() }),
            None => json!({ "error": "not found" }),
        },
        // `get_attr(id, key)`: `req.arg` is the attribute name (the driver's
        // `?attr=` query param). Read the matching element's inline attr, or
        // `Null` when the element or that key is absent — the same "absent reads
        // Null" contract the GUI apps' AT-SPI/UIA attr reads honor.
        //
        // **`disabled` is DERIVED, not inline** (fixed 2026-07-30). It is a
        // universal cross-app attribute every GUI app answers off the widget's own
        // sensitivity, and no tui page sets it as an inline pair — so before this,
        // `get_attr(id, "disabled")` answered `Null` for *every* element on tui.
        // That did not merely lose one assertion: the shared capability suite reads
        // `disabled in ("false", None)`, which `Null` satisfies, so **every
        // enabled-affordance assertion in the fleet passed vacuously on tui** while
        // the `== "true"` disabled-affordance ones could only fail. Deriving it from
        // `Element::enabled` makes both directions real. Inline attrs still win, so
        // a page that ever needs to say something else can.
        ElementKind::Attr => json!({
            "value": registry
                .matches(id, scope)
                .nth(req.index)
                .and_then(|e| {
                    e.attrs
                        .iter()
                        .find(|(k, _)| *k == req.arg)
                        .map(|(_, v)| Value::String(v.clone()))
                        .or_else(|| {
                            (req.arg == "disabled")
                                .then(|| Value::String((!e.enabled).to_string()))
                        })
                        .or_else(|| {
                            // The frame's own measurement — linux's `in-viewport`
                            // attribute, the same "middle inside the visible
                            // band" rule (`drivers/tui.py::in_viewport`). `Null`
                            // when the frame has nothing to say, never a guess.
                            (req.arg == "in-viewport")
                                .then(|| e.in_view.map(|v| Value::String(v.to_string())))
                                .flatten()
                        })
                        .or_else(|| {
                            // The full list of option TEXTS this frame painted
                            // for a picker — not just the selected one — JSON-
                            // encoded so `drivers/base.py::option_texts` can
                            // assert the whole set is pairwise distinct, which
                            // a single selected-value read-back cannot do (two
                            // colliding options read back identically once one
                            // is picked). `Null` (not `"[]"`) for a non-picker
                            // element, mirroring the web bridge's own `null`
                            // for a non-`<select>` — a real picker with zero
                            // options is a different, distinguishable fact.
                            (req.arg == "options").then(|| match &e.select {
                                Some((_, options)) => {
                                    Value::String(serde_json::to_string(options).unwrap_or_default())
                                }
                                None => Value::Null,
                            })
                        })
                })
                .unwrap_or(Value::Null)
        }),
        ElementKind::Click => {
            let Some(entry) = registry.matches(id, scope).nth(req.index) else {
                return json!({ "error": "not found" });
            };
            // Structural first, state second: "not actuable" says the element
            // is not a button AT ALL (a wrong id — a different bug class), and
            // is true whatever its enabled state. Only once the route applies
            // does the gate ask whether the UI permits it.
            let Some(action) = entry.action.clone() else {
                return json!({ "error": "not actuable" });
            };
            if let Some(refusal) = gate("click", id, req.index, entry.enabled) {
                return refusal;
            }
            match action {
                Action::Gesture(gesture) => run_gesture(app, gesture).await,
            }
            json!({ "ok": true })
        }
        // The element's SECOND gesture (`Element::double_clickable`) — the
        // month day cell's "open the new-event compose prefilled with this
        // date", where a single press drills into Day view instead
        // (`ui/events.md` § Layout & flow).
        //
        // Dispatched DIRECTLY, never as two timed clicks. Two clicks would make
        // the test depend on beating a wall-clock threshold, which convention 14
        // forbids; running the second gesture is also the only honest answer,
        // since aliasing onto `Click` would let a caller believe the
        // double-click arm ran when it did not (`testing.md` point 11). The
        // threshold that turns two real mouse presses into this lives on the
        // human path in `main.rs`, where wall-clock belongs.
        //
        // An element without a second gesture is refused PER ELEMENT. This used
        // to be a blanket client-wide refusal reading "a terminal has no
        // double-press … will be a keypress, not a second click" — a design
        // claim that both contradicted the ratified Outlook model above and
        // rested on a false premise, since tui has had mouse input since
        // 2026-07-20. Terminals do not label a double-press; every GUI toolkit
        // synthesizes one from two presses at one target inside a threshold,
        // and so does this client.
        ElementKind::DoubleClick => {
            let Some(entry) = registry.matches(id, scope).nth(req.index) else {
                return json!({ "error": "not found" });
            };
            let Some(action) = entry.dbl.clone() else {
                return json!({ "error": "no double-press gesture on this element" });
            };
            if let Some(refusal) = gate("double-click", id, req.index, entry.enabled) {
                return refusal;
            }
            match action {
                Action::Gesture(gesture) => run_gesture(app, gesture).await,
            }
            json!({ "ok": true })
        }
        ElementKind::Type | ElementKind::Clear => {
            let Some(entry) = registry.matches(id, scope).nth(req.index) else {
                return json!({ "error": "not found" });
            };
            let Some(field) = entry.field.clone() else {
                return json!({ "error": "not editable" });
            };
            // Typing into a disabled field is the same illegal act as clicking
            // a disabled button — convention 11 says so in as many words, and
            // this arm honoured it for as long as the gate existed on `click`
            // alone. The human path refuses it too (`App::focused_input`), so
            // the two agree rather than the harness out-reaching the user.
            let route = match req.kind {
                ElementKind::Clear => "clear",
                _ => "type",
            };
            if let Some(refusal) = gate(route, id, req.index, entry.enabled) {
                return refusal;
            }
            // `type` appends (the driver's `clear_and_type` clears first), so
            // it behaves like real keystrokes into a focused field.
            let value = match req.kind {
                ElementKind::Clear => String::new(),
                _ => format!("{}{}", app.field(field.clone()), req.arg),
            };
            // Awaited: a search write is a re-query, and the driver's next
            // `count` is single-shot. Started instead while a test holds the next
            // feed reload (`feed_reload_held`) — a search is a feed reload.
            let held = feed_reload_held(app);
            if let Some(pending) = app.set_field(field, value) {
                if held && matches!(pending, crate::app::PendingWrite::FeedSearch(_)) {
                    tokio::spawn(pending.run());
                } else {
                    pending.run().await;
                }
            }
            json!({ "ok": true })
        }
        // `vps-location-picker` is the one <select>-style picker in the wizard
        // (radios and checkboxes are Click targets). The driver selects by the
        // option's **display value**, so the target maps it back to the id the
        // machine wants. Awaited like a Click, for the same single-shot-read
        // reason.
        ElementKind::Select => {
            let Some(entry) = registry.matches(id, scope).nth(req.index) else {
                return json!({ "error": "not found" });
            };
            let Some((target, options)) = entry.select.clone() else {
                return json!({ "error": "not selectable" });
            };
            // BEFORE the membership check below, and the order is load-bearing
            // for the diagnosis rather than merely for correctness: a disabled
            // picker's option list is routinely empty or stale — often for the
            // very reason it is disabled (`folder-paywall-tier-select` offers
            // nothing precisely because the creator has no tiers) — so asking
            // membership first answers "this frame painted []" for a control
            // whose real story is "you cannot touch it at all". Both are 409s,
            // so only the message distinguishes them.
            if let Some(refusal) = gate("select", id, req.index, entry.enabled) {
                return refusal;
            }
            // Refuse a value this frame never offered. A driver that can drive
            // the picker to a state no keystroke can reach is testing something
            // no user can do — convention 8's "API-only mutation path" wearing a
            // UI costume — and it greens while the option list itself is broken
            // (found the hard way: the first `test_upload_into_a_freshly_created
            // _empty_folder` PASSED against the un-fixed option list, because
            // `select` wrote the set through without asking whether Media had
            // painted it). The keyboard path cycles `options`, so membership here
            // IS reachability. `409`, not the default `404`: the element was
            // found, so the driver must not read this as "not rendered yet" and
            // burn its scroll-retry loop.
            let Some(chosen) = fauna_e2e_agent::select_match(&options, &req.arg) else {
                return json!({
                    "error": format!(
                        "select target {:?} is not offered by {:?} — this frame painted [{}]",
                        req.arg,
                        id,
                        options.join(", "),
                    ),
                    "status": 409,
                });
            };
            // A picker dispatches through the same one door as a button, so a
            // feed select and a wizard select need no separate code path here.
            // It used to be a THIRD hand-written copy of the per-page dispatch,
            // whose unmatched-gesture arm warned and then acked `{"ok": true}`
            // anyway — a select on any page the table forgot to list came back
            // green having done nothing, which is indistinguishable from a real
            // product bug (testing.md point 10). Routing through `gesture_work`
            // retires that arm by construction: every gesture is handled.
            //
            // Dispatch the **matched option**, never the raw wire string: the
            // suite drives stable keys and a picker may paint labels, so the
            // mutator must receive what the frame actually offered — which is
            // exactly what choosing that row with the keyboard would send. (An
            // exact match makes this a no-op; it matters only for the
            // normalized case, where handing on `req.arg` would reintroduce the
            // write-through bug one layer down, with a value the settings layer
            // then fails to resolve.)
            run_gesture(app, target.gesture(options[chosen].clone())).await;
            json!({ "ok": true })
        }
        ElementKind::WindowClose => json!({ "error": "no window on the tui client" }),
        ElementKind::Registry => registry.snapshot_json(),
        // A terminal app owns no clipboard: it asks the terminal to set one
        // over OSC 52, and the tui driver IS that terminal, reading the copy
        // off the pty (`drivers/tui.py::get_clipboard_text`).
        ElementKind::ClipboardText => {
            json!({ "error": "the tui clipboard is read off the pty by the driver" })
        }
        // `Enter` on an actuable element is the KEYBOARD's activation of it:
        // the ring moves to the element the way the user's arrow keys would,
        // and the gesture runs through `App::spawn_gesture` — the spawn half
        // of the actuation duality, exactly what `actuate_focused` does for a
        // focused button. So, unlike `Click`, the reply does NOT wait for the
        // gesture's network half: a submit is still in flight when this
        // returns, and the agent keeps serving `type`/reads meanwhile. That is
        // the only way a test can stand where a user stands who keeps typing
        // while their post sends (`feed.md` § User actions,
        // `post-submit-button`); a `Click` holds every other agent request
        // until the send has landed. A caller polls the resulting surface.
        //
        // Every other named key is a GUI text-field gesture (caret movement in
        // a styled compose field); the tui composer's caret has no styling to
        // reveal, so there is no consumer — refused, never a silent ack
        // (point 11).
        ElementKind::Key if req.arg == "Enter" => {
            let Some(entry) = registry.matches(id, scope).nth(req.index) else {
                return json!({ "error": "not found" });
            };
            let Some(action) = entry.action.clone() else {
                return json!({ "error": "not actuable" });
            };
            if let Some(refusal) = gate("key", id, req.index, entry.enabled) {
                return refusal;
            }
            if let Some(index) = entry.page_index {
                app.focus_page_element(index);
            }
            match action {
                Action::Gesture(gesture) => app.spawn_gesture(gesture),
            }
            json!({ "ok": true })
        }
        ElementKind::Key => json!({
            "error": format!(
                "press_key {:?} is not driven on the tui client — only Enter, the \
                 keyboard activation of an actuable element",
                req.arg
            ),
        }),
        // Targeted scroll-into-view — a REAL scroll. The registry is never
        // viewport-clipped, so every element is readable wherever it sits; but
        // the viewport follows the focus ring, and a dwell (`crate::feed::cues`)
        // or an `in-viewport` read measures the frame, not the registry. So this
        // moves the ring to the target exactly as the user's arrow keys would
        // (`App::focus_page_element`), and the next draw paints it in view. The
        // reply mirrors linux's shape (`{"found": true}` / an error).
        //
        // Loud rather than green whenever nothing moved: an absent element, or
        // a page element with no focusable control at or after it that the
        // last frame did not already show — a scroll acked without moving would
        // let a dwell measure nothing (testing.md convention 11). The sidebar
        // and the shell chrome never scroll, so they are simply found.
        ElementKind::ScrollIntoView => match registry.matches(id, scope).nth(req.index) {
            None => json!({ "error": "not found" }),
            Some(entry) => match entry.page_index {
                None => json!({ "found": true }),
                Some(index) if app.focus_page_element(index) => json!({ "found": true }),
                Some(_) if entry.in_view == Some(true) => json!({ "found": true }),
                Some(_) => json!({
                    "error": format!(
                        "cannot scroll {id} into view: nothing at or after it takes focus, \
                         and the viewport follows the focus ring"
                    ),
                }),
            },
        },
    }
}

/// Apply one state-protocol command. The ack happens either way — the driver's
/// poll must terminate — but an unrecognized command is logged loudly rather
/// than silently greened.
///
/// Async so `call_machine_method` can drive the machine's **async** methods to
/// completion before the ack (`onboarding.md` § E2E bridge contract: "Async
/// methods block until complete"). The main loop already awaits this call, so
/// it costs nothing; the shared sync dispatcher, by contrast, drops every async
/// method into its silent-ignore arm.
// 6 of the 8 params are `AgentRequest::Command`'s own fields, borrowed straight
// from the one destructure site (main.rs) — a wrapper struct would just move the
// naming here without reducing real complexity for this single-caller function.
///
/// Unreachable in a release build without `e2e-agent` (see [`perform`]'s
/// same note) — compiled out rather than left to unreachability alone.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
#[allow(clippy::too_many_arguments)]
pub async fn apply_command(
    _app: &mut App,
    _tx: &mpsc::UnboundedSender<crate::app::UiMessage>,
    _action: &str,
    _state: &Value,
    _method: &str,
    _json_arg: &str,
    _posts: &Value,
    _payload: &Value,
) -> CommandResult {
    CommandResult {
        recognized: false,
        machine_result: None,
    }
}

/// Dispatch, then make a refusal **loud on the app's own `error-message`** —
/// convention 11's "honour it or fail loudly", in the one place every refusal
/// funnels through, so no future arm can add a silent `recognized(false)`.
///
/// A wrapper rather than a line in the main loop for two reasons: the loop is not
/// unit-testable, and an arm that returns `recognized(false)` early (a bad payload,
/// a pre-auth manager) would bypass an end-of-function hook.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
#[allow(clippy::too_many_arguments)]
pub async fn apply_command(
    app: &mut App,
    tx: &mpsc::UnboundedSender<crate::app::UiMessage>,
    action: &str,
    state: &Value,
    method: &str,
    json_arg: &str,
    posts: &Value,
    payload: &Value,
) -> CommandResult {
    let result = dispatch_command(app, tx, action, state, method, json_arg, posts, payload).await;
    if !result.recognized {
        app.report_refused_agent_command(action);
    }
    result
}

/// `barrier`'s tui mechanism: apply every [`crate::app::UiMessage`] **already
/// queued** on the app's UI channel, then return the count.
///
/// Why tui needs a drain at all, when the command ack is already ordered after
/// the apply: the ack orders the command against *itself*, not against the other
/// `select!` arms. `tokio::select!` polls its branches in **random** order, so a
/// `UiMessage` sitting in `rx` when the command arrives has no guarantee of being
/// applied first — the agent arm can win the race and ack while the earlier work
/// is still queued. That is precisely the false-pass convention 14's negative
/// asserts must not inherit.
///
/// `try_recv` to exhaustion is the right bound, and the reason is the contract's
/// own wording — *work enqueued before the command*. Anything enqueued before the
/// command reached the loop is already in the channel, so draining what is there
/// is complete; blocking for more would instead wait on work enqueued *after*,
/// which the barrier neither promises nor could terminate on.
///
/// Returns the number applied so the caller can log it; the count is not part of
/// the contract (a correct barrier over an empty queue drains zero).
///
/// Unreachable in a release build without `e2e-agent` (see [`perform`]'s same
/// note: `start_if_enabled` never returns `Some`, so no barrier can arrive) —
/// compiled to the inert twin below rather than left to unreachability alone.
/// The twin exists because this was the ONE automation entry point without one,
/// which made the shipped tui flavor fail to compile — found 2026-08-19 by the
/// shipped-profile ruling's workspace release build (`build-system.md`
/// § Shipped-profile overflow checks), the first thing to build tui in release.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn drain_pending_ui_messages(
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<crate::app::UiMessage>,
) -> usize {
    let mut applied = 0;
    while let Ok(msg) = rx.try_recv() {
        app.handle_message(msg);
        applied += 1;
    }
    applied
}

/// The inert release twin — see the gated variant's doc just above.
#[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
pub fn drain_pending_ui_messages(
    _app: &mut App,
    _rx: &mut mpsc::UnboundedReceiver<crate::app::UiMessage>,
) -> usize {
    0
}

/// Ack a real-wire `conversations_real_*` command, making a failure **loud on
/// the app's own `error-message`** (convention 11) instead of a
/// `tracing::error!` no driver can read.
///
/// The command stays `recognized` either way — it *was* dispatched, and the
/// refusal slot already distinguishes "never tried" from "tried and failed". The
/// driver reads the reason through `error_text()`, which is what lets the action
/// layer raise at the failing command rather than at whichever later product
/// assertion first notices the missing effect.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn report_real_outcome(app: &mut App, action: &str, outcome: Result<(), String>) -> CommandResult {
    if let Err(e) = outcome {
        app.report_failed_agent_command(action, &e);
    }
    CommandResult {
        recognized: true,
        machine_result: None,
    }
}

#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
#[allow(clippy::too_many_arguments)]
async fn dispatch_command(
    app: &mut App,
    tx: &mpsc::UnboundedSender<crate::app::UiMessage>,
    action: &str,
    state: &Value,
    method: &str,
    json_arg: &str,
    posts: &Value,
    payload: &Value,
) -> CommandResult {
    let recognized = |recognized| CommandResult {
        recognized,
        machine_result: None,
    };
    match action {
        // The only way to drive a `Failed`/quoted-embed-badge or a pre-resolved
        // link preview (a real signed post the nest serves is only ever
        // Unchecked/Verified, and a real link-preview resolve needs a live
        // OpenGraph fetch) — the tui twin of linux's `handle_feed_inject_posts`.
        // No-op pre-auth: the feed manager is built at the post-auth hook, so
        // `app.feed.manager` is `None` until then.
        // `barrier` — the causal anchor (`e2e-conventions.md` § convention 14).
        // Recognized here so the refusal path stays quiet, but the WORK is in
        // `main.rs`'s command arm: the drain needs `&mut rx`, which only the loop
        // owns. See `drain_pending_ui_messages` for why the drain is the shape
        // tui needs. ⚠ The split means the tier_1 pin
        // (`barrier_drains_work_queued_before_the_command`) covers the drain's
        // SEMANTICS only — that the loop actually calls it is pinned end-to-end
        // by `test_agent_barrier.py`, which drives the real HTTP command path.
        fauna_e2e_agent::BARRIER => recognized(true),
        // The barrier's self-test probe: enqueue UI work on the real channel and
        // return WITHOUT applying it. That asymmetry is the whole test — the
        // command's own ack is early by construction, so the token can only be
        // visible if `barrier` drained the channel.
        fauna_e2e_agent::BARRIER_PROBE => {
            let token = payload
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if token.is_empty() {
                // Convention 11: a probe with no token would ack green and prove
                // nothing, which is the silent-drop failure wearing a disguise.
                app.report_failed_agent_command(
                    fauna_e2e_agent::BARRIER_PROBE,
                    "payload needs a non-empty `token`",
                );
                return recognized(true);
            }
            let count = payload
                .get("count")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(fauna_e2e_agent::BARRIER_PROBE_DEFAULT_COUNT);
            for i in 0..count {
                let value = fauna_e2e_agent::barrier_probe_value(&token, i);
                if tx.send(crate::app::UiMessage::BarrierProbe(value)).is_err() {
                    app.report_failed_agent_command(
                        fauna_e2e_agent::BARRIER_PROBE,
                        "ui channel closed",
                    );
                    break;
                }
            }
            recognized(true)
        }
        "feed_inject_posts" => {
            let Some(manager) = app.feed.manager.clone() else {
                tracing::debug!("[agent] feed_inject_posts: no feed manager (pre-auth)");
                return recognized(false);
            };
            let specs: Vec<fauna_feed::test_support::TestPostSpec> =
                match serde_json::from_value(posts.clone()) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("[agent] feed_inject_posts: bad posts payload: {e}");
                        return recognized(false);
                    }
                };
            manager.set_feed_snapshot_for_test(fauna_feed::test_support::feed_snapshot_with_posts(
                specs,
            ));
            recognized(true)
        }
        // The archive-import restart-resume journey's causal anchor (testing.md
        // convention 14): arms the machine's one-shot pause after N settled
        // records, so the test can stop a run at a known point instead of
        // racing a wall clock. `{records: u64}`.
        //
        // A missing machine is a LOUD failure on the app's own `error-message`
        // (convention 11), not a quiet `recognized(false)`: unlike
        // `feed_inject_posts` this command's whole purpose is to change what
        // happens later in the journey, so a silently-unarmed pause would show
        // up as an unexplained assertion failure several steps downstream.
        "archive_import_pause_after" => {
            let records = payload.get("records").and_then(|v| v.as_u64()).unwrap_or(0);
            let Some(machine) = crate::settings::archive_import_machine(&app.settings) else {
                app.report_failed_agent_command(
                    "archive_import_pause_after",
                    "no archive-import machine (pre-auth, or the glue refused)",
                );
                return recognized(true);
            };
            machine.set_test_pause_after_records(records);
            recognized(true)
        }
        // The feed twin of `conversations_inject_page_error` above — there is no
        // *product* path that fails a feed fetch on demand (a real failure needs
        // the nest's own query to error), so this drives
        // `FeedManager::inject_error_for_test` directly. Was apple-only until
        // now.
        // `{key: string, message: string}`.
        "feed_inject_error" => {
            let Some(manager) = app.feed.manager.clone() else {
                tracing::debug!("[agent] feed_inject_error: no feed manager (pre-auth)");
                return recognized(false);
            };
            let key = payload
                .get("key")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("feed.error_load");
            let message = payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("feed load failed")
                .to_string();
            manager.inject_error_for_test(fauna_core::localized::LocalizedText::key_arg(
                key, "message", message,
            ));
            recognized(true)
        }
        // Hold the NEXT feed reload open, and release it: the one way to read the
        // page WHILE a reload is in flight (`feed.md` § The read model — a refresh
        // keeps its posts until the new page lands, a switch clears them up front)
        // without a clock. `FeedManager::hold_next_reload_for_test`; while armed,
        // this agent starts feed ops instead of awaiting them (`feed_reload_held`).
        "feed_hold_next_reload" => {
            let Some(manager) = app.feed.manager.clone() else {
                tracing::debug!("[agent] feed_hold_next_reload: no feed manager (pre-auth)");
                return recognized(false);
            };
            manager.hold_next_reload_for_test();
            recognized(true)
        }
        "feed_release_held_reload" => {
            let Some(manager) = app.feed.manager.clone() else {
                tracing::debug!("[agent] feed_release_held_reload: no feed manager (pre-auth)");
                return recognized(false);
            };
            manager.release_held_reload_for_test();
            recognized(true)
        }
        // Seed the live engagement-cue engine with a real `cues:v1` nest row (a
        // real network round trip, unlike `feed_inject_posts` above), so a
        // capture-less test can reach "Clear activity data" with something to
        // actually delete — the web/windows twin of the same seam.
        // `{content_ids: string[]}`. See `fauna_feed::FeedManager::set_cue_rollup_for_test`.
        "feed_seed_cue_rollup_for_test" => {
            let Some(manager) = app.feed.manager.clone() else {
                tracing::debug!(
                    "[agent] feed_seed_cue_rollup_for_test: no feed manager (pre-auth)"
                );
                return recognized(false);
            };
            let content_ids: Vec<String> = match payload.get("content_ids") {
                Some(v) => match serde_json::from_value(v.clone()) {
                    Ok(ids) => ids,
                    Err(e) => {
                        tracing::warn!(
                            "[agent] feed_seed_cue_rollup_for_test: bad content_ids payload: {e}"
                        );
                        return recognized(false);
                    }
                },
                None => Vec::new(),
            };
            match manager.set_cue_rollup_for_test(content_ids).await {
                Ok(()) => recognized(true),
                Err(e) => {
                    tracing::warn!("[agent] feed_seed_cue_rollup_for_test: {e}");
                    recognized(false)
                }
            }
        }
        // Flush the ward's batched Guardian Notify report now, instead of
        // waiting out the 60 s `LOCK_TICK` that normally carries it — the other
        // leg of the same tick as `screen_time_heartbeat` below, and the same
        // convention 14 `run_now` poke. Sleeping for it is not merely slow here
        // but a race: the ward's client only exists until the test switches
        // identity, so the flush has to happen inside a window the test itself
        // closes. Runs the REAL path (`due_notify_report` → the same `take_due`
        // gate the tick calls) and AWAITS the round trip, so the ack is a true
        // barrier — the guardian can be logged in the moment this returns.
        // The ≤hourly cadence gate is untouched; the first flush is eager, which
        // is the only case a test needs. web's twin is `$lib/family-notify-e2e`
        // (fire-and-forget), windows' is `GuardianNotifyCache.CheckNowAsync`.
        // Contract: `fauna_e2e_agent::FAMILY_NOTIFY_CHECK_NOW`.
        fauna_e2e_agent::FAMILY_NOTIFY_CHECK_NOW => {
            match crate::family::due_notify_report(app) {
                Some(op) => {
                    let outcome = op.run().await;
                    crate::family::apply_outcome(app, outcome);
                    recognized(true)
                }
                // Nothing pending (or pre-auth) is a legitimate answer — the
                // accumulator is empty or the knob is off — and unlike the
                // heartbeat above it is genuinely common, since the poke is
                // safe to call speculatively. Logged, not failed: the property
                // under test is positive, so a flush that never happened fails
                // loudly at the guardian's readout rather than silently here.
                None => {
                    tracing::info!(
                        "[agent] family_notify_check_now: nothing due (empty accumulator, \
                         knob off, or pre-auth)"
                    );
                    recognized(true)
                }
            }
        }
        // Ask the client receive loop for one cycle NOW, instead of waiting out
        // its backstop ticker (30 s in production, and the e2e's answer used to
        // be shortening it to 2 s — still a wall-clock dependence, convention
        // 14). Runs the REAL path: the poke is an arm of the loop's own
        // `select!` and expands the identical `full_sweep!` the ticker does, so
        // a poked delivery exercises the drain → ingest → decrypt chain exactly
        // as a ticked one would.
        //
        // Fire-and-forget on purpose — the barrier is `conv_receive_cycles`, not
        // this ack (an awaited reply would hang when no loop is running).
        // Pre-auth, or a session that never started its loop, is a legitimate
        // quiet no-op: the property under test is positive, so a cycle that
        // never ran fails at the consumer's own deadline poll, naming the app.
        // Contract: `fauna_e2e_agent::CONV_RECEIVE_NOW`.
        fauna_e2e_agent::CONV_RECEIVE_NOW => match app.conversations.real_session.as_ref() {
            Some(session) => {
                session.poke_receive_cycle();
                recognized(true)
            }
            None => {
                tracing::info!("[agent] conv_receive_now: no conversations session yet (pre-auth)");
                recognized(true)
            }
        },
        // One full account-pump pass NOW (`AccountStoreHandle::reconcile_now`
        // — the ticker's own work on demand, never a bypass), then a ceremony
        // drive so any act the pass left owed (an unposted custody receipt,
        // an owed registry write) posts without waiting for a production
        // edge. `fauna_e2e_agent::ACCOUNT_PUMP_NOW` owns the contract.
        //
        // Fire-and-forget like the receive poke — the barrier is the
        // `account_pump_cycles` counters, and a pass that never ran fails at
        // the consumer's own deadline poll. Pre-auth (no store yet) is a
        // legitimate quiet no-op for the same reason as `conv_receive_now`.
        fauna_e2e_agent::ACCOUNT_PUMP_NOW => match app.settings.account_store.clone() {
            Some(store) => {
                let ctx = crate::settings::custody_ctx_of(&app.settings);
                let session = app.conversations.real_session.clone();
                tokio::spawn(async move {
                    match store.reconcile_now().await {
                        Ok(report) => {
                            tracing::info!(?report, "[agent] account_pump_now: pass complete");
                        }
                        Err(e) => tracing::warn!("[agent] account_pump_now: {e}"),
                    }
                    if let Some(ctx) = ctx {
                        crate::custody_glue::spawn_drive(
                            ctx.nest,
                            ctx.secret,
                            session,
                            Some(store),
                        );
                    }
                });
                recognized(true)
            }
            None => {
                tracing::info!("[agent] account_pump_now: no account store yet (pre-auth)");
                recognized(true)
            }
        },
        // Drive the ward's screen-time heartbeat forward by `minutes` of
        // simulated foreground use, then flush a report, so a tier_3 test can
        // exhaust a daily budget without depending on wall-clock time. This is
        // convention 14's fake clock + `run_now` poke: a test that slept for a
        // real heartbeat interval would be *defunct* under testing.md § point
        // 14, not merely slow. The cadence and accrual rules themselves are pure
        // and already proven at tier_1 (`fauna_core::screen_time::tests` and
        // `crate::screen_lock::tests`); what this exercises is the WIRING — that
        // the client really calls `fauna.family.usage_report` and feeds the reply
        // back into the lock. The linux twin is its `screen_time_heartbeat` arm.
        // Compiled out of release artifacts (convention 15), like every command
        // in this table.
        "screen_time_heartbeat" => {
            let minutes = payload.get("minutes").and_then(|v| v.as_i64()).unwrap_or(0);
            tracing::info!(
                minutes,
                "[agent] screen_time_heartbeat: advancing the ward clock"
            );
            app.screen_lock.advance_test_clock(minutes * 60);
            // `focused = true`: the poke asserts what a ward actively using the
            // app accrues. Awaited rather than spawned — the driver's next
            // (single-shot, un-retried) element read must already observe the
            // lock this report's reply produces.
            match crate::family::due_usage_report(app, true) {
                Some(op) => {
                    let outcome = op.run().await;
                    crate::family::apply_outcome(app, outcome);
                    recognized(true)
                }
                // Nothing due is a legitimate answer (no budget set, or no
                // session yet) — but it is NOT silent: convention 11 forbids a
                // dropped command, and a green ack here with no report sent
                // would read downstream as a product bug in the heartbeat.
                None => {
                    tracing::warn!(
                        "[agent] screen_time_heartbeat: no report due (no budget set, or pre-auth)"
                    );
                    recognized(false)
                }
            }
        }
        // Hands the keyboard to the sidebar or the page pane, exactly as
        // `KeyCode::Left`/`Right` would (`App::handle_key`, `app.rs:2350-2351`)
        // — it calls the SAME `App::enter_sidebar_zone`/`enter_page_zone` a
        // real Left/Right keystroke calls. Exists for the same reason
        // `focus_move` does: `/element/key` has no consumers and is a
        // documented no-op, and a page-pane focus test needs `Zone::Page` —
        // which a `nav` patch alone never sets (`App::apply` is zone-agnostic
        // by design, so a page reached only through the state protocol stays
        // in whatever zone the session was already in).
        fauna_e2e_agent::SWITCH_PANE => match fauna_e2e_agent::switch_pane_target(payload) {
            Ok(fauna_e2e_agent::Pane::Page) => {
                app.enter_page_zone();
                recognized(true)
            }
            Ok(fauna_e2e_agent::Pane::Sidebar) => {
                app.enter_sidebar_zone();
                recognized(true)
            }
            // Recognized-but-failed, not refused: the action name IS ours, so
            // the generic "not implemented on tui" line the refusal path writes
            // would replace a message that names the actual mistake with one
            // that misdirects. `report_failed_agent_command` keeps the parser's
            // own wording on `error-message` (convention 6).
            Err(reason) => {
                app.report_failed_agent_command(fauna_e2e_agent::SWITCH_PANE, &reason);
                recognized(true)
            }
        },
        // Moves the focus ring exactly as `KeyCode::Tab`/`Down`
        // (`KeyCode::BackTab`/`Up`) would (`App::handle_key`, `app.rs:2346-2347`):
        // it calls the SAME `App::focus_next`/`focus_prev` the real key handler
        // calls, so a test proving a focus-paint invariant end-to-end (real
        // `ui::render`, real registry) exercises the identical state mutation a
        // human's keystroke performs — the only thing skipped is `KeyEvent`
        // parsing, which this class of bug never lived in. Exists because
        // `/element/key` has no consumers and is a documented no-op
        // (`fauna-e2e-agent::handle`), and the rows this matters most for
        // (`settings/root.rs`'s rail) are deliberately id-less, so no
        // per-element click/key can reach them by id anyway.
        fauna_e2e_agent::FOCUS_MOVE => match fauna_e2e_agent::focus_move_request(payload) {
            Ok((direction, times)) => {
                for _ in 0..times {
                    match direction {
                        fauna_e2e_agent::FocusDirection::Next => app.focus_next(),
                        fauna_e2e_agent::FocusDirection::Prev => app.focus_prev(),
                    }
                }
                recognized(true)
            }
            // See the `switch_pane` arm above for why a bad payload is
            // recognized-and-failed rather than refused.
            Err(reason) => {
                app.report_failed_agent_command(fauna_e2e_agent::FOCUS_MOVE, &reason);
                recognized(true)
            }
        },
        "patch" => {
            let mut ok = true;
            if let Some(obj) = state.as_object() {
                for (key, value) in obj {
                    match key.as_str() {
                        "nav" => apply_nav(app, value).await,
                        "session" => {
                            ok &= crate::session::apply_session_patch(app, tx, value);
                        }
                        // `set_input_files` on a native driver is NOT a file
                        // picker — it is `set_state({"compose": {"file": path}})`
                        // (`drivers/http_bridge.py`). Stage the path exactly as a
                        // human typing into the target's own path input would, so
                        // both paths converge on one value; `apply_compose` routes
                        // on `target` and matches that control's staging timing.
                        "compose" => ok &= apply_compose(app, value).await,
                        // The cross-app transient-message injection
                        // (`{"messages": {"error"|"warning"|"info": text|null}}`)
                        // — linux's messages patch, verbatim semantics.
                        "messages" => ok &= apply_messages(app, value),
                        other => {
                            tracing::warn!("[agent] unsupported patch key {other:?}");
                            ok = false;
                        }
                    }
                }
            }
            recognized(ok)
        }
        // `logout` ("clear session, return to onboarding") intentionally takes
        // the same arm as `reset`, exactly like linux's `"reset" | "logout"`
        // handler — and like tui's own sign-out-button, whose `SignOutConfirm`
        // already reuses `App::reset` (`settings.md` § User actions: identical
        // local effect).
        "reset" | "logout" => {
            app.reset();
            recognized(true)
        }
        // The cross-app E2E bridge (`onboarding.md` § E2E bridge contract):
        // forward (name, json_arg) to the *shared* dispatcher, so every
        // `set_*_for_test` / `seed_*` / `navigate_to_*` fixture works here with
        // no per-method churn — the same delegation linux, apple and windows do.
        // Reader methods return JSON the driver reads back as
        // `state.machine_method_result`; setters return None.
        //
        // We go through the **async** dispatcher, which additionally runs the
        // machine's async methods (`verify_dns`, `verify_vps`,
        // `wizard_submit_claim_code`, …) to completion — through the sync one
        // they fall into the silent-ignore arm and ack green having done
        // nothing. Awaiting here (rather than hand-listing the async names, as
        // linux does in its own shell) keeps the name table in shared Rust.
        "call_machine_method" => {
            // The **registry** arms first. They need this app's own
            // `AccountRegistry` — built from tui's `SecretStore` — which no free
            // dispatcher can reach, so shared Rust holds the name table and the
            // semantics (`fauna_client_accounts::call_registry_method_for_test`)
            // and tui contributes only the registry. One delegation, and every
            // later registry-level seam costs this file nothing.
            if let fauna_client_accounts::RegistryMethodOutcome::Handled(machine_result) =
                fauna_client_accounts::call_registry_method_for_test(
                    &crate::session::registry(app),
                    method,
                    json_arg,
                )
            {
                return CommandResult {
                    recognized: true,
                    machine_result,
                };
            }
            let machine_result = match method {
                // The one arm the shared dispatcher deliberately leaves to the
                // client: provisioning spawns a task that outlives the call
                // (minutes — the driver polls `provisioning_snapshot` instead of
                // blocking on the ack), so *which* runtime owns it is
                // platform-divergent. tui's main loop is tokio, so the
                // production entry points — the very ones the
                // `provisioning-start-button` / `provisioning-retry-button`
                // click, not a test-only bypass — spawn correctly as-is.
                "start_provisioning" => {
                    app.wizard.machine.clone().start_provisioning();
                    None
                }
                "retry_provisioning" => {
                    app.wizard.machine.clone().retry_provisioning();
                    None
                }
                _ => {
                    app.wizard
                        .machine
                        .clone()
                        .call_machine_method_async(method.to_string(), json_arg.to_string())
                        .await
                }
            };
            // A `set_step_for_test` lands on a page with a different element
            // count; re-seat the focus ring before the next draw.
            app.clamp_focus();
            CommandResult {
                recognized: true,
                machine_result,
            }
        }
        // Mint the logged-in actor's shared MSEK via the CalDAV-enable recipe —
        // the tui twin of linux's `enable_caldav_mailbox` arm (`main.rs:3105` ->
        // `client.rs::enable_caldav_mailbox_for_test`). The dedicated-mail-nest
        // helper (`tests/e2e-unified/helpers/mail_dedicated_nest.py:94`) fires
        // this and then polls `state.caldav_mailbox_reply` for `{"ok": true}`.
        //
        // Awaited inline rather than spawned: this dispatcher already awaits, and
        // the ack must not land before the reply is set or the driver's first
        // poll would read the CLEARED value and wait out its own timeout.
        "enable_caldav_mailbox" => {
            app.settings.set_caldav_mailbox_reply(None);
            let password = payload
                .get("password")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            match app.settings.mail_machine() {
                Some(machine) => {
                    // A caller-provided password mints a credential the test
                    // knows (so a stock CalDAV client can AUTH as this actor);
                    // absent → generated. Same two-arm split as linux.
                    let outcome = match password {
                        Some(pw) => {
                            machine
                                .enable_caldav_mailbox_with_password("Default".to_string(), pw)
                                .await
                        }
                        None => machine
                            .enable_caldav_mailbox_with_generated_password("Default".to_string())
                            .await
                            .map(|_password| ()),
                    };
                    app.settings
                        .set_caldav_mailbox_reply(Some(outcome.map_err(|e| format!("{e:?}"))));
                }
                None => {
                    app.settings.set_caldav_mailbox_reply(Some(Err(
                        "mail-settings machine not built (not logged in?)".to_string(),
                    )));
                }
            }
            recognized(true)
        }
        // e2e-only fixture setup for the WebDAV round trip — a served,
        // content-keyed Sync set plus its MSEK-sealed `WebdavKeysBlob`, arranged
        // through the same `folders::serve_set` the `folder-webdav-toggle`
        // gesture runs (`SettingsState::serve_enable_folder_for_test`). The tui
        // twin of linux's command of the same name; `helpers/webdav_roundtrip.py`
        // polls `state.webdav_serve_reply`. Awaited inline, like the CalDAV arm
        // above and for its reason: the reply must be set before the ack.
        "serve_enable_folder" => {
            app.settings.set_webdav_serve_reply(None);
            let folder = payload
                .get("folder")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let create = payload
                .get("create")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let outcome = if folder.is_empty() {
                Err("payload needs a non-empty `folder`".to_string())
            } else {
                app.settings
                    .serve_enable_folder_for_test(
                        app.conversations.real_session.clone(),
                        folder,
                        create,
                    )
                    .await
            };
            app.settings.set_webdav_serve_reply(Some(outcome));
            recognized(true)
        }
        // e2e-only: select a message in a thread the way a Search `Mail` result
        // does — dispatching the very `Gesture::OpenSearchResult(SearchNav::Mail)`
        // a result click dispatches (`crate::search::open_result`: open the
        // thread, `select_thread_and_message`, land the focus ring — and so the
        // viewport — on the marked message). The tui twin of apple's and linux's
        // `conversations_select_message`, for the one reason they have it: a
        // single-seat run cannot always build a local index segment to search.
        "conversations_select_message" => {
            let field = |name: &str| {
                payload
                    .get(name)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let (thread_id, message_id) = (field("thread_id"), field("message_id"));
            if thread_id.is_empty() || message_id.is_empty() {
                app.report_failed_agent_command(
                    "conversations_select_message",
                    "payload needs a non-empty `thread_id` and `message_id`",
                );
                return recognized(true);
            }
            run_gesture(
                app,
                crate::element::Gesture::OpenSearchResult(fauna_client_search::SearchNav::Mail {
                    thread_id,
                    message_id,
                }),
            )
            .await;
            recognized(true)
        }
        // e2e-only: the fleet-scope removal convergence read — whether
        // `device_id_hex`'s plane `fauna.state.device-set` row reads
        // Removed/Enrolled from THIS app's own account runtime. A pure
        // on-demand reader, not a per-tick state key: it does async
        // local-store I/O (convention 11's corollary forbids that on the
        // state path), so it rides the same "reader method" shape
        // `call_machine_method` uses — JSON-serialized in `machine_result`
        // for `machine_method_result_json` to decode.
        "device_set_state" => {
            let device_id_hex = payload
                .get("device_id_hex")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let value = fauna_client_account_runtime::device_set_state_json(
                app.settings.account_store.as_ref(),
                device_id_hex,
            )
            .await;
            CommandResult {
                recognized: true,
                machine_result: Some(value.to_string()),
            }
        }
        // e2e-only: the read-state durability barrier (`conversation-read-state.md`
        // § The read-marker record) — this app's own account store's read
        // marker for `channel_id_hex`, so a test knows a read reached the store
        // before it quits the app. The `device_set_state` reader shape: async
        // store I/O, never a per-tick state key.
        "read_marker_state" => {
            let channel_id_hex = payload
                .get("channel_id_hex")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let value = fauna_client_account_runtime::read_marker_state_json(
                app.settings.account_store.as_ref(),
                channel_id_hex,
            )
            .await;
            CommandResult {
                recognized: true,
                machine_result: Some(value.to_string()),
            }
        }
        // The conversations mock-backend inject seams (the tui twins of linux's
        // `handle_conversations_*`). Their fields ride flat at the top level of
        // the request body, so they read `payload`, not `state` (only `patch`
        // populates `state`). No-op pre-auth: the manager is built at the post-auth
        // hook, so `app.conversations.manager` is `None` until then.
        "conversations_inject_inbound" => recognized(crate::conversations::inject_inbound(
            &app.conversations,
            payload,
        )),
        "conversations_create_mls_group" => recognized(crate::conversations::create_mls_group(
            &app.conversations,
            payload,
        )),
        "conversations_inject_send_failure" => recognized(
            crate::conversations::inject_send_failure(&app.conversations, payload),
        ),
        "conversations_inject_page_error" => recognized(crate::conversations::inject_page_error(
            &app.conversations,
            payload,
        )),
        "conversations_seed_resolved_link_preview" => recognized(
            crate::conversations::seed_resolved_link_preview(&app.conversations, payload),
        ),
        // Drop a thread's cached attachment bytes the way the store's budget
        // eviction does (the shared `evict_thread_attachments_for_test`), so a test
        // reaches the re-fetch of an evicted attachment — and the declared
        // placeholder of one with nowhere to be fetched from — without filling the
        // 128 MiB store. Nothing evicted is a FAILED command, never an ack
        // (convention 11): a render asserted after a no-op evict witnesses nothing.
        "conversations_evict_attachment" => {
            let field = |name: &str| {
                payload
                    .get(name)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let (thread_id, filename) = (field("thread_id"), field("filename"));
            let evicted = app.conversations.manager.as_ref().map_or(0, |manager| {
                manager.evict_thread_attachments_for_test(
                    fauna_conversations::ThreadId(thread_id.clone()),
                    filename.clone(),
                )
            });
            if evicted == 0 {
                app.report_failed_agent_command(
                    "conversations_evict_attachment",
                    &format!("no resident attachment named {filename:?} in thread {thread_id:?}"),
                );
            }
            recognized(true)
        }
        // The action layer's `accept_recipient_chip` calls this when no suggestion
        // is visible (it can't press Enter over the bridge). The manager decides
        // which picker is active and whether its text parses.
        //
        // Reported through `report_real_outcome` rather than `recognized(bool)`:
        // the arm IS recognised, so the wrapper's generic "not implemented on
        // tui, or its payload/preconditions were rejected" would be both wrong
        // and less useful than the reason the manager can name. Convention 11
        // separates "never tried" (a refusal) from "tried and the op did not
        // happen" (a failure), and a commit that lands no chip is the latter.
        "conversations_accept_recipient" => {
            let outcome = crate::conversations::accept_recipient(&app.conversations).await;
            report_real_outcome(app, action, outcome)
        }
        // The real-wire FaunaMls surface (the tui twins of linux's
        // `handle_conversations_real_*`). The real session is wired
        // unconditionally at login, so "enable" is a readiness probe (the driver
        // polls `data.conv_real_backend_active`) and "disable" only wipes the
        // manager's threads for a later snapshot test. The drivers are plain
        // async manager calls — this dispatch already awaits, so each failure is
        // reported on the app's own `error-message` via
        // `report_failed_agent_command` (convention 11). It used to go to
        // `tracing::error!` under a `recognized(true)` ack, which is exactly the
        // swallow that made a nest `forbidden` read as "no Welcome delivered".
        "conversations_enable_real_faunamls" => recognized(true),
        "conversations_disable_real_faunamls" => {
            crate::conversations::conv_backend::disable_e2e_real_backend(&app.conversations);
            recognized(true)
        }
        "conversations_real_resolve_send_new" => {
            let outcome = crate::conversations::conv_backend::e2e_resolve_send_new(
                &app.conversations,
                payload
                    .get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload.get("body").and_then(|v| v.as_str()).unwrap_or(""),
            )
            .await;
            report_real_outcome(app, action, outcome.map(|_| ()))
        }
        "conversations_real_send" => {
            let outcome = crate::conversations::conv_backend::e2e_send(
                &app.conversations,
                payload
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload.get("body").and_then(|v| v.as_str()).unwrap_or(""),
            )
            .await;
            report_real_outcome(app, action, outcome)
        }
        "conversations_real_send_attachment" => {
            use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
            let bytes = match B64.decode(
                payload
                    .get("data_base64")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            ) {
                Ok(b) => b,
                Err(e) => {
                    // Reported like every sibling arm rather than as a bare
                    // `recognized(false)`: the wrapper's generic refusal text
                    // would overwrite this one, and "bad base64" is the whole
                    // diagnosis.
                    return report_real_outcome(app, action, Err(format!("bad base64: {e}")));
                }
            };
            let outcome = crate::conversations::conv_backend::e2e_send_with_attachment(
                &app.conversations,
                payload
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload.get("body").and_then(|v| v.as_str()).unwrap_or(""),
                payload
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload
                    .get("mime_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                bytes,
            )
            .await;
            report_real_outcome(app, action, outcome)
        }
        "conversations_real_add" => {
            let outcome = crate::conversations::conv_backend::e2e_add(
                &app.conversations,
                payload
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload
                    .get("peer_actor_id_hex")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload
                    .get("peer_handle")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )
            .await;
            report_real_outcome(app, action, outcome.map(|_| ()))
        }
        "conversations_real_remove" => {
            let outcome = crate::conversations::conv_backend::e2e_remove(
                &app.conversations,
                payload
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload
                    .get("peer_actor_id_hex")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload
                    .get("peer_handle")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )
            .await;
            report_real_outcome(app, action, outcome)
        }
        "conversations_real_rename" => {
            let outcome = crate::conversations::conv_backend::e2e_rename(
                &app.conversations,
                payload
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                payload.get("label").and_then(|v| v.as_str()).unwrap_or(""),
            )
            .await;
            report_real_outcome(app, action, outcome)
        }
        // Backups-page audit-alert e2e (tests/e2e-unified/tests/test_backups.py).
        // Runs one **real** audit pass — real connection to each configured
        // destination, real `fauna.backup.custody.list`, the client's own
        // persisted observation high-water — with only the clock shifted by
        // `now_offset_secs`. That shift is what a staleness proof needs and
        // cannot fake any other way: freshness floors a destination's high-water
        // at its `added_at`, so a destination enrolled seconds ago is *correctly*
        // never stale in real time (convention 14 — poke the clock, never sleep
        // out a 48 h window).
        // Trigger the production post-auth silent challenge
        // (`security.md` § Post-auth surfacing, channel 3). This runs the REAL
        // refresh — the same `session::silent_refresh` body the background
        // spawn drives — so the verdict it produces is the production one; it is
        // NOT a shortcut that fakes `IdentityChanged`. The e2e
        // (`test_nest_identity_pin_post_auth.py`) seeds an unprovable pin, fires
        // this, and asserts the blocking surface, so a faked verdict would
        // assert nothing about the classify → escalate → re-enter path.
        //
        // Awaited, not spawned: the ack then means "the refresh COMPLETED", which
        // is what makes the module's negative test ("a valid refresh must not
        // escalate") a measurement rather than a race against an in-flight call.
        // Force the launch machine's bearer refresh NOW — the production
        // `LaunchMachine::refresh_token`, the very call the TTL loop and the
        // 401 path make, not a test-only twin. The wrong-clock refresh witness
        // (case M) fires it so the refresh outcome is observed inside the test
        // instead of an hour later; awaited inline so the ack lands after the
        // machine's transition, the way `enable_caldav_mailbox` below reasons.
        // Contract: `fauna_e2e_agent::ALERT_SWEEP_WAKE` — end the current
        // identity's re-sweep wait so the production loop sweeps again; the
        // caller's barrier is `alert_sweep_passes`, never this ack.
        // Contract: `fauna_e2e_agent::RECONNECT_BACKOFF` — pace this session's
        // reconnect retries (never the threshold), or restore them with `{}`.
        fauna_e2e_agent::RECONNECT_BACKOFF => {
            match (
                app.session.as_ref(),
                fauna_e2e_agent::reconnect_backoff_bounds(payload),
            ) {
                (Some(session), Ok(bounds)) => {
                    session.client.set_reconnect_backoff_for_test(bounds)
                }
                // Convention 11: a pace that did not land leaves the production
                // one in force, and the journey would spend its budget waiting.
                (None, _) => app.report_refused_agent_command(
                    "reconnect_backoff (no authenticated session, so no client to pace)",
                ),
                (Some(_), Err(e)) => app.report_failed_agent_command("reconnect_backoff", &e),
            }
            recognized(true)
        }
        fauna_e2e_agent::ALERT_SWEEP_WAKE => {
            if app.authenticated() {
                app.alert_sweep_wake.wake();
            } else {
                // Convention 11: no session means no loop, so the wake would be
                // acked and read by nobody.
                app.report_refused_agent_command(
                    "alert_sweep_wake (no authenticated session, so no sweep loop to wake)",
                );
            }
            recognized(true)
        }
        fauna_e2e_agent::LAUNCH_REFRESH_TOKEN => {
            match app.launch_machine.clone() {
                Some(machine) => machine.refresh_token().await,
                // Convention 11: never silently dropped — no machine means the
                // app never ran the launch routing (a bypass-seeded session).
                None => app.report_refused_agent_command(
                    "launch_refresh_token (no launch machine in this session)",
                ),
            }
            recognized(true)
        }
        "silent_sign_in" => {
            // Convention 11: a command the app cannot honour fails loudly on the
            // app's own `error-message`, never silently. The honest reasons are
            // pre-auth (no active account) and a malformed stored secret.
            //
            // The REFUSAL slot, not `errors[page]`: both honest reasons are
            // pre-auth states, where a launch surface owns the screen and the
            // page-keyed map is never read — an error there would be invisible
            // for exactly the case that produces it.
            if let Err(e) = crate::session::run_silent_sign_in(app, tx).await {
                app.report_refused_agent_command(&format!("silent_sign_in ({e})"));
            }
            recognized(true)
        }
        // The `fauna://` route's e2e seam (`apps/tui.md` § System integration →
        // *In-app routes*): feeds a URI to the same door the launch argument
        // takes, so a journey needs no relaunch inside PAR's 90-second window.
        // The route's page work is AWAITED, so the card is painted when this
        // acks. Signed out, the door holds the route and the loop applies it
        // after sign-in. An unparseable URI is a refusal — the launch argument
        // drops one silently, but a driver that sent one must hear about it.
        "open_route" => {
            let Some(route) = payload
                .get("uri")
                .and_then(|v| v.as_str())
                .and_then(fauna_core::app_route::AppRoute::parse)
            else {
                return recognized(false);
            };
            if let Some(op) = app.apply_route(route) {
                let outcome = op.run().await;
                crate::app::apply_page_outcome(app, outcome);
            }
            recognized(true)
        }
        "backup_audit_run_now" => {
            let offset = payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            crate::backup_audit::set_clock_offset_secs(offset);
            match crate::backups::audit_op(&app.backups, app.sync_agent.custodian_store()) {
                // The production op, awaited like every other agent-driven op —
                // so the banners are already painted when this command acks and
                // the driver's next read cannot see the pre-pass frame.
                Some(op) => {
                    let outcome = op.run().await;
                    crate::backups::apply_outcome(app, outcome);
                }
                // Convention 11: a command the app cannot honour fails loudly on
                // the page's own `error-message`, never silently. The one honest
                // reason is pre-auth — there is no session to audit with.
                None => {
                    app.errors.insert(
                        crate::pages::Page::Backups,
                        "backup_audit_run_now: no authenticated session".to_string(),
                    );
                }
            }
            recognized(true)
        }
        // Runs ONE custodian pull pass on the sync agent's hosted replica and
        // returns what it did, as `state.machine_method_result`. The production
        // first pass is `PERIODIC_INTERVAL` away — `CustodianPull::run_loop`
        // deliberately mutes the interval's immediate first tick — so this is
        // the causal barrier the tier_3 enroll→pull→check-in→status proof runs
        // on, never a settle-sleep (convention 14). See
        // `RequestMethod::CustodianRunPassNow` for why a poke is not the thing
        // sync-agent.md § Control plane split forbids over this seam.
        "custodian_pull_run_now" => {
            // Convention 14's fake clock, same `now_offset_secs` spelling as
            // `backup_audit_run_now` above and `atproto_delegation_advance_clock`
            // below. Absent/0 is an ordinary pass at the real clock; a positive
            // offset is how a test reaches the self-audit's 24-hour debounce
            // without sleeping a day out.
            let offset = payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            match app.sync_agent.custodian_run_pass_now(offset).await {
                Ok(Some(report)) => {
                    return CommandResult {
                        recognized: true,
                        machine_result: Some(
                            serde_json::json!({
                                "hosting": report.hosting,
                                "kinds_run": report.kinds_run,
                                "held_bytes": report.held_bytes,
                                "cap_state": report.cap_state,
                                "audit_state": report.audit_state,
                                "checked_in": report.checked_in,
                            })
                            .to_string(),
                        ),
                    };
                }
                // Convention 11: a command the app cannot honour fails loudly on
                // the page's own `error-message`, never silently. Both arms are
                // real refusals a caller must be able to tell apart — no agent
                // on this platform at all, versus an agent that refused the op.
                Ok(None) => {
                    app.errors.insert(
                        crate::pages::Page::Backups,
                        "custodian_pull_run_now: tui drives no sync agent on this platform"
                            .to_string(),
                    );
                }
                Err(e) => {
                    app.errors.insert(
                        crate::pages::Page::Backups,
                        format!("custodian_pull_run_now: {e}"),
                    );
                }
            }
            recognized(true)
        }
        // Moves the D10 delegation row's RENDER clock (never the mint clock —
        // `authorize_external_apps` always stamps a fresh cert with the real
        // wall clock) so a lapse e2e can reach `expiring_soon`/`expired`
        // without sleeping out the real ~90-day window (convention 14 — a
        // fake clock, never a sleep). `now_offset_secs: 0` resets it — the
        // offset is process-wide and nothing auto-resets it
        // (`delegation_clock` module docs).
        "atproto_delegation_advance_clock" => {
            let offset = payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            fauna_atproto_settings_machine::set_delegation_clock_offset_secs(offset);
            match app.settings.atproto_machine() {
                // Awaited inline, same reason as `backup_audit_run_now`: the
                // recomputed liveness must already be painted when this
                // command acks, or the driver's next read races the old frame.
                // `refresh()` alone only updates the MACHINE's own state — the
                // page paints off `app.settings.atproto.snapshot`, a cached
                // copy only `apply_atproto_snapshot` (the single error-bridge
                // site every other Bluesky dispatch already funnels through)
                // updates.
                Some(machine) => {
                    machine.refresh().await;
                    crate::settings::apply_atproto_snapshot(app, machine.snapshot());
                }
                // Convention 11: fail loudly, never a silent no-op.
                None => {
                    app.errors.insert(
                        crate::pages::Page::Settings,
                        "atproto_delegation_advance_clock: no authenticated session".to_string(),
                    );
                }
            }
            recognized(true)
        }
        // Moves the Nests trust facet's RENDER clock — the `now` a grant's
        // liveness and a custodian's receipt freshness are judged against
        // (`fauna_client_capabilities::trust_clock`), never the mint/renew
        // clock. Both windows (~90-day grant, 48-hour receipt staleness) are
        // Rust constants, so a journey reaches `expiring soon` / `expired` /
        // stale only by moving the clock — convention 14's fake clock, never
        // a sleep. `now_offset_secs: 0` resets it; the offset is process-wide
        // and nothing auto-resets it.
        //
        // Nothing to re-render here: both folds read the clock on the Nests
        // page's nav-edge hydrate, so the journey's next navigate paints the
        // moved state (the `offline_share_advance_clock` shape).
        "trust_facet_advance_clock" => {
            let offset = payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            fauna_client_capabilities::trust_clock::set_clock_offset_secs(offset);
            recognized(true)
        }
        // Moves the co-present ceremony's ADMISSION clock — the `now` a
        // receive-act expectation is minted and judged against
        // (`fauna_sync_engine::ceremony_clock`, read per call by the `NowFn`
        // a seat is bound with). The window is a 15-minute Rust constant and
        // never a knob, so a journey can only witness "someone arriving after
        // it has lapsed is refused like a stranger" (`p2p.md` § Offline share
        // initiation) by moving the clock — convention 14's fake clock, never
        // a sleep. `now_offset_secs: 0` resets it; the offset is process-wide
        // and nothing auto-resets it, so a leftover value would lapse the
        // next expectation this process mints.
        //
        // Nothing to re-render: the offset is read at admission time, by the
        // seat's own listener, so unlike the delegation clock this command
        // owes no refresh. It is also correct with no seat bound yet — the
        // next bind's `now_fn` reads the same static.
        #[cfg(feature = "p2p-share")]
        "offline_share_advance_clock" => {
            let offset = payload
                .get("now_offset_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            fauna_sync_engine::ceremony_clock::set_clock_offset_secs(offset);
            recognized(true)
        }
        // Drops every connection a counterpart has open to THIS seat's
        // ceremony listener, keeping the listener up — the link between two
        // devices failing part-way, which a journey cannot otherwise cause.
        // It is how "the share picks up again without either person entering
        // the code a second time" (`p2p.md` § Offline share initiation) gets a
        // witness. Returns how many connections were dropped, so the journey
        // can prove there was one to drop. With no seat bound it fails loudly
        // (convention 11) rather than reporting a drop that never happened.
        #[cfg(feature = "p2p-share")]
        "offline_share_drop_connections" => match app.settings.offline_share.seat() {
            Some(seat) => CommandResult {
                recognized: true,
                machine_result: Some(seat.node.close_inbound().to_string()),
            },
            None => {
                app.errors.insert(
                    crate::pages::Page::Settings,
                    "offline_share_drop_connections: no ceremony seat is bound".to_string(),
                );
                recognized(true)
            }
        },
        // Turns the share plane's SERVE hold on or off
        // (`fauna_sync_engine::share_serve_tally`). While it is on, the next
        // manifest a peer asks this seat for parks unanswered, and the
        // `share_serve_tally` state key's `parked` count says so. That turns
        // "part-way through a transfer" into a state a journey can wait for,
        // cut with `offline_share_drop_connections`, and then release: the
        // parked request fails and is never counted as served. This is the
        // witness for "an interrupted transfer picks up where it stopped
        // without re-sending what arrived" (`p2p.md` § Cross-user shared-set
        // transfer). Process-wide, and nothing auto-resets it, so a journey
        // lifts it (`{"hold": false}`) before it asserts arrival.
        #[cfg(feature = "p2p-share")]
        "offline_share_hold_serves" => {
            let on = payload
                .get("hold")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            fauna_sync_engine::share_serve_tally::set_hold(on);
            recognized(true)
        }
        // Reads one shared set straight off another seat's share plane, as
        // THIS seat's identity, and reports what came back
        // (`fauna_sync_engine::share_probe`): the admission verdict, the rows,
        // and every manifest it asked for — `manifest_hashes` plus any the
        // rows name. It keeps asking after a refused admission, which the pump
        // never does, so from a seat the set was never shared with it is the
        // witness that "a person the folder was never shared with gets nothing
        // readable from your device" (`p2p.md` § Cross-user shared-set
        // transfer); from a member it is that witness's control. The peer is
        // named by its compare code (`offline-share-own-code`) and the set by
        // its raw MLS group id. Needs this seat's listener bound (either panel
        // opens it) and fails loudly without one (convention 11).
        #[cfg(feature = "p2p-share")]
        "offline_share_probe_set" => {
            let seat = app.settings.offline_share.seat();
            let arg = |key: &str| {
                payload
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let hashes: Vec<String> = payload
                .get("manifest_hashes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|h| h.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let Some(seat) = seat else {
                app.errors.insert(
                    crate::pages::Page::Settings,
                    "offline_share_probe_set: no ceremony seat is bound".to_string(),
                );
                return recognized(true);
            };
            match fauna_sync_engine::share_probe::probe_set_from_args(
                &seat.node,
                &arg("peer_code"),
                &arg("group_id_hex"),
                &hashes,
            )
            .await
            {
                Ok(report) => CommandResult {
                    recognized: true,
                    machine_result: serde_json::to_string(&report).ok(),
                },
                Err(e) => {
                    app.errors.insert(
                        crate::pages::Page::Settings,
                        format!("offline_share_probe_set: {e}"),
                    );
                    recognized(true)
                }
            }
        }
        // The folder↔set binding pair the shared `_wait_for_engine` fixtures
        // drive (`conftest.py::bound_location_media_app`,
        // `test_sync_live_apply.py`) — tui's leg of the commands linux and
        // windows already implement over their own control channels
        // (`sync-agent.md` § Implementation status today, the windows A5
        // follow-on paragraph).
        //
        // Both go through the **same** shared optimistic model the `folder-location-*`
        // Folders UI writes (`SyncAgentState::add_binding`/`remove_binding` over
        // `fauna_client_sync::agent::LocationBindingsModel`, = `AddLocation` +
        // the ref-keyed `SetLocationFolder`), so a command-driven bind and a user's bind are one
        // code path — a fixture precondition, not a test-only backdoor that could
        // pass while the real gesture is broken.
        "sync_add_location" => {
            let path = payload.get("path").and_then(|v| v.as_str());
            let folder = payload.get("folder").and_then(|v| v.as_str());
            // The set's `FolderRef` wire form — the bind is keyed by it alone,
            // exactly as the Folders UI's gesture is (`folder_ref_for_row`);
            // the name-keyed bind is retired, so a missing ref is refused below.
            let folder_id = payload.get("folder_id").and_then(|v| v.as_str());
            match (path, folder, folder_id) {
                (Some(path), Some(folder), Some(folder_id)) if app.sync_agent.is_active() => {
                    app.sync_agent.add_binding(
                        path.to_string(),
                        folder.to_string(),
                        folder_id.to_string(),
                    );
                    recognized(true)
                }
                // Convention 11: honour it or fail loudly on the app's own
                // `error-message` — never the silent no-op every mutator on
                // `SyncAgentState` performs before install. An unreachable agent
                // that acks clean reads downstream as a product bug in whatever
                // the test asserted next (here: "the engine never started"),
                // which is the exact misdiagnosis point 11 exists to prevent.
                // Keyed on `app.page` because `screen_error_text` reads the
                // CURRENT page's entry, and these commands arrive wherever the
                // fixture happens to be — usually not the Folders page.
                _ => {
                    let why = if app.sync_agent.is_active() {
                        "needs `path`, `folder` and `folder_id`"
                    } else {
                        "no agent surface (pre-auth, or this platform drives no agent)"
                    };
                    app.errors
                        .insert(app.page, format!("sync_add_location: {why}"));
                    recognized(true)
                }
            }
        }
        // Unbind. Accepts linux's `folder` key **or** windows' native `path`
        // (the same either-key tolerance the windows leg landed, so the shared
        // action layer stays uniform): the shared model exposes only
        // `remove_by_set`, so a `path` is resolved to its set through the
        // rendered rows first.
        "sync_remove_location" => {
            let by_set = payload
                .get("folder")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let by_path = payload.get("path").and_then(|v| v.as_str()).and_then(|p| {
                app.sync_agent
                    .rendered_locations()
                    .into_iter()
                    .find(|b| b.path == p)
                    .map(|b| b.folder)
            });
            match by_set.or(by_path) {
                Some(folder) if app.sync_agent.is_active() => {
                    app.sync_agent.remove_binding(&folder);
                    recognized(true)
                }
                // Convention 11, as above.
                _ => {
                    let why = if app.sync_agent.is_active() {
                        "needs `folder`, or a `path` matching a bound folder"
                    } else {
                        "no agent surface (pre-auth, or this platform drives no agent)"
                    };
                    app.errors
                        .insert(app.page, format!("sync_remove_location: {why}"));
                    recognized(true)
                }
            }
        }
        // `events_ensure_mail_enabled` lived here until the mail-settings page
        // landed: tui had no Settings surface to click, so the Events fixture
        // minted the actor's MSEK by calling the shared machine directly. The
        // page now exists, so `_enable_calendar_backend` drives the same real UI
        // path as every other app (`mail_settings.ensure_mail_enabled()`) and
        // the shim is gone — an API shortcut standing in for a user's clicks is
        // exactly what the e2e conventions' point 8 exists to prevent.
        other => {
            tracing::warn!("[agent] unsupported command action {other:?}");
            recognized(false)
        }
    }
}

/// `{"compose": {"file": "<path>", "target": "<element_id>"}}` — the native
/// drivers' file-attach path (`drivers/http_bridge.py::set_input_files`).
///
/// `target` disambiguates which staged buffer the path lands in — absent or
/// `compose-file` keeps the pre-existing feed-attach default so no other driver
/// call site needs to change.
///
/// **Two staging timings, both matching what the target's real affordance does**,
/// which is the contract `set_input_files`' own docstring spells out:
/// - feed's `compose-file` and profile's avatar/banner **defer** the read — the blob
///   is uploaded when the owning form submits (`post-submit-button` /
///   `profile-edit-save-button`), so the patch only fills the path buffer (feed's
///   field also stages the file's hash-less handle from a `stat`, exactly as a
///   typed path does — `crate::feed::set_field`);
/// - conversations' `attachment-button` **reads the bytes now**, because the
///   composer must render `dm-compose-attachment-chip` immediately and let the user
///   unstage before sending. So the patch fills the buffer *and* commits it,
///   exactly as a human's Enter on that `input_commit` element does.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
async fn apply_compose(app: &mut App, compose: &Value) -> bool {
    let Some(path) = compose.get("file").and_then(|v| v.as_str()) else {
        tracing::warn!("[agent] compose patch without a `file` key: {compose:?}");
        return false;
    };
    let target = compose.get("target").and_then(|v| v.as_str());
    let field = match target {
        Some("profile-edit-avatar") => {
            crate::element::Field::Profile(crate::profile::ProfileField::Avatar)
        }
        Some("profile-edit-banner") => {
            crate::element::Field::Profile(crate::profile::ProfileField::Banner)
        }
        Some("attachment-button") => crate::element::Field::Conversations(
            crate::conversations::ConversationsField::AttachPath,
        ),
        _ => crate::element::Field::Feed(crate::feed::FeedField::ComposeFile),
    };
    let _ = app.set_field(field, path.to_string());
    if target == Some("attachment-button") {
        // The commit half, through the same door the agent's `click` uses — so the
        // patch and a human's Enter on this `input_commit` run identical code. The
        // gesture is sync (a local file read + an in-memory manager stage), so the
        // chip is in the tree before the single-shot reply; awaiting the door keeps
        // that true if it ever grows an `Op`.
        run_gesture(
            app,
            crate::element::Gesture::Conversations(crate::conversations::Action::CommitAttachment),
        )
        .await;
    }
    true
}

/// `{"messages": {"error"|"warning"|"info": text|null}}` — inject (string) or
/// clear (null) a transient banner message, the cross-app test-agent patch
/// linux's messages handler implements. The injected error rides the shared
/// `error-message` line ([`App::error_line_text`]); warning/info paint the
/// global `warning-message`/`info-message` lines. All three clear on the next
/// nav patch ([`apply_nav`]).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn apply_messages(app: &mut App, messages: &Value) -> bool {
    let Some(obj) = messages.as_object() else {
        tracing::warn!("[agent] messages patch is not an object: {messages:?}");
        return false;
    };
    let mut ok = true;
    for (key, value) in obj {
        let slot = match key.as_str() {
            "error" => &mut app.injected_error,
            "warning" => &mut app.injected_warning,
            "info" => &mut app.injected_info,
            other => {
                tracing::warn!("[agent] unsupported messages level {other:?}");
                ok = false;
                continue;
            }
        };
        // A string injects; null (or any non-string) clears — linux's
        // `as_str()` branch shape.
        *slot = value.as_str().map(|s| s.to_string());
    }
    ok
}

/// `{"nav": {"stack": [{"view": "events"}]}}` — navigate to the top (= last)
/// stack entry's view. A `profile` entry may carry an `actor_id` (the OTHER
/// profile: `{"view": "profile", "actor_id": "<hex>"}`) — the state-protocol
/// equivalent of a contact-row tap.
///
/// Async (unlike every other nav path) only for the Settings branch: routing
/// to the Privacy sub-page returns a self-fetch `Op` that must be **awaited**
/// here, before this command acks — see `settings::route_subpage`'s docs for
/// why a fire-and-forget spawn would race a same-visit mutation.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
use fauna_core::format::profile_nav_target;

#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
async fn apply_nav(app: &mut App, nav: &Value) {
    // A nav patch dismisses injected transient messages (linux: "mirrors real
    // app behavior where navigating away dismisses transient messages").
    app.injected_error = None;
    app.injected_warning = None;
    app.injected_info = None;
    let Some(top) = nav
        .get("stack")
        .and_then(|s| s.as_array())
        .and_then(|arr| arr.last())
    else {
        return;
    };
    let Some(view) = top.get("view").and_then(|v| v.as_str()) else {
        return;
    };
    // A `profile` view carries an optional `actor_id` for the OTHER profile — it
    // sets the viewed actor, not just the page, so it goes through
    // `open_profile` (a bare `apply(Profile)` would always open SELF).
    //
    // The `actor_id` is NORMALIZED first (non-obvious half,
    // linux's `test_agent::profile_nav_target` mirrored): an id naming the
    // VIEWER resolves back to the SELF target, because `is_self` is
    // `viewing.is_none()` — passing the viewer's own hex through verbatim
    // would render their own profile in OTHER shape (follow button where the
    // edit button belongs). Blank ids resolve to SELF too, never to a profile
    // for the empty actor.
    if view == Page::Profile.view_name() {
        let entry_actor = top.get("actor_id").and_then(|v| v.as_str());
        match entry_actor {
            Some(raw) => {
                let own = app.session.as_ref().map(|s| s.actor_id.as_str());
                app.open_profile(profile_nav_target(raw, own));
            }
            None => run_nav_enter(app, Page::Profile).await,
        }
        return;
    }
    // A `settings` view carries an optional sub-page `id` (the admin-style
    // two-element nav `{"view":"settings"},{"view":"settings","id":"<page>"}` —
    // `settings.md` § Navigation model). Enter the tab through the one nav door
    // (which resets the shell to its Root on the nav edge), then route the id.
    if view == Page::Settings.view_name() {
        let id = top.get("id").and_then(|v| v.as_str());
        // `{"view":"settings","id":"nostr"}` is an ALIAS onto the top-level
        // Nostr page, not a settings sub-page: on tui, Nostr's home is the
        // `nostr-tab` sidebar row (`ui.yaml` navigation.tabs), exactly as web
        // renders one `NostrSettingsSection` from both `/app/nostr` and its
        // settings rail (`ui/nostr.md:91`). Routing it here means the shared
        // cross-app `NostrActions.navigate()` reaches the page without tui
        // inventing a settings sub-page the spec doesn't give it.
        if id == Some(Page::Nostr.view_name()) {
            run_nav_enter(app, Page::Nostr).await;
            return;
        }
        run_nav_enter(app, Page::Settings).await;
        let signals = app.feed.manager.clone();
        if let Some(op) = crate::settings::route_subpage(&mut app.settings, id, signals) {
            let outcome = op.run().await;
            crate::settings::apply_outcome(app, outcome);
        }
        // Entering the Account sub-page refreshes the account-switcher snapshot
        // from the registry — the two-element-nav twin of the rail-row
        // `OpenAccount` refresh. Account produces no self-hydrate `Op` (unlike
        // Mail/Privacy), so this is its nav-edge hook.
        if app.settings.sub == crate::settings::SubPage::Account {
            crate::settings::refresh_accounts(app);
        }
        // The permanent member-review page reads its roster on the nav edge, and
        // `route_subpage` cannot produce that op — the account-store seam lives on
        // `App` — so this is its nav-edge hook, the Account shape above. Awaited
        // like every other self-hydrate: acking before the read lands would let
        // a journey assert an empty page that is merely un-read.
        if app.settings.sub == crate::settings::SubPage::MemberReview
            && let Some(op) = crate::settings::member_review_hydrate_op(app)
        {
            let outcome = op.run().await;
            crate::settings::apply_outcome(app, outcome);
        }
        return;
    }
    // The admin shell — a GATED top-level page (`Page::Admin`), deliberately
    // outside `Page::ALL`, so it needs its own branch just like Settings above
    // (the `Page::ALL` lookup below can't find it). Enter through the one nav
    // door (which resets the shell to its Dashboard on the nav edge and loads it),
    // then route the optional sub-page `id` (`{"view":"admin","id":"admin-calendar"}`
    // — the ui.yaml page id the driver sends), mirroring the Settings branch. The
    // route's self-hydrate op is **awaited** here for the same single-shot-read
    // reason (a fire-and-forget spawn would race a same-visit mutation).
    if view == Page::Admin.view_name() {
        let id = top.get("id").and_then(|v| v.as_str());
        run_nav_enter(app, Page::Admin).await;
        if let Some(op) = crate::admin::route_subpage(&mut app.admin, id) {
            let outcome = op.run().await;
            crate::admin::apply_outcome(app, outcome);
        }
        return;
    }
    // `devices` is a legacy single-element nav (`{"view":"devices"}`) from
    // before the 2026-06-28 sync/folder UI unification, when it was a
    // top-level page — `devices` is no longer in `Page::ALL` (ui.yaml's
    // `navigation.pages`), only reachable today via the Settings shell
    // (`{"view":"settings","id":"devices"}`). The generic cross-app nav
    // smoke tests (`test_navigation.py`/`test_sp_authenticated.py`) still send
    // the bare form, so it's aliased onto the Settings branch — mirroring
    // linux's `test_agent.rs::` legacy-nav translator.
    if view == "devices" {
        run_nav_enter(app, Page::Settings).await;
        // The devices alias never reaches the Personalization branch, so the
        // live-manager half is irrelevant here.
        if let Some(op) = crate::settings::route_subpage(&mut app.settings, Some("devices"), None) {
            let outcome = op.run().await;
            crate::settings::apply_outcome(app, outcome);
        }
        return;
    }
    // `Page::Family` is GATED and so outside `Page::ALL` (like `Page::Admin`
    // above), but unlike Admin it is one flat page with no sub-page `id` to
    // route — so it needs no branch of its own, only inclusion in the lookup.
    // Leaving it out is what would make `navigate_to("family")` a silent
    // "unknown view" warn (testing.md point 11: never drop a command quietly).
    match Page::ALL
        .iter()
        .copied()
        .chain([Page::Family])
        .find(|p| p.view_name() == view)
    {
        // Through the one navigation door (`App::apply`), not a bare field
        // write: the driver's `navigate_to` is a `nav` patch, and pages with a
        // nav-edge side effect (the contacts refetch) must see it exactly as a
        // sidebar click would deliver it.
        Some(page) => run_nav_enter(app, page).await,
        None => tracing::warn!("[agent] nav to unknown view {view:?}"),
    }
}

#[cfg(test)]
mod heartbeat_tests {
    //! tui's half of convention 11's UI-thread heartbeat: the beat must tell a
    //! loop that is merely WAITING (idle, or parked in an arm on an op's own
    //! network await) from one whose thread is HELD by synchronous work. The
    //! first must read fresh, the second stale — that split is the whole
    //! reason the stamp exists, and a beat that cannot make it is worse than
    //! none, because it manufactures a verdict.
    use super::beat_while_pending_with;
    use fauna_e2e_agent::HEARTBEAT_CADENCE;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// A loop parked on a slow op that YIELDS keeps its stamp fresh for as long
    /// as it waits — this is the case a `select!` beat arm got wrong, because
    /// tui's Element and Command arms await their op inline and no arm is
    /// polled until the op returns. Virtual time, so the count is exact.
    #[tokio::test(start_paused = true)]
    async fn a_loop_waiting_on_a_slow_op_keeps_beating_at_the_cadence() {
        let beats = Arc::new(Mutex::new(0u32));
        let counted = Arc::clone(&beats);
        let wait = HEARTBEAT_CADENCE * 40;
        let out = beat_while_pending_with(move || *counted.lock().unwrap() += 1, async move {
            tokio::time::sleep(wait).await;
            "the op's own result"
        })
        .await;
        assert_eq!(
            out, "the op's own result",
            "the wrapped future's output passes through"
        );
        let beats = *beats.lock().unwrap();
        assert!(
            beats >= 40,
            "a 40-cadence wait must beat ~once per cadence (first tick is immediate), got {beats}"
        );
    }

    /// A loop whose thread is HELD synchronously beats nothing while held — so
    /// the stamp goes stale and the agent's verdict names synchronous work on
    /// the UI thread. Multi-threaded on purpose: it is the runtime flavor tui's
    /// `#[tokio::main]` uses, and it is the flavor on which a beat running
    /// anywhere but the loop's own future (a spawned task, another worker)
    /// would keep beating through the hold and read fresh — the false verdict
    /// this test exists to forbid.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_loop_whose_thread_is_held_beats_nothing_while_held() {
        let beats: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&beats);
        let hold: Arc<Mutex<Option<(Instant, Instant)>>> = Arc::new(Mutex::new(None));
        let held = Arc::clone(&hold);
        beat_while_pending_with(
            move || recorded.lock().unwrap().push(Instant::now()),
            async move {
                // WAIT first — a real await that parks the runtime, so the
                // timer driver turns and the ticker fires: the hold below then
                // starts with beats already on record, the shape of a loop that
                // was running and then got held. (A bare `yield_now` is not
                // enough: it reschedules without parking, and a tokio interval's
                // first tick needs a driver turn to complete.)
                tokio::time::sleep(HEARTBEAT_CADENCE * 2).await;
                let start = Instant::now();
                // sleep-ok: the phenomenon under test IS a thread held
                // synchronously; anything that yields would be the other case.
                std::thread::sleep(HEARTBEAT_CADENCE * 4);
                *held.lock().unwrap() = Some((start, Instant::now()));
            },
        )
        .await;
        let (start, end) = hold.lock().unwrap().expect("the hold ran");
        let beats = beats.lock().unwrap();
        assert!(
            beats.iter().any(|b| *b <= start),
            "the loop waited before the hold, so it had beaten — without that, \
             'no beat during the hold' would prove nothing"
        );
        let inside: Vec<_> = beats.iter().filter(|b| **b > start && **b < end).collect();
        assert!(
            inside.is_empty(),
            "no beat may land while the loop's thread is held ({} did, over a {:?} hold)",
            inside.len(),
            end - start
        );
    }

    /// No agent, no ticker: a real user's loop is awaited as it always was.
    #[tokio::test(start_paused = true)]
    async fn without_a_heartbeat_the_future_is_awaited_untouched() {
        let out = super::beat_while_pending(None, async { 7 }).await;
        assert_eq!(out, 7);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{authed_app, test_app};
    use fauna_ui_ids as ids;

    // profile_nav_target now delegates to
    // `fauna_core::format::profile_nav_target`, which carries its own
    // identical 6-case pin set — no local duplicate here.

    use crate::wizard::WizardField;
    use fauna_onboarding_machine::OnboardingStep;
    use fauna_ws_substrate::supervisor::ConnectionState;

    fn req(kind: ElementKind, id: &str) -> ElementReq {
        ElementReq {
            kind,
            id: id.to_string(),
            index: 0,
            scope: Vec::new(),
            arg: String::new(),
        }
    }

    fn frame_registry(app: &App) -> Registry {
        let mut registry = Registry::default();
        crate::ui::register_listed(app, &mut registry);
        registry
    }

    /// The Mail → Spam threshold draft, addressed through the same generic
    /// `Field` door both actuation paths use.
    fn threshold_field() -> crate::element::Field {
        crate::element::Field::Settings(crate::settings::SettingsField::MailSpamThresholdOverride)
    }

    /// An app parked on Mail → Spam with **no transport**, where
    /// `mail-spam-threshold-override-input` is desensitized by the offline gate
    /// (its commit is `fauna.bridges.set_spam_threshold_override`, classified
    /// `OnlineOnly`). The cheapest honest disabled INPUT in the client — and the
    /// only class there is, since a plain `Role::Input` carries no gesture and
    /// so is never offline-gated.
    fn disconnected_mail_spam_app() -> App {
        let mut app = authed_app();
        app.connection = fauna_ws_substrate::supervisor::ConnectionState::Disconnected;
        app.page = crate::pages::Page::Settings;
        app.settings.sub = crate::settings::SubPage::MailSpam;
        app
    }

    /// An app parked on Settings → Folders with one expanded website-enabled folder and
    /// no own tiers, where `folder-paywall-tier-select` paints DISABLED with the
    /// "create a tier first" hint — there is nothing to paywall to
    /// (`settings/folders.rs`; `ui/folders.md` § per-set paywall).
    ///
    /// Deliberately a CAPABILITY-disabled control rather than an offline-gated
    /// one, so the select pins prove the arm reads `entry.enabled` itself and
    /// not some connection-state proxy.
    fn paywall_select_app() -> App {
        let mut app = authed_app();
        app.page = crate::pages::Page::Settings;
        crate::settings::park_on_disabled_paywall_select_for_test(&mut app.settings);
        app
    }

    /// Every disabled-actuation refusal answers the shared shape: **409** (never
    /// the default 404, which would send `drivers/http_bridge.py` into its
    /// scroll-retry loop and report "not rendered yet" — the opposite
    /// diagnosis), naming the element and the route so one grep finds every
    /// instance and the failure diagnoses itself (convention 6).
    fn assert_disabled_refusal(reply: &Value, route: &str, id: &str) {
        assert_eq!(
            reply["status"], 409,
            "a disabled-actuation refusal must be a 409, not the default 404: {reply}"
        );
        let message = reply["error"]
            .as_str()
            .unwrap_or_else(|| panic!("refusal carries no `error` string: {reply}"));
        assert!(
            message.contains("element is disabled"),
            "the refusal must be greppable across every app's sweep: {message:?}"
        );
        assert!(
            message.contains(id),
            "the refusal must name the element the caller asked for: {message:?}"
        );
        assert!(
            message.contains(route),
            "the refusal must name the ROUTE, or a sweep log cannot be triaged \
             per route: {message:?}"
        );
    }

    fn test_tx() -> mpsc::UnboundedSender<crate::app::UiMessage> {
        mpsc::unbounded_channel().0
    }

    /// `perform` and `apply_command` are async only because each awaits a
    /// machine call; every assertion below is on a synchronous path, so a
    /// current-thread runtime suffices.
    fn run<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(fut)
    }

    async fn command_async(app: &mut App, action: &str, state: &Value) -> CommandResult {
        apply_command(
            app,
            &test_tx(),
            action,
            state,
            "",
            "",
            &Value::Null,
            &Value::Null,
        )
        .await
    }

    fn machine_command(app: &mut App, method: &str, json_arg: &str) -> CommandResult {
        run(apply_command(
            app,
            &test_tx(),
            "call_machine_method",
            &Value::Null,
            method,
            json_arg,
            &Value::Null,
            &Value::Null,
        ))
    }

    /// `barrier`'s tui contract, at the seam the main loop actually calls
    /// (`e2e-conventions.md` § convention 14): work already queued on the UI
    /// channel is applied *before* the barrier acks.
    ///
    /// The probe is what makes this assertable at all — `barrier_probe` enqueues
    /// and acks early by construction, so a token visible after `barrier` can only
    /// have got there through the drain. Red-verify by deleting the
    /// `drain_pending_ui_messages` call: the token stays `None`.
    #[tokio::test]
    async fn barrier_drains_work_queued_before_the_command() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = authed_app();

        // The probe enqueues onto `rx` and returns; it deliberately does NOT
        // apply, which is the asymmetry the whole self-test rests on.
        let probe = apply_command(
            &mut app,
            &tx,
            fauna_e2e_agent::BARRIER_PROBE,
            &Value::Null,
            "",
            "",
            &Value::Null,
            &json!({ "token": "tok-1", "count": 3 }),
        )
        .await;
        assert!(probe.recognized, "the probe is a recognized command");
        assert_eq!(
            app.barrier_probe, None,
            "the probe must ack BEFORE its work applies — otherwise the barrier \
             below is asserting nothing"
        );

        // The barrier's own arm is a no-op; the drain is the mechanism.
        apply_command(
            &mut app,
            &tx,
            fauna_e2e_agent::BARRIER,
            &Value::Null,
            "",
            "",
            &Value::Null,
            &Value::Null,
        )
        .await;
        let applied = drain_pending_ui_messages(&mut app, &mut rx);

        assert_eq!(applied, 3, "every queued message was applied, not just one");
        assert_eq!(
            app.barrier_probe.as_deref(),
            Some(fauna_e2e_agent::barrier_probe_value("tok-1", 2)).as_deref(),
            "after `barrier`, ALL work enqueued before it has run — the LAST \
             item's value is what proves the drain did not stop partway"
        );
    }

    /// The contract's bound: a barrier drains what was enqueued *before* it, and
    /// does not block waiting for work enqueued after. A barrier that waited for
    /// more could not terminate, and would silently convert every negative assert
    /// built on it into a hang.
    #[tokio::test]
    async fn barrier_does_not_wait_for_work_enqueued_after_it() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = authed_app();

        assert_eq!(
            drain_pending_ui_messages(&mut app, &mut rx),
            0,
            "an empty queue drains zero and returns — it does not block"
        );

        // Enqueue only AFTER the barrier has already returned.
        tx.send(crate::app::UiMessage::BarrierProbe("late".into()))
            .expect("send");
        assert_eq!(
            app.barrier_probe, None,
            "the earlier barrier made no promise about this message"
        );
    }

    /// Convention 11 again, on the probe itself: a token-less probe would ack
    /// green and prove nothing, so it must surface rather than pass quietly.
    #[tokio::test]
    async fn a_token_less_barrier_probe_surfaces_instead_of_passing_quietly() {
        let mut app = authed_app();

        apply_command(
            &mut app,
            &test_tx(),
            fauna_e2e_agent::BARRIER_PROBE,
            &Value::Null,
            "",
            "",
            &Value::Null,
            &json!({}),
        )
        .await;

        let shown = app
            .error_line_text()
            .expect("a probe with no token must surface on error-message");
        assert!(
            shown.contains(fauna_e2e_agent::BARRIER_PROBE),
            "the surfaced text names the command: {shown:?}"
        );
    }

    /// Convention 11: a command the app cannot honour lands on the app's own
    /// `error-message`, so the driver reads the *cause* instead of a green ack
    /// followed by a wrong-looking product assertion three steps later.
    #[tokio::test]
    async fn an_unknown_command_surfaces_on_error_message() {
        let mut app = authed_app();
        assert_eq!(app.error_line_text(), None, "clean before the refusal");

        let result = command_async(&mut app, "no_such_command_at_all", &Value::Null).await;

        assert!(!result.recognized, "an unknown action is not recognized");
        let shown = app
            .error_line_text()
            .expect("a refused command must surface on error-message, not only in a log");
        assert!(
            shown.contains("no_such_command_at_all"),
            "the surfaced text must name the refused action so the driver can \
             tell WHICH command was dropped: {shown:?}"
        );
        // The registered element, not just the accessor — this is what a driver's
        // `error_text()` actually reads.
        let registry = frame_registry(&app);
        let reply = perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "error-message"),
        )
        .await;
        assert_eq!(reply, json!({ "text": shown }));

        // ⚠ The load-bearing half — the only assertions here that DISCRIMINATE.
        // Both wrong slots tried before this one passed every assertion above:
        // `injected_error` clears on any nav patch, and the per-page `errors` map is
        // keyed by the page the refusal was raised on. Every action-layer helper
        // navigates (`navigate_to` IS `patch {"nav": …}`), and it commonly navigates
        // to a DIFFERENT page than the one the refusal came from — so pin both.
        let from_page = app.page;
        assert!(
            command_async(
                &mut app,
                "patch",
                &json!({ "nav": { "stack": [ { "view": "conversations" } ] } })
            )
            .await
            .recognized
        );
        assert_ne!(app.page, from_page, "the nav must actually change page");
        assert_eq!(
            app.error_line_text().as_deref(),
            Some(shown.as_str()),
            "a refusal must outlive the cross-page nav every action helper issues"
        );

        // …and it must NOT outlive the test: `reset()` is the per-test boundary the
        // `app` fixture drives, so a refusal cannot leak into the next test of a
        // reused app process.
        assert!(
            command_async(&mut app, "reset", &Value::Null)
                .await
                .recognized
        );
        assert_eq!(
            app.refused_agent_command, None,
            "reset is the per-test clear point for a refusal banner"
        );
    }

    /// …and an arm that was **honoured and then failed** takes it too — the case
    /// the `conversations_real_*` family used to route to `tracing::error!` under
    /// a green ack.
    ///
    /// The reason is the load-bearing half, not the fact of failure: the swallow
    /// this closes turned a nest `forbidden: recipient is not accepting new
    /// conversations` into a *missing* Welcome two assertions later, which reads
    /// as a half-completed MLS bootstrap and was triaged as one for a full pass.
    /// So assert the arm's own reason text reaches `error-message`, not merely
    /// that something did.
    #[tokio::test]
    async fn a_real_wire_command_that_fails_surfaces_its_reason() {
        let mut app = authed_app();
        assert_eq!(app.error_line_text(), None, "clean before the failure");

        // No conversations manager exists until the post-auth hook builds one, so
        // this arm is honoured and fails for a reason it can state exactly.
        let result = apply_command(
            &mut app,
            &test_tx(),
            "conversations_real_remove",
            &Value::Null,
            "",
            "",
            &Value::Null,
            &json!({
                "thread_id": "t1",
                // A well-formed actor id on purpose: a malformed one fails one
                // step EARLIER (the hex parse), which would pass every assertion
                // below without ever proving the manager-precondition arm reports.
                "peer_actor_id_hex":
                    "1111111111111111111111111111111111111111111111111111111111111111",
                "peer_handle": "carol",
            }),
        )
        .await;

        assert!(
            result.recognized,
            "the arm exists and ran — a failure is not a refusal"
        );
        let shown = app.error_line_text().expect(
            "a real-wire command that fails must surface on error-message, not only in a log",
        );
        assert!(
            shown.contains("conversations_real_remove"),
            "the text must name WHICH command failed: {shown:?}"
        );
        assert!(
            shown.contains("pre-auth"),
            "the text must carry the arm's OWN reason — a bare 'it failed' is the \
             same dead end as the log line this replaced: {shown:?}"
        );
    }

    /// …and an arm that *declines* is the same failure to a driver, so it takes
    /// the same funnel. `conversations_seed_resolved_link_preview` with no `url`
    /// is the concrete case: the arm exists, the payload is unusable.
    #[tokio::test]
    async fn a_declined_command_surfaces_too_not_just_an_unknown_one() {
        let mut app = authed_app();
        let result = apply_command(
            &mut app,
            &test_tx(),
            "conversations_seed_resolved_link_preview",
            &Value::Null,
            "",
            "",
            &Value::Null,
            &serde_json::json!({ "title": "no url here" }),
        )
        .await;
        assert!(!result.recognized);
        let shown = app.error_line_text().unwrap_or_default();
        assert!(
            shown.contains("conversations_seed_resolved_link_preview"),
            "a declined arm must surface too: {shown:?}"
        );
    }

    /// `alert_sweep_wake` with no session has no loop to wake: refused loudly
    /// (convention 11), never acked into a void a journey would then wait on.
    #[tokio::test]
    async fn an_alert_sweep_wake_with_no_session_is_refused() {
        let mut app = test_app();
        let result = apply_command(
            &mut app,
            &test_tx(),
            fauna_e2e_agent::ALERT_SWEEP_WAKE,
            &Value::Null,
            "",
            "",
            &Value::Null,
            &Value::Null,
        )
        .await;
        assert!(result.recognized);
        let shown = app.error_line_text().unwrap_or_default();
        assert!(
            shown.contains("alert_sweep_wake"),
            "the refusal must surface: {shown:?}"
        );
    }

    /// …and with a session it reaches the CURRENT identity's loop — never a
    /// departed identity's, whose lingering loop would read the teardown and
    /// exit, leaving the live loop unwoken and the journey waiting.
    #[tokio::test]
    async fn an_alert_sweep_wake_reaches_only_the_current_identitys_loop() {
        let mut app = authed_app();
        let departed = app.alert_sweep_wake.clone();
        // What `session::establish` does for the next identity.
        app.alert_sweep_wake = crate::critical_alerts::SweepWake::default();
        let current = app.alert_sweep_wake.clone();

        apply_command(
            &mut app,
            &test_tx(),
            fauna_e2e_agent::ALERT_SWEEP_WAKE,
            &Value::Null,
            "",
            "",
            &Value::Null,
            &Value::Null,
        )
        .await;

        assert_eq!(
            app.error_line_text(),
            None,
            "an honoured wake surfaces nothing"
        );
        assert!(
            current.take_pending_wake().await,
            "the current identity's loop must be woken"
        );
        assert!(
            !departed.take_pending_wake().await,
            "a departed identity's loop must never be the one woken"
        );
    }

    /// A pace that did not parse leaves the production pace in force, so it must
    /// fail loudly rather than ack; with no session there is no client to pace.
    #[tokio::test]
    async fn a_reconnect_backoff_that_cannot_land_says_so() {
        let mut app = authed_app();
        let result = apply_command(
            &mut app,
            &test_tx(),
            fauna_e2e_agent::RECONNECT_BACKOFF,
            &Value::Null,
            "",
            "",
            &Value::Null,
            &json!({ "initial_ms": 20 }),
        )
        .await;
        assert!(result.recognized);
        let shown = app.error_line_text().unwrap_or_default();
        assert!(
            shown.contains("reconnect_backoff"),
            "a malformed pace must surface: {shown:?}"
        );

        let mut unauthed = test_app();
        apply_command(
            &mut unauthed,
            &test_tx(),
            fauna_e2e_agent::RECONNECT_BACKOFF,
            &Value::Null,
            "",
            "",
            &Value::Null,
            &json!({ "initial_ms": 20, "max_ms": 100 }),
        )
        .await;
        let shown = unauthed.error_line_text().unwrap_or_default();
        assert!(
            shown.contains("reconnect_backoff"),
            "no session must surface: {shown:?}"
        );
    }

    /// The indicator's own state messages feed `connection_reports`, and a repeat
    /// of the word is a report without a transition — the stickiness proof's
    /// whole arithmetic, read off the published state.
    #[test]
    fn the_indicators_reports_and_transitions_are_published() {
        let mut app = authed_app();
        for state in [
            ConnectionState::Connecting,
            ConnectionState::Unreachable,
            ConnectionState::Unreachable,
        ] {
            app.handle_message(crate::app::UiMessage::Data(
                crate::app::DataMessage::ConnectionState(state),
            ));
        }
        assert_eq!(
            state_json(&app, None)[fauna_e2e_agent::CONNECTION_REPORTS_KEY],
            json!({ "reports": 3, "transitions": 1, "word": "unreachable" })
        );
    }

    /// The nav projection mirrors the screen: `welcome` while a launch/wizard
    /// surface owns it (the cross-app reset/logout → welcome assertion),
    /// the page ring once authenticated.
    #[test]
    fn state_nav_reads_welcome_while_a_launch_surface_owns_the_screen() {
        let app = test_app();
        assert_eq!(state_json(&app, None)["nav"]["stack"][0]["view"], "welcome");
        let authed = authed_app();
        assert_eq!(state_json(&authed, None)["nav"]["stack"][0]["view"], "feed");
    }

    /// `logout` takes the same arm as `reset` (linux's `"reset" | "logout"`;
    /// tui's own SignOutConfirm reuses `App::reset` too): session down, nav
    /// projection back on `welcome`, command recognized — never a silent drop.
    #[tokio::test]
    async fn logout_command_signs_out() {
        let mut app = authed_app();
        assert!(
            command_async(&mut app, "logout", &Value::Null)
                .await
                .recognized
        );
        assert!(app.session.is_none());
        assert_eq!(state_json(&app, None)["nav"]["stack"][0]["view"], "welcome");
    }

    /// The `messages` patch: inject reads back through state AND the painted
    /// registry lines (state-vs-UI honesty), null clears one level, and a nav
    /// patch dismisses the rest (linux's transient-message semantics).
    #[tokio::test]
    async fn messages_patch_injects_reads_back_and_clears_on_nav() {
        let mut app = authed_app();
        assert!(
            command_async(
                &mut app,
                "patch",
                &json!({ "messages": {
                    "error": "boom", "warning": "careful", "info": "fyi",
                } })
            )
            .await
            .recognized
        );
        let state = state_json(&app, None);
        assert_eq!(state["messages"]["error"], "boom");
        assert_eq!(state["messages"]["warning"], "careful");
        assert_eq!(state["messages"]["info"], "fyi");
        // The registry paints what the state reports — same source function.
        let registry = frame_registry(&app);
        for (id, text) in [
            ("error-message", "boom"),
            ("warning-message", "careful"),
            ("info-message", "fyi"),
        ] {
            let reply = perform(&mut app, &registry, &req(ElementKind::Text, id)).await;
            assert_eq!(reply, json!({ "text": text }), "{id} paints the injection");
        }
        // Null clears a single level.
        assert!(
            command_async(
                &mut app,
                "patch",
                &json!({ "messages": { "warning": Value::Null } })
            )
            .await
            .recognized
        );
        assert!(app.injected_warning.is_none());
        // A nav patch dismisses the remaining injected messages.
        assert!(
            command_async(
                &mut app,
                "patch",
                &json!({ "nav": { "stack": [ { "view": "settings" } ] } })
            )
            .await
            .recognized
        );
        assert!(app.injected_error.is_none());
        assert!(app.injected_info.is_none());
        assert_eq!(state_json(&app, None)["messages"]["error"], Value::Null);
    }

    /// Scroll-into-view resolves against the never-clipped registry: a present
    /// element is `found` (there is nothing to move), an absent one 404s.
    #[test]
    fn scroll_into_view_resolves_registry_targets() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::ScrollIntoView, "feed-tab"),
        ));
        assert_eq!(reply, json!({ "found": true }));
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::ScrollIntoView, "no-such-id"),
        ));
        assert_eq!(reply, json!({ "error": "not found" }));
    }

    #[test]
    fn click_tab_navigates_like_the_key_handler() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "events-tab"),
        ));
        assert_eq!(reply, json!({ "ok": true }));
        assert_eq!(app.page, Page::Events);
    }

    /// `press_key(id, "Enter")` activates the element the keyboard way — the
    /// same gesture a click runs, through `spawn_gesture` — so a tab's Enter
    /// lands on its page just as the key handler's does.
    #[test]
    fn enter_on_a_tab_activates_it_the_keyboard_way() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        let mut enter = req(ElementKind::Key, "events-tab");
        enter.arg = "Enter".to_string();
        let reply = run(perform(&mut app, &registry, &enter));
        assert_eq!(reply, json!({ "ok": true }));
        assert_eq!(app.page, Page::Events);
    }

    /// Any other named key has no consumer on tui and is refused loudly, never
    /// acked as if it had run (point 11); an Enter on a non-actuable id is
    /// refused the way `Click` refuses it.
    #[test]
    fn a_key_other_than_enter_is_refused() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        let mut left = req(ElementKind::Key, "events-tab");
        left.arg = "ArrowLeft".to_string();
        let reply = run(perform(&mut app, &registry, &left));
        assert!(
            reply["error"]
                .as_str()
                .is_some_and(|e| e.contains("ArrowLeft")),
            "got {reply}"
        );
        let mut missing = req(ElementKind::Key, "no-such-id");
        missing.arg = "Enter".to_string();
        assert_eq!(
            run(perform(&mut app, &registry, &missing)),
            json!({ "error": "not found" })
        );
    }

    /// `exit-tab` over `/element/click` — the e2e agent's own actuation path,
    /// which bypasses `App::actuate_focused` entirely (`Click` reads the
    /// registry's `action` and calls `run_gesture`/`gesture_work` directly).
    /// A special case living only in `actuate_focused` would leave this path
    /// treating Exit as an ordinary nav target instead of quitting — exactly
    /// the class of bug `gesture_work`'s "one door" doc comment exists to
    /// rule out. Guards that regression class directly.
    #[test]
    fn click_exit_tab_quits_via_the_agent_path() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        assert!(!app.should_quit);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "exit-tab"),
        ));
        assert_eq!(reply, json!({ "ok": true }));
        assert!(
            app.should_quit,
            "the agent click path must quit, not navigate"
        );
    }

    /// `in-viewport` reports what the frame measured and never guesses: a page
    /// element registered with a measurement answers it, one registered with
    /// none (a registry built outside a draw) answers `Null` — which the
    /// driver's `in_viewport` raises on rather than reading as "in view".
    #[test]
    fn in_viewport_is_the_frames_measurement_or_null() {
        let mut app = test_app();
        let mut registry = Registry::default();
        registry.page_element(crate::element::Element::label("shown", "a"), 0, Some(true));
        registry.page_element(
            crate::element::Element::label("hidden", "b"),
            1,
            Some(false),
        );
        registry.page_element(crate::element::Element::label("unmeasured", "c"), 2, None);
        let mut attr = |id: &str| {
            let mut r = req(ElementKind::Attr, id);
            r.arg = "in-viewport".to_string();
            run(perform(&mut app, &registry, &r))
        };
        assert_eq!(attr("shown"), json!({ "value": "true" }));
        assert_eq!(attr("hidden"), json!({ "value": "false" }));
        assert_eq!(attr("unmeasured"), json!({ "value": Value::Null }));
    }

    /// A scroll-into-view that cannot move anything says so instead of acking:
    /// a page element with nothing focusable at or after it, which the last
    /// frame did not already show, is an error — an acked no-op would let a
    /// dwell measure nothing (testing.md convention 11).
    #[test]
    fn a_scroll_that_cannot_move_the_viewport_is_refused() {
        let mut app = test_app();
        let mut registry = Registry::default();
        registry.page_element(
            crate::element::Element::label("stuck", "x"),
            9_999,
            Some(false),
        );
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::ScrollIntoView, "stuck"),
        ));
        assert!(reply.get("error").is_some(), "{reply}");
        registry.page_element(
            crate::element::Element::label("shown", "y"),
            9_999,
            Some(true),
        );
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::ScrollIntoView, "shown"),
        ));
        assert_eq!(reply, json!({ "found": true }), "already in view is found");
    }

    /// `/element/attr` (`get_attr(id, key)`): `req.arg` names the attribute, and
    /// the arm reads the matching element's inline attr — or `Null` when the
    /// element or that key is absent, the same "absent reads Null" contract the
    /// GUI apps honor. Exercises the generic wiring; the live consumer is
    /// `recipient-resolve-status`'s `state`.
    #[test]
    fn attr_reads_an_inline_element_attribute_or_null() {
        let mut app = test_app();
        let mut registry = Registry::default();
        registry.element(
            crate::element::Element::label(ids::RECIPIENT_RESOLVE_STATUS, "Resolved")
                .attr("state", "resolved"),
        );
        let mut attr = |arg: &str, id: &str| {
            let mut r = req(ElementKind::Attr, id);
            r.arg = arg.to_string();
            run(perform(&mut app, &registry, &r))
        };
        // Present key → its value.
        assert_eq!(
            attr("state", "recipient-resolve-status"),
            json!({ "value": "resolved" })
        );
        // Absent key on a present element → Null.
        assert_eq!(
            attr("nonexistent", "recipient-resolve-status"),
            json!({ "value": Value::Null })
        );
        // Absent element → Null.
        assert_eq!(attr("state", "no-such-id"), json!({ "value": Value::Null }));
    }

    /// `checked` is DERIVED for every checkbox, like `disabled` is for every
    /// element: the box's state lived only in the paint (`[x]` / `[ ]`), so a
    /// test reading `get_attr(id, "checked")` got `Null` for both states and
    /// could not tell a toggle that stuck from one that never moved. An inline
    /// `checked` still wins, and a non-checkbox still answers `Null`.
    #[test]
    fn attr_checked_is_derived_for_every_checkbox() {
        let mut app = test_app();
        let mut registry = Registry::default();
        let gesture =
            || crate::element::Gesture::Feed(crate::feed::Action::ToggleSellSubscribersFree);
        registry.element(crate::element::Element::checkbox_gesture(
            "on",
            "On",
            true,
            gesture(),
        ));
        registry.element(crate::element::Element::checkbox_gesture(
            "off",
            "Off",
            false,
            gesture(),
        ));
        registry.element(
            crate::element::Element::checkbox_gesture("inline", "Inline", false, gesture())
                .attr("checked", "true"),
        );
        registry.element(crate::element::Element::label("plain", "Plain"));
        let mut attr = |id: &str| {
            let mut r = req(ElementKind::Attr, id);
            r.arg = "checked".to_string();
            run(perform(&mut app, &registry, &r))
        };
        assert_eq!(attr("on"), json!({ "value": "true" }));
        assert_eq!(attr("off"), json!({ "value": "false" }));
        assert_eq!(
            attr("inline"),
            json!({ "value": "true" }),
            "an inline attr wins"
        );
        assert_eq!(attr("plain"), json!({ "value": Value::Null }));
    }

    #[test]
    fn connection_status_reads_disconnected() {
        let mut app = test_app();
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "connection-status"),
        ));
        assert_eq!(reply, json!({ "text": "Disconnected" }));
        app.connection = ConnectionState::Connected;
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "connection-status"),
        ));
        assert_eq!(reply, json!({ "text": "Connected" }));
    }

    #[test]
    fn error_message_registers_only_when_set() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Visible, "error-message"),
        ));
        assert_eq!(reply, json!({ "visible": false }));

        app.errors.insert(app.page, "boom".to_string());
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Visible, "error-message"),
        ));
        assert_eq!(reply, json!({ "visible": true }));
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "error-message"),
        ));
        assert_eq!(reply, json!({ "text": "boom" }));
    }

    /// `GET /registry` answers the whole frame in the cross-app record shape:
    /// every record re-drives through the unscoped query by its `(id, index)`,
    /// a tab is actuable, and a critical-alert row is text with no control —
    /// the whole-frame read the no-dismiss journey stands on.
    #[test]
    fn the_registry_route_answers_the_whole_frame() {
        let mut app = authed_app();
        app.alerts.post(
            "atproto-custody:did:plc:abc",
            vec![fauna_core::localized::LocalizedText::key_arg(
                "critical_alerts.atproto_custody_mismatch",
                "handle",
                "alice@fauna.test",
            )],
        );
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Registry, ""),
        ));
        let rows = reply["elements"]
            .as_array()
            .expect("a frame, never a refusal");
        assert!(
            !rows.is_empty(),
            "an authenticated shell registers elements"
        );

        let tab = rows
            .iter()
            .find(|r| r["id"] == "feed-tab")
            .expect("the sidebar's tabs are in the frame");
        assert_eq!(tab["actuable"], true);
        assert_eq!(tab["declares_enabled"], true);
        let alert = rows
            .iter()
            .find(|r| r["id"] == "critical-alert")
            .expect("the posted alert's row is in the frame");
        assert_eq!(alert["actuable"], false, "an alert row carries no gesture");
        assert_eq!(alert["editable"], false);

        for row in rows {
            let id = row["id"].as_str().expect("id");
            let index = row["index"].as_u64().expect("index") as usize;
            assert!(
                registry.matches(id, &[]).nth(index).is_some(),
                "record {row} must re-drive through the unscoped query"
            );
        }
    }

    /// The every-page critical-alerts banner (`critical-alerts.md` § Mechanism →
    /// *Rendering contract*): absent with no alerts, present with one, one
    /// indexed `critical-alert[N]` row per active alert.
    ///
    /// Pinned here — the layer that can actually break — rather than left to the
    /// e2e alone: this is a *wiring constant*, the cheapest possible tier for it.
    /// The e2e's job is the seam this tier structurally cannot see (a real
    /// machine posting to the registry at all), so the two do not overlap.
    #[test]
    fn critical_alerts_banner_registers_one_indexed_row_per_active_alert() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        // Absent, not an empty banner: `is_visible` must discriminate, or every
        // "the alarm cleared" assertion downstream passes vacuously.
        assert_eq!(
            run(perform(
                &mut app,
                &registry,
                &req(ElementKind::Visible, "critical-alerts")
            )),
            json!({ "visible": false })
        );

        app.alerts.post(
            "atproto-custody:did:plc:abc",
            vec![fauna_core::localized::LocalizedText::key_arg(
                "critical_alerts.atproto_custody_mismatch",
                "handle",
                "alice@fauna.test",
            )],
        );
        let registry = frame_registry(&app);
        assert_eq!(
            run(perform(
                &mut app,
                &registry,
                &req(ElementKind::Visible, "critical-alerts")
            )),
            json!({ "visible": true })
        );
        let row = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "critical-alert"),
        ));
        let text = row["text"].as_str().expect("the row carries text");
        assert!(
            text.contains("alice@fauna.test") && !text.contains("critical_alerts."),
            "the row is the resolved sentence naming the handle: {text:?}"
        );

        // A second feeder's alert becomes `critical-alert[1]` — the indexed-child
        // contract, which a single-row implementation would satisfy by accident.
        app.alerts.post(
            "other-feeder:1",
            vec![fauna_core::localized::LocalizedText::key("boom")],
        );
        let registry = frame_registry(&app);
        let mut second = req(ElementKind::Text, "critical-alert");
        second.index = 1;
        assert_eq!(
            run(perform(&mut app, &registry, &second)),
            json!({ "text": "boom" }),
            "one indexed row per active alert, in the registry's deterministic order"
        );

        // Session teardown clears the REGISTRY, asserted on the registry itself.
        //
        // ⚠ Asserting `is_visible("critical-alerts") == false` here instead would
        // be VACUOUS, and a mutation proved it: `critical_alert_lines` gates on
        // `App::authenticated`, so a signed-out app paints no banner whether or
        // not the alerts were cleared — dropping `clear_all()` left that version
        // of this test green. What the clear actually protects is
        // `App::switch_account`, which calls this teardown and then RE-AUTHENTICATES
        // as the next identity: a surviving `atproto-custody:<old-did>` alert would
        // paint, un-dismissably and on every page, against an account the user no
        // longer has, with nothing left running to ever re-check and clear it.
        // Run inside a runtime — the teardown's `sign_out` spawns the disconnect.
        run(async {
            app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch)
        });
        assert!(
            app.alerts.active().is_empty(),
            "session teardown drops every alert, so the next identity starts clean"
        );
    }

    /// The banner is gated on an authenticated shell: pre-identity there is no
    /// user to warn, and painting it would cover the sign-in flow.
    #[test]
    fn critical_alerts_banner_is_absent_pre_authentication() {
        let mut app = test_app();
        app.alerts.post(
            "atproto-custody:did:plc:abc",
            vec![fauna_core::localized::LocalizedText::key("boom")],
        );
        let registry = frame_registry(&app);
        assert_eq!(
            run(perform(
                &mut app,
                &registry,
                &req(ElementKind::Visible, "critical-alerts")
            )),
            json!({ "visible": false })
        );
    }

    /// The wizard's `error-message` comes from the client-side slot as well as
    /// the machine's own — an empty one still doesn't register.
    #[test]
    fn wizard_error_message_registers_only_when_set() {
        let mut app = test_app();
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Visible, "error-message"),
        ));
        assert_eq!(reply, json!({ "visible": false }));

        app.wizard.error = Some("invalid secret".to_string());
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "error-message"),
        ));
        assert_eq!(reply, json!({ "text": "invalid secret" }));
    }

    #[test]
    fn scoped_query_misses_top_level_elements() {
        let mut app = authed_app();
        let registry = frame_registry(&app);
        let mut r = req(ElementKind::Visible, "feed-tab");
        r.scope = vec![("post-card".to_string(), 0)];
        assert_eq!(
            run(perform(&mut app, &registry, &r)),
            json!({ "visible": false })
        );
    }

    #[test]
    fn reads_are_default_safe_for_absent_ids() {
        let mut app = test_app();
        let registry = frame_registry(&app);
        // `Text` is NOT default-safe — see `a_text_read_on_a_not_found_widget_
        // reports_an_error_not_empty_text` below: a missing widget must
        // be distinguishable from a found-but-empty one, so it errors instead.
        assert_eq!(
            run(perform(
                &mut app,
                &registry,
                &req(ElementKind::Count, "no-such-id")
            )),
            json!({ "count": 0 })
        );
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "no-such-id"),
        ));
        assert_eq!(reply, json!({ "error": "not found" }));
    }

    /// `/element/text` on a widget that is not on screen must report an error,
    /// not the empty string — otherwise "not found" and "found but empty" are
    /// the same reply and every failure downstream reads as a content bug
    /// (convention 6). Matches the `Click`/`Type`/… routes' existing not-found
    /// shape, linux's identical fix, and the
    /// apple/android/windows precedent, which already raise on a missing text
    /// read.
    #[test]
    fn a_text_read_on_a_not_found_widget_reports_an_error_not_empty_text() {
        let mut app = test_app();
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "definitely-nonexistent-test-id-row-95"),
        ));
        assert_eq!(
            reply.get("error").and_then(|e| e.as_str()),
            Some("not found"),
            "a missing widget must not reply `{{\"text\": \"\"}}` — that is \
             indistinguishable from a found-but-empty widget: {reply:?}"
        );
        assert!(
            reply.get("text").is_none(),
            "a not-found reply must not also carry a text field: {reply:?}"
        );
    }

    /// The counterpart: a widget that IS found but genuinely carries no text —
    /// the `critical-alerts` landmark, which "carries no text of its own"
    /// (`ui.rs::register_frame`) whenever ≥1 alert is active — still replies the
    /// plain empty string, not an error. The flip narrows only the
    /// missing-widget case.
    #[test]
    fn a_text_read_on_a_found_but_empty_widget_still_replies_empty_text() {
        let mut app = authed_app();
        app.alerts.post(
            "atproto-custody:did:plc:abc",
            vec![fauna_core::localized::LocalizedText::key("boom")],
        );
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Text, "critical-alerts"),
        ));
        assert_eq!(
            reply,
            json!({ "text": "" }),
            "a found-but-empty widget must still 200 with an empty string, not \
             be swept into the not-found error: {reply:?}"
        );
    }

    /// A disabled control refuses actuation rather than silently succeeding —
    /// otherwise an e2e click on a greyed-out Continue would "pass".
    ///
    /// The refusal used to be a bare `{"error": "disabled"}`, which carried no
    /// `status` and so rode `fauna_e2e_agent::element`'s default **404**. That
    /// sent `drivers/http_bridge.py` into its `_post_with_scroll` retry loop and
    /// killed the test with *"not rendered yet"* — the opposite diagnosis, for
    /// an element the agent had just successfully resolved. It named neither the
    /// element nor the route either, so a sweep log could not be triaged. Now it
    /// answers the shared 409 shape every host of `fauna_e2e_agent` produces.
    #[test]
    fn clicking_a_disabled_button_is_refused() {
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "handle-entry-continue-button"),
        ));
        assert_disabled_refusal(&reply, "click", "handle-entry-continue-button");
    }

    /// An element that is not a button at all still says so — the STRUCTURAL
    /// refusal survives the gate, and is not swallowed by it.
    ///
    /// The two answer different questions and a caller needs to tell them
    /// apart: "not actuable" means the id names something inert (usually a
    /// wrong id — a test bug), while "element is disabled" means the id is
    /// right and the UI is refusing (which may be a real product bug). Folding
    /// them together is how apple's rollout would have lost its most valuable
    /// signal.
    #[test]
    fn an_inert_element_reports_not_actuable_rather_than_disabled() {
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "handle-message-area"),
        ));
        assert_eq!(reply, json!({ "error": "not actuable" }));
    }

    /// The agent half of convention 11's actuation clause: `type` must not
    /// drive a control the UI disabled.
    ///
    /// tui gated `click` and `double-click` and **not** `type`/`clear`/`select`,
    /// which is the "half-applied gate" the convention names explicitly —
    /// *"Gating click alone is not compliance: typing into a disabled field is
    /// the same illegal act."* Same subject and same disabling mechanism as
    /// `app::tests::typing_into_a_disabled_input_is_inert`, one layer up: the
    /// human path and the agent path must refuse the same things, or the
    /// harness stops standing in for a user.
    #[test]
    fn typing_into_a_disabled_input_is_refused() {
        let mut app = disconnected_mail_spam_app();
        let registry = frame_registry(&app);
        let mut r = req(ElementKind::Type, ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT);
        r.arg = "9".to_string();

        let reply = run(perform(&mut app, &registry, &r));
        assert_disabled_refusal(&reply, "type", ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT);
        assert_eq!(
            app.field(threshold_field()),
            "",
            "a refused `type` must not also have written the field"
        );
    }

    /// `clear` shares the arm with `type`, so it inherits the gate — pinned
    /// rather than assumed, since the two could be split again.
    #[test]
    fn clearing_a_disabled_input_is_refused() {
        let mut app = disconnected_mail_spam_app();
        let _ = app.set_field(threshold_field(), "7".to_string());
        let registry = frame_registry(&app);

        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Clear, ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT),
        ));
        assert_disabled_refusal(&reply, "clear", ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT);
        assert_eq!(
            app.field(threshold_field()),
            "7",
            "a refused `clear` must leave the draft alone"
        );
    }

    /// The `select` route, third of the three that were ungated.
    ///
    /// A DIFFERENT disabling mechanism on purpose: `folder-paywall-tier-select`
    /// is disabled by an explicit `.enabled(!own_tiers.is_empty())` capability
    /// check, not by the offline gate, so the two pins together prove the arm
    /// consults `entry.enabled` itself rather than some connection-state proxy.
    #[test]
    fn selecting_on_a_disabled_picker_is_refused() {
        let mut app = paywall_select_app();
        let registry = frame_registry(&app);
        let mut r = req(ElementKind::Select, ids::FOLDER_PAYWALL_TIER_SELECT);
        r.arg = "gold".to_string();

        let reply = run(perform(&mut app, &registry, &r));
        assert_disabled_refusal(&reply, "select", ids::FOLDER_PAYWALL_TIER_SELECT);
    }

    /// The gate fires BEFORE the option-membership check.
    ///
    /// Order matters for the diagnosis, not just for correctness: a disabled
    /// picker's option list is frequently empty or stale (here it is empty —
    /// there are no tiers to offer, which is *why* it is disabled), so
    /// membership-first would answer *"this frame painted []"* for a control
    /// whose real story is "you cannot touch it at all". Both are 409s, so only
    /// the message distinguishes them.
    #[test]
    fn a_disabled_picker_reports_disabled_rather_than_an_empty_option_list() {
        let mut app = paywall_select_app();
        let registry = frame_registry(&app);
        let mut r = req(ElementKind::Select, ids::FOLDER_PAYWALL_TIER_SELECT);
        r.arg = "a-tier-that-was-never-offered".to_string();

        let reply = run(perform(&mut app, &registry, &r));
        let message = reply["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("element is disabled"),
            "the disabled state is the more fundamental fact and must be \
             reported first; got {message:?}"
        );
    }

    /// The gate is PRECISE — the same routes still drive an ENABLED control.
    ///
    /// The regression that would matter most: this gate sits on the hot path of
    /// every type/clear/select the harness issues, so an over-broad predicate
    /// would not fail one test, it would fail hundreds in ways that read as
    /// product bugs.
    #[test]
    fn the_gate_leaves_enabled_controls_alone() {
        let mut app = disconnected_mail_spam_app();
        app.connection = fauna_ws_substrate::supervisor::ConnectionState::Connected;
        let registry = frame_registry(&app);
        let mut r = req(ElementKind::Type, ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT);
        r.arg = "9".to_string();

        assert_eq!(run(perform(&mut app, &registry, &r)), json!({ "ok": true }));
        assert_eq!(app.field(threshold_field()), "9");

        let registry = frame_registry(&app);
        assert_eq!(
            run(perform(
                &mut app,
                &registry,
                &req(ElementKind::Clear, ids::MAIL_SPAM_THRESHOLD_OVERRIDE_INPUT),
            )),
            json!({ "ok": true })
        );
        assert_eq!(app.field(threshold_field()), "");
    }

    #[test]
    fn typing_and_clearing_an_input_round_trips() {
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::ClaimCode);
        let registry = frame_registry(&app);

        let mut r = req(ElementKind::Type, "claim-code-input");
        r.arg = "abcd-efgh".to_string();
        assert_eq!(run(perform(&mut app, &registry, &r)), json!({ "ok": true }));
        assert_eq!(app.wizard.field(WizardField::ClaimCode), "abcd-efgh");

        // A second `type` appends, like real keystrokes.
        let registry = frame_registry(&app);
        let mut r = req(ElementKind::Type, "claim-code-input");
        r.arg = "-ijkl".to_string();
        run(perform(&mut app, &registry, &r));
        assert_eq!(app.wizard.field(WizardField::ClaimCode), "abcd-efgh-ijkl");

        let registry = frame_registry(&app);
        run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Clear, "claim-code-input"),
        ));
        assert_eq!(app.wizard.field(WizardField::ClaimCode), "");
    }

    #[test]
    fn typing_into_a_label_is_refused() {
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::HandleEntry);
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Type, "handle-message-area"),
        ));
        assert_eq!(reply, json!({ "error": "not editable" }));
    }

    /// `reset` signs out, which spawns the supervisor's `disconnect()` — so
    /// this one needs a live reactor.
    #[tokio::test]
    async fn nav_patch_and_reset_commands() {
        let mut app = authed_app();
        assert!(
            command_async(
                &mut app,
                "patch",
                &json!({ "nav": { "stack": [ { "view": "settings" } ] } })
            )
            .await
            .recognized
        );
        assert_eq!(app.page, Page::Settings);

        app.errors.insert(Page::Settings, "boom".to_string());
        assert!(
            command_async(&mut app, "reset", &Value::Null)
                .await
                .recognized
        );
        assert_eq!(app.page, Page::ALL[0]);
        assert!(app.errors.is_empty());

        // Unsupported shapes ack but report unrecognized (loud, not silent).
        // A session patch with no node_url/secret and authenticated=false is a
        // sign-out — recognized. A malformed authenticated one is not.
        assert!(
            !command_async(
                &mut app,
                "patch",
                &json!({ "session": { "authenticated": true } })
            )
            .await
            .recognized
        );
        assert!(
            !command_async(&mut app, "unknown_action", &Value::Null)
                .await
                .recognized
        );
    }

    /// The cross-app bridge: `call_machine_method` reaches the shared
    /// dispatcher, so `machine_test_setter.py`'s fixtures drive tui unchanged.
    #[test]
    fn call_machine_method_reaches_the_shared_dispatcher() {
        let mut app = test_app();
        let result = machine_command(&mut app, "set_step_for_test", "\"ClaimCode\"");
        assert!(result.recognized);
        assert!(result.machine_result.is_none(), "a setter returns no value");
        assert_eq!(app.wizard.machine.step(), OnboardingStep::ClaimCode);
    }

    /// Reader methods hand their JSON back through `state.machine_method_result`.
    #[test]
    fn call_machine_method_returns_reader_values() {
        let mut app = test_app();
        machine_command(
            &mut app,
            "set_provider_base_urls",
            &json!({ "dns": "http://127.0.0.1:9" }).to_string(),
        );
        let result = machine_command(&mut app, "provider_base_url", "\"dns\"");
        assert_eq!(
            result.machine_result.as_deref(),
            Some("\"http://127.0.0.1:9\"")
        );
    }

    /// The bug the async dispatcher closes: an **async** machine method driven
    /// over the bridge must have landed its effect before the ack. Through the
    /// old sync dispatcher `verify_dns` hit the silent-ignore arm and acked
    /// green having done nothing.
    ///
    /// `verify_dns` against a dead port fails — and that failure *is* the
    /// evidence it ran: the machine surfaces an error and leaves the config
    /// unverified. A no-op leaves `error_message()` untouched.
    #[test]
    fn call_machine_method_runs_async_methods_to_completion() {
        let mut app = test_app();
        // Point the DNS provider's HTTP surface at a closed port, so the probe
        // fails fast and can never reach the live internet.
        machine_command(
            &mut app,
            "set_provider_base_urls",
            &json!({ "dns": "http://127.0.0.1:1" }).to_string(),
        );
        // Cloudflare takes exactly one required credential, so this is the
        // cheapest way to get `verify_dns` past its precondition guards and
        // all the way to the (doomed) HTTP round-trip.
        machine_command(&mut app, "select_dns_provider", "\"cloudflare\"");
        machine_command(&mut app, "set_dns_cred", r#"["api-token","MOCK"]"#);
        app.wizard.machine.clear_error();

        machine_command(&mut app, "verify_dns", "");

        // The failed probe is the *evidence the method ran*: `verify_dns` sets
        // "Verify failed: …" on the unauthorized/unreachable path. Through the
        // sync dispatcher the name hits the silent-ignore arm, no probe happens,
        // and the error stays `None` — i.e. this assertion is what bites.
        let error = app.wizard.machine.error_message();
        assert!(
            error
                .as_deref()
                .is_some_and(|e| e.contains("Verify failed")),
            "verify_dns must run to completion before the ack; got {error:?}"
        );
    }

    /// A reader's value reaches `/app/state` **parsed**, not double-encoded.
    ///
    /// The shared dispatcher hands back a JSON-*serialized* string, and
    /// `HttpBridgeDriver.call_machine_method` returns `machine_method_result`
    /// verbatim (it does no decoding) — so publishing the `String` as-is made the
    /// driver read `"\"http://…\""` instead of `http://…`, and would have handed
    /// a JSON *string* where `provisioning_snapshot` must be an *object*. Caught
    /// by the canonical cross-app `test_provisioning_progress.py`; linux
    /// parses in the same place.
    #[test]
    fn reader_values_reach_app_state_parsed_not_double_encoded() {
        let app = test_app();

        // A scalar reader: the driver must see the bare string.
        let state = state_json(&app, Some("\"http://127.0.0.1:9\"".to_string()));
        assert_eq!(
            state["machine_method_result"],
            json!("http://127.0.0.1:9"),
            "a double-encoded reader value reads as a quoted string on the driver"
        );

        // A structured reader: the driver must see an object it can index.
        let state = state_json(&app, Some(r#"{"overall":"Idle"}"#.to_string()));
        assert_eq!(state["machine_method_result"]["overall"], json!("Idle"));

        // A setter reports no value.
        let state = state_json(&app, None);
        assert_eq!(state["machine_method_result"], Value::Null);
    }

    /// A step-returning async method hands its routed-to step back as the
    /// reader value, so a driver can assert where the machine landed.
    #[test]
    fn async_step_returning_methods_return_the_step() {
        let mut app = test_app();
        // `recheck_manual_dns` is a no-op unless the outcome is AwaitingManualDns
        // (its own contract), so it returns the current step without any I/O.
        let result = machine_command(&mut app, "recheck_manual_dns", "");
        assert_eq!(
            result.machine_result.as_deref(),
            Some("\"IdentityChoice\""),
            "the step JSON must ride back through machine_method_result"
        );
    }

    #[test]
    fn tabs_all_register_with_labels() {
        let app = authed_app();
        let registry = frame_registry(&app);
        for page in Page::ALL {
            let mut a = authed_app();
            let reply = run(perform(
                &mut a,
                &registry,
                &req(ElementKind::Text, page.tab_id()),
            ));
            assert_eq!(reply, json!({ "text": page.label() }));
        }
    }

    /// Every wizard page paints only IDs ui.yaml scopes to it — the "no
    /// invisible shim elements" rule, pinned so a new page can't invent one.
    #[test]
    fn wizard_pages_register_only_ui_yaml_ids() {
        let cases: [(OnboardingStep, &[&str]); 10] = [
            // The phrase-only identity restore. `recovery-entry-account-field`
            // is a ui.yaml `optional_element` and is always painted here: a
            // terminal cannot scan the QR whose payload would otherwise carry
            // the handle, so the field is the only way a handle-less kit names
            // the account whose nest the ceremony must find.
            (
                OnboardingStep::RecoveryEntry,
                &[
                    "recovery-entry-phrase-field",
                    "recovery-entry-account-field",
                    "recovery-entry-submit-button",
                    "recovery-entry-back-button",
                ],
            ),
            (
                OnboardingStep::IdentityChoice,
                &[
                    "create-identity-button",
                    "import-identity-button",
                    // ui.yaml `optional_element`: the fresh-client entry to box
                    // recovery (`box-recovery.md` § Recovery UI (step 4)).
                    "recover-lost-box-button",
                    // The other recovery entry, adjacent and different: the
                    // phrase-only IDENTITY restore (`onboarding.md` § 1
                    // Identity). Both must paint — the standing warning about
                    // their labels only means something if both are here.
                    "restore-from-recovery-kit-button",
                ],
            ),
            (
                OnboardingStep::IdentityCreated,
                &[
                    "secret-key-display",
                    "secret-key-copy-btn",
                    "identity-continue-button",
                    "identity-created-back-button",
                ],
            ),
            (
                OnboardingStep::IdentityImport,
                &[
                    "paste-secret-field",
                    "import-submit-button",
                    "identity-import-back-button",
                ],
            ),
            (
                OnboardingStep::HandleEntry,
                &[
                    "handle-input",
                    "handle-check-button",
                    "handle-message-area",
                    "handle-entry-continue-button",
                    "handle-entry-back-button",
                ],
            ),
            (
                OnboardingStep::InviteRequest,
                &[
                    "invite-request-submit-button",
                    "invite-request-status",
                    "invite-code-input",
                    "invite-code-check-button",
                    "invite-code-status",
                    "invite-request-continue-button",
                    "invite-request-back-button",
                ],
            ),
            (
                OnboardingStep::ClaimCode,
                &[
                    "claim-code-input",
                    "claim-code-submit-button",
                    "claim-code-status",
                    "claim-code-back-button",
                ],
            ),
            // The single, terminal admin-path step reached directly on claim
            // completion — no-modes, ratified 2026-07-12 (onboarding.md
            // § 3b-bis). No Back button — the admin is server-committed.
            (
                OnboardingStep::NatModeChoice,
                &[
                    "public-nat-mode-radio",
                    "private-nat-mode-radio",
                    "nat-mode-confirm-button",
                    "nat-mode-defer-button",
                    "nat-mode-status",
                ],
            ),
            // Box recovery, step 4. `NestRecovery` is pinned in its EMPTY state
            // (nothing custodied): the rows are data-driven, so the populated
            // shape — `recover-box-list` + the indexed `recover-box-item-{i}` —
            // has its own test below.
            (
                OnboardingStep::NestRecovery,
                &[
                    "recover-box-empty-message",
                    "recover-method-cloud-button",
                    "recover-method-selfhosted-button",
                    "recover-back-button",
                ],
            ),
            (
                OnboardingStep::RecoverSelfhostedInstructions,
                &[
                    "recover-selfhosted-command",
                    "recover-selfhosted-copy-button",
                    "recover-restore-cta",
                    "recover-selfhosted-continue-button",
                ],
            ),
        ];
        for (step, expected) in cases {
            let app = test_app();
            app.wizard.machine.set_step_for_test(step);
            let ids: Vec<String> = app.wizard.elements().iter().map(|e| e.id.clone()).collect();
            assert_eq!(
                ids, expected,
                "{step:?} paints exactly its ui.yaml elements"
            );
        }
    }

    /// The §§ 4–7 pages, pinned the same way — but by the *distinct* id set
    /// rather than an ordered list, because their repeats are data-driven: there
    /// is one `dns-provider-row` per DNS-capable provider in the generated
    /// providers table, one `provisioning-step-row` per provisioning step. An
    /// ordered list would pin the provider *count*, so adding a provider to
    /// `i18n/providers.yaml` would fail a test that has nothing to say about it.
    /// What must not drift is the id *vocabulary*: no page may invent an id
    /// ui.yaml doesn't scope to it.
    ///
    /// Un-id'd chrome (`Element::chrome`, empty id) is excluded for the same
    /// reason: it is the deliberate *absence* of an id, not an invented one.
    /// [`crate::ui::register_frame`] skips the empty id, so no driver can ever
    /// see it — and rule 5's explainer lines (`ui/README.md` § Copy
    /// comprehensibility) are exactly this shape, so counting chrome as drift
    /// would make "explain your disabled control" fail the id invariant.
    ///
    /// Each expectation below is the page's default (pre-verify) state — no
    /// provider selected, so no credentials form; nothing verified, so no
    /// locations or server types; provisioning idle, so no elapsed row and no
    /// cancel/retry. The conditional elements have their own tests.
    ///
    /// The per-provider buttons (`dns-provider-row[cloudflare]`, …) are excluded
    /// from the comparison and checked separately below: their *ids* embed
    /// generated provider data, so pinning them here would again pin the
    /// providers table.
    #[test]
    fn provisioning_wizard_pages_register_only_ui_yaml_ids() {
        use std::collections::BTreeSet;

        let cases: [(OnboardingStep, &[&str]); 4] = [
            (
                OnboardingStep::DnsConfig,
                &[
                    "dns-buy-domain-checkbox",
                    "dns-same-provider-checkbox",
                    "dns-provider-row",
                    "dns-status-text",
                    "dns-set-up-later-button",
                    "dns-config-back-button",
                    "dns-config-continue-button",
                ],
            ),
            (
                OnboardingStep::VpsConfig,
                &[
                    "vps-provider-row",
                    "vps-config-mail-mode-toggle",
                    "vps-config-update-channel-row",
                    "vps-config-back-button",
                    "vps-config-continue-button",
                ],
            ),
            (
                OnboardingStep::NestProvisioning,
                &[
                    "provisioning-progress",
                    "provisioning-price-bom",
                    "provisioning-step-row",
                    "provisioning-step-checkbox",
                    "provisioning-step-label",
                    "provisioning-start-button",
                    "provisioning-back-button",
                    "provisioning-continue-button",
                ],
            ),
            (
                OnboardingStep::DnsPostInstructions,
                &[
                    "dns-post-instructions-text",
                    "dns-post-instructions-copy-button",
                    "dns-post-instructions-continue-button",
                ],
            ),
        ];

        for (step, expected) in cases {
            let app = test_app();
            app.wizard.machine.set_step_for_test(step);
            let ids: BTreeSet<String> = app
                .wizard
                .elements()
                .iter()
                .map(|e| e.id.clone())
                // The `{dns,vps}-provider-row[<id>]` buttons and un-id'd
                // chrome — see doc comment.
                .filter(|id| !id.ends_with(']') && !id.is_empty())
                .collect();
            let want: BTreeSet<String> = expected.iter().map(|s| s.to_string()).collect();
            assert_eq!(ids, want, "{step:?} paints exactly its ui.yaml elements");
        }
    }

    /// The provider buttons are addressed by a literal, provider-keyed id —
    /// `dns-provider-row[cloudflare]` — not by a positional scope index. That is
    /// the cross-app shape the shared `test_dns_config.py` / `test_vps_config.py`
    /// click and read enabled-state from, so it is worth pinning: registering
    /// N bare `dns-provider-row`s instead would leave every one of those clicks
    /// resolving to nothing.
    #[test]
    fn provider_rows_are_keyed_by_provider_id() {
        for (step, want) in [
            (OnboardingStep::DnsConfig, "dns-provider-row[cloudflare]"),
            (OnboardingStep::VpsConfig, "vps-provider-row[hetzner]"),
        ] {
            let app = test_app();
            app.wizard.machine.set_step_for_test(step);
            let registry = frame_registry(&app);
            assert_eq!(
                registry.matches(want, &[]).count(),
                1,
                "{step:?} must expose the provider button as `{want}`"
            );
        }
    }

    /// The four provisioning step rows are `provisioning-step-row[0..3]`, and
    /// each row's children hang off *that* row's scope — so a scoped query
    /// resolves one row's substep/label without counting globally and slicing.
    /// This is the first tui page to nest elements, so it is worth pinning that
    /// the scope path actually lands in the registry.
    #[test]
    fn provisioning_step_rows_are_indexed_and_scope_their_children() {
        let app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::NestProvisioning);
        let registry = frame_registry(&app);

        assert_eq!(
            registry.matches("provisioning-step-row", &[]).count(),
            4,
            "Domain, Server, Dns, Online — always exactly four"
        );

        // Unscoped, the label id repeats once per row...
        assert_eq!(registry.matches("provisioning-step-label", &[]).count(), 4);
        // ...but scoped to one row it resolves to exactly that row's label.
        for (i, want) in ["Domain", "Server", "DNS", "Online"].iter().enumerate() {
            let scope = vec![("provisioning-step-row".to_string(), i)];
            let labels: Vec<&str> = registry
                .matches("provisioning-step-label", &scope)
                .map(|e| e.text.as_str())
                .collect();
            assert_eq!(
                labels,
                vec![*want],
                "provisioning-step-row[{i}] scopes exactly its own label"
            );
        }

        // A top-level element must never match a scoped query.
        let scope = vec![("provisioning-step-row".to_string(), 0)];
        assert_eq!(
            registry
                .matches("provisioning-start-button", &scope)
                .count(),
            0,
            "the start button is not inside a step row"
        );

        // `provisioning-progress` is a page element in its own right (ui.yaml
        // scopes it, and the shared test asserts it is visible before it reads a
        // single row). Since the 2026-08-14 descendant ruling it may also wrap
        // the rows without breaking them — a scope naming only the row still
        // resolves through an unnamed ancestor — but it must stay a sibling
        // anyway, because it is a *page* element and not a container.
        assert_eq!(registry.matches("provisioning-progress", &[]).count(), 1);
        assert_eq!(
            registry
                .matches("provisioning-step-label", &scope)
                .next()
                .map(|e| e.text.as_str()),
            Some("Domain"),
            "a scoped row read must still resolve with provisioning-progress present"
        );
    }

    // ── Scope resolution is DESCENDANT matching (e2e-conventions.md
    // § convention 1, ruled 2026-08-14) ────────────────────────────────────
    //
    // Until that ruling tui matched a scope as a prefix of the entry's ancestor
    // path *from the root*, so a query naming only an inner container matched
    // **nothing** — while web (`root.locator(id).nth(i)`), linux (`scope_root`'s
    // `collect_in`) and windows (`WalkScope`) all resolved it as a descendant
    // and answered. `is_visible` said `False` for an element painted on screen,
    // which reads exactly like "the app never rendered it".
    //
    // These build the registry by hand rather than through a page, because the
    // shape under test is *nesting depth* — the two-step chains a page happens
    // to paint today are incidental to the contract.

    /// Two post-cards; the second holds two link-preview-cards, the second of
    /// which holds a title. Mirrors `feed::post_elements`' real shape
    /// (`.within(ids::LINK_PREVIEW_CARD, n).within(ids::POST_CARD, i)`).
    fn nested_registry() -> Registry {
        use crate::element::Element;
        let mut r = Registry::default();
        r.element(Element::label(ids::POST_CARD, "first"));
        r.element(Element::label(ids::FEED_POST_TEXT, "first body").within(ids::POST_CARD, 0));
        r.element(Element::label(ids::POST_CARD, "second"));
        r.element(Element::label(ids::FEED_POST_TEXT, "second body").within(ids::POST_CARD, 1));
        for n in 0..2 {
            r.element(
                Element::label(ids::LINK_PREVIEW_CARD, format!("card {n}"))
                    .within(ids::POST_CARD, 1),
            );
            // TWO children per card, as the real preview paints (title,
            // description, domain): one card must count as ONE instance when a
            // step indexes into it, however many entries reveal it.
            r.element(
                Element::label(ids::LINK_PREVIEW_TITLE, format!("title {n}"))
                    .within(ids::LINK_PREVIEW_CARD, n)
                    .within(ids::POST_CARD, 1),
            );
            r.element(
                Element::label(ids::LINK_PREVIEW_DOMAIN, format!("domain {n}"))
                    .within(ids::LINK_PREVIEW_CARD, n)
                    .within(ids::POST_CARD, 1),
            );
        }
        r
    }

    fn texts(r: &Registry, id: &str, scope: &[ScopeStep]) -> Vec<String> {
        r.matches(id, scope).map(|e| e.text.clone()).collect()
    }

    fn step(id: &str, index: usize) -> ScopeStep {
        (id.to_string(), index)
    }

    /// The defect the ruling closes: a scope naming only the inner container
    /// resolves through the unnamed `post-card` above it, exactly as it does on
    /// every DOM/AT-SPI/UIA app.
    #[test]
    fn a_partial_scope_resolves_through_unnamed_ancestors() {
        let r = nested_registry();
        assert_eq!(
            texts(&r, "link-preview-title", &[step("link-preview-card", 0)]),
            vec!["title 0"],
            "naming only the inner container must resolve, not match nothing"
        );
    }

    /// A step's index counts the container instances **in document order below
    /// the previous step** — web's `root.locator(id).nth(i)`. It is not a
    /// literal comparison against the index the paint code baked in, which is
    /// what made a partial scope's index meaningless before.
    #[test]
    fn a_scope_step_indexes_in_document_order_below_the_previous_step() {
        let r = nested_registry();
        assert_eq!(
            texts(&r, "link-preview-title", &[step("link-preview-card", 1)]),
            vec!["title 1"],
            "the two entries revealing card 0 must count as ONE instance, so \
             index 1 is the second CARD and not that card's second child"
        );
        // The same two cards, reached through their post: same answers, because
        // document order below `post-card[1]` is the order they were painted.
        assert_eq!(
            texts(
                &r,
                "link-preview-title",
                &[step("post-card", 1), step("link-preview-card", 1)],
            ),
            vec!["title 1"]
        );
    }

    /// The full chain the suite writes today (`post-card[1]/link-preview-card[0]`)
    /// keeps resolving exactly as it did — the widening is strictly more
    /// permissive, so no currently-green scoped query changes answer.
    #[test]
    fn a_full_chain_scope_is_unchanged_by_the_widening() {
        let r = nested_registry();
        assert_eq!(
            texts(&r, "feed-post-text", &[step("post-card", 0)]),
            vec!["first body"]
        );
        assert_eq!(
            texts(&r, "feed-post-text", &[step("post-card", 1)]),
            vec!["second body"]
        );
        assert_eq!(
            texts(
                &r,
                "link-preview-title",
                &[step("post-card", 1), step("link-preview-card", 0)],
            ),
            vec!["title 0"]
        );
    }

    /// An element's ancestor path excludes itself, so a container is not inside
    /// its own scope — the DOM rule that a node is not its own descendant.
    #[test]
    fn a_container_is_not_inside_its_own_scope() {
        let r = nested_registry();
        assert!(texts(&r, "post-card", &[step("post-card", 0)]).is_empty());
        // ...and the card entries painted *inside* post-card[1] still are.
        assert_eq!(
            texts(&r, "link-preview-card", &[step("post-card", 1)]),
            vec!["card 0", "card 1"]
        );
    }

    /// A scope step this frame never painted resolves to nothing at all — the
    /// "absent scope → no results" contract linux's `scope_root` states. Widening
    /// the match must not turn an unpaintable scope into a global query.
    #[test]
    fn an_unpainted_scope_step_matches_nothing() {
        let r = nested_registry();
        assert!(texts(&r, "feed-post-text", &[step("no-such-container", 0)]).is_empty());
        assert!(texts(&r, "feed-post-text", &[step("post-card", 7)]).is_empty());
        assert!(
            texts(
                &r,
                "link-preview-title",
                &[step("post-card", 0), step("link-preview-card", 0)],
            )
            .is_empty(),
            "post-card[0] paints no preview card, so nothing is inside one"
        );
    }

    /// A top-level element matches no scoped query, before or after the ruling —
    /// the empty path can contain no container.
    #[test]
    fn a_top_level_element_matches_no_scoped_query() {
        let r = nested_registry();
        assert!(texts(&r, "post-card", &[step("link-preview-card", 0)]).is_empty());
    }

    /// A container that painted no scoped child still OCCUPIES its ordinal. The
    /// walk counts container *instances*, exactly as a real widget tree does, so
    /// a query naming the empty row resolves to **that** row and finds nothing —
    /// never, silently, to the next row's contents. `backups`' restore history is
    /// the production case: only a diverged snapshot paints a banner inside its
    /// row, and `restore-history-item[0]` must not answer with row 1's banner.
    #[test]
    fn a_childless_container_still_occupies_its_ordinal() {
        use crate::element::Element;
        let mut r = Registry::default();
        r.element(Element::label(ids::RESTORE_HISTORY_ITEM, "row 0")); // diverged: no
        r.element(Element::label(ids::RESTORE_HISTORY_ITEM, "row 1"));
        r.element(
            Element::label(ids::RESTORE_DIVERGENCE_BANNER, "1 write")
                .within(ids::RESTORE_HISTORY_ITEM, 1),
        );

        assert!(
            texts(
                &r,
                "restore-divergence-banner",
                &[step("restore-history-item", 0)]
            )
            .is_empty(),
            "row 0 painted no banner, so its scope must find nothing"
        );
        assert_eq!(
            texts(
                &r,
                "restore-divergence-banner",
                &[step("restore-history-item", 1)]
            ),
            vec!["1 write"]
        );
    }

    /// The second container idiom — the entry names **itself**
    /// (`Element::label(row, "").within(row, i)`, what the provisioning steps and
    /// the subscription rows paint) — indexes identically to the first. A
    /// self-scoping row is one instance at one level, not two.
    #[test]
    fn the_self_scoping_container_idiom_indexes_identically() {
        use crate::element::Element;
        let mut r = Registry::default();
        for i in 0..3 {
            r.element(
                Element::label(ids::PROVISIONING_STEP_ROW, "")
                    .within(ids::PROVISIONING_STEP_ROW, i),
            );
            r.element(
                Element::label(ids::PROVISIONING_STEP_LABEL, format!("step {i}"))
                    .within(ids::PROVISIONING_STEP_ROW, i),
            );
        }
        for i in 0..3 {
            assert_eq!(
                texts(
                    &r,
                    "provisioning-step-label",
                    &[step("provisioning-step-row", i)]
                ),
                vec![format!("step {i}")],
                "provisioning-step-row[{i}] scopes exactly its own label"
            );
        }
    }

    /// The profile Tiers-tab §2 request rows carry BOTH addressing modes at
    /// once, because `actions/subscriptions.py` uses both on the same rows:
    /// flat (`get_text("subscription-request-tier", index=i)`) for the ordinary
    /// leaves, and scoped (`scope="subscription-request-row[i]"`) for
    /// `request_paid`. A `.within()` child stays flat-addressable (an empty
    /// scope resolves to the whole frame) while gaining the scoped read — and a
    /// top-level element could never serve the scoped one, which is exactly why
    /// the CONDITIONAL paid badge has to hang off its row rather than sit flat.
    #[test]
    fn subscription_request_rows_are_flat_indexed_and_scope_their_children() {
        use fauna_protocol::subscriptions::PendingRequest;

        fn request(id: i64, tier: &str, paid: bool) -> PendingRequest {
            PendingRequest {
                request_id: id,
                subscriber_id: fauna_core::identity::ActorId::from_hex(&"ef".repeat(32)).unwrap(),
                tier_name: tier.to_string(),
                kind: "subscribe".to_string(),
                created_at: fauna_core::data::Timestamp(0),
                mlkem_encaps_key: None,
                payment_entitled: paid,
                extra: Default::default(),
            }
        }

        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Profile;
        app.profile.set_tab_for_test(crate::profile::Tab::Tiers);
        app.profile.author.requests = vec![
            request(1, "gold", false),
            request(2, "silver", true),
            request(3, "bronze", false),
        ];
        let registry = frame_registry(&app);

        assert_eq!(registry.matches("subscription-request-row", &[]).count(), 3);

        // Flat: one tier label per row, in row (= registration) order.
        let tiers: Vec<&str> = registry
            .matches("subscription-request-tier", &[])
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(tiers, vec!["gold", "silver", "bronze"]);

        // Scoped: each row resolves exactly its own tier.
        for (i, want) in ["gold", "silver", "bronze"].iter().enumerate() {
            let scope = vec![("subscription-request-row".to_string(), i)];
            let got: Vec<&str> = registry
                .matches("subscription-request-tier", &scope)
                .map(|e| e.text.as_str())
                .collect();
            assert_eq!(got, vec![*want], "row[{i}] scopes its own tier");
        }

        // The paid badge resolves through the entitled row's scope, and only
        // that row's — the read `request_paid` actually performs.
        for (i, want) in [false, true, false].iter().enumerate() {
            let scope = vec![("subscription-request-row".to_string(), i)];
            assert_eq!(
                registry
                    .matches("subscription-request-paid-badge", &scope)
                    .count(),
                usize::from(*want),
                "row[{i}] paid badge presence must track payment_entitled"
            );
        }
    }

    /// Top-region price summary ("Bill of Materials", `onboarding.md` §6):
    /// `provisioning-price-bom` always renders (pinned above); the two line
    /// items reflect `bill_of_materials()` — `provisioning-bom-vps-line` once
    /// a server type is selected, `provisioning-bom-domain-line` additionally
    /// when the wizard is buying a new domain. Unit-level twin of linux's
    /// `test_bom_vps_line_renders_selected_price_domain_line_hidden` /
    /// `test_bom_domain_line_renders_when_buying_a_new_domain`
    /// (`test_provisioning_progress.py`).
    #[test]
    fn provisioning_bom_lines_reflect_bill_of_materials() {
        let app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::NestProvisioning);

        // No VPS/DNS state seeded: neither line renders yet.
        let elements = app.wizard.elements();
        assert!(
            elements
                .iter()
                .all(|e| e.id != "provisioning-bom-vps-line"
                    && e.id != "provisioning-bom-domain-line"),
            "no BOM line renders before any VPS/DNS state is seeded"
        );

        // A selected VPS server type: the recurring line appears.
        app.wizard.machine.set_vps_state_for_test(|st| {
            st.selected_provider_id = Some("hetzner".into());
            st.server_types = vec![fauna_provisioning::vps::ServerTypeInfo {
                id: "cax11".into(),
                vcpu: 2,
                mem_gb: 4.0,
                disk_gb: 40,
                price_monthly_cents: 451,
                currency: "EUR".into(),
            }];
            st.selected_server_type_id = Some("cax11".into());
        });
        let elements = app.wizard.elements();
        let vps_line = elements
            .iter()
            .find(|e| e.id == "provisioning-bom-vps-line")
            .expect("vps line renders once a server type is selected");
        assert!(vps_line.text.contains("4.51"), "{}", vps_line.text);
        assert!(vps_line.text.contains("EUR"), "{}", vps_line.text);
        assert!(
            elements
                .iter()
                .all(|e| e.id != "provisioning-bom-domain-line"),
            "no domain line when the wizard isn't buying a new domain"
        );

        // A buyable domain quote on top: both lines render together.
        app.wizard.machine.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("test-registrar".into());
            d.verified = true;
            d.current_availability = Some(
                fauna_provisioning::registrar::RegistrarAvailability::Buyable {
                    price_cents: 1099,
                    currency: Some("EUR".into()),
                    renewal_cents: None,
                },
            );
            d.buy_domain = true;
        });
        let elements = app.wizard.elements();
        let domain_line = elements
            .iter()
            .find(|e| e.id == "provisioning-bom-domain-line")
            .expect("domain line renders once the wizard is buying a new domain");
        assert!(domain_line.text.contains("10.99"), "{}", domain_line.text);
        let vps_line = elements
            .iter()
            .find(|e| e.id == "provisioning-bom-vps-line")
            .expect("vps line still renders alongside the domain line");
        assert!(vps_line.text.contains("4.51"), "{}", vps_line.text);
    }

    /// `nest_recovery` in its POPULATED state (the pin above covers the empty
    /// one): one indexed `recover-box-item-{i}` per custodied box, and the two
    /// re-provision methods gated on a selection — the client mirroring the
    /// machine's `require_selected_recovery_box` guard rather than letting the
    /// admin click into an error.
    #[test]
    fn nest_recovery_rows_are_indexed_and_gate_the_method_buttons() {
        let box_a = "aa".repeat(32);
        let box_b = "bb".repeat(32);
        let app = test_app();
        app.wizard
            .machine
            .set_recovery_boxes(vec![box_a.clone(), box_b.clone()]);
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::NestRecovery);

        let registry = frame_registry(&app);
        assert_eq!(
            registry.matches("recover-box-empty-message", &[]).count(),
            0,
            "the empty message must not paint when boxes ARE custodied"
        );
        for i in 0..2 {
            assert_eq!(
                registry
                    .matches(&format!("recover-box-item-{i}"), &[])
                    .count(),
                1,
                "one indexed row per custodied box"
            );
        }
        // Gated until a box is picked.
        let enabled = |r: &Registry, id: &str| r.matches(id, &[]).next().map(|e| e.enabled);
        assert_eq!(
            enabled(&registry, "recover-method-cloud-button"),
            Some(false)
        );
        assert_eq!(
            enabled(&registry, "recover-method-selfhosted-button"),
            Some(false)
        );

        app.wizard.machine.select_recovery_box(box_a.clone());
        let registry = frame_registry(&app);
        assert_eq!(
            enabled(&registry, "recover-method-cloud-button"),
            Some(true)
        );
        assert_eq!(
            enabled(&registry, "recover-method-selfhosted-button"),
            Some(true)
        );
    }

    /// The self-hosted page must NEVER show a command it has not resolved.
    ///
    /// The command carries the box's deployment **seed**, and a rebuilt box adopts
    /// whatever seed it is given: a placeholder copied as if it were real, or one
    /// box's command shown while another is selected, rebuilds the box under the
    /// WRONG `nest_actor_id` — the exact trust break recovery exists to prevent
    /// (`box-recovery.md` § Goal assertion 1). So: placeholder until resolved, and
    /// the copy button stays disabled while it is a placeholder.
    #[test]
    fn selfhosted_command_is_a_disabled_placeholder_until_it_resolves() {
        let mut app = test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::RecoverSelfhostedInstructions);

        let registry = frame_registry(&app);
        let command = |r: &Registry| {
            r.matches("recover-selfhosted-command", &[])
                .next()
                .map(|e| e.text.clone())
                .unwrap_or_default()
        };
        assert!(
            !command(&registry).contains("FAUNA_DEPLOYMENT_SEED"),
            "an unresolved command must be the placeholder, never an installer line"
        );
        assert_eq!(
            registry
                .matches("recover-selfhosted-copy-button", &[])
                .next()
                .map(|e| e.enabled),
            Some(false),
            "copying the placeholder into the admin's clipboard is worse than a \
             disabled button"
        );

        // The resolved read lands on the Wizard (the machine surfaces no seed).
        app.wizard.selfhosted_command = Some(format!("FAUNA_DEPLOYMENT_SEED={}", "ab".repeat(32)));
        let registry = frame_registry(&app);
        assert_eq!(
            command(&registry),
            format!("FAUNA_DEPLOYMENT_SEED={}", "ab".repeat(32))
        );
        assert_eq!(
            registry
                .matches("recover-selfhosted-copy-button", &[])
                .next()
                .map(|e| e.enabled),
            Some(true)
        );
    }

    fn seed_almost_ready(app: &crate::app::App) {
        app.wizard.machine.seed_awaiting_manual_dns(
            "https://nest.example".into(),
            "alice".into(),
            vec![fauna_onboarding_machine::DnsRecordPlain {
                record_type: "A".into(),
                name: "@".into(),
                value: "203.0.113.7".into(),
                ttl: 300,
                priority: None,
            }],
            "claim-abc".into(),
        );
    }

    /// Seed the surface the way a RESUMED STANDARD-PATH run reaches it: a slot
    /// with a claim code and no DNS records at all, because that path's records
    /// were ours to write. `onboarding.md` § "Almost ready" surface, *Two modes*.
    fn seed_almost_ready_records_less(app: &crate::app::App) {
        app.wizard.machine.seed_awaiting_manual_dns(
            "https://nest.example".into(),
            "alice".into(),
            Vec::new(),
            "claim-abc".into(),
        );
    }

    /// The records-less mode's second rule (the first being the status copy):
    /// "Copy all" is INERT, because there is nothing to copy. A button that
    /// answers a click by silently copying the empty string reads as a broken
    /// page rather than an empty one — and it is disabled rather than removed,
    /// since ui.yaml scopes the ID to this page's required elements.
    #[test]
    fn copy_all_is_disabled_in_the_records_less_mode_and_live_with_records() {
        let app = test_app();
        seed_almost_ready_records_less(&app);
        let enabled = |app: &crate::app::App| {
            app.wizard
                .elements()
                .into_iter()
                .find(|e| e.id == "awaiting-dns-copy-button")
                .expect("the copy button is one of the page's required elements")
                .enabled
        };
        assert!(
            !enabled(&app),
            "a resumed standard-path run has no records, so Copy all must be inert"
        );

        let with_records = test_app();
        seed_almost_ready(&with_records);
        assert!(
            enabled(&with_records),
            "with records to add at a registrar, Copy all is exactly the affordance \
             the mode exists for"
        );
    }

    /// The "Almost ready" surface is **not** an `OnboardingStep`, so
    /// `set_step_for_test` cannot reach it — it is keyed on `wizard_outcome()`.
    /// The "Almost ready" surface is **not** an `OnboardingStep`, so
    /// `set_step_for_test` cannot reach it — it is keyed on `wizard_outcome()`.
    /// Seeding is exactly what `launch::route_wizard_entry` does on the
    /// awaiting-manual-dns hydration row, so this drives the production path.
    #[test]
    fn awaiting_manual_dns_surface_registers_only_its_ui_yaml_ids() {
        let app = test_app();
        seed_almost_ready(&app);
        let ids: Vec<String> = app.wizard.elements().iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            [
                "awaiting-dns-records",
                "awaiting-dns-status",
                "awaiting-dns-recheck-button",
                "awaiting-dns-copy-button",
                "awaiting-dns-fallthrough-button",
            ],
            "the 'Almost ready' surface paints exactly its ui.yaml elements"
        );
    }

    /// The exit is live at rest in BOTH modes of the surface — it is the way off
    /// a box that will never answer, and the records-less mode (the resumed
    /// standard path) is precisely where that box lives. Unlike "Copy all" it
    /// is not gated on the record list, and unlike the recheck button it is not
    /// gated on a probe in flight.
    #[test]
    fn the_exit_is_live_at_rest_in_both_modes_of_the_surface() {
        let enabled = |app: &crate::app::App| {
            app.wizard
                .elements()
                .into_iter()
                .find(|e| e.id == "awaiting-dns-fallthrough-button")
                .expect("the exit is one of the page's required elements")
                .enabled
        };
        let records_less = test_app();
        seed_almost_ready_records_less(&records_less);
        assert!(
            enabled(&records_less),
            "records-less mode: the exit must be live"
        );

        let with_records = test_app();
        seed_almost_ready(&with_records);
        assert!(
            enabled(&with_records),
            "records mode: the exit must be live"
        );
    }

    /// Clicking "Use a different nest" on the surface retires it and lands the
    /// wizard on `handle_entry` holding the identity — the `awaiting_manual_dns`
    /// → `handle_entry` transition ui.yaml declares. Driven through the real
    /// registry click path, so an element that paints but never dispatches (the
    /// failure mode the single-source rule exists to prevent) goes red here.
    #[test]
    fn the_exit_click_lands_the_wizard_on_handle_entry_holding_the_identity() {
        let mut app = test_app();
        app.wizard.machine.seed_identity("01".repeat(32));
        seed_almost_ready(&app);
        assert!(app.wizard.is_awaiting_manual_dns());
        let registry = frame_registry(&app);

        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "awaiting-dns-fallthrough-button"),
        ));

        assert_eq!(reply, json!({ "ok": true }));
        assert!(
            !app.wizard.is_awaiting_manual_dns(),
            "the exit must clear the outcome the surface renders off"
        );
        assert_eq!(app.wizard.machine.step(), OnboardingStep::HandleEntry);
        assert_eq!(
            app.wizard.machine.effective_secret(),
            Some("01".repeat(32)),
            "a different nest, not a different self"
        );
    }

    /// The surface must win over the `Done` placeholder: seeding lands the
    /// machine at `step == Done`, which is also where `done_description` paints
    /// "Onboarding finished." Registering zero elements there would leave
    /// `driver.is_visible("awaiting-dns-records")` false on the relaunch path.
    #[test]
    fn awaiting_manual_dns_surface_outranks_the_done_placeholder() {
        let app = test_app();
        seed_almost_ready(&app);
        assert_eq!(app.wizard.machine.step(), OnboardingStep::Done);
        assert!(app.wizard.is_awaiting_manual_dns());
        assert!(!app.wizard.elements().is_empty());
    }

    /// The records the label shows are the records the copy button copies — one
    /// formatter, so a user pasting at their registrar gets what they read.
    #[test]
    fn awaiting_dns_records_element_renders_the_snapshot_records() {
        let app = test_app();
        seed_almost_ready(&app);
        let text = app
            .wizard
            .elements()
            .into_iter()
            .find(|e| e.id == "awaiting-dns-records")
            .expect("records element")
            .text;
        assert!(text.contains("203.0.113.7"), "got: {text}");
        assert!(text.contains('A'), "record type is shown: {text}");
    }

    // ── launch_retry surface (`onboarding.md` § App-launch routing) ─────────
    //
    // These assert what the e2e driver actually reads: the per-frame registry.
    // A launch element that paints but never registers would leave
    // `driver.is_visible("launch-retry-button")` false — the failure mode the
    // element-list-is-the-single-source rule exists to prevent.

    fn transient_app() -> App {
        let mut app = test_app();
        app.launch = crate::launch::LaunchSurface::TransientRetry {
            error: "connection refused".to_string(),
            recover_boxes: Vec::new(),
        };
        app
    }

    /// The transient row: Retry + "Use a different nest" are both visible and
    /// enabled, and the error text is readable through `launch-transient-error`.
    #[test]
    fn transient_retry_registers_its_four_elements_and_hides_the_wizard() {
        let app = transient_app();
        let registry = frame_registry(&app);
        for id in [
            "launch-transient-error",
            "launch-retry-button",
            "launch-fallthrough-button",
            "launch-retire-button",
        ] {
            assert_eq!(
                run(perform(
                    &mut transient_app(),
                    &registry,
                    &req(ElementKind::Visible, id)
                )),
                json!({ "visible": true }),
                "{id} must register while the launch flow owns the screen"
            );
        }
        assert_eq!(
            run(perform(
                &mut transient_app(),
                &registry,
                &req(ElementKind::Text, "launch-transient-error")
            )),
            json!({ "text": "connection refused" })
        );
        // The wizard does not own the screen, so none of its elements exist.
        assert_eq!(
            run(perform(
                &mut transient_app(),
                &registry,
                &req(ElementKind::Visible, "create-identity-button")
            )),
            json!({ "visible": false }),
            "the launch surface replaces the wizard, never overlays it"
        );
    }

    /// `onboarding.md:544` — an outdated nest gets a NON-retry surface: the
    /// localized message in `error-message`, and no `launch-retry-button` for
    /// the user to spin a doomed loop with.
    #[test]
    fn needs_update_registers_error_message_and_no_retry_button() {
        let mut app = test_app();
        app.launch = crate::launch::LaunchSurface::NeedsUpdate {
            error: "This nest is running an outdated version".to_string(),
        };
        let registry = frame_registry(&app);
        assert_eq!(
            run(perform(
                &mut test_app(),
                &registry,
                &req(ElementKind::Visible, "launch-retry-button")
            )),
            json!({ "visible": false }),
            "retrying an outdated nest is futile — the CTA must not exist"
        );
        assert_eq!(
            run(perform(
                &mut test_app(),
                &registry,
                &req(ElementKind::Visible, "launch-fallthrough-button")
            )),
            json!({ "visible": true })
        );
        assert_eq!(
            run(perform(
                &mut test_app(),
                &registry,
                &req(ElementKind::Text, "error-message")
            )),
            json!({ "text": "This nest is running an outdated version" }),
            "the update message renders in the canonical error-message element"
        );
    }

    /// The spinner phase exposes no ID (ui.yaml scopes none), so a driver can
    /// never mistake "still launching" for "retry surface up".
    #[test]
    fn launching_registers_no_launch_element() {
        let mut app = test_app();
        app.launch = crate::launch::LaunchSurface::Launching;
        let registry = frame_registry(&app);
        for id in [
            "launch-transient-error",
            "launch-retry-button",
            "launch-fallthrough-button",
            "error-message",
        ] {
            assert_eq!(
                run(perform(
                    &mut test_app(),
                    &registry,
                    &req(ElementKind::Visible, id)
                )),
                json!({ "visible": false }),
                "{id} must not register while the challenge is in flight"
            );
        }
    }

    /// Clicking "Use a different nest" seeds the identity and hands the screen
    /// to the wizard at `handle_entry` — the `launch_retry` → `handle_entry`
    /// transition ui.yaml declares. Driven through the real registry click path.
    #[test]
    fn fallthrough_click_hands_the_screen_to_the_wizard() {
        let mut app = transient_app();
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "launch-fallthrough-button"),
        ));
        assert_eq!(reply, json!({ "ok": true }));
        assert_eq!(
            app.launch,
            crate::launch::LaunchSurface::Wizard,
            "fallthrough yields the screen to the wizard"
        );
        // No stored identity in a bare test app, so the machine stays put; the
        // surface hand-off is what this pins. `seed_identity`'s landing on
        // HandleEntry is covered by the shared machine's own tests.
        assert!(!app.authenticated());
    }

    /// A `Retry` click with no launch machine (a reset app) is inert rather
    /// than a panic — the reset path drops the machine deliberately.
    #[test]
    fn retry_without_a_launch_machine_is_inert() {
        let mut app = transient_app();
        assert!(app.launch_machine.is_none());
        let registry = frame_registry(&app);
        let reply = run(perform(
            &mut app,
            &registry,
            &req(ElementKind::Click, "launch-retry-button"),
        ));
        assert_eq!(reply, json!({ "ok": true }));
    }
}
