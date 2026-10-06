//! Talking to the *other* instance — the launch-collision chooser's two
//! forwarding exits (`account-scoping.md` § Concurrent instances).
//!
//! Both exits need the same thing: reach the process that already serves the
//! colliding account and make it do something, then leave. The channel is the
//! desktop's own — the `org.freedesktop.Application` interface — so nothing is
//! invented here and no second IPC surface exists to keep in sync. The two
//! exits deliberately address **different names**, because they are asking
//! different processes for different things:
//!
//! - **focus-existing** → `Activate` on the **per-account** name this module
//!   also serves ([`claim_account_endpoint`]). Whoever serves the account
//!   owns that name, plain or bound alike, so the raise reaches the right
//!   window uniformly — see the raise-channel section below for why the
//!   app-wide name could not do this job.
//! - **add-account** → `ActivateAction("add-account")` on the **app-wide**
//!   name, dispatched to the `add-account` [`gio::SimpleAction`] the running
//!   instance registers in `main.rs`. The wizard runs *there*, which is the
//!   point: the onboarding scratchpad belongs to the primary. Its one
//!   remaining case — nobody owns the app-wide name, i.e. the server is bound
//!   — resolves the other way: the colliding process is then the only plain
//!   instance, hence the primary, and runs the wizard itself
//!   ([`name_has_owner`]).
//!
//! **Reachability is not guaranteed, and the caller must handle that.** Both
//! functions therefore return `bool` rather than pretending, and the chooser
//! either falls through to the launch it was making (the account is genuinely
//! free again) or surfaces the failure on `error-message` — a button that
//! silently quits the app is worse than one that says why it can't.

use adw::prelude::*;
// `fauna_client_accounts::BOUND_ACCOUNT_ENV` is already `pub` — that crate
// exposes only the *read* side (`requested_bound_account`), this spawner
// needs to *set* the variable on the child it launches.
use fauna_client_accounts::BOUND_ACCOUNT_ENV;

/// Object path for the application id, per the freedesktop D-Bus Application
/// convention: the id with `.` → `/`, prefixed by `/`.
fn object_path(app_id: &str) -> String {
    format!("/{}", app_id.replace('.', "/"))
}

// ---------------------------------------------------------------------------
// The per-(OS login, account) raise channel
// (`account-scoping.md` § Concurrent instances → *The per-(OS login, account)
// raise channel*, ratified 2026-07-23).
//
// The app-wide channel above reaches only the instance that owns the app-wide
// well-known name — and a *bound* instance deliberately owns none, so
// "focus the instance serving account X" had nothing to call whenever X's
// server was a bound sibling. The ratified fix: **every serving instance,
// plain and bound alike, additionally owns a per-account name derived from
// the account it serves**, and focus-existing targets that name uniformly.
//
// Nothing is registered and no rendezvous state is written: the name is
// derivable by any would-be raiser, and liveness IS ownership — it dies with
// the process exactly as the instance lock's flock does. That is the whole
// reason a rendezvous file beside the lock was rejected: a file needs
// staleness reconciliation and a lock-then-read protocol; an owned name needs
// neither.
// ---------------------------------------------------------------------------

/// The D-Bus well-known name a serving instance claims for `actor_id_hex`.
///
/// `<APP_ID>.a<token>`, where the token is the shared
/// [`fauna_client_accounts::account_instance_token`] — the *same* derivation
/// the per-account lock file is named from, so a raiser's name and its
/// server's name can never key differently.
///
/// Two things about the shape are load-bearing rather than stylistic:
///
/// - **`APP_ID` itself never changes.** The app id anchors the desktop-entry /
///   icon association and the Flatpak `--own-name` grant, and the plain-launch
///   affordance layer must stay keyed on exactly one of it — which is why a
///   per-account *app id* was rejected outright in the goal doc. This is an
///   extra name our process owns, not a different identity.
/// - **The `a` prefix is required, not decorative.** A D-Bus name element may
///   not begin with a digit, and half of all actor ids do.
fn account_endpoint_name(actor_id_hex: &str) -> Option<String> {
    fauna_client_accounts::account_instance_token(actor_id_hex)
        .map(|token| format!("{}.a{token}", crate::APP_ID))
}

/// The endpoint's `Activate` landed — drained by `main.rs`'s raise poll loop,
/// which turns it into `app.activate()` (present the window, in whatever state
/// the app is in). Deliberately the same shape as `tray::TRAY_RAISE` rather
/// than a shared flag: two independent raise sources, each owning its own
/// signal, is what keeps either one removable.
pub(crate) static RAISE_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// How many endpoint raises this instance has *served* — incremented by the
/// `Activate` handler, read by the e2e state protocol.
///
/// Separate from [`RAISE_REQUESTED`] because they answer different questions:
/// the flag is drained (a raise pending → consumed), while a test needs a
/// monotone record that the raise arrived at all.
static RAISES_SERVED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Snapshot for the test agent's state protocol (`raises_served`).
pub(crate) fn raises_served() -> u32 {
    RAISES_SERVED.load(std::sync::atomic::Ordering::SeqCst)
}

/// The endpoint this process currently owns: `(name, owner id)`. `None` until
/// a session account resolves — a launch that never authenticates serves
/// nothing and claims nothing.
static OWNED_ENDPOINT: std::sync::Mutex<Option<(String, gio::OwnerId)>> =
    std::sync::Mutex::new(None);

/// Claim the per-account endpoint for the account this process now serves —
/// called from `account_scope::become_session_instance` on every path that
/// proceeds, so **endpoint ownership tracks lock ownership at exactly one
/// site**, the in-process cross-account switch included (it unowns the old
/// name before claiming the new one, mirroring the lock's swap-by-replacement).
///
/// Best-effort by design, and silent about failure: the endpoint is an
/// *affordance*, never a guard. Losing the race for the name means some other
/// process claims to serve this account, which the instance lock — the actual
/// arbiter, taken moments earlier — has already ruled on; a client that
/// refused to run because it could not own a convenience name would turn a
/// bus hiccup into a launch failure (the same degrade-open posture as the lock
/// itself).
pub(crate) fn claim_account_endpoint(actor_id_hex: &str) {
    let Some(name) = account_endpoint_name(actor_id_hex) else {
        return;
    };
    let mut owned = OWNED_ENDPOINT.lock().unwrap_or_else(|e| e.into_inner());
    // Same account (a session rebuild) — the name is already ours, and
    // re-owning would drop it for the length of the re-acquire.
    if owned.as_ref().is_some_and(|(held, _)| *held == name) {
        return;
    }
    if let Some((_, id)) = owned.take() {
        gio::bus_unown_name(id);
    }
    let path = object_path(&name);
    let id = gio::bus_own_name(
        gio::BusType::Session,
        &name,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| export_activate_endpoint(&conn, &path),
        |_, name| tracing::info!("[raise-channel] serving on {name}"),
        |_, name| tracing::warn!("[raise-channel] lost {name} — raises will not reach us"),
    );
    *owned = Some((name, id));
}

/// Export the one method the endpoint answers: `org.freedesktop.Application`'s
/// `Activate`, the same method (and the same interface) GApplication itself
/// exports on the app-wide name — the desktop's own activation channel, not a
/// second IPC surface of our invention.
///
/// **Only `Activate`.** `ActivateAction` deliberately stays app-wide: the
/// add-account forward hands the wizard to the *primary*, and the onboarding
/// scratchpad belongs to it, so a per-account endpoint answering it would
/// contradict that ownership rule.
fn export_activate_endpoint(conn: &gio::DBusConnection, path: &str) {
    const IFACE_XML: &str = "<node><interface name='org.freedesktop.Application'>\
        <method name='Activate'><arg type='a{sv}' name='platform_data' direction='in'/></method>\
        </interface></node>";
    let info = match gio::DBusNodeInfo::for_xml(IFACE_XML) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("[raise-channel] interface info: {e}");
            return;
        }
    };
    let Some(iface) = info.lookup_interface("org.freedesktop.Application") else {
        tracing::error!("[raise-channel] interface missing from its own XML");
        return;
    };
    let registered = conn
        .register_object(path, &iface)
        .method_call(|_, _, _, _, _, _, invocation| {
            RAISE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
            RAISES_SERVED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Reply before the raise actually happens: the caller is a
            // colliding launch about to exit, and the raise is a main-loop
            // hop away. Leaving it hanging would make it fall back to its
            // no-channel error for a raise that is in fact under way.
            invocation.return_value(None);
        })
        .build();
    if let Err(e) = registered {
        tracing::error!("[raise-channel] cannot export {path}: {e}");
    }
}

/// Ask the instance serving `actor_id_hex` to raise its window — the raiser
/// half of the channel, targeting the per-account endpoint *uniformly*
/// (a plain server and a bound server are reached identically; only the
/// app-wide channel ever distinguished them).
///
/// `false` means the endpoint is unowned — the sibling died between the
/// chooser's probe and the click, or its server is endpoint-less by design
/// (tui claims no endpoint: no window manager can raise a terminal app). The
/// caller must then re-probe the lock rather than guess; see the chooser's
/// focus-existing exit.
pub(crate) fn raise_account_instance(actor_id_hex: &str) -> bool {
    let Some(name) = account_endpoint_name(actor_id_hex) else {
        return false;
    };
    let empty: std::collections::HashMap<String, glib::Variant> = std::collections::HashMap::new();
    call(&name, "Activate", &(empty,).to_variant())
}

/// Does anyone own `name` on the session bus right now?
///
/// The chooser's add-account exit needs the *ownership* question, not the
/// call-succeeded question: an unowned app-wide name means the running server
/// is bound, hence this colliding process is the install's only plain instance
/// — and therefore the primary, which is what licenses it to run the wizard
/// itself (`account-scoping.md`: "a colliding *plain* launch that finds the
/// app-wide name unowned runs the wizard itself"). A failed forward would be
/// an ambiguous stand-in: it also fires when an owner exists but is wedged,
/// where running a second wizard is exactly wrong.
pub(crate) fn name_has_owner(name: &str) -> bool {
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    match conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(name,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        gio::Cancellable::NONE,
    ) {
        Ok(reply) => reply.child_value(0).get::<bool>().unwrap_or(false),
        Err(e) => {
            tracing::warn!("[instance-remote] NameHasOwner({name}) failed: {e}");
            false
        }
    }
}

/// Ask the running instance to open the add-account wizard. `true` if the
/// call was delivered.
pub(crate) fn forward_add_account(app_id: &str) -> bool {
    // `ActivateAction(IN s action_name, IN av parameter, IN a{sv} platform_data)`.
    // An empty `av` means the action takes no parameter, which matches the
    // `add-account` SimpleAction's `None` parameter type.
    let params: Vec<glib::Variant> = Vec::new();
    let empty: std::collections::HashMap<String, glib::Variant> = std::collections::HashMap::new();
    call(
        app_id,
        "ActivateAction",
        &("add-account", params, empty).to_variant(),
    )
}

/// One synchronous session-bus call, with every failure collapsed to `false`
/// and logged. Synchronous on purpose: both callers exit the process straight
/// afterwards, so there is nothing to keep responsive, and the alternative —
/// quitting from an async callback — races the reply.
fn call(app_id: &str, method: &str, args: &glib::Variant) -> bool {
    let conn = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("[instance-remote] no session bus: {e}");
            return false;
        }
    };
    match conn.call_sync(
        Some(app_id),
        &object_path(app_id),
        "org.freedesktop.Application",
        method,
        Some(args),
        None,
        gio::DBusCallFlags::NONE,
        // Bounded: an unreachable or wedged peer must fail the button, not
        // hang the chooser (the harness's "never unbounded" rule applies to
        // product code for the same reason).
        3000,
        gio::Cancellable::NONE,
    ) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("[instance-remote] {method} to {app_id} failed: {e}");
            false
        }
    }
}

/// Spawn a NEW instance bound to `actor_id_hex` — the **running** instance's
/// concurrent-instances affordance (ui.yaml `account-open-new-instance-button`
/// on the switcher's non-active rows; `account-scoping.md` § Concurrent
/// instances → "the running instance's surface").
///
/// The chosen account travels as `FAUNA_BOUND_ACCOUNT` in the child's
/// environment — the one launch-wiring channel all seven apps use, so
/// nothing per-platform is invented. apple's twin is `InstanceSpawner`.
///
/// The spawner deliberately **validates nothing**: `bind_account` in the child
/// is the single gate, so an unknown account, a re-auth-flagged account and an
/// already-served account all get their correct outcomes there rather than
/// three duplicated pre-checks here that could drift from it.
///
/// Returns the child's pid on success.
pub(crate) fn spawn_bound_instance(actor_id_hex: &str) -> Option<u32> {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("[instance-spawn] cannot resolve own executable: {e}");
            return None;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.env(BOUND_ACCOUNT_ENV, actor_id_hex);
    // Under e2e the child needs its OWN automation port — the agent server
    // binds exactly once per process, and the rest of the environment is
    // inherited wholesale on purpose (same credential dir, same XDG base: the
    // child must share this instance's install world, which is the whole
    // point of a second instance of the same install). Mirrors apple's
    // `InstanceSpawner`. Compiled out of a release build together with the
    // server it forwards to (convention 15): a shipped app spawns its second
    // instance with no automation port at all.
    let agent_port = e2e_child_agent_port();
    if let Some(port) = agent_port {
        cmd.env(E2E_AGENT_PORT_ENV, port.to_string());
    }
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            record_spawn(actor_id_hex, pid, agent_port);
            tracing::info!("[instance-spawn] {actor_id_hex} → pid {pid}");
            Some(pid)
        }
        Err(e) => {
            tracing::error!("[instance-spawn] launch failed for {actor_id_hex}: {e}");
            None
        }
    }
}

const E2E_AGENT_PORT_ENV: &str = "FAUNA_E2E_AGENT_PORT";

/// A fresh automation port for a spawned second instance, when this build has an
/// automation server at all. Gated pair (convention 15) — see the twin below.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn e2e_child_agent_port() -> Option<u16> {
    std::env::var(E2E_AGENT_PORT_ENV)
        .ok()
        .and_then(|_| free_port())
}

/// Production twin: no automation server to give the child a port for.
#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
fn e2e_child_agent_port() -> Option<u16> {
    None
}

/// Spawn records for the e2e state protocol (`spawned_instances`): the harness
/// reads the child's `agent_port` here and drives the child over its own
/// automation server. Cross-app shape — apple's `InstanceSpawner.records`.
static SPAWNED: std::sync::Mutex<Vec<(String, u32, Option<u16>)>> =
    std::sync::Mutex::new(Vec::new());

fn record_spawn(actor_id_hex: &str, pid: u32, agent_port: Option<u16>) {
    SPAWNED.lock().unwrap_or_else(|e| e.into_inner()).push((
        actor_id_hex.to_string(),
        pid,
        agent_port,
    ));
}

/// Snapshot for the test agent's state protocol.
pub(crate) fn spawned_instances() -> Vec<serde_json::Value> {
    SPAWNED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(actor, pid, port)| {
            let mut o = serde_json::json!({ "actor_id": actor, "pid": pid });
            if let Some(p) = port {
                o["agent_port"] = serde_json::json!(p);
            }
            o
        })
        .collect()
}

/// Bind-to-port-0 free-port allocation — the same trick the harness's
/// `find_free_port` uses. Racy in principle; the window is milliseconds and
/// this runs e2e-only — hence the gate: its one caller
/// ([`e2e_child_agent_port`]) is compiled out of a release build, and an
/// ungated private fn with no callers is a `dead_code` warning there.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The freedesktop path derivation the running instance's GApplication
    /// exports on — a wrong path is an unreachable peer, and the failure
    /// looks identical to "no instance running", so pin it.
    #[test]
    fn object_path_follows_the_freedesktop_convention() {
        assert_eq!(object_path("social.fauna.fauna"), "/social/fauna/fauna");
    }

    /// The per-account endpoint name: app id + `.a` + the shared token.
    ///
    /// Every property here is one a raiser and its server must agree on, and
    /// every disagreement degrades into the *same* symptom — an unowned name,
    /// which reads as "that instance died" and sends the user to an error for
    /// a window that is very much alive.
    #[test]
    fn the_account_endpoint_name_is_the_app_id_plus_the_shared_token() {
        let actor = "b".repeat(64);
        let name = account_endpoint_name(&actor).expect("a well-formed actor");
        assert_eq!(name, format!("social.fauna.fauna.a{actor}"));
        assert!(
            name.starts_with(&format!("{}.", crate::APP_ID)),
            "the endpoint is a SUBNAME of the app id, never a replacement for it: {name}"
        );
        // A D-Bus name element may not begin with a digit, and half of all
        // actor ids do — the `a` prefix is what makes those nameable at all.
        let numeric = "7".repeat(64);
        let numeric_name = account_endpoint_name(&numeric).expect("a well-formed actor");
        assert_eq!(numeric_name, format!("social.fauna.fauna.a{numeric}"));
        assert!(
            numeric_name
                .rsplit('.')
                .next()
                .is_some_and(|last| last.starts_with(|c: char| c.is_ascii_alphabetic())),
            "no name element may start with a digit: {numeric_name}"
        );
    }

    /// Case and whitespace variants of one account name the SAME endpoint —
    /// inherited from the shared token, which is the whole reason the
    /// derivation is shared rather than inlined here.
    #[test]
    fn endpoint_names_normalize_like_the_lock_file() {
        let actor = "c".repeat(64);
        assert_eq!(
            account_endpoint_name(&format!("  {}  ", actor.to_ascii_uppercase())),
            account_endpoint_name(&actor)
        );
        for not_ours in ["", "not-hex", &"c".repeat(63)] {
            assert!(
                account_endpoint_name(not_ours).is_none(),
                "{not_ours:?} must name no endpoint"
            );
        }
    }

    /// The server exports its object at `object_path(name)` and the raiser
    /// calls `object_path(name)` — pinned as one assertion because they are
    /// two call sites of the same derivation in opposite directions, and a
    /// drift between them is invisible until a real second instance exists.
    #[test]
    fn the_endpoint_object_path_is_derived_from_the_endpoint_name() {
        let actor = "d".repeat(64);
        let name = account_endpoint_name(&actor).expect("a well-formed actor");
        assert_eq!(object_path(&name), format!("/social/fauna/fauna/a{actor}"));
    }

    /// The spawn record is what the e2e harness drives the child through —
    /// `agent_port` is how it reaches the child's own automation server, and
    /// its absence outside e2e is deliberate, not an oversight.
    #[test]
    fn spawn_records_carry_the_actor_and_pid() {
        record_spawn("a".repeat(64).as_str(), 4242, Some(7000));
        record_spawn("b".repeat(64).as_str(), 4243, None);
        let recs = spawned_instances();
        let a = recs.iter().find(|r| r["pid"] == 4242).expect("pid 4242");
        assert_eq!(a["actor_id"], "a".repeat(64));
        assert_eq!(a["agent_port"], 7000);
        let b = recs.iter().find(|r| r["pid"] == 4243).expect("pid 4243");
        assert!(
            b.get("agent_port").is_none(),
            "no agent port outside e2e; got {b}"
        );
    }
}
