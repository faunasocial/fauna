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
//!    none) and whenever an engine registers or leaves.
//! 2. **Route.** The nest's ask, the `fauna.sync.chunk.wanted` push, names the
//!    folder; the seat hands it to that folder's engine and to no other. An ask
//!    for a folder this process did not announce is ignored.
//! 3. **Answer.** [`ServeInbox::serve`] runs beside the engine's own loop and
//!    answers each ask through [`SyncEngine::answer_serve_ask`] — the bytes on
//!    the bulk rail, or a decline — at most [`SERVE_CONCURRENCY`] at a time.
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
//! (`bins/fauna-sync-agent`'s engine driver). A run-and-drop host — an engine
//! built for one pass and dropped — holds no connection for the nest to ask
//! and registers nothing here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_core::folder_keys::FolderRef;
use fauna_protocol::PushEvent;
use fauna_protocol::push_events::SyncChunkWantedPayload;
use fauna_protocol::sync::{
    KIND_SYNC_SERVE_ANNOUNCE, SERVE_ANNOUNCE_MAX_FOLDERS, SyncServeAnnounceReply,
    SyncServeAnnounceRequest,
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

/// One registered run of a folder's engine: its registration id and the
/// queue its asks go to.
type RegisteredRun = (u64, mpsc::Sender<ServeAsk>);

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
    /// A folder whose home is another nest is registered nowhere: its seat
    /// announces through its own nest, which is not built (`file-sync.md`
    /// § Relay serving → *A member on another nest*). Its inbox stays empty.
    pub fn register(self: &Arc<Self>, folder: &FolderRef) -> ServeInbox {
        let (tx, rx) = mpsc::channel(ASK_QUEUE);
        let key = folder.to_wire();
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let local = matches!(folder, FolderRef::Local(_));
        if local {
            let mut folders = self.folders.lock().expect("relay seat registry");
            let runs = folders.entry(key.clone()).or_default();
            let newly_announced = runs.is_empty();
            runs.push((id, tx));
            drop(folders);
            if newly_announced {
                self.changed.notify_one();
            }
        }
        ServeInbox {
            rx,
            _registration: Registration {
                seat: Arc::clone(self),
                key,
                id,
                local,
            },
        }
    }

    /// What the nest admitted of the last announce — a witness's barrier for
    /// "this seat is now asked for the folder".
    pub fn subscribe_admitted(&self) -> watch::Receiver<Vec<String>> {
        self.admitted.subscribe()
    }

    /// The folders to announce: every registered engine's, in a stable order,
    /// capped at what one announce may name (the nest refuses a longer one
    /// whole).
    fn announced(&self) -> Vec<String> {
        let mut folders: Vec<String> = self
            .folders
            .lock()
            .expect("relay seat registry")
            .keys()
            .cloned()
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
        folders
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
        let Some((_, tx)) = folders.get(&ask.folder).and_then(|runs| runs.last()) else {
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
            let (_, tx) = folders.get(folder).and_then(|runs| runs.last())?;
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

async fn announce(
    nest: &fauna_client::NestClient,
    device_id_hex: &str,
    folders: Vec<String>,
) -> Result<Vec<String>, fauna_client::NestClientError> {
    let reply: SyncServeAnnounceReply = nest
        .request(
            KIND_SYNC_SERVE_ANNOUNCE,
            SyncServeAnnounceRequest {
                device_id: device_id_hex.to_string(),
                folders,
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
                    // foreign one). Nothing will ever be asked: finish what is
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
    /// Whether [`RelaySeat::register`] entered it at all (a foreign folder is
    /// never registered).
    local: bool,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if !self.local {
            return;
        }
        let mut folders = self.seat.folders.lock().expect("relay seat registry");
        let Some(runs) = folders.get_mut(&self.key) else {
            return;
        };
        // This run's entry only — never another run of the same folder.
        runs.retain(|(id, _)| *id != self.id);
        if runs.is_empty() {
            folders.remove(&self.key);
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
        let mut inbox_a = seat.register(&a);
        let mut inbox_b = seat.register(&b);
        assert_eq!(seat.announced(), vec![a.to_wire(), b.to_wire()]);

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
        let mut inbox = seat.register(&folder);
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
        let mut inbox = seat.register(&folder);
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
        let first = seat.register(&folder);
        let mut second = seat.register(&folder);
        drop(first);
        assert_eq!(
            seat.announced(),
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
        let mut replacement = seat.register(&folder);
        let late_cancelled = seat.register(&folder);
        drop(late_cancelled);
        assert_eq!(
            seat.announced(),
            vec![folder.to_wire()],
            "the live run is still registered"
        );
        assert!(seat.route(&ask(&folder, 1, &"ab".repeat(32))));
        assert!(
            replacement.rx.try_recv().is_ok(),
            "the ask reaches the run that is still alive"
        );
    }

    /// A never-registered inbox pends rather than ending its caller's race.
    #[tokio::test]
    async fn serving_a_foreign_folder_never_returns() {
        let seat = RelaySeat::new();
        let mut inbox = seat.register(&FolderRef::Foreign([0xcd; 32]));
        let served =
            tokio::time::timeout(Duration::from_millis(50), inbox.serve_with(|_ask| async {}))
                .await;
        assert!(served.is_err(), "serving must pend, never return");
    }

    /// A folder homed on another nest is not announced here.
    #[tokio::test]
    async fn a_foreign_folder_is_not_registered() {
        let seat = RelaySeat::new();
        let _inbox = seat.register(&FolderRef::Foreign([0xcd; 32]));
        assert!(seat.announced().is_empty());
    }

    /// The constant bounds the serves in flight, and the queue behind it
    /// drains as they finish.
    #[tokio::test]
    async fn serves_in_flight_never_exceed_the_constant() {
        let seat = RelaySeat::new();
        let folder = FolderRef::Local(7);
        let mut inbox = seat.register(&folder);
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
