//! **A member's presence root against a real nest: a reader's root hydrates
//! read-only, a `writer` grant remounts it two-way, and a dropped presence is
//! unmounted and removed once drained.**
//!
//! `on-demand-files-linux.md` § Linux FUSE binding, the presence-root bullet,
//! and `on-demand-files-shared-sets.md` § Shared sets on a capability host,
//! decision 3d: a set the presence plan names and no bound location reaches is
//! served at `$HOME/Fauna/<display name>` as a FUSE view over a private backing
//! in the agent's state dir. A reader's view is mounted `ro` and answers
//! `EROFS`; the reader flag rides the engine stamp, so a grant restarts the
//! engine and remounts the view read-write; a root the plan no longer names is
//! unmounted and its presence path, backing and state DB removed once nothing
//! is un-recorded.
//!
//! ## What is real here
//!
//! One in-process `fauna-nest` with a real byte plane (`common::start_nest`);
//! two REAL `fauna-sync-agent` processes driven over their REAL per-user
//! sockets — the owner's, serving a bound location, and the member's, with NO
//! bound location, so the only root it can serve is the one its own presence
//! plan names; the owner's REAL share and grant (`FoldersAuthor::share_set`
//! over a real MLS engine); real FUSE mounts, read through `/proc/self/mountinfo`
//! and driven from this test process. No step hands either agent a key.
//!
//! The member's re-plan after the grant is fired by the presence toggle the
//! member's app sets (`SetPresenceEnabled`), one of the plan's IPC edges; with
//! no app running the content-key backstop re-plans within 300 s
//! (`content_keys::run`).
//!
//! ## Run
//!
//! ```text
//! cargo test -p fauna-sync-agent --features tier3-nest,fuse-live --test presence_root_tier3
//! ```
//!
//! Needs an openable `/dev/fuse` and the distro `fuse3` package's
//! `fusermount3`, as `fuse_live_integration` does (`just fuse-live-test-check`).

#![cfg(target_os = "linux")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::sets::{Actor, OwnerSeat, RECORD_WINDOW, ingest_custody, register_agent_device};
use common::{AgentWorld, AppSeat, await_agent, await_serving_engine, connected_client, expect_ok};
use fauna_core::folder_keys::FolderRef;
use fauna_ipc::sync::{BearerToken, RequestMethod, SyncCapability};
use fauna_nest::routes::AppState;

const SET_NAME: &str = "team-docs";
const OWNER_DEVICE: [u8; 32] = [0x0a; 32];
const MEMBER_DEVICE: [u8; 32] = [0x0b; 32];
/// One causal chain — a plan read and a mount, a hydration, a restart and a
/// remount, an upload and the owner's pull — is seconds when healthy and far
/// below the 300 s rescan tick and backstop.
const EDGE_WINDOW: Duration = Duration::from_secs(90);

/// A real agent process with its private root.
struct Agent {
    world: AgentWorld,
    _process: common::KillOnDrop,
    socket: PathBuf,
}

impl Agent {
    /// Enrol this machine for `actor` (so the agent mounts the account store
    /// and loads a change signer), spawn the real agent and provision it with
    /// ONLY the account's own `BackupKey` and a bearer.
    async fn provisioned(
        state: &AppState,
        base: &str,
        actor: &Actor,
        app: &AppSeat,
        device: [u8; 32],
        prefix: &str,
    ) -> Self {
        let world = AgentWorld::new(prefix);
        let socket = world.socket_path();
        common::enroll_as_w5_app(
            actor.secret,
            &world,
            base,
            &hex::encode(actor.id.0),
            None,
            hex::encode(device),
        )
        .await;
        app.sync().await;
        let process = world.spawn_agent_trusting(&[base]);
        await_agent(&socket);
        let token = state.auth.token_store.insert(actor.id, 3600).await;
        expect_ok(
            &socket,
            RequestMethod::ProvisionCapability(SyncCapability::new(
                actor.backup_key().to_bytes().to_vec(),
                actor.id.0.to_vec(),
                base.to_string(),
                hex::encode(device),
                BearerToken::new(token, 4_000_000_000),
            )),
        );
        Self {
            world,
            _process: process,
            socket,
        }
    }

    /// Bind `location` to the set and wait for its engine to serve.
    fn bind(&self, location: &Path, folder_id: i64) {
        std::fs::create_dir_all(location).unwrap();
        let path = location.display().to_string();
        expect_ok(
            &self.socket,
            RequestMethod::AddLocation { path: path.clone() },
        );
        expect_ok(
            &self.socket,
            RequestMethod::SetLocationFolder {
                path,
                folder: SET_NAME.to_string(),
                folder_id: FolderRef::Local(folder_id).to_wire(),
            },
        );
        await_serving_engine(&self.socket, SET_NAME, EDGE_WINDOW);
    }

    /// The presence toggle the member's app sets — an edge that re-plans.
    fn set_presence(&self, scoped_id: &str, enabled: bool) {
        expect_ok(
            &self.socket,
            RequestMethod::SetPresenceEnabled {
                scoped_id: scoped_id.to_string(),
                enabled,
            },
        );
    }

    /// The nudge the owner's app sends on a `SyncChanged` push.
    fn pull_now(&self) {
        expect_ok(
            &self.socket,
            RequestMethod::PullFolderNow {
                folder: SET_NAME.to_string(),
                folder_hash: None,
            },
        );
    }

    /// `$HOME/Fauna` — the agent's `HOME` is its world root.
    fn presence_dir(&self) -> PathBuf {
        self.world.root().join("Fauna")
    }
}

/// One mount of this agent's binding: its mount point and whether it is `ro`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    point: PathBuf,
    read_only: bool,
}

/// The mounts under `dir`, off `/proc/self/mountinfo` — the kernel's own
/// record, the observable no agent log can fake.
fn mounts_under(dir: &Path) -> Vec<Mount> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    mountinfo
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            let point = PathBuf::from(unescape_mountinfo(fields.get(4)?));
            let options = fields.get(5)?;
            point.starts_with(dir).then(|| Mount {
                point,
                read_only: options.split(',').any(|o| o == "ro"),
            })
        })
        .collect()
}

/// `mountinfo` escapes a space, tab, newline and backslash as `\ooo`.
fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4].iter().all(u8::is_ascii_digit)
        {
            let octal = std::str::from_utf8(&bytes[i + 1..i + 4]).unwrap();
            out.push(u8::from_str_radix(octal, 8).unwrap());
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// Poll until the one mount under `dir` reads `read_only`; return it.
fn await_mount(dir: &Path, read_only: bool, what: &str) -> Mount {
    let deadline = Instant::now() + EDGE_WINDOW;
    loop {
        let mounts = mounts_under(dir);
        if let [mount] = mounts.as_slice()
            && mount.read_only == read_only
        {
            return mount.clone();
        }
        assert!(
            Instant::now() < deadline,
            "{what}: no single {} mount under {} within {EDGE_WINDOW:?} — mounts: {mounts:?}",
            if read_only { "ro" } else { "rw" },
            dir.display()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Poll until `check` holds.
fn await_true(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + EDGE_WINDOW;
    while !check() {
        assert!(
            Instant::now() < deadline,
            "{what}: not within {EDGE_WINDOW:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Every directory named `presence` under `root` that is not under `skip` —
/// the agent's per-account `<state dir>/presence`, wherever the data dir puts it.
fn find_presence_roots(root: &Path, skip: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.starts_with(skip) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if entry.file_name() == "presence" {
            out.push(path.clone());
        }
        find_presence_roots(&path, skip, out);
    }
}

fn payload(seed: u32) -> Vec<u8> {
    (0..32_000u32)
        .map(|i| (i.wrapping_mul(seed) % 251) as u8)
        .collect()
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_members_presence_root_hydrates_read_only_remounts_on_a_grant_and_goes_once_dropped() {
    init_tracing();
    let (base, state) = common::start_nest().await;
    let alice = Actor::create(&state, "alice").await;
    let bob = Actor::create(&state, "bob").await;
    let bob_engine = fauna_mls::engine::MlsEngine::new_in_memory(bob.keypair()).unwrap();
    bob.publish_key_package(&state, &bob_engine).await;

    // Alice creates the set and shares it with Bob as a READER.
    let seat = OwnerSeat::open(&state, &base, &alice, SET_NAME).await;
    let folder_id = seat.create_set().await;
    let channel = seat.share(&bob, "reader").await;
    register_agent_device(&seat.nest, OWNER_DEVICE).await;

    // Alice's agent serves the set at a bound location; her file is recorded
    // before Bob's agent ever runs, so his root's first listing names it.
    let owner = Agent::provisioned(
        &state,
        &base,
        &alice,
        &seat.app,
        OWNER_DEVICE,
        "fauna-presence-owner-",
    )
    .await;
    let owner_location = owner.world.root().join("bound");
    owner.bind(&owner_location, folder_id);
    let alices = payload(7);
    std::fs::write(owner_location.join("from-alice.bin"), &alices).unwrap();
    seat.await_recorded("from-alice.bin", "the owner's file")
        .await;

    // Bob's app holds the set's keys; his agent has NO bound location.
    let bob_app = AppSeat::sign_in(&state, &base, bob.secret).await;
    ingest_custody(&bob_app, channel, seat.envelope(channel).await).await;
    let bob_nest = connected_client(&base, bob.keypair()).await;
    register_agent_device(&bob_nest, MEMBER_DEVICE).await;
    // Vacuity guards: what the member's plan reads — the member row on the
    // nest's list, and the set's keys in the member's custody.
    let listed = fauna_client_folders::FoldersClient::new(Arc::clone(&bob_nest))
        .list_owned_and_shared_wire()
        .await
        .expect("the member lists the sets shared with him")
        .folders;
    let row = listed
        .iter()
        .find(|r| r.id == folder_id)
        .unwrap_or_else(|| panic!("the shared set is not on the member's list: {listed:?}"));
    assert_eq!(row.role.as_deref(), Some("member"), "row: {row:?}");
    assert!(
        row.is_reader_member(),
        "the member reads the set as a reader: {row:?}"
    );
    let bob_custody = bob_app
        .folder_keys()
        .load()
        .await
        .expect("the member's custody");
    assert_eq!(
        fauna_client_folders::engine_binding::custody_channel_for(row, &bob_custody),
        Ok(Some(channel)),
        "the member's custody keys the set's row at its channel"
    );
    let member = Agent::provisioned(
        &state,
        &base,
        &bob,
        &bob_app,
        MEMBER_DEVICE,
        "fauna-presence-member-",
    )
    .await;
    let presence_dir = member.presence_dir();

    // (1) The reader's root: mounted `ro` under `$HOME/Fauna`.
    let mount = await_mount(&presence_dir, true, "the reader's presence root");
    let view = mount.point.clone();
    assert_eq!(
        view.parent(),
        Some(presence_dir.as_path()),
        "the presence root sits one level under $HOME/Fauna"
    );

    // Opening the owner's file hydrates it into the private backing.
    let mut read = None;
    await_true(
        "the owner's file listed and hydrated through the view",
        || {
            read = std::fs::read(view.join("from-alice.bin")).ok();
            read.as_deref() == Some(alices.as_slice())
        },
    );
    let mut presence_roots = Vec::new();
    find_presence_roots(member.world.root(), &presence_dir, &mut presence_roots);
    let [presence_root] = presence_roots.as_slice() else {
        panic!("exactly one `<state dir>/presence` expected, found {presence_roots:?}");
    };
    let backing = std::fs::read_dir(presence_root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.join("from-alice.bin").is_file())
        .unwrap_or_else(|| {
            panic!(
                "the hydration landed in no backing under {}",
                presence_root.display()
            )
        });
    assert_eq!(
        std::fs::read(backing.join("from-alice.bin")).unwrap(),
        alices,
        "the backing holds the hydrated bytes"
    );

    // A write through a reader's view is refused by the filesystem.
    let refused = std::fs::write(view.join("from-bob.bin"), payload(3))
        .expect_err("a reader's presence root took a write");
    assert_eq!(
        refused.raw_os_error(),
        Some(libc::EROFS),
        "a reader's view answers EROFS: {refused}"
    );

    // (2) Alice grants Bob `writer`; the next plan pass (the toggle Bob's app
    // sets) reads the row, flips the stamp, restarts the engine and remounts
    // the same path read-write.
    seat.share(&bob, "writer").await;
    let scoped = FolderRef::Local(folder_id).scoped_to(bob.id.0).to_wire();
    member.set_presence(&scoped, true);
    let remounted = await_mount(&presence_dir, false, "the granted root's remount");
    assert_eq!(remounted.point, view, "the grant remounts the SAME path");

    // A write through the view now reaches the owner: first the member's
    // upload, read on the nest's change log (the member's half alone)…
    let bobs = payload(11);
    await_true("a write through the granted view", || {
        std::fs::write(view.join("from-bob.bin"), &bobs).is_ok()
    });
    seat.await_recorded(
        "from-bob.bin",
        "the member's write through the granted view",
    )
    .await;
    // …then the owner's pull. The agent holds no push subscription of its own:
    // the owner's app hears the nest's `SyncChanged` and nudges her agent
    // (`apps/fauna-linux/src/app.rs` → `sync_agent::pull_set_now`; the link
    // `same_nest_push_nudge` proves). No app runs here, so the test sends that
    // nudge — otherwise only the 300 s rescan tick would pull.
    owner.pull_now();
    common::await_file_content_within(
        &owner_location.join("from-bob.bin"),
        &bobs,
        RECORD_WINDOW,
        "the member's write through the granted view, at the owner",
        "presence_root_tier3",
        "the member's view did not upload, or the owner's engine did not pull it",
    );

    // (3) Bob's app toggles the set off: the plan drops the root; it is
    // unmounted, and — nothing un-recorded — its presence path, backing and
    // state DB go.
    let state_dir = presence_root.parent().unwrap().to_path_buf();
    let state_db = FolderRef::Local(folder_id).state_db_path(&state_dir);
    assert!(
        state_db.is_file(),
        "vacuity guard: the root's state DB is at {}",
        state_db.display()
    );
    member.set_presence(&scoped, false);
    await_true("the dropped root's view unmounted", || {
        mounts_under(&presence_dir).is_empty()
    });
    await_true("the dropped root's presence path removed", || {
        !view.exists()
    });
    await_true("the dropped root's backing removed", || !backing.exists());
    await_true("the dropped root's state DB removed", || !state_db.exists());
    drop(owner);
}
