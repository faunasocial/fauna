//! The agent's **SIGTERM shutdown proof** — the real binary, a real signal, a
//! real exit status (`docs/goal/architecture/apps/sync-agent.md` § Implementation
//! status today → the SIGTERM paragraph).
//!
//! ## Why this test exists
//!
//! `service.rs`'s shutdown select waited on `tokio::signal::ctrl_c()` alone, so
//! SIGTERM — what systemd's `stop`, launchd, and the e2e harness's
//! `terminate_tree` send *first* — never ran the orderly teardown (shutdown
//! broadcast → engine host drop → cfapi root disconnect). Observed live
//! 2026-07-24 as a macOS orphan that survived SIGTERM and needed SIGKILL while
//! holding a nest connection. The fix added `SignalKind::terminate()` to the
//! same select on unix — and until this file, that wiring was **compile-verified
//! only**, which the goal doc said in as many words.
//!
//! ## What makes it a proof rather than a formality
//!
//! SIGTERM's *default disposition already terminates the process*. So "the
//! child went away" proves nothing at all: a build with the handler removed
//! passes that assertion every time. The discriminator is the **exit status**:
//!
//! - handler ran  → the process returns from `main` → `code() == Some(0)`
//! - no handler   → the kernel kills it → `code() == None`, `signal() == Some(SIGTERM)`
//!
//! Reverting the `SignalKind::terminate()` arm therefore turns this red, which
//! is the property that makes it coverage rather than a formality. It is also
//! why no human step is owed here: watching a daemon exit needs no eye, and the
//! exit status is a stricter witness than one — a person watching the process
//! disappear would have "confirmed" the broken build every time.
//!
//! ## Isolation + timing
//!
//! Same private world as `agent_process_tier3` (temp `HOME` / `XDG_RUNTIME_DIR`,
//! `FAUNA_E2E_CREDENTIAL_DIR`, explicit `--data-dir`), so a run never touches
//! the developer's real agent (testing.md § point 10). No nest and no
//! `tier3-nest` feature: the agent serves its socket with no capability
//! provisioned, which is all this needs — so the shutdown proof runs on the
//! ordinary test loop rather than behind the heavy opt-in harness.
//!
//! Every wait is a **deadline poll on a generous budget**, never a sleep-as-
//! assertion (testing.md convention 14): a green run pays only the first poll,
//! and the ceiling is sized far above any non-pathological delay so a loaded
//! machine cannot make it flake.

#![cfg(unix)]

mod common;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Far above any plausible orderly teardown; a green run never approaches it.
const EXIT_BUDGET: Duration = Duration::from_secs(30);

/// A private agent world under `/tmp` — see the module doc on `sun_path`.
///
/// Holds the `TempDir` guard rather than `.keep()`ing its path, so the world is
/// removed when the test ends. (`agent_process_tier3` keeps its world because it
/// respawns into the same one — persistence across processes is that test's
/// subject. Nothing here outlives the single child, so keeping would just litter
/// `/tmp` a directory per run.) Declared *before* the child in the test body, so
/// drop order tears the agent down first and the removal never races a live
/// process.
struct AgentWorld {
    dir: tempfile::TempDir,
}

impl AgentWorld {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("fauna-agent-sigterm-")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn socket_path(&self) -> PathBuf {
        common::agent_socket_path(self.root())
    }

    fn spawn_agent(&self) -> KillOnDrop {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fauna-sync-agent"));
        cmd.arg("--data-dir")
            .arg(self.root().join("data"))
            .env("FAUNA_E2E_CREDENTIAL_DIR", self.root().join("creds"))
            .env("RUST_LOG", "info");
        // `HOME` on every unix, not just macOS, and `XDG_CONFIG_HOME` on
        // non-macOS too (round 32): `--data-dir` only relocates `SyncPaths`,
        // never `StoreRoot::platform()` (`account_host.rs`'s unconditional
        // `assemble()` root) — inert today, since this file's tests never
        // provision a capability and so never reach `assemble()`
        // (`account_host.rs`'s own comment on `run`'s shutdown-check confirms
        // it), but a future test here that DOES provision one would silently
        // read/write the developer's real account store without this.
        // Mirrors `common::spawn_agent`'s isolation posture (round 30).
        cmd.env("HOME", self.root());
        if !cfg!(target_os = "macos") {
            let config = self.root().join("config");
            std::fs::create_dir_all(&config).unwrap();
            cmd.env("XDG_CONFIG_HOME", config);
            let runtime = self.root().join("runtime");
            std::fs::create_dir_all(&runtime).unwrap();
            cmd.env("XDG_RUNTIME_DIR", runtime);
        }
        KillOnDrop(Some(cmd.spawn().expect("spawn fauna-sync-agent")))
    }

    /// The agent on its **production** resolution path: no `--data-dir`, cwd
    /// `/` (launchd's), so the data root, socket and logs all come from `HOME`
    /// exactly as they do under the shipped LaunchAgent — and `HOME` being this
    /// world's private root is what keeps the run off the developer's real
    /// agent (the same override `agent_process_tier3` documents). macOS-only:
    /// the linux production socket additionally needs `XDG_RUNTIME_DIR`, which
    /// the resolver refuses to guess, and the only caller is the container
    /// witness below.
    #[cfg(target_os = "macos")]
    fn spawn_agent_on_production_paths(&self) -> KillOnDrop {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fauna-sync-agent"));
        cmd.env("FAUNA_E2E_CREDENTIAL_DIR", self.root().join("creds"))
            .env("RUST_LOG", "info")
            .env("HOME", self.root())
            .current_dir("/");
        KillOnDrop(Some(cmd.spawn().expect("spawn fauna-sync-agent")))
    }
}

/// A spawned agent that cannot outlive the test, however the test ends.
///
/// ⚠ Load-bearing on the **failing** path, which is the one that matters here:
/// `std::process::Child` does **not** kill on drop, so the first version of this
/// file leaked a live agent on every red run — found while mutation-verifying,
/// when the deliberately-broken build left an orphan holding its socket. A
/// shutdown test that orphans daemons when it fails is precisely the shape
/// testing.md convention 9 exists to forbid, and the failure mode is invisible
/// on a green run.
struct KillOnDrop(Option<Child>);

impl KillOnDrop {
    fn id(&self) -> u32 {
        self.0.as_ref().expect("child still owned").id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.as_mut().expect("child still owned").try_wait()
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            // Already-exited is the green path; `kill` then simply errors.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// SIGTERM the real agent and assert it takes the **orderly** path.
///
/// The exit status is the whole assertion: see the module doc on why "it exited"
/// would be vacuous.
#[test]
fn the_agent_binary_exits_orderly_on_sigterm() {
    let world = AgentWorld::new();
    let mut agent = world.spawn_agent();
    common::await_agent(&world.socket_path());

    // SAFETY: `kill(2)` on our own live child's pid. The child is still owned
    // here (not yet reaped), so the pid cannot have been recycled.
    let pid = agent.id() as libc::pid_t;
    let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(
        rc,
        0,
        "kill(SIGTERM) failed: {}",
        std::io::Error::last_os_error()
    );

    // Deadline-poll `try_wait` rather than a blocking `wait`: a hung agent must
    // fail this test with a diagnosis, not hang the suite until the harness
    // timeout fires with no explanation.
    let deadline = Instant::now() + EXIT_BUDGET;
    let status = loop {
        match agent.try_wait().expect("try_wait must not error") {
            Some(status) => break status,
            None => {
                assert!(
                    Instant::now() < deadline,
                    "the agent did not exit within {EXIT_BUDGET:?} of SIGTERM — \
                     the shutdown select is not completing on SignalKind::terminate()"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };

    assert_eq!(
        status.signal(),
        None,
        "the agent was KILLED BY THE SIGNAL ({:?}) instead of handling it — this is \
         exactly the pre-fix behaviour: SIGTERM's default disposition terminates the \
         process, so an unwired handler still makes the child go away. Only the exit \
         status tells the two apart.",
        status.signal()
    );
    assert_eq!(
        status.code(),
        Some(0),
        "the agent handled SIGTERM but exited non-zero — the orderly path ran and failed"
    );
}

/// **The deny-crash fix's headless witness** (`sync-agent.md` § Implementation
/// status today → A4 item 2): an agent whose data root it CANNOT write must
/// still reach ready, serving its socket on memory-ring + stderr logging,
/// instead of panicking in `fauna_log::init` and crash-looping under
/// `KeepAlive` — the shape a macOS TCC deny on the (now abandoned) app-group
/// container produced live on 2026-08-23 and 2026-08-25, and that an unmounted
/// or read-only disk produces on any platform.
///
/// The measured 2026-08-25 round could not verify the fix (the box ran a
/// build predating it), and a human watching a prompt is the wrong witness
/// anyway: the mechanism is "log-init degrades instead of panicking", and the
/// exact steps the human would take — start the real binary against a root it
/// cannot write, see whether it comes up — are what this test performs. It
/// needs no nest and no `tier3-nest` feature, same world as the SIGTERM proof
/// above. Red-verified by reverting `fauna_log`'s `try_file_appender` to the
/// panicking `rolling::daily` (the socket never appears; the child is dead).
///
/// The unwritable root is a **file** where the data dir should be: every
/// `create_dir_all` under it fails `ENOTDIR`, regardless of uid — a `chmod
/// 000` dir would pass vacuously under root (CI containers) and under any
/// process with Full Disk Access, and TCC itself cannot be provoked headlessly.
#[cfg(unix)]
#[test]
fn the_agent_binary_reaches_ready_with_an_unwritable_data_root() {
    let world = AgentWorld::new();
    // A regular FILE at the data-dir path: nothing can be created beneath it.
    std::fs::write(world.root().join("data"), b"not a directory").unwrap();
    let mut agent = world.spawn_agent();
    common::await_agent(&world.socket_path());
    assert!(
        agent.try_wait().expect("try_wait must not error").is_none(),
        "the agent exited after coming up — log-init degraded but something else \
         refused an unwritable data root"
    );
}

/// **The agent never opens the app-group container — the headless witness
/// behind dropping its `com.apple.security.application-groups` entitlement**
/// (`installers/macos.md` § Identifier domain, record item 6; `sync-agent.md`
/// § Implementation status today → A4 item 2, remaining leg iii).
///
/// The real binary on its PRODUCTION resolution path — no `--data-dir`, `HOME`
/// a private root exactly as launchd hands it a real one — comes up serving its
/// socket from the user domain (`~/Library/Application Support/Fauna`) and has
/// created **nothing** under `Library/Group Containers`: the one location
/// macOS 15+ TCC-prompts a launchd-spawned process for, per instance, with no
/// user decision of either polarity ever binding the next one (record item 5).
/// The claim the entitlement used to make — "the agent's data root is in the
/// group container" — is therefore false by construction, and a dead claim
/// invites the next "why does it prompt" hunt, which is why it goes.
///
/// What a human would do — install, reboot, watch for a prompt — is the last
/// inch no test can reach (TCC cannot be provoked headlessly). The mechanism
/// underneath is entirely reachable, and it is the whole story: every
/// 2026-07-20..2026-08-25 agent prompt came from a boot-path touch (log-init
/// and config-read both resolved under the container). Red-verified by
/// pointing `fauna_account_store::root::macos_user_domain_base` back at
/// `apple_group_container` — the container dir appears and the assertion
/// fires. macOS-only because the container is a macOS concept; the same
/// production-path spawn on linux would additionally need `XDG_RUNTIME_DIR`,
/// which the socket resolver deliberately refuses to guess.
#[cfg(target_os = "macos")]
#[test]
fn the_agent_binary_boots_in_the_user_domain_and_never_creates_the_container() {
    let world = AgentWorld::new();
    let mut agent = world.spawn_agent_on_production_paths();
    common::await_agent(&world.socket_path());
    assert!(
        agent.try_wait().expect("try_wait must not error").is_none(),
        "the agent exited after coming up"
    );

    let user_domain = world.root().join("Library/Application Support/Fauna");
    assert!(
        user_domain.join("sync").is_dir(),
        "the data root must be the user-domain `Fauna/sync` (found: {:?})",
        std::fs::read_dir(&user_domain)
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    assert!(
        world.socket_path().exists(),
        "the socket must sit beside it in the user domain"
    );
    let container_root = world.root().join("Library/Group Containers");
    assert!(
        !container_root.exists(),
        "the agent created {container_root:?} — it opened the TCC-protected app-group \
         container, the exact touch that prompts a launchd-spawned process on every \
         instance (installers/macos.md § Identifier domain, item 5)"
    );
}
