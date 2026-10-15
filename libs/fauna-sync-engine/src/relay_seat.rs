//! The **relay-serving seat** — how a process hosting resident engines hands a
//! metadata-only folder's bytes to the nest's relay
//! (`docs/goal/behavior/file-sync.md` § Relay serving).
//!
//! Three moves, all over what the host already holds:
//!
//! 1. **Announce.** [`RelaySeat::run`] sends `fauna.sync.serve.announce` on the
//!    host's WS-RPC connection, naming the folder of every engine registered
//!    here. The set is sent whole — at start, on every reconnect (the nest
//!    keeps an announce on the connection, so a new connection starts with
//!    none) and whenever an engine registers or leaves. A folder homed on
//!    another nest rides the same announce, on the same connection to the
//!    host's own nest, named by its foreign ref and its home nest
//!    (`file-sync.md` § Relay serving → *A member on another nest*, step (2));
//!    that nest forwards it to the home nest.
//! 2. **Route.** The nest's ask, the `fauna.sync.chunk.wanted` push, names the
//!    folder; the seat hands it to that folder's engine and to no other. An ask
//!    for a folder this process did not announce is ignored.
//! 3. **Answer.** [`ServeInbox::serve`] runs beside the engine's own loop and
//!    answers each ask through [`SyncEngine::answer_serve_ask`] — the bytes on
//!    the bulk rail, or a decline — at most [`SERVE_CONCURRENCY`] at a time.
//!    The answer goes out on the engine's own byte plane, so a foreign
//!    folder's goes straight to its home nest under the write token the
//!    engine's uploads carry (step (4)).
//!
//! **A sibling's want rides the same queue.** The same-account peer leg's
//! chunk door ([`RelaySeat::serve_peer`]) hands a want to the folder's engine
//! exactly as the nest's ask is handed; only where the answer goes differs
//! ([`AskRoute`]). One serve core, one concurrency bound, every consumer.
//!
//! **Beside the loop, never inside it.** The seat that *asks* for a chunk is
//! itself an announced seat of the folder, so the nest asks it too — while its
//! own loop is blocked on that very fetch. Its index does not name the key, so
//! it declines at once; an arm of the engine loop could not, and the reader
//! would wait out the relay's deadline for a seat that was never going to
//! answer.
//!
//! Hosts: the per-user sync agent wires its connection in
//! (`bins/fauna-sync-agent`'s engine driver). An app whose replicas are
//! hosted on demand — a phone's provider, each set's engine on its host's own
//! worker — reaches them through [`ServingHost`] instead and runs
//! [`RelaySeat::serve_hosts`] over its own connection while it is in front
//! (`p2p-shared-set-build.md` § *Phone peers — design*, decision 2). A
//! run-and-drop host — an engine built for one pass and dropped — holds no
//! connection for the nest to ask and registers nothing here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_core::folder_keys::FolderRef;
use fauna_protocol::PushEvent;
use fauna_protocol::push_events::SyncChunkWantedPayload;
use fauna_protocol::sync::{
    KIND_SYNC_SERVE_ANNOUNCE, SERVE_ANNOUNCE_MAX_FOLDERS, SyncServeAnnounceReply,
    SyncServeAnnounceRequest, SyncServeForeignEntry,
};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::sync::{broadcast, mpsc, watch};

use crate::engine::SyncEngine;

/// The most asks one engine answers at once (`file-sync.md` § Relay serving →
/// *The seat serves from the file*: "a constant bounds its concurrent
/// serves"). Each serve reads, seals and posts one chunk of up to 8 MiB, so
/// this is also the bound on what serving holds in memory per folder.
pub const SERVE_CONCURRENCY: usize = 4;

/// Asks queued for one engine beyond the ones it is answering. An ask that
/// finds the queue full is dropped, which costs what a lost push costs: the
/// relay passes this seat over.
const ASK_QUEUE: usize = 32;

/// How long a failed announce waits before it is sent again. A reconnect and a
/// change to the engine set re-announce at once; this covers the failures
/// neither follows — above all a device the app beside this host has not
/// registered yet.
const ANNOUNCE_RETRY: Duration = Duration::from_secs(60);

/// How long a sibling's want waits for the folder's engine to answer before
/// the serve side reports the chunk missing (the puller then asks the nest).
/// Covers a full queue's worth of serves ahead of it.
const PEER_SERVE_DEADLINE: Duration = Duration::from_secs(30);

/// One ask, as the seat hands it to the folder's engine.
#[derive(Debug)]
pub struct ServeAsk {
    /// The store key of the wanted chunk.
    pub store_key: [u8; 32],
    /// Where the answer goes.
    pub route: AskRoute,
}

/// Who asked, and so where the engine's answer goes. The serve itself is the
/// same for both — [`SyncEngine::serve_chunk`], the one serve core.
#[derive(Debug)]
pub enum AskRoute {
    /// The nest's relay (`fauna.sync.chunk.wanted`): the answer is posted back
    /// on the bulk rail under the nest's handle for the ask.
    Relay { request_id: u64 },
    /// A sibling device over the peer leg (`fauna.peer.sync.chunks.pull`): the
    /// answer goes back to the peer serve side waiting on it.
    Peer(tokio::sync::oneshot::Sender<Option<Vec<u8>>>),
}

/// One registered run of a folder's engine: its registration id, the home
/// nest it announces a foreign folder by (`None` for a folder of the host's
/// own nest), and the queue its asks go to.
type RegisteredRun = (u64, Option<String>, mpsc::Sender<ServeAsk>);

/// One announce's folders, split as the wire carries them.
#[derive(Debug, Default, PartialEq)]
struct Announcement {
    /// Folders of the host's own nest, by `FolderRef` wire string.
    folders: Vec<String>,
    /// Folders homed on another nest, each with that nest's URL.
    foreign: Vec<SyncServeForeignEntry>,
}

impl Announcement {
    fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.foreign.is_empty()
    }

    fn len(&self) -> usize {
        self.folders.len() + self.foreign.len()
    }
}

/// The seat of one host process: which folders its engines serve, and the
/// loop that announces them and routes the nest's asks.
pub struct RelaySeat {
    /// The running engines, keyed by their folder's `FolderRef` wire string —
    /// the spelling both the announce and the ask carry. Several runs of one
    /// folder's engine can be registered at once, oldest first: a restart
    /// cancels the old run, but that run may finish building — and register —
    /// after its replacement did, and only then notice it was cancelled. Each
    /// run removes its own entry and nothing else, so the folder stays
    /// announced while any run of it is registered.
    folders: Mutex<HashMap<String, Vec<RegisteredRun>>>,
    /// The id the next registration takes.
    next_id: std::sync::atomic::AtomicU64,
    /// Fired when the set above changes, so [`Self::run`] announces again.
    changed: tokio::sync::Notify,
    /// What the nest admitted of the last announce; empty before the first
    /// one and after one that failed.
    admitted: watch::Sender<Vec<String>>,
}

impl RelaySeat {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            folders: Mutex::new(HashMap::new()),
            next_id: std::sync::atomic::AtomicU64::new(0),
            changed: tokio::sync::Notify::new(),
            admitted: watch::channel(Vec::new()).0,
        })
    }

    /// Register a running engine's folder and get the inbox its asks arrive
    /// on. The folder is announced from the next announce on and withdrawn
    /// when the inbox drops, so an engine that is not running is not
    /// announced.
    ///
    /// `home_nest_url` is the home nest of a folder homed on another nest —
    /// the engine's byte plane, [`SyncEngine::foreign_routing`]'s URL — which
    /// the announce names it by (`file-sync.md` § Relay serving → *A member on
    /// another nest*, step (2)); it is ignored for a folder of the host's own
    /// nest. A foreign folder without one cannot be announced and is
    /// registered nowhere: its inbox stays empty.
    pub fn register(
        self: &Arc<Self>,
        folder: &FolderRef,
        home_nest_url: Option<&str>,
    ) -> ServeInbox {
        let (tx, rx) = mpsc::channel(ASK_QUEUE);
        let key = folder.to_wire();
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let home = match folder {
            FolderRef::Local(_) => Some(None),
            FolderRef::Foreign(_) => home_nest_url
                .filter(|url| !url.is_empty())
                .map(|url| Some(url.to_string())),
        };
        let registered = home.is_some();
        if let Some(home) = home {
            let mut folders = self.folders.lock().expect("relay seat registry");
            let runs = folders.entry(key.clone()).or_default();
            // A new folder, or a run naming another home nest than the run
            // announced so far (a set re-accepted from elsewhere): either way
            // the announce changes.
            let announce_changed = runs.last().is_none_or(|(_, last, _)| *last != home);
            runs.push((id, home, tx));
            drop(folders);
            if announce_changed {
                self.changed.notify_one();
            }
        } else {
            tracing::warn!(
                folder = %key,
                "a folder homed on another nest with no home nest URL cannot be announced; \
                 it is not served by relay"
            );
        }
        ServeInbox {
            rx,
            _registration: Registration {
                seat: Arc::clone(self),
                key,
                id,
                registered,
            },
        }
    }

    /// What the nest admitted of the last announce — a witness's barrier for
    /// "this seat is now asked for the folder".
    pub fn subscribe_admitted(&self) -> watch::Receiver<Vec<String>> {
        self.admitted.subscribe()
    }

    /// The folders to announce: every registered engine's, in a stable order,
    /// each by the home nest its newest run names, capped at what one
    /// announce may name — both lists together (the nest refuses a longer one
    /// whole).
    fn announced(&self) -> Announcement {
        let mut folders: Vec<(String, Option<String>)> = self
            .folders
            .lock()
            .expect("relay seat registry")
            .iter()
            .filter_map(|(key, runs)| Some((key.clone(), runs.last()?.1.clone())))
            .collect();
        folders.sort();
        if folders.len() > SERVE_ANNOUNCE_MAX_FOLDERS {
            tracing::warn!(
                engines = folders.len(),
                announced = SERVE_ANNOUNCE_MAX_FOLDERS,
                "more engines than one announce may name; the rest are not served by relay"
            );
            folders.truncate(SERVE_ANNOUNCE_MAX_FOLDERS);
        }
        let mut announcement = Announcement::default();
        for (folder, home) in folders {
            match home {
                None => announcement.folders.push(folder),
                Some(nest_url) => announcement.foreign.push(SyncServeForeignEntry {
                    folder,
                    nest_url,
                    ..Default::default()
                }),
            }
        }
        announcement
    }

    /// Hand one ask to the engine of the folder it names. `false` when it went
    /// nowhere: a folder this process did not announce, a store key that is
    /// not a well-formed content name, or an engine whose queue is full.
    pub fn route(&self, ask: &SyncChunkWantedPayload) -> bool {
        let Some(store_key) = parse_store_key(&ask.store_key) else {
            return false;
        };
        let folders = self.folders.lock().expect("relay seat registry");
        // The newest run: an older one is a cancelled run on its way out.
        let Some((_, _, tx)) = folders.get(&ask.folder).and_then(|runs| runs.last()) else {
            return false;
        };
        tx.try_send(ServeAsk {
            store_key,
            route: AskRoute::Relay {
                request_id: ask.request_id,
            },
        })
        .is_ok()
    }

    /// Serve one stored chunk of `folder` (its `FolderRef` wire string) to a
    /// sibling device — the peer leg's door into the one serve core
    /// (`file-sync.md` § Relay serving: the same-account peer leg is that
    /// core's further consumer). The want joins the folder's engine's queue
    /// like a relay ask and is answered by the same serve. `None` — never an
    /// error — for a folder no engine here runs, a full queue, an engine that
    /// holds no body, or no answer within [`PEER_SERVE_DEADLINE`].
    pub async fn serve_peer(&self, folder: &str, store_key: [u8; 32]) -> Option<Vec<u8>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        {
            let folders = self.folders.lock().expect("relay seat registry");
            let (_, _, tx) = folders.get(folder).and_then(|runs| runs.last())?;
            tx.try_send(ServeAsk {
                store_key,
                route: AskRoute::Peer(answer),
            })
            .ok()?;
        }
        tokio::time::timeout(PEER_SERVE_DEADLINE, answered)
            .await
            .ok()?
            .ok()?
    }

    /// Announce and route until the connection's push stream ends. `nest` is
    /// the host's own WS-RPC connection and `device_id_hex` the sync device
    /// its engines run as — one of the account's registered devices, or the
    /// nest refuses the announce.
    pub async fn run(&self, nest: &fauna_client::NestClient, device_id_hex: &str) {
        let mut pushes = nest.subscribe_pushes();
        let mut reconnects = nest.subscribe_reconnects();
        // Whether this connection holds an announce the nest admitted
        // anything of — what an empty set would have to withdraw.
        let mut holds_announce = false;
        loop {
            let folders = self.announced();
            let mut retry = false;
            if !folders.is_empty() || holds_announce {
                let named = folders.len();
                match announce(nest, device_id_hex, folders).await {
                    Ok(admitted) => {
                        // Once per change to the set: the only line that says
                        // whether this seat is a holder the relay can ask.
                        tracing::info!(
                            named,
                            admitted = admitted.len(),
                            "relay serving: announced this process's folders"
                        );
                        holds_announce = !admitted.is_empty();
                        self.admitted.send_replace(admitted);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "relay serving announce failed; will retry");
                        self.admitted.send_replace(Vec::new());
                        retry = true;
                    }
                }
            }
            loop {
                tokio::select! {
                    _ = self.changed.notified() => break,
                    changed = reconnects.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        holds_announce = false;
                        break;
                    }
                    _ = tokio::time::sleep(ANNOUNCE_RETRY), if retry => break,
                    push = pushes.recv() => match push {
                        Ok(PushEvent::SyncChunkWanted(ask)) => {
                            self.route(&ask);
                        }
                        Ok(_) => {}
                        // Asks lost to a lagging subscriber are lost pushes:
                        // the relay passes this seat over for them.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
        }
    }
}

/// How often [`RelaySeat::track`] re-reads its hosts: a host built or dropped
/// is announced or withdrawn within this. Hosts come and go with the
/// provider, at human pace.
pub const HOST_RESCAN: Duration = Duration::from_secs(10);

/// The folder a [`ServingHost`]'s engine serves, and the home nest
/// [`RelaySeat::register`] announces it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedFolder {
    pub folder: FolderRef,
    /// The home nest of a folder homed on another nest, as `register` takes
    /// it ([`BuiltEngine::serve_home_nest_url`]): `None` for a folder of the
    /// host's own nest, and for a reader's engine, whose seat does not serve.
    ///
    /// [`BuiltEngine::serve_home_nest_url`]: crate::engine_lifecycle::BuiltEngine::serve_home_nest_url
    pub home_nest_url: Option<String>,
}

/// One replica host whose engine the seat reaches through a door rather than
/// holding it — an on-demand host, whose engine lives on the host's own
/// worker (the FFI's file-provider host).
#[async_trait::async_trait]
pub trait ServingHost: Send + Sync {
    /// The folder this host's engine serves now, or `None` while it serves
    /// none (its engine is not built — a refused binding).
    async fn serving_folder(&self) -> Option<ServedFolder>;

    /// Answer one ask through the host's engine
    /// ([`SyncEngine::answer_serve_ask`], beside the host's own work).
    async fn answer_serve_ask(&self, ask: ServeAsk);
}

/// The live [`ServingHost`]s of one connection's account, read afresh at
/// every rescan.
pub trait ServingHosts: Send + Sync {
    fn hosts(&self) -> Vec<Arc<dyn ServingHost>>;
}

/// One tracked host: who it is (held weakly — the seat never keeps a host
/// alive past whoever built it), what it was announced by, and the task
/// serving its inbox.
struct Tracked {
    host: std::sync::Weak<dyn ServingHost>,
    /// The home nest it was registered with ([`ServedFolder::home_nest_url`]).
    home_nest_url: Option<String>,
    serving: tokio::task::AbortHandle,
}

impl RelaySeat {
    /// Keep one registration per live host's folder, re-reading `hosts` every
    /// `rescan`: a host that gains a folder is registered and its asks are
    /// answered through it; one that goes, or loses its folder, is withdrawn.
    /// Never returns; dropping it withdraws every folder it registered.
    pub async fn track(self: &Arc<Self>, hosts: Arc<dyn ServingHosts>, rescan: Duration) {
        // Dropping the set aborts every serving task, and each task's inbox
        // with it — the withdrawal.
        let mut tasks = tokio::task::JoinSet::new();
        let mut tracked: HashMap<String, Tracked> = HashMap::new();
        loop {
            let mut live: HashMap<String, (ServedFolder, Arc<dyn ServingHost>)> = HashMap::new();
            for host in hosts.hosts() {
                if let Some(served) = host.serving_folder().await {
                    live.insert(served.folder.to_wire(), (served, host));
                }
            }
            // A host whose folder now names another home nest (its engine
            // rebuilt over a set re-accepted from elsewhere) is re-registered,
            // so the announce follows it.
            tracked.retain(|key, t| {
                let same = live.get(key).is_some_and(|(served, host)| {
                    t.home_nest_url == served.home_nest_url
                        && t.host
                            .upgrade()
                            .is_some_and(|held| Arc::ptr_eq(&held, host))
                });
                if !same {
                    t.serving.abort();
                }
                same
            });
            for (key, (served, host)) in live {
                if tracked.contains_key(&key) {
                    continue;
                }
                let mut inbox = self.register(&served.folder, served.home_nest_url.as_deref());
                let weak = Arc::downgrade(&host);
                let answering = weak.clone();
                let serving = tasks.spawn(async move {
                    inbox
                        .serve_with(|ask| {
                            let host = answering.upgrade();
                            async move {
                                // A host gone between rescans answers nothing:
                                // a lost ask, which the relay passes over.
                                if let Some(host) = host {
                                    host.answer_serve_ask(ask).await;
                                }
                            }
                        })
                        .await
                });
                tracked.insert(
                    key,
                    Tracked {
                        host: weak,
                        home_nest_url: served.home_nest_url,
                        serving,
                    },
                );
            }
            while tasks.try_join_next().is_some() {}
            tokio::time::sleep(rescan).await;
        }
    }

    /// Serve `hosts` to the relay over `nest` until its push stream ends: the
    /// announce-and-route loop ([`Self::run`]) beside the host tracking
    /// ([`Self::track`]). Dropping it stops both; the connection's announce
    /// stays until [`withdraw`] or the connection's close.
    pub async fn serve_hosts(
        self: &Arc<Self>,
        nest: &fauna_client::NestClient,
        device_id_hex: &str,
        hosts: Arc<dyn ServingHosts>,
        rescan: Duration,
    ) {
        tokio::select! {
            () = self.run(nest, device_id_hex) => {}
            () = self.track(hosts, rescan) => {}
        }
    }
}

/// Withdraw this connection's announce: an empty one replaces it whole
/// (`file-sync.md` § Relay serving, step (1)), so the nest asks this
/// connection for nothing more. What a foreground seat sends when its app
/// leaves the front.
pub async fn withdraw(
    nest: &fauna_client::NestClient,
    device_id_hex: &str,
) -> Result<(), fauna_client::NestClientError> {
    announce(nest, device_id_hex, Announcement::default())
        .await
        .map(|_| ())
}

async fn announce(
    nest: &fauna_client::NestClient,
    device_id_hex: &str,
    Announcement { folders, foreign }: Announcement,
) -> Result<Vec<String>, fauna_client::NestClientError> {
    let reply: SyncServeAnnounceReply = nest
        .request(
            KIND_SYNC_SERVE_ANNOUNCE,
            SyncServeAnnounceRequest {
                device_id: device_id_hex.to_string(),
                folders,
                foreign,
                ..Default::default()
            },
        )
        .await?;
    Ok(reply.admitted)
}

/// A store key as the ask spells it: exactly 64 lowercase hex characters, the
/// digest's own spelling. Anything else is not a content name and is never
/// looked up.
fn parse_store_key(hex_key: &str) -> Option<[u8; 32]> {
    if hex_key.len() != 64
        || !hex_key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    <[u8; 32]>::try_from(hex::decode(hex_key).ok()?.as_slice()).ok()
}

/// One engine's asks. Dropping it withdraws the folder from the seat.
pub struct ServeInbox {
    rx: mpsc::Receiver<ServeAsk>,
    _registration: Registration,
}

impl ServeInbox {
    /// Answer this folder's asks from `engine` for as long as the caller
    /// polls it — beside the engine's own loop, on the same task (the engine
    /// is `!Sync`), never as an arm of it. Never returns on its own.
    pub async fn serve(&mut self, engine: &SyncEngine) {
        self.serve_with(|ask| engine.answer_serve_ask(ask)).await;
    }

    /// [`Self::serve`] over any answerer — the seam a host whose engine sits
    /// behind a trait serves through.
    pub async fn serve_with<F, Fut>(&mut self, answer: F)
    where
        F: Fn(ServeAsk) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let mut in_flight = FuturesUnordered::new();
        loop {
            tokio::select! {
                ask = self.rx.recv(), if in_flight.len() < SERVE_CONCURRENCY => match ask {
                    Some(ask) => in_flight.push(answer(ask)),
                    // No sender left — a folder that was never registered (a
                    // foreign one with no home nest URL). Nothing will ever be
                    // asked: finish what is
                    // in flight, then pend, so a caller racing this against the
                    // engine's own loop never ends that loop by it.
                    None => {
                        while in_flight.next().await.is_some() {}
                        return std::future::pending().await;
                    }
                },
                Some(()) = in_flight.next(), if !in_flight.is_empty() => {}
            }
        }
    }
}

/// The registry entry of one [`ServeInbox`], removed when the inbox drops.
struct Registration {
    seat: Arc<RelaySeat>,
    key: String,
    id: u64,
    /// Whether [`RelaySeat::register`] entered it at all (a foreign folder
    /// with no home nest URL is never registered).
    registered: bool,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let mut folders = self.seat.folders.lock().expect("relay seat registry");
        let Some(runs) = folders.get_mut(&self.key) else {
            return;
        };
        // This run's entry only — never another run of the same folder.
        let home_before = runs.last().map(|(_, home, _)| home.clone());
        runs.retain(|(id, ..)| *id != self.id);
        if runs.is_empty() {
            folders.remove(&self.key);
            drop(folders);
            self.seat.changed.notify_one();
        } else if runs.last().map(|(_, home, _)| home.clone()) != home_before {
            // The newest run left and the one before it names another home.
            drop(folders);
            self.seat.changed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ask(folder: &FolderRef, request_id: u64, store_key: &str) -> SyncChunkWantedPayload {
        SyncChunkWantedPayload {
            request_id,
            folder: folder.to_wire(),
            store_key: store_key.to_string(),
            extra: Default::default(),
        }
    }

    /// An ask reaches the engine of the folder it names and no other, and an
    /// ask for a folder nobody registered goes nowhere.
    #[tokio::test]
    async fn an_ask_is_routed_to_its_own_folder_and_to_no_other() {
        let seat = RelaySeat::new();
        let (a, b) = (FolderRef::Local(7), FolderRef::Local(9));
        let mut inbox_a = seat.register(&a, None);
        let mut inbox_b = seat.register(&b, None);
        assert_eq!(seat.announced().folders, vec![a.to_wire(), b.to_wire()]);

        let key = "ab".repeat(32);
        assert!(seat.route(&ask(&a, 1, &key)));
        let routed = inbox_a.rx.try_recv().unwrap();
        assert_eq!(routed.store_key, [0xab; 32]);
        assert!(matches!(routed.route, AskRoute::Relay { request_id: 1 }));
        assert!(inbox_b.rx.try_recv().is_err(), "b was not asked");
        assert!(
            !seat.route(&ask(&FolderRef::Local(8), 2, &key)),
            "a folder this process did not announce is ignored"
        );
    }

    /// A sibling's want rides the folder's own serve queue and comes back on
    /// the peer route; a folder no engine here runs answers nothing.
    #[tokio::test]
    async fn a_sibling_want_is_served_through_the_folders_queue() {
        let seat = RelaySeat::new();
        let folder = FolderRef::Local(7);
        let mut inbox = seat.register(&folder, None);
        tokio::spawn(async move {
            inbox
                .serve_with(|ask: ServeAsk| async move {
                    match ask.route {
                        AskRoute::Peer(answer) => {
                            let _ = answer.send(Some(ask.store_key.to_vec()));
                        }
                        AskRoute::Relay { .. } => panic!("a sibling's want is not a relay ask"),
                    }
                })
                .await
        });
        assert_eq!(
            seat.serve_peer(&folder.to_wire(), [0x5a; 32]).await,
            Some(vec![0x5a; 32])
        );
        assert_eq!(
            seat.serve_peer(&FolderRef::Local(8).to_wire(), [0x5a; 32])
                .await,
            None
        );
    }

    /// Only the digest's own spelling is a store key.
    #[tokio::test]
    async fn a_malformed_store_key_is_never_routed() {
        let seat = RelaySeat::new();
        let folder = FolderRef::Local(7);
        let mut inbox = seat.register(&folder, None);
        for bad in [
            "",
            "abcd",
            &"AB".repeat(32),
            &"zz".repeat(32),
            &format!("../{}", "a".repeat(61)),
            &"ab".repeat(33),
        ] {
            assert!(!seat.route(&ask(&folder, 1, bad)), "{bad:?} was routed");
        }
        assert!(inbox.rx.try_recv().is_err());
    }

    /// An engine that stopped is no longer announced — and a restart that
    /// registered before the old run dropped keeps its entry.
    #[tokio::test]
    async fn a_dropped_inbox_withdraws_its_folder_but_never_a_restarts() {
        let seat = RelaySeat::new();
        let folder = FolderRef::Local(7);
        let first = seat.register(&folder, None);
        let mut second = seat.register(&folder, None);
        drop(first);
        assert_eq!(
            seat.announced().folders,
            vec![folder.to_wire()],
            "the restarted engine's entry survives the old run's drop"
        );
        assert!(seat.route(&ask(&folder, 1, &"ab".repeat(32))));
        assert!(second.rx.try_recv().is_ok());
        drop(second);
        assert!(seat.announced().is_empty());
    }

    /// The order the agent measured (an e2e run, 2026-10-01): a restart's new
    /// run registers, then the CANCELLED old run finishes building and
    /// registers after it, then the old run drops. The folder must stay
    /// announced — a registry keyed one entry per folder lost it here, and
    /// the seat withdrew a folder it was still serving.
    #[tokio::test]
    async fn a_cancelled_run_registering_after_its_replacement_never_withdraws_it() {
        let seat = RelaySeat::new();
        let folder = FolderRef::Local(7);
        let mut replacement = seat.register(&folder, None);
        let late_cancelled = seat.register(&folder, None);
        drop(late_cancelled);
        assert_eq!(
            seat.announced().folders,
            vec![folder.to_wire()],
            "the live run is still registered"
        );
        assert!(seat.route(&ask(&folder, 1, &"ab".repeat(32))));
        assert!(
            replacement.rx.try_recv().is_ok(),
            "the ask reaches the run that is still alive"
        );
    }

    /// A never-registered inbox — a foreign folder with no home nest URL —
    /// pends rather than ending its caller's race.
    #[tokio::test]
    async fn serving_an_unannounceable_foreign_folder_never_returns() {
        let seat = RelaySeat::new();
        let mut inbox = seat.register(&FolderRef::Foreign([0xcd; 32]), None);
        assert!(seat.announced().is_empty());
        let served =
            tokio::time::timeout(Duration::from_millis(50), inbox.serve_with(|_ask| async {}))
                .await;
        assert!(served.is_err(), "serving must pend, never return");
    }

    /// A folder homed on another nest is announced by its foreign ref and home
    /// nest beside the host's own folders, and the ask its own nest pushes —
    /// named by the foreign ref — reaches its engine (`file-sync.md` § Relay
    /// serving → *A member on another nest*, steps (2) and (3)).
    #[tokio::test]
    async fn a_foreign_folder_is_announced_by_its_home_nest_and_asked_by_its_ref() {
        let seat = RelaySeat::new();
        let (own, foreign) = (FolderRef::Local(7), FolderRef::Foreign([0xcd; 32]));
        let _own_inbox = seat.register(&own, Some("https://ignored.example"));
        let mut foreign_inbox = seat.register(&foreign, Some("https://home.example"));
        assert_eq!(
            seat.announced(),
            Announcement {
                folders: vec![own.to_wire()],
                foreign: vec![SyncServeForeignEntry {
                    folder: foreign.to_wire(),
                    nest_url: "https://home.example".into(),
                    ..Default::default()
                }],
            },
            "an own folder is named by its ref alone, whatever URL its host passes"
        );
        assert!(seat.route(&ask(&foreign, 3, &"ab".repeat(32))));
        assert!(matches!(
            foreign_inbox.rx.try_recv().unwrap().route,
            AskRoute::Relay { request_id: 3 }
        ));
    }

    /// The newest run's home nest is the one announced; its leaving falls back
    /// to the run before it, and the last run's leaving withdraws the folder.
    #[tokio::test]
    async fn a_foreign_folder_is_announced_by_its_newest_runs_home_nest() {
        let seat = RelaySeat::new();
        let folder = FolderRef::Foreign([0xcd; 32]);
        let homes = |seat: &RelaySeat| {
            seat.announced()
                .foreign
                .into_iter()
                .map(|e| e.nest_url)
                .collect::<Vec<_>>()
        };
        let old = seat.register(&folder, Some("https://old.example"));
        let new = seat.register(&folder, Some("https://new.example"));
        assert_eq!(homes(&seat), vec!["https://new.example".to_string()]);
        drop(new);
        assert_eq!(homes(&seat), vec!["https://old.example".to_string()]);
        drop(old);
        assert!(seat.announced().is_empty());
    }

    /// A host the seat reaches through a door: its folder, the home nest its
    /// engine names, and the asks it was handed.
    struct DoorHost {
        folder: Mutex<Option<FolderRef>>,
        home: Mutex<Option<String>>,
        asked: Mutex<Vec<[u8; 32]>>,
    }

    impl DoorHost {
        fn new(folder: Option<FolderRef>) -> Arc<Self> {
            Arc::new(Self {
                folder: Mutex::new(folder),
                home: Mutex::new(None),
                asked: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl ServingHost for DoorHost {
        async fn serving_folder(&self) -> Option<ServedFolder> {
            let folder = (*self.folder.lock().unwrap())?;
            Some(ServedFolder {
                folder,
                home_nest_url: self.home.lock().unwrap().clone(),
            })
        }

        async fn answer_serve_ask(&self, ask: ServeAsk) {
            self.asked.lock().unwrap().push(ask.store_key);
        }
    }

    /// The hosts of one account, as the provider holds them.
    #[derive(Default)]
    struct Registry(Mutex<Vec<Arc<DoorHost>>>);

    impl ServingHosts for Registry {
        fn hosts(&self) -> Vec<Arc<dyn ServingHost>> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .map(|h| Arc::clone(h) as Arc<dyn ServingHost>)
                .collect()
        }
    }

    const RESCAN: Duration = Duration::from_millis(5);

    /// Wait (bounded) until `seat` announces exactly `want`.
    async fn announced_becomes(seat: &RelaySeat, want: Vec<String>) {
        let reached = tokio::time::timeout(Duration::from_secs(5), async {
            while seat.announced().folders != want {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        assert!(
            reached.is_ok(),
            "announced {:?}, want {want:?}",
            seat.announced().folders
        );
    }

    /// A tracked host's folder is announced while the host lives and has an
    /// engine, its asks reach that host, and it is withdrawn when the host
    /// goes — without the seat ever keeping the host alive.
    #[tokio::test]
    async fn a_tracked_host_is_served_while_it_lives_and_withdrawn_when_it_goes() {
        let seat = RelaySeat::new();
        let registry = Arc::new(Registry::default());
        let folder = FolderRef::Local(7);
        let host = DoorHost::new(None);
        registry.0.lock().unwrap().push(Arc::clone(&host));
        let tracking = {
            let (seat, registry) = (Arc::clone(&seat), Arc::clone(&registry));
            tokio::spawn(async move { seat.track(registry, RESCAN).await })
        };

        // No engine yet: nothing to announce.
        tokio::time::sleep(RESCAN * 4).await;
        assert!(seat.announced().is_empty());

        *host.folder.lock().unwrap() = Some(folder);
        announced_becomes(&seat, vec![folder.to_wire()]).await;
        assert!(seat.route(&ask(&folder, 1, &"ab".repeat(32))));
        let answered = tokio::time::timeout(Duration::from_secs(5), async {
            while host.asked.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        assert!(answered.is_ok(), "the ask reached the host");
        assert_eq!(host.asked.lock().unwrap().as_slice(), &[[0xab; 32]]);

        registry.0.lock().unwrap().clear();
        announced_becomes(&seat, Vec::new()).await;
        assert_eq!(
            Arc::strong_count(&host),
            1,
            "the seat holds no host its builder dropped"
        );
        tracking.abort();
    }

    /// A tracked host's folder homed on another nest is announced by the home
    /// nest its engine names, follows that nest when it changes, and is not
    /// announced while the host names none — a reader's engine, whose seat
    /// does not serve (`file-sync.md` § Relay serving → *A member on another
    /// nest*, step (2)).
    #[tokio::test]
    async fn a_tracked_hosts_foreign_folder_is_announced_by_its_home_nest() {
        let seat = RelaySeat::new();
        let registry = Arc::new(Registry::default());
        let folder = FolderRef::Foreign([0xcd; 32]);
        let host = DoorHost::new(Some(folder));
        registry.0.lock().unwrap().push(Arc::clone(&host));
        let tracking = {
            let (seat, registry) = (Arc::clone(&seat), Arc::clone(&registry));
            tokio::spawn(async move { seat.track(registry, RESCAN).await })
        };
        let homes_become = |want: Vec<&'static str>| {
            let seat = Arc::clone(&seat);
            async move {
                let homes = || {
                    seat.announced()
                        .foreign
                        .into_iter()
                        .map(|e| (e.folder, e.nest_url))
                        .collect::<Vec<_>>()
                };
                let want: Vec<_> = want
                    .into_iter()
                    .map(|url| (folder.to_wire(), url.to_string()))
                    .collect();
                let reached = tokio::time::timeout(Duration::from_secs(5), async {
                    while homes() != want {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await;
                assert!(reached.is_ok(), "announced {:?}, want {want:?}", homes());
            }
        };

        // No home nest named: not announced, however long the seat tracks it.
        tokio::time::sleep(RESCAN * 4).await;
        assert!(seat.announced().is_empty());

        *host.home.lock().unwrap() = Some("https://home.example".into());
        homes_become(vec!["https://home.example"]).await;
        assert!(seat.route(&ask(&folder, 1, &"ab".repeat(32))));
        let answered = tokio::time::timeout(Duration::from_secs(5), async {
            while host.asked.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        assert!(answered.is_ok(), "the ask reached the host");

        *host.home.lock().unwrap() = Some("https://moved.example".into());
        homes_become(vec!["https://moved.example"]).await;

        *host.home.lock().unwrap() = None;
        homes_become(Vec::new()).await;
        assert!(seat.announced().is_empty());
        tracking.abort();
    }

    /// Stopping the tracker withdraws every folder it registered.
    #[tokio::test]
    async fn stopping_the_tracker_withdraws_its_folders() {
        let seat = RelaySeat::new();
        let registry = Arc::new(Registry::default());
        let (a, b) = (FolderRef::Local(3), FolderRef::Local(4));
        registry
            .0
            .lock()
            .unwrap()
            .extend([DoorHost::new(Some(a)), DoorHost::new(Some(b))]);
        let tracking = {
            let (seat, registry) = (Arc::clone(&seat), Arc::clone(&registry));
            tokio::spawn(async move { seat.track(registry, RESCAN).await })
        };
        announced_becomes(&seat, vec![a.to_wire(), b.to_wire()]).await;
        tracking.abort();
        announced_becomes(&seat, Vec::new()).await;
    }

    /// One announce names at most the wire's cap, own and foreign folders
    /// counted together.
    #[tokio::test]
    async fn the_announce_cap_counts_own_and_foreign_folders_together() {
        let seat = RelaySeat::new();
        let mut inboxes = Vec::new();
        for i in 0..SERVE_ANNOUNCE_MAX_FOLDERS as i64 {
            inboxes.push(seat.register(&FolderRef::Local(i), None));
        }
        inboxes.push(seat.register(&FolderRef::Foreign([0xcd; 32]), Some("https://h.example")));
        assert_eq!(seat.announced().len(), SERVE_ANNOUNCE_MAX_FOLDERS);
    }

    /// The constant bounds the serves in flight, and the queue behind it
    /// drains as they finish.
    #[tokio::test]
    async fn serves_in_flight_never_exceed_the_constant() {
        let seat = RelaySeat::new();
        let folder = FolderRef::Local(7);
        let mut inbox = seat.register(&folder, None);
        // A literal, not the constant: the bar must not move with its subject.
        const ASKS: u64 = 12;
        for id in 0..ASKS {
            assert!(seat.route(&ask(&folder, id, &"ab".repeat(32))));
        }
        let (now, peak, done) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        let serving = inbox.serve_with(|_ask| async {
            let n = now.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            tokio::task::yield_now().await;
            now.fetch_sub(1, Ordering::SeqCst);
            done.fetch_add(1, Ordering::SeqCst);
        });
        let all_done = async {
            while done.load(Ordering::SeqCst) < ASKS as usize {
                tokio::task::yield_now().await;
            }
        };
        tokio::select! {
            _ = serving => unreachable!("serving never ends on its own"),
            _ = all_done => {}
        }
        assert_eq!(
            peak.load(Ordering::SeqCst),
            4,
            "the bound is reached and never passed"
        );
    }
}
