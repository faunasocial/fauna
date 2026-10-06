//! Transport-agnostic RPC dispatcher (L3). Per spec § 1.2.
//!
//! Generic over `S: Stream<Item = Result<Bytes, E>> + Sink<Bytes, Error = E>`.
//! Consumers wrap their L1+L2 substrate (WebSocket, raw-TCP-over-WG,
//! in-process tokio mpsc) as a Bytes stream/sink and hand it to the dispatcher.
//!
//! The dispatcher owns the pending-request table (correlation_id →
//! oneshot sender), allocates correlation_ids, decodes incoming frames,
//! routes Reply → resolve pending future, routes Push → broadcast.

use std::collections::HashMap;
use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::{Either, select};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

use fauna_cbor::Value;

use crate::envelope::{Cancel, Frame, Reply, Request, decode_frame, encode_frame};
use crate::error::{LocalizedText, RpcError};
use crate::kind::KindRegistry;
use crate::push_events::PushEvent;

/// Result of a request; either a typed Reply payload or an error.
pub type RpcResult = Result<Value, RpcError>;

/// The wire code this dispatcher **synthesises** when a call's connection drops
/// before its reply arrives — never a code the nest sends.
///
/// Every client classifies against it to tell a transport drop from a genuine
/// server error, so it is named once here rather than spelled as a literal at
/// each synthesis and each classification site.
pub const DISCONNECTED_CODE: &str = "fauna.protocol.disconnected";

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("transport closed")]
    Closed,
    /// The outbound queue stayed full for the whole of the call's budget, so
    /// the frame never reached the wire.
    ///
    /// Grouped with [`DispatchError::Closed`] rather than surfaced as a
    /// timeout because the distinction that matters to a caller is *was
    /// anything sent* — nothing was, so a re-issue takes a **fresh**
    /// idempotency key. [`TypedRequestError::Disconnected`] is the opposite
    /// case (sent, then lost) and must reuse its key.
    ///
    /// Both directions raise it: an originated Request
    /// ([`RpcDispatcher::request_raw_bounded`]) and a served Reply
    /// ([`RpcDispatcher::send_reply_bounded`]). On the reply direction it is
    /// also the signal that the peer has stopped draining, which
    /// `transport.md` § Backpressure rules a dead connection — the serving
    /// loop stops serving rather than retrying.
    #[error("outbound queue full for the whole deadline")]
    EnqueueTimeout,
    #[error("frame: {0}")]
    Frame(#[from] crate::envelope::FrameError),
    #[error("io: {0}")]
    Io(String),
}

/// Capacity of the dispatcher's bounded outbound queue.
///
/// Named because it is load-bearing in three places at once: it is the
/// backpressure valve on a slow peer, it is the depth a test has to fill to
/// reach the enqueue-blocked window at all, and it is the number a
/// peer-symmetric serve loop sizes its in-flight admission cap against
/// (`fauna_peer_channel::SERVE_MAX_INFLIGHT`) so admission cannot outrun what
/// the outbound queue can drain.
pub const OUTBOUND_CAPACITY: usize = 64;

/// Handle returned from `RpcDispatcher::request_raw`. Drop sends a Cancel.
pub struct RpcCall {
    rx: Option<oneshot::Receiver<RpcResult>>,
    correlation_id: u64,
    cancel_tx: mpsc::UnboundedSender<u64>,
    armed: bool,
}

impl RpcCall {
    pub async fn await_reply(mut self) -> RpcResult {
        let rx = self.rx.take().expect("rx already taken");
        let result = rx.await;
        // Disarm cancel-on-drop only now that we've actually awaited the
        // reply — disarming before the `.await` above would mean a future
        // dropped *while still suspended there* (the exact case cancel-on-drop
        // exists for) never observes `armed == true`, so `Drop` never sends
        // the Cancel frame.
        self.armed = false;
        match result {
            Ok(r) => r,
            Err(_) => Err(RpcError::new(
                DISCONNECTED_CODE,
                "error.protocol.disconnected",
            )),
        }
    }
}

impl Drop for RpcCall {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.cancel_tx.send(self.correlation_id);
        }
    }
}

/// L3 dispatcher. Owns the pending table; spawns a task that drives
/// the inbound stream and dispatches frames to pending oneshots or
/// the push broadcast.
///
/// **Peer-symmetric serving (spec Y2 slice 4 §4.A).** Besides originating
/// requests (outbound `Request` → inbound `Reply`), the driver can surface
/// *inbound* `Request`/`Cancel` frames to a consumer that takes
/// [`Self::inbound_requests`] / [`Self::inbound_cancels`], and that consumer
/// replies via [`Self::send_reply`] over the same outbound sink. The
/// client↔nest case never takes those receivers, so inbound Request/Cancel are
/// dropped exactly as before — the surface is opt-in and additive.
pub struct RpcDispatcher {
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResult>>>>,
    next_corr_id: Arc<AtomicU64>,
    push_tx: broadcast::Sender<PushEvent>,
    out_tx: mpsc::Sender<Frame>,
    cancel_tx: mpsc::UnboundedSender<u64>,
    /// Take-once receiver for inbound peer Requests (peer-symmetric serving).
    /// `None` once taken; never taken in the client↔nest case.
    inbound_req_rx: StdMutex<Option<mpsc::Receiver<Request>>>,
    /// Take-once receiver for inbound peer Cancels (aborts the served handler).
    inbound_cancel_rx: StdMutex<Option<mpsc::Receiver<Cancel>>>,
    /// The kind metadata this dispatcher's requests are described by, attached
    /// once by the client crates via [`RpcDispatcher::set_kind_registry`].
    /// Unset on the federation / peer / sidecar dispatchers, where it should be
    /// (see that method's doc).
    kind_registry: OnceLock<KindRegistry>,
}

impl RpcDispatcher {
    /// Build the dispatcher and its driver future. Consumer provides a
    /// Bytes-shaped stream + sink; the returned future drives it until the
    /// stream closes.
    ///
    /// Returns `(dispatcher, driver)` — the caller **must drive `driver`**
    /// on its own runtime: native via `tokio::spawn(driver)`, wasm via
    /// `wasm_bindgen_futures::spawn_local(driver)`. This keeps L3
    /// runtime-agnostic (no `tokio::spawn` here; spec § 1.9), so the crate
    /// compiles to wasm. There is intentionally no `Send` bound on `S` —
    /// the browser `WebSocket` adapter is `!Send`; native callers re-impose
    /// `Send` simply by handing the future to `tokio::spawn`.
    ///
    /// The driver holds the last sender clone of the push broadcast; the
    /// caller awaits its completion (native: the `JoinHandle` from
    /// `tokio::spawn`) to know the transport closed before dropping the
    /// dispatcher, so the broadcast fully closes (required for
    /// `PushBroker::bridge_from` to terminate naturally).
    ///
    /// **Dropping every dispatcher handle also ends the driver** — gracefully,
    /// after writing whatever is still queued. That is the other half of the
    /// same contract, not a competing one: a caller that awaits the driver
    /// first (the supervisor's shape, above) is unaffected, because the driver
    /// has already exited via the closed transport. What it adds is a way to
    /// hang up from *this* side — a listener refusing a handshake drops its
    /// `Arc`s and the `unauthenticated` reply it queued still reaches the wire,
    /// where `abort()` would race it and usually win. Without this exit the
    /// only alternative to aborting was to detach the driver, which leaves the
    /// task alive for as long as the refused peer holds the socket open.
    pub fn new<S, E>(stream: S) -> (Self, impl Future<Output = ()> + 'static)
    where
        S: futures_util::Stream<Item = Result<Bytes, E>>
            + futures_util::Sink<Bytes, Error = E>
            + Unpin
            + 'static,
        E: std::fmt::Display + 'static,
    {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let next_corr_id = Arc::new(AtomicU64::new(1));
        let (push_tx, _) = broadcast::channel(256);
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(OUTBOUND_CAPACITY);
        let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel::<u64>();
        // Peer-symmetric serving: the driver surfaces inbound Request/Cancel on
        // these bounded channels via `try_send` (never `.await` inside the
        // select — that would stall Reply/Push routing). Drop-on-full is the
        // backpressure valve; un-subscribed dispatchers (clients) simply never
        // drain them, so inbound Request/Cancel are dropped as before.
        let (inbound_req_tx, inbound_req_rx) = mpsc::channel::<Request>(64);
        let (inbound_cancel_tx, inbound_cancel_rx) = mpsc::channel::<Cancel>(64);

        let pending_recv = Arc::clone(&pending);
        let push_send = push_tx.clone();
        let pending_write = Arc::clone(&pending);
        let pending_read = Arc::clone(&pending);
        let driver = async move {
            let (mut sink, mut stream) = stream.split();

            // The write half and the read half are driven CONCURRENTLY — they
            // are not two arms of one `select!`. An arm *body* owns the loop
            // for as long as it runs, so the previous shape, which awaited
            // `sink.send(...)` inside the outbound arm, stopped polling
            // `stream.next()` for exactly as long as the peer refused to
            // accept bytes. Nothing inbound was routed while that lasted, and
            // the substrate evaluates its dead-link deadline only from the
            // Stream side (`fauna_ws_substrate::poll_ws_frames`), so a peer
            // that held its receive window shut without closing the socket
            // pinned this task and its socket with no timer of any kind
            // covering it.
            //
            // Splitting them is sound because `split()`'s BiLock is released
            // between polls, never held across a pending one — the same
            // property that already lets `poll_ws_frames` emit its keepalive
            // Ping from inside a *read* poll.
            let write_half = async {
                loop {
                    let frame = tokio::select! {
                        out_frame = out_rx.recv() => {
                            // `None` = every `RpcDispatcher` handle is gone, so no
                            // further frame can ever be queued: exit. Draining first
                            // is free and load-bearing — `recv()` yields everything
                            // already buffered before it ever yields `None` — which
                            // is what lets a listener hang up by DROPPING its
                            // dispatcher instead of aborting this task, and still
                            // have the reply it just queued reach the wire.
                            //
                            // Before this the arm merely went silent on `None` (a
                            // refutable pattern disables the branch), and the `else`
                            // below could not compensate: `stream.next()` bound any
                            // value, so it was never disabled and `else` was
                            // unreachable while the stream stayed open. The driver
                            // had no exit of its own at all, leaving `abort()` —
                            // which races the queued frame — as a listener's only
                            // way to hang up.
                            let Some(out_frame) = out_frame else { break };
                            out_frame
                        }
                        Some(corr) = cancel_rx.recv() => {
                            Frame::Cancel(Cancel {
                                ty: Cancel::TYPE,
                                correlation_id: corr,
                            })
                        }
                        else => break,
                    };
                    // A Cancel withdraws its pending entry once the frame is on
                    // the wire, so a Reply that crosses it is dropped.
                    let cancelled = match &frame {
                        Frame::Cancel(cancel) => Some(cancel.correlation_id),
                        _ => None,
                    };
                    let bytes = match encode_frame(&frame) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    if sink.send(bytes).await.is_err() {
                        break;
                    }
                    if let Some(corr) = cancelled {
                        pending_write.lock().await.remove(&corr);
                    }
                }
            };

            let read_half = async {
                loop {
                    let next = stream.next().await;
                    match next {
                        Some(Ok(bytes)) => {
                            let frame = match decode_frame(&bytes) {
                                Ok(f) => f,
                                Err(_) => continue,
                            };
                            match frame {
                                Frame::Reply(reply) => {
                                    if let Some(sender) =
                                        pending_read.lock().await.remove(&reply.correlation_id)
                                    {
                                        let result = if reply.ok {
                                            Ok(reply.payload)
                                        } else {
                                            // Decode RpcError from payload bytes.
                                            let err_bytes = match crate::codec::encode_canonical(
                                                &reply.payload,
                                            ) {
                                                Ok(b) => b,
                                                Err(_) => Bytes::new(),
                                            };
                                            let err: RpcError =
                                                fauna_cbor::decode_strict(&err_bytes)
                                                    .unwrap_or_else(|_| RpcError {
                                                        code: "fauna.protocol.malformed_error"
                                                            .into(),
                                                        message: Box::new(LocalizedText::new(
                                                            "error.protocol.malformed",
                                                        )),
                                                        details: None,
                                                        extra: Default::default(),
                                                    });
                                            Err(err)
                                        };
                                        let _ = sender.send(result);
                                    }
                                }
                                Frame::Push(push) => {
                                    let event = PushEvent::from_push(&push.kind, push.payload);
                                    let _ = push_send.send(event);
                                }
                                // Peer-symmetric serving (§4.A): surface inbound
                                // Request/Cancel to a subscribed consumer (the
                                // federation channel). `try_send` so a slow/absent
                                // consumer never stalls the driver — the client↔nest
                                // case has no consumer, so these are simply dropped.
                                Frame::Request(req) => {
                                    let _ = inbound_req_tx.try_send(req);
                                }
                                Frame::Cancel(cancel) => {
                                    let _ = inbound_cancel_tx.try_send(cancel);
                                }
                            }
                        }
                        Some(Err(_)) | None => break,
                    }
                }
            };

            // Either half finishing ends the driver: a transport that has
            // failed in one direction is finished in both, and `out_rx`
            // closing means no frame can ever be queued again. Whichever half
            // is still live is dropped here, which is safe in both directions
            // — the write half only ever pends inside `sink.send`, and the
            // read half inside `stream.next()`.
            let write_half = pin!(write_half);
            let read_half = pin!(read_half);
            select(write_half, read_half).await;

            // Clean up: resolve all pending with disconnected error.
            let mut p = pending_recv.lock().await;
            for (_, sender) in p.drain() {
                let _ = sender.send(Err(RpcError::new(
                    DISCONNECTED_CODE,
                    "error.protocol.disconnected",
                )));
            }
        };

        (
            Self {
                pending,
                next_corr_id,
                push_tx,
                out_tx,
                cancel_tx,
                inbound_req_rx: StdMutex::new(Some(inbound_req_rx)),
                inbound_cancel_rx: StdMutex::new(Some(inbound_cancel_rx)),
                kind_registry: OnceLock::new(),
            },
            driver,
        )
    }

    /// Attach the kind registry this dispatcher's requests are described by.
    ///
    /// Set-once, and **optional by design**: a dispatcher with no registry
    /// emits no `replay_forbidden` hint, which is the *correct* value for the
    /// peer (`fauna.peer.*`) and sidecar (`fauna.relay.*`) channels —
    /// `KindRegistry::full()` is the client's table and deliberately declares
    /// none of those kinds (`bins/fauna-nest/src/rpc_router.rs`, the parity
    /// test's scope note). The federation channel (`fauna.federation.*`) is
    /// registry-*bearing* since 2026-08-01: the nest attaches a table derived
    /// from its own serving `FederationRouter`
    /// (`federation_router.rs::hint_registry` — never an extension of the
    /// client's), because that table has forbid-replay kinds whose hint the
    /// peer's stale-caller warning keys on.
    ///
    /// Callers: the three client crates and the nest's federation channel
    /// (`federation_channel::{dial, serve_listener}`), each once per
    /// dispatcher, right after [`RpcDispatcher::new`]. Attaching it here
    /// rather than passing the hint per call is what stops the hint from being
    /// *forgettable*: there is no per-call-site opt-in to omit, so every
    /// present and future `request_raw` on a registry-bearing dispatcher
    /// carries it. Returns `Err` with the registry back if one was already
    /// attached.
    pub fn set_kind_registry(&self, registry: KindRegistry) -> Result<(), KindRegistry> {
        self.kind_registry.set(registry)
    }

    /// The wire hint for `kind`: `Some(true)` exactly when the attached
    /// registry says the kind is replay-forbidden, `None` otherwise.
    ///
    /// `None` and `Some(false)` are wire-equivalent — the nest reads the field
    /// as `unwrap_or(false)` (`dispatch_core.rs`) — so the hint is emitted only
    /// when it carries information. That keeps it off every read on the hot
    /// path and keeps its *presence* meaningful: a nest seeing a forbid-replay
    /// kind arrive without it is talking to a caller that does not know the
    /// kind's metadata, which is precisely what `dispatch_core.rs`'s "caller
    /// missing replay_forbidden hint" warning is for. Before the registry was
    /// wired into the apps that warning fired for every forbid-replay
    /// request; now it is a stale-client detector.
    fn replay_forbidden_hint(&self, kind: &str) -> Option<bool> {
        self.kind_registry
            .get()?
            .meta(kind)
            .filter(|m| m.forbid_replay)
            .map(|_| true)
    }

    /// Send a Request; returns an `RpcCall` that resolves to the typed reply.
    ///
    /// The enqueue itself is **unbounded** here: on a peer that has stopped
    /// reading, `out_tx` fills and this parks until the driver exits. Callers
    /// with a budget to spend should take [`Self::request_raw_bounded`]
    /// instead — [`Self::request_encoded`] does, which is what makes the
    /// deadline cover the whole call rather than only the reply wait.
    pub async fn request_raw(
        &self,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Value,
        deadline: Option<Duration>,
    ) -> Result<RpcCall, DispatchError> {
        self.request_raw_bounded(
            kind,
            idempotency_key,
            payload,
            deadline,
            std::future::pending(),
        )
        .await
    }

    /// [`Self::request_raw`] with the **enqueue** bounded by `enqueue_budget`.
    ///
    /// The outbound queue is bounded (`OUTBOUND_CAPACITY`), and the driver
    /// empties it only as fast as the peer accepts bytes. A peer that holds
    /// its receive window shut without closing the socket therefore fills the
    /// queue and parks every subsequent enqueue — a window that used to be
    /// covered by **no timer at all**: the caller's deadline backstop is
    /// constructed downstream of this call, and the substrate's dead-link
    /// detection cannot fire while the driver is blocked writing.
    ///
    /// `enqueue_budget` is the same runtime-agnostic future shape
    /// [`Self::request_typed`] takes for its reply wait, and callers pass the
    /// *same* budget to both, so the two races share one clock rather than
    /// granting the call two deadlines. Losing the race drops the send future,
    /// so the frame is never queued: the error is
    /// [`DispatchError::EnqueueTimeout`], a **never-sent** classification, and
    /// the pending entry is withdrawn rather than left to leak.
    pub async fn request_raw_bounded<Budget>(
        &self,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Value,
        deadline: Option<Duration>,
        enqueue_budget: Budget,
    ) -> Result<RpcCall, DispatchError>
    where
        Budget: Future<Output = ()>,
    {
        let correlation_id = self.next_corr_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(correlation_id, tx);

        let req = Request {
            ty: Request::TYPE,
            correlation_id,
            kind: kind.to_string(),
            idempotency_key,
            payload,
            replay_forbidden: self.replay_forbidden_hint(kind),
            deadline_ms: deadline.map(|d| d.as_millis() as u32),
        };
        let enqueue = pin!(self.out_tx.send(Frame::Request(req)));
        let budget = pin!(enqueue_budget);
        let failure = match select(enqueue, budget).await {
            Either::Left((Ok(()), _)) => None,
            Either::Left((Err(_), _)) => Some(DispatchError::Closed),
            Either::Right(((), _)) => Some(DispatchError::EnqueueTimeout),
        };
        if let Some(err) = failure {
            self.pending.lock().await.remove(&correlation_id);
            return Err(err);
        }
        Ok(RpcCall {
            rx: Some(rx),
            correlation_id,
            cancel_tx: self.cancel_tx.clone(),
            armed: true,
        })
    }

    /// Encode `payload`, dispatch it under `kind`, await the reply bounded by
    /// `sleep`, decode the typed `Reply` — the whole typed-request ceremony
    /// every fauna transport performs around [`Self::request_raw`], written
    /// once instead of mirrored per transport.
    ///
    /// `sleep` is the caller's **runtime-agnostic deadline backstop**: a future
    /// that resolves once the local budget is spent. Native callers pass
    /// `tokio::time::sleep(budget)`; the wasm client passes
    /// `gloo_timers::future::TimeoutFuture::new(budget_ms)`, since `tokio::time`
    /// does not exist on `wasm32`; a caller wanting no local backstop at all —
    /// the wire `deadline` being bound enough — passes `std::future::pending()`.
    /// Whichever wins, the reply future is dropped, so [`RpcCall`]'s
    /// cancel-on-drop still sends the `Cancel` frame.
    ///
    /// **It bounds the enqueue as well as the reply wait**, so the guarantee
    /// `transport.md` § Request lifecycle step 5 states — "the deadline bounds
    /// the *whole* call" — holds even against a peer that has stopped reading
    /// its socket and filled the outbound queue. The two outcomes stay
    /// distinguishable, because they differ for the caller: a request that
    /// never reached the wire is `Dispatch(EnqueueTimeout)` and may be
    /// re-issued with a **fresh** idempotency key; one that timed out waiting
    /// for its reply is [`TypedRequestError::Timeout`] and must reuse it.
    ///
    /// `deadline` is the *wire* deadline handed to the nest. Pass the same
    /// budget `sleep` counts down, so the two ends agree on the bound.
    ///
    /// **Deliberately not included:** the native client's wait-for-reconnect
    /// step (`transport.md` § Request lifecycle, step 3). That step chooses
    /// *which* dispatcher to send on, so it sits upstream of any one dispatcher
    /// and only exists where a reconnect supervisor owns the slot; it stays in
    /// `fauna_client::NestClient`, which deducts the time it spent from the
    /// budget it passes here — "the deadline bounds the whole call".
    pub async fn request_typed<Req, Reply, Sleep>(
        &self,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Req,
        deadline: Duration,
        sleep: Sleep,
    ) -> Result<Reply, TypedRequestError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
        Sleep: Future<Output = ()>,
    {
        let payload = encode_payload(&payload)?;
        self.request_encoded(kind, idempotency_key, payload, deadline, sleep)
            .await
    }

    /// [`Self::request_typed`] over an **already-encoded** payload — the half a
    /// caller takes when it must encode ahead of an await of its own, so no
    /// caller-typed value is held across that await. Pair it with
    /// [`encode_payload`]. (`fauna_client::NestClient` is that caller: it
    /// encodes before the wait-for-reconnect step.)
    pub async fn request_encoded<Reply, Sleep>(
        &self,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Value,
        deadline: Duration,
        sleep: Sleep,
    ) -> Result<Reply, TypedRequestError>
    where
        Reply: serde::de::DeserializeOwned,
        Sleep: Future<Output = ()>,
    {
        // One budget, pinned once, spent across *both* races below — the
        // enqueue and then the reply wait. Handing each its own copy would
        // grant the call two deadlines; sharing this one is what makes
        // "the deadline bounds the whole call" literally true.
        let mut budget = pin!(sleep);

        let call = self
            .request_raw_bounded(
                kind,
                idempotency_key,
                payload,
                Some(deadline),
                budget.as_mut(),
            )
            .await
            .map_err(TypedRequestError::Dispatch)?;

        // Race the reply against what is left of the caller's budget. Losing
        // the race drops the reply future, so `RpcCall::drop` sends the Cancel
        // frame.
        let reply = pin!(call.await_reply());
        let reply_value = match select(reply, budget).await {
            Either::Left((Ok(v), _)) => v,
            Either::Left((Err(rpc_err), _)) => {
                // The dispatcher synthesises `DISCONNECTED_CODE` on transport
                // close — distinguish that from a genuine server error.
                if rpc_err.code == DISCONNECTED_CODE {
                    return Err(TypedRequestError::Disconnected);
                }
                // Log the nest's `details` — never surfaced anywhere
                // user-facing (`RpcError::log_operator_details`'s own doc: the
                // ratified contract is *no diagnostic ever reaches
                // error-message*, and every caller's error `Display` feeds
                // exactly that slot).
                rpc_err.log_operator_details(kind);
                return Err(TypedRequestError::Rpc(Box::new(rpc_err)));
            }
            Either::Right(((), _)) => return Err(TypedRequestError::Timeout),
        };

        // Decode the typed Reply from the Value. Round-trip through canonical
        // bytes (the cleanest serde path that exists today).
        let reply_bytes = crate::encode_canonical(&reply_value)
            .map_err(|e| TypedRequestError::Codec(format!("reply→bytes: {e}")))?;
        crate::decode_strict(&reply_bytes)
            .map_err(|e| TypedRequestError::Codec(format!("decode reply: {e}")))
    }

    /// Subscribe to all decoded push events.
    pub fn push_subscriber(&self) -> broadcast::Receiver<PushEvent> {
        self.push_tx.subscribe()
    }

    /// Take the inbound-request receiver for **peer-symmetric serving**
    /// (spec Y2 slice 4 §4.A). The federation channel driver drains this to
    /// dispatch peer-originated Requests and replies via [`Self::send_reply`].
    ///
    /// Take-once: returns `None` if already taken (mpsc is single-consumer).
    /// The client↔nest case never calls this, so inbound Requests are dropped
    /// — existing behavior is unchanged.
    pub fn inbound_requests(&self) -> Option<mpsc::Receiver<Request>> {
        self.inbound_req_rx.lock().unwrap().take()
    }

    /// Take the inbound-cancel receiver for peer-symmetric serving (§4.A).
    /// The serving side drains this to abort the in-flight handler for the
    /// referenced `correlation_id`. Take-once; `None` if already taken.
    pub fn inbound_cancels(&self) -> Option<mpsc::Receiver<Cancel>> {
        self.inbound_cancel_rx.lock().unwrap().take()
    }

    /// Send a `Reply` for a served inbound Request, routed through the **same**
    /// outbound sink as outbound Requests (spec Y2 slice 4 §4.A). The serving
    /// side builds the `Reply` (encoding an `RpcError` into `payload` with
    /// `ok=false` on failure) and hands it here.
    ///
    /// The enqueue is **unbounded** here, exactly as in [`Self::request_raw`]:
    /// `out_tx` is bounded ([`OUTBOUND_CAPACITY`]) and drains only as fast as
    /// the peer accepts bytes, so on a peer that has stopped reading this parks
    /// until the driver exits — and the driver does not exit, because the peer
    /// is still *sending*. `Closed` is therefore not this call's only outcome,
    /// which is what the doc used to imply. **Every serving loop must take
    /// [`Self::send_reply_bounded`] instead**, so a served request cannot spawn
    /// work that never retires; this unbudgeted form
    /// remains for callers that genuinely have no budget to spend.
    pub async fn send_reply(&self, reply: Reply) -> Result<(), DispatchError> {
        self.send_reply_bounded(reply, std::future::pending()).await
    }

    /// [`Self::send_reply`] with the **enqueue** bounded by `enqueue_budget` —
    /// the reply-direction twin of [`Self::request_raw_bounded`], and the form
    /// every peer-symmetric serving loop takes.
    ///
    /// A `Reply` that cannot be enqueued for the whole budget means the peer is
    /// not draining, and `transport.md` § Backpressure rules exactly that case:
    /// *replies cannot be dropped; if the client isn't draining, the connection
    /// is dead*. So losing the race is [`DispatchError::EnqueueTimeout`] — the
    /// frame was **never sent** — and the caller's obligation is to stop serving
    /// that channel, never to drop the Reply and leave the originator to burn
    /// its own deadline.
    ///
    /// `enqueue_budget` is caller-injected for the same reason
    /// [`Self::request_typed`]'s deadline backstop is: this crate is
    /// runtime-agnostic and has no `tokio::time` on `wasm32`.
    pub async fn send_reply_bounded<Budget>(
        &self,
        reply: Reply,
        enqueue_budget: Budget,
    ) -> Result<(), DispatchError>
    where
        Budget: Future<Output = ()>,
    {
        let enqueue = pin!(self.out_tx.send(Frame::Reply(reply)));
        let budget = pin!(enqueue_budget);
        match select(enqueue, budget).await {
            Either::Left((Ok(()), _)) => Ok(()),
            Either::Left((Err(_), _)) => Err(DispatchError::Closed),
            // `select` holds both racing futures by `Pin<&mut _>` — nothing
            // drops *at this match*. What actually makes this a *never-sent*
            // classification is that `enqueue` (and `budget`) are dropped when
            // this **function returns**, a few lines below: the send is
            // abandoned before it ever completes, so the frame was never
            // handed to the queue. Inlining this body into a caller with a
            // longer-lived scope, or hoisting `enqueue` out of a loop, would
            // silently turn "never sent" into "holds a reserved queue slot
            // that outlives this call".
            Either::Right(((), _)) => Err(DispatchError::EnqueueTimeout),
        }
    }
}

/// The typed `Req` → canonical bytes → dag-cbor [`Value`] step every transport
/// runs before [`RpcDispatcher::request_encoded`]. Split out of
/// [`RpcDispatcher::request_typed`] for the caller that must encode ahead of an
/// await of its own.
pub fn encode_payload<Req: serde::Serialize>(payload: &Req) -> Result<Value, TypedRequestError> {
    let req_bytes = crate::encode_canonical(payload)
        .map_err(|e| TypedRequestError::Codec(format!("encode request: {e}")))?;
    crate::decode_strict(&req_bytes)
        .map_err(|e| TypedRequestError::Codec(format!("encode→value: {e}")))
}

/// Why a typed request did not produce a decoded `Reply`.
///
/// Transport-neutral on purpose: each client crate maps these arms onto its own
/// public error type (`NestClientError`, `WsRpcError`, `AnonClientError`,
/// `SidecarDialError`), so the *classification* — which of these five things
/// went wrong — is written once here, while the user-facing vocabulary stays
/// each crate's own.
#[derive(Debug, thiserror::Error)]
pub enum TypedRequestError {
    /// The typed `Req` could not be encoded, or the reply could not be decoded
    /// into the caller's `Reply`. The message names which of the four steps.
    #[error("{0}")]
    Codec(String),
    /// The request never reached the wire — the transport was already closed.
    #[error("{0}")]
    Dispatch(DispatchError),
    /// The connection dropped while the request was outstanding: the
    /// dispatcher's synthesised [`DISCONNECTED_CODE`]. The frame *was* sent, so
    /// a caller that re-issues must reuse its idempotency key.
    #[error("disconnected")]
    Disconnected,
    /// A genuine server error (`Reply.ok = false`). Its `details` are already
    /// logged operator-side by the time this is returned; never render them.
    /// Boxed so this arm cannot dominate the size of every `Result` on the
    /// size-sensitive wasm bundle — and so `WsRpcError::Rpc`, boxed for that
    /// same reason, maps across as a move rather than a re-box.
    #[error("{}", .0.localized())]
    Rpc(Box<RpcError>),
    /// The caller's `sleep` backstop won the race before the nest replied.
    #[error("timeout")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Push, Reply};
    use crate::test_transport::{MpscTransport, make_pair, stalled_sink_transport};

    /// **Dropping every dispatcher handle drains what is queued, then ends the
    /// driver** — the graceful shutdown a listener needs in order to reject a
    /// handshake without losing the reason it just sent
    /// (`docs/goal/architecture/transport.md` § Layers, L3 frame lifecycle).
    ///
    /// `send_reply` only *enqueues* onto the bounded `out_tx`; this driver is
    /// what writes it to the sink. Before this, the driver had no exit of its
    /// own: `out_rx.recv()` returning `None` merely disabled that select arm,
    /// and the `stream.next()` arm binds any value so the `else` branch was
    /// unreachable while the stream stayed open. A listener that wanted to hang
    /// up therefore had only `abort()`, which races the queued frame — and the
    /// alternative, detaching the driver, leaks the task for as long as the
    /// PEER keeps an unauthenticated socket open. Both halves matter, so both
    /// are asserted here: the frame lands, **and** the driver finishes.
    #[tokio::test]
    async fn dropping_the_dispatcher_drains_queued_frames_then_ends_the_driver() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        let driver = tokio::spawn(driver);

        // Queue a Reply, then drop the only handle. Nothing aborts the driver.
        dispatcher
            .send_reply(Reply {
                ty: Reply::TYPE,
                correlation_id: 7,
                payload: Value::String("refused".into()),
                ok: false,
            })
            .await
            .expect("the frame enqueues");
        drop(dispatcher);

        // The driver must still write what was already queued...
        let (_srv_sink, mut srv_stream) = server.split();
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(10), srv_stream.next())
            .await
            .expect("the queued frame must reach the wire, not die with the dispatcher")
            .expect("stream open")
            .expect("no transport error");
        match decode_frame(&bytes).unwrap() {
            Frame::Reply(reply) => {
                assert_eq!(reply.correlation_id, 7);
                assert!(!reply.ok, "the queued frame is the one that was enqueued");
            }
            other => panic!("expected the queued Reply, got {other:?}"),
        }

        // ...and then exit on its own, without the peer having to close the
        // socket. The budget is a generous ceiling on a state transition that
        // is already complete, not a settle-sleep: `out_tx` is closed, so the
        // only correct outcome is a prompt exit.
        tokio::time::timeout(std::time::Duration::from_secs(10), driver)
            .await
            .expect("the driver must exit once no handle can queue another frame")
            .expect("driver task did not panic");
    }

    #[tokio::test]
    async fn request_reply_round_trip() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        // Server side: read one Request, send one Reply.
        let (mut srv_sink, mut srv_stream) = server.split();
        let server_task = tokio::spawn(async move {
            let bytes = srv_stream.next().await.unwrap().unwrap();
            let frame = decode_frame(&bytes).unwrap();
            match frame {
                Frame::Request(req) => {
                    let reply = Frame::Reply(Reply {
                        ty: Reply::TYPE,
                        correlation_id: req.correlation_id,
                        payload: Value::String("ok".into()),
                        ok: true,
                    });
                    let bytes = encode_frame(&reply).unwrap();
                    let _ = srv_sink.send(bytes).await;
                }
                _ => panic!("expected Request"),
            }
        });

        let call = dispatcher
            .request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None)
            .await
            .unwrap();
        let result = call.await_reply().await.unwrap();
        assert_eq!(result, Value::String("ok".into()));
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn duplex_peer_symmetric_round_trip() {
        // Two dispatchers over one duplex: A originates a Request; B serves it
        // via the new peer-symmetric surface (inbound_requests + send_reply).
        // Proves the dispatcher is fully bidirectional (§4.A).
        let (a, b) = make_pair();
        let (disp_a, driver_a) = RpcDispatcher::new(a);
        let (disp_b, driver_b) = RpcDispatcher::new(b);
        tokio::spawn(driver_a);
        tokio::spawn(driver_b);

        let disp_b = Arc::new(disp_b);
        let mut b_requests = disp_b
            .inbound_requests()
            .expect("inbound_requests available");
        let disp_b_serve = Arc::clone(&disp_b);
        let serve = tokio::spawn(async move {
            let req = b_requests.recv().await.expect("a peer request");
            assert_eq!(req.kind, "fauna.federation.hello");
            disp_b_serve
                .send_reply(Reply {
                    ty: Reply::TYPE,
                    correlation_id: req.correlation_id,
                    payload: Value::String("pong".into()),
                    ok: true,
                })
                .await
                .unwrap();
        });

        let call = disp_a
            .request_raw("fauna.federation.hello", [0u8; 16], Value::Null, None)
            .await
            .unwrap();
        let reply = call.await_reply().await.unwrap();
        assert_eq!(reply, Value::String("pong".into()));
        serve.await.unwrap();
    }

    #[tokio::test]
    async fn inbound_requests_taken_once() {
        let (client, _server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);
        assert!(dispatcher.inbound_requests().is_some());
        assert!(
            dispatcher.inbound_requests().is_none(),
            "second take must return None"
        );
        assert!(dispatcher.inbound_cancels().is_some());
        assert!(dispatcher.inbound_cancels().is_none());
    }

    #[tokio::test]
    async fn unsubscribed_inbound_request_dropped_client_unaffected() {
        // A dispatcher that never takes inbound_requests() (the client↔nest
        // case) must still drop an unsolicited inbound Request without
        // disturbing its own pending-reply routing — "existing behavior
        // unchanged" guarantee (§4.A).
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        let (mut srv_sink, mut srv_stream) = server.split();
        let call = dispatcher
            .request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None)
            .await
            .unwrap();

        // Server reads the request.
        let bytes = srv_stream.next().await.unwrap().unwrap();
        let req = match decode_frame(&bytes).unwrap() {
            Frame::Request(r) => r,
            other => panic!("expected Request, got {other:?}"),
        };
        // Server sends an UNSOLICITED inbound Request (must be dropped by the
        // un-subscribed client) ...
        let unsolicited = Frame::Request(Request {
            ty: Request::TYPE,
            correlation_id: 999,
            kind: "fauna.federation.hello".into(),
            idempotency_key: [0u8; 16],
            payload: Value::Null,
            replay_forbidden: None,
            deadline_ms: None,
        });
        srv_sink
            .send(encode_frame(&unsolicited).unwrap())
            .await
            .unwrap();
        // ... then the real Reply.
        let reply = Frame::Reply(Reply {
            ty: Reply::TYPE,
            correlation_id: req.correlation_id,
            payload: Value::String("ok".into()),
            ok: true,
        });
        srv_sink.send(encode_frame(&reply).unwrap()).await.unwrap();

        let result = call.await_reply().await.unwrap();
        assert_eq!(result, Value::String("ok".into()));
    }

    #[tokio::test]
    async fn push_event_routes_to_subscribers() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);
        let mut sub = dispatcher.push_subscriber();

        let (mut srv_sink, _srv_stream) = server.split();
        let push = Frame::Push(Push {
            ty: Push::TYPE,
            kind: "fauna.knock".into(),
            payload: fauna_cbor::decode_strict::<Value>(
                &crate::codec::encode_canonical(&crate::push_events::KnockPayload {
                    sender_id: "abc".into(),
                    summary: "hi".into(),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap(),
            seq: 1,
        });
        let bytes = encode_frame(&push).unwrap();
        srv_sink.send(bytes).await.unwrap();

        let event = sub.recv().await.unwrap();
        match event {
            PushEvent::Knock(k) => assert_eq!(k.sender_id, "abc"),
            other => panic!("expected Knock, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn drop_while_awaiting_reply_sends_cancel_frame() {
        use futures_util::future::FutureExt;

        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        let (_srv_sink, mut srv_stream) = server.split();
        let call = dispatcher
            .request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None)
            .await
            .unwrap();

        // Server reads the Request but never replies — the call stays pending.
        let bytes = srv_stream.next().await.unwrap().unwrap();
        let req = match decode_frame(&bytes).unwrap() {
            Frame::Request(r) => r,
            other => panic!("expected Request, got {other:?}"),
        };

        // `now_or_never()` polls the returned future exactly once — it
        // suspends at the internal `rx.await` since no Reply has arrived —
        // then drops it without it ever completing. That is precisely the
        // "future dropped while still awaiting the reply" scenario
        // cancel-on-drop exists for.
        let polled = call.await_reply().now_or_never();
        assert!(polled.is_none(), "must not resolve — server never replied");

        // A Cancel frame for this call's correlation_id must appear on the
        // wire — bounded, so a regression (no Cancel ever sent) fails fast
        // instead of hanging the test forever.
        let bytes = tokio::time::timeout(Duration::from_secs(5), srv_stream.next())
            .await
            .expect("Cancel frame did not arrive on the wire within 5s")
            .expect("stream ended before a Cancel frame arrived")
            .expect("infallible transport");
        match decode_frame(&bytes).unwrap() {
            Frame::Cancel(c) => assert_eq!(c.correlation_id, req.correlation_id),
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    /// Read the `Request` frame a single `request_raw` puts on the wire.
    ///
    /// Asserts on the **encoded frame the server would receive**, not on the
    /// dispatcher's internals: the whole point of the hint is what arrives at
    /// `dispatch_core.rs`, and a test reading a struct field would keep passing
    /// if `Request`'s serializer ever stopped emitting field `5`.
    async fn sent_request(
        dispatcher: &RpcDispatcher,
        server: MpscTransport,
        kind: &str,
    ) -> Request {
        let (_srv_sink, mut srv_stream) = server.split();
        let _call = dispatcher
            .request_raw(kind, [0u8; 16], Value::Null, None)
            .await
            .unwrap();
        let bytes = srv_stream.next().await.unwrap().unwrap();
        match decode_frame(&bytes).unwrap() {
            Frame::Request(r) => r,
            other => panic!("expected Request, got {other:?}"),
        }
    }

    /// The hint is emitted for a forbid-replay kind — the half that stops
    /// `dispatch_core.rs`'s "caller missing replay_forbidden hint" warning.
    #[tokio::test]
    async fn a_forbid_replay_kind_carries_the_wire_hint() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);
        dispatcher.set_kind_registry(KindRegistry::full()).unwrap();

        // `fauna.posts.interact` is declared `forbid_replay = true` on both
        // sides (the nest router and the registry, held in lockstep by
        // `rpc_router::tests::router_and_kind_registry_agree_on_every_kind`).
        let req = sent_request(&dispatcher, server, "fauna.posts.interact").await;
        assert_eq!(
            req.replay_forbidden,
            Some(true),
            "a forbid-replay kind must carry the wire hint"
        );
    }

    /// The other half, and the one that keeps the hint's *presence*
    /// meaningful: a replay-permitted kind must NOT set it. `Some(false)` is
    /// wire-equivalent to absent (the nest reads `unwrap_or(false)`), so
    /// emitting it would put a field on every read for no information.
    #[tokio::test]
    async fn a_replay_permitted_kind_omits_the_wire_hint() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);
        dispatcher.set_kind_registry(KindRegistry::full()).unwrap();

        let req = sent_request(&dispatcher, server, "fauna.protocol.echo").await;
        assert_eq!(
            req.replay_forbidden, None,
            "a replay-permitted kind must not carry the hint"
        );
    }

    /// A kind the registry never declares gets no hint rather than a guess —
    /// the dispatcher must not invent metadata it was not given.
    #[tokio::test]
    async fn an_undeclared_kind_gets_no_hint() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);
        dispatcher.set_kind_registry(KindRegistry::full()).unwrap();

        let req = sent_request(&dispatcher, server, "fauna.not.a.real.kind").await;
        assert_eq!(req.replay_forbidden, None);
    }

    /// The federation / peer / sidecar shape: no registry attached, so no hint
    /// — including for a kind that *would* be forbid-replay if the client
    /// registry described it. This is the behavior that lets those channels
    /// keep their own metadata tables (`rpc_router.rs`'s parity scope note)
    /// instead of being forced into the client's.
    #[tokio::test]
    async fn a_registry_less_dispatcher_sends_no_hint() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        let req = sent_request(&dispatcher, server, "fauna.posts.interact").await;
        assert_eq!(
            req.replay_forbidden, None,
            "no registry attached ⇒ no hint, not a guessed one"
        );
    }

    /// Set-once: a second attach is refused rather than silently swapping the
    /// table a live dispatcher describes its own requests with.
    #[test]
    fn the_registry_attaches_only_once() {
        let (client, _server) = make_pair();
        let (dispatcher, _driver) = RpcDispatcher::new(client);
        assert!(dispatcher.set_kind_registry(KindRegistry::full()).is_ok());
        assert!(dispatcher.set_kind_registry(KindRegistry::full()).is_err());
    }

    // ── the shared typed-request path ────────────────────────────────────────
    //
    // `transport.md` § Request lifecycle steps 4-5. Four client crates
    // (`fauna-client`, `fauna-rpc-wasm`, `fauna-anon-client`,
    // `fauna-sidecar-client`) hand-wrote this ceremony before it moved here, and
    // each mapped its own error type off the *classification* these tests pin.
    // Every deadline below is a generous ceiling on an already-complete state
    // transition; the one test that must observe a lost deadline race arms an
    // already-ready backstop rather than a wall-clock nap (convention 14).

    /// A typed request encodes, dispatches, and decodes the typed reply.
    #[tokio::test]
    async fn request_typed_round_trips_a_typed_payload() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        let server_task = tokio::spawn(async move {
            let (mut sink, mut stream) = server.split();
            let bytes = stream.next().await.unwrap().unwrap();
            let Frame::Request(req) = decode_frame(&bytes).unwrap() else {
                panic!("expected a Request");
            };
            // The typed `Req` arrived as its dag-cbor node, not as bytes.
            assert_eq!(req.payload, Value::String("ping".into()));
            assert_eq!(req.idempotency_key, [7u8; 16], "the caller's key is used");
            let reply = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id: req.correlation_id,
                payload: Value::String("pong".into()),
                ok: true,
            });
            sink.send(encode_frame(&reply).unwrap()).await.unwrap();
        });

        let reply: String = dispatcher
            .request_typed(
                "fauna.protocol.echo",
                [7u8; 16],
                "ping",
                Duration::from_secs(30),
                std::future::pending(),
            )
            .await
            .expect("the typed round trip succeeds");
        assert_eq!(reply, "pong");
        server_task.await.unwrap();
    }

    /// A `Reply { ok: false }` classifies as [`TypedRequestError::Rpc`] — the
    /// server *answered*, so it must never be reported as a transport drop.
    #[tokio::test]
    async fn a_server_error_classifies_as_rpc_not_as_a_drop() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        let server_task = tokio::spawn(async move {
            let (mut sink, mut stream) = server.split();
            let bytes = stream.next().await.unwrap().unwrap();
            let Frame::Request(req) = decode_frame(&bytes).unwrap() else {
                panic!("expected a Request");
            };
            let wire_err = RpcError::new("fauna.test.refused", "error.test.refused");
            let reply = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id: req.correlation_id,
                payload: encode_payload(&wire_err).unwrap(),
                ok: false,
            });
            sink.send(encode_frame(&reply).unwrap()).await.unwrap();
        });

        let err = dispatcher
            .request_typed::<_, Value, _>(
                "fauna.protocol.echo",
                [0u8; 16],
                Value::Null,
                Duration::from_secs(30),
                std::future::pending(),
            )
            .await
            .expect_err("the nest refused");
        match err {
            TypedRequestError::Rpc(e) => assert_eq!(e.code, "fauna.test.refused"),
            other => panic!("a server error must classify as Rpc, got {other:?}"),
        }
        server_task.await.unwrap();
    }

    /// A peer that hangs up with the request outstanding classifies as
    /// [`TypedRequestError::Disconnected`] — the dispatcher synthesises
    /// [`DISCONNECTED_CODE`] for that case, and the shared path must recognise
    /// its own synthesised code rather than passing it on as a server error.
    #[tokio::test]
    async fn a_mid_flight_hangup_classifies_as_disconnected_not_as_a_server_error() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        let server_task = tokio::spawn(async move {
            let (_sink, mut stream) = server.split();
            // Read the Request, then drop both halves without replying.
            let _ = stream.next().await.unwrap().unwrap();
        });

        let err = dispatcher
            .request_typed::<_, Value, _>(
                "fauna.protocol.echo",
                [0u8; 16],
                Value::Null,
                Duration::from_secs(30),
                std::future::pending(),
            )
            .await
            .expect_err("the peer hung up mid-flight");
        assert!(
            matches!(err, TypedRequestError::Disconnected),
            "a transport drop must classify as Disconnected, got {err:?}"
        );
        server_task.await.unwrap();
    }

    /// Losing the deadline race yields [`TypedRequestError::Timeout`] **and**
    /// still sends the `Cancel` frame (§ Cancellation): the drop-cancellation
    /// contract survives the move onto the shared path, because the reply future
    /// is dropped on the losing arm exactly as each hand-written mirror did.
    ///
    /// The backstop is an already-ready future rather than a short nap, so the
    /// race has one outcome by construction and no wall-clock dependence
    /// (convention 14); the server never replies, so the reply arm cannot win.
    #[tokio::test]
    async fn a_lost_deadline_race_times_out_and_cancels_on_the_wire() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);
        let (_srv_sink, mut srv_stream) = server.split();

        let err = dispatcher
            .request_typed::<_, Value, _>(
                "fauna.protocol.echo",
                [0u8; 16],
                Value::Null,
                Duration::from_secs(30),
                std::future::ready(()),
            )
            .await
            .expect_err("the backstop was already ready, so it wins");
        assert!(
            matches!(err, TypedRequestError::Timeout),
            "the backstop winning is a Timeout, got {err:?}"
        );

        // Both frames are already queued by the time the call returns — the
        // Request on the outbound channel, the Cancel on the dispatcher's
        // separate unbounded cancel channel — and the driver's select over the
        // two does not order them, so drain until both have been seen rather
        // than assuming the Request comes first. (A Cancel that overtakes its
        // own Request finds no `pending_handlers` entry nest-side and is a
        // no-op; only a call abandoned within microseconds of being issued, as
        // this test deliberately does, can reach that.)
        let mut request_corr = None;
        let mut cancel_corr = None;
        while request_corr.is_none() || cancel_corr.is_none() {
            let bytes = tokio::time::timeout(Duration::from_secs(10), srv_stream.next())
                .await
                .expect("a timed-out call must put both its Request and its Cancel on the wire")
                .expect("the stream stays open")
                .expect("infallible transport");
            match decode_frame(&bytes).unwrap() {
                Frame::Request(req) => request_corr = Some(req.correlation_id),
                Frame::Cancel(cancel) => cancel_corr = Some(cancel.correlation_id),
                other => panic!("expected only the Request and its Cancel, got {other:?}"),
            }
        }
        assert_eq!(
            cancel_corr, request_corr,
            "the Cancel must name the abandoned call's correlation_id"
        );
    }

    /// A dispatcher whose transport is already gone fails before the wire, as
    /// [`TypedRequestError::Dispatch`] — distinct from `Disconnected`, because
    /// nothing was sent and a caller may re-issue with a fresh key.
    #[tokio::test]
    async fn a_send_onto_a_dead_transport_classifies_as_dispatch() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        let driver = tokio::spawn(driver);
        drop(server);
        // The driver observes the closed transport and exits, closing `out_tx`.
        tokio::time::timeout(Duration::from_secs(10), driver)
            .await
            .expect("the driver exits once the transport is gone")
            .unwrap();

        let err = dispatcher
            .request_typed::<_, Value, _>(
                "fauna.protocol.echo",
                [0u8; 16],
                Value::Null,
                Duration::from_secs(30),
                std::future::pending(),
            )
            .await
            .expect_err("there is no transport to send on");
        assert!(
            matches!(err, TypedRequestError::Dispatch(DispatchError::Closed)),
            "a pre-wire failure is Dispatch, not Disconnected, got {err:?}"
        );
    }

    /// Fill `out_tx` to capacity against a peer that never reads, so the next
    /// enqueue has nowhere to go.
    ///
    /// A stalled peer absorbs exactly `OUTBOUND_CAPACITY + 1` frames before the
    /// first one blocks: one sits in the driver's hand, parked in the sink, and
    /// the rest fill the bounded channel behind it. `already_enqueued` is how
    /// many of those a test has issued for its own purposes already.
    async fn saturate_outbound(
        dispatcher: &RpcDispatcher,
        already_enqueued: usize,
    ) -> Vec<RpcCall> {
        let mut held = Vec::new();
        for _ in 0..(OUTBOUND_CAPACITY + 1 - already_enqueued) {
            held.push(
                tokio::time::timeout(
                    Duration::from_secs(10),
                    dispatcher.request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None),
                )
                .await
                .expect("the queue still has room for this one")
                .expect("the transport is open"),
            );
        }
        held
    }

    /// **A peer that stops reading cannot make a request outlive its declared
    /// deadline** (`docs/goal/architecture/transport.md` § Request lifecycle,
    /// step 5: "the deadline bounds the *whole* call").
    ///
    /// The enqueue onto the bounded `out_tx` used to sit *upstream* of the
    /// deadline backstop: `request_encoded` awaited `request_raw` before it
    /// ever constructed the `select(reply, budget)`, so the caller's `sleep`
    /// was never polled and `Timeout` was unreachable for a request stuck at
    /// the enqueue. A peer holding its receive window shut — without closing
    /// the socket — therefore hung the call forever. The budget now covers the enqueue too, and a request that never
    /// reached the wire is a `Dispatch` error, not `Disconnected`, so a caller
    /// may re-issue it with a **fresh** idempotency key.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_never_reads_cannot_make_a_request_outlive_its_deadline() {
        let (transport, _feed) = stalled_sink_transport();
        let (dispatcher, driver) = RpcDispatcher::new(transport);
        tokio::spawn(driver);

        let _held = saturate_outbound(&dispatcher, 0).await;

        // A generous ceiling on the declared budget, not a settle-sleep: the
        // only correct outcome is that the call resolves at its own deadline.
        let budget = Duration::from_secs(5);
        let outcome = tokio::time::timeout(
            budget * 10,
            dispatcher.request_typed::<_, Value, _>(
                "fauna.protocol.echo",
                [1u8; 16],
                Value::Null,
                budget,
                tokio::time::sleep(budget),
            ),
        )
        .await
        .expect("the call must resolve at its own deadline, not park at the enqueue forever");

        let err = outcome.expect_err("the peer never reads, so no reply can arrive");
        assert!(
            matches!(
                err,
                TypedRequestError::Dispatch(DispatchError::EnqueueTimeout)
            ),
            "a request that never reached the wire is Dispatch, so it may be \
             re-issued with a fresh idempotency key; got {err:?}"
        );
    }

    /// **A reply that loses the enqueue race is genuinely never-sent: nothing
    /// reaches the wire for it, and it must not hold — or leak — a queue slot
    /// of its own** (`docs/goal/architecture/transport.md` § Backpressure).
    ///
    /// `send_reply_bounded`'s `Either::Right` arm depends on a scope detail its
    /// comment used to misname: `select` holds both racing futures by
    /// `Pin<&mut _>`, so nothing drops *at that match* — it is this function
    /// **returning**, a few lines below, that drops `enqueue` and is what
    /// actually makes the classification true. This test pins both observable
    /// halves of that claim directly against the real API rather than trusting
    /// the returned error alone: we hold every permit on the outbound queue
    /// ourselves throughout the race, so the loss is deterministic (no driver
    /// progress to race against), and if the losing `enqueue` had held or
    /// leaked a permit of its own, releasing exactly the permits we reserved
    /// would leave capacity one short.
    #[tokio::test]
    async fn a_reply_that_loses_the_enqueue_race_is_never_sent_and_leaks_no_slot() {
        let (client, server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        // Hold every permit on the outbound queue ourselves, so the losing
        // `enqueue` never gets a real chance at one.
        let mut held = Vec::new();
        for _ in 0..OUTBOUND_CAPACITY {
            held.push(
                dispatcher
                    .out_tx
                    .reserve()
                    .await
                    .expect("the channel is open"),
            );
        }
        assert_eq!(dispatcher.out_tx.capacity(), 0);

        let outcome = dispatcher
            .send_reply_bounded(
                Reply {
                    ty: Reply::TYPE,
                    correlation_id: 999,
                    payload: Value::String("late".into()),
                    ok: true,
                },
                std::future::ready(()),
            )
            .await;
        assert!(
            matches!(outcome, Err(DispatchError::EnqueueTimeout)),
            "a reply that never reached the wire is EnqueueTimeout, not a \
             silent success; got {outcome:?}"
        );

        // (b) capacity intact: releasing exactly the permits we reserved must
        // restore full capacity — the losing send held none of its own.
        drop(held);
        assert_eq!(
            dispatcher.out_tx.capacity(),
            OUTBOUND_CAPACITY,
            "a losing send_reply_bounded must not hold or leak a permit on the \
             outbound queue"
        );

        // (a) never delivered: now that capacity is free, a fresh reply goes
        // through normally — and it, not the one that lost the race, is what
        // reaches the wire.
        dispatcher
            .send_reply_bounded(
                Reply {
                    ty: Reply::TYPE,
                    correlation_id: 1,
                    payload: Value::String("ok".into()),
                    ok: true,
                },
                std::future::pending(),
            )
            .await
            .expect("capacity is free again");

        let (_srv_sink, mut srv_stream) = server.split();
        let bytes = tokio::time::timeout(Duration::from_secs(10), srv_stream.next())
            .await
            .expect("the fresh reply must reach the wire")
            .expect("stream open")
            .expect("no transport error");
        match decode_frame(&bytes).unwrap() {
            Frame::Reply(reply) => assert_eq!(
                reply.correlation_id, 1,
                "the reply that lost the race (999) must never reach the wire, \
                 not even after capacity frees up"
            ),
            other => panic!("expected a Reply, got {other:?}"),
        }
    }

    /// **A *request* that loses the enqueue race is genuinely never-sent, and
    /// must not hold — or leak — a queue slot or a pending-table entry of its
    /// own** — the client-side twin of
    /// [`a_reply_that_loses_the_enqueue_race_is_never_sent_and_leaks_no_slot`],
    /// which only covers the serve-side `send_reply_bounded`. `request_raw_bounded` races the *same* shape
    /// (`dispatcher.rs`'s `select(enqueue, budget)`), and nothing had pinned
    /// its own permit/pending-table hygiene with the same rigor.
    #[tokio::test]
    async fn a_request_that_loses_the_enqueue_race_is_never_sent_and_leaks_no_slot() {
        let (client, _server) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(client);
        tokio::spawn(driver);

        // Hold every permit ourselves, so the losing enqueue never gets a
        // real chance at one — deterministic, no driver progress to race.
        let mut held = Vec::new();
        for _ in 0..OUTBOUND_CAPACITY {
            held.push(
                dispatcher
                    .out_tx
                    .reserve()
                    .await
                    .expect("the channel is open"),
            );
        }
        assert_eq!(dispatcher.out_tx.capacity(), 0);

        let pending_before = dispatcher.pending.lock().await.len();
        let outcome = dispatcher
            .request_raw_bounded(
                "fauna.protocol.echo",
                [7u8; 16],
                Value::Null,
                None,
                std::future::ready(()),
            )
            .await;
        let err = outcome.err();
        assert!(
            matches!(err, Some(DispatchError::EnqueueTimeout)),
            "a request that never reached the wire is EnqueueTimeout, not a \
             silent success; got {err:?}"
        );

        // No pending-table entry leaked: the failure arm must withdraw the
        // entry it inserted before racing.
        assert_eq!(
            dispatcher.pending.lock().await.len(),
            pending_before,
            "a losing request_raw_bounded must not leak a pending-table entry"
        );

        // No permit leaked: releasing exactly the permits we reserved must
        // restore full capacity.
        drop(held);
        assert_eq!(
            dispatcher.out_tx.capacity(),
            OUTBOUND_CAPACITY,
            "a losing request_raw_bounded must not hold or leak a permit on \
             the outbound queue"
        );
    }

    /// **A blocked write must not stall inbound reads.** The driver used to
    /// `sink.send(...).await` inside a `select!` *arm body*, which ends the
    /// select: while that write pended, the `stream.next()` arm was not
    /// polled, so nothing inbound was routed and the substrate's own
    /// liveness check (`fauna_ws_substrate::poll_ws_frames`, which evaluates
    /// the dead-link deadline only from the Stream side) could never fire.
    /// One peer refusing to read therefore pinned a task and its socket with
    /// no timer of any kind covering it.
    ///
    /// Asserting the *observable*: with every write stalled, a Reply arriving
    /// on the wire still resolves its pending call.
    #[tokio::test]
    async fn a_blocked_write_does_not_stall_inbound_reads() {
        let (transport, feed) = stalled_sink_transport();
        let (dispatcher, driver) = RpcDispatcher::new(transport);
        tokio::spawn(driver);

        let call = dispatcher
            .request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None)
            .await
            .expect("the first enqueue has room");
        let correlation_id = call.correlation_id;

        // Saturating first proves the driver is genuinely parked in the sink,
        // not merely between frames.
        let _held = saturate_outbound(&dispatcher, 1).await;

        let reply = encode_frame(&Frame::Reply(Reply {
            ty: Reply::TYPE,
            correlation_id,
            payload: Value::String("ok".into()),
            ok: true,
        }))
        .expect("the reply encodes");
        feed.send(reply).await.expect("the inbound half is open");

        let result = tokio::time::timeout(Duration::from_secs(10), call.await_reply())
            .await
            .expect("a stalled write must not stop the driver reading its socket")
            .expect("the reply is a success");
        assert_eq!(result, Value::String("ok".into()));
    }
}
