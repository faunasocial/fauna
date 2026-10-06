//! Shared test doubles for the typed feature crates that are generic over
//! [`fauna_protocol::RpcRequester`] (`fauna-client-posts`,
//! `fauna-client-spam`, `fauna-client-bridges`, …).
//!
//! # Why this crate exists
//!
//! Every one of those crates pins its wire contract the same way: drive a
//! client method against a fake requester, then assert the exact `kind` string
//! and that the recorded payload round-trips back to the typed request. That
//! needs three pieces — a recording [`RpcRequester`], a way to read back what
//! it recorded, and a way to drive the returned future — and each piece was
//! **hand-copied into 30-odd crates**, byte-identical apart from the reply
//! table. `RpcRequester` is a live seam (it has already churned once, over
//! `async fn in trait` vs. `#[async_trait]`); with the double copied per crate,
//! any future change to that signature is a 30-site edit whose sites are found
//! only by grep. Here it is one.
//!
//! What genuinely differs per crate is the **reply table** — the `match kind`
//! that answers with a minimal valid instance of that crate's reply type — so
//! that is the one thing a caller supplies ([`ReplyTable`]).
//!
//! # Why a separate crate rather than a feature on `fauna-protocol`
//!
//! Nothing depends on this crate normally — it is a `[dev-dependencies]` entry
//! everywhere — so test scaffolding cannot reach a release artifact by
//! construction, no compile-time gate required (the same posture as
//! `docs/goal/architecture/e2e-conventions.md` § point 15, arrived at
//! structurally instead of by `cfg`).
//!
//! # Dependency posture: no runtime, wasm-clean
//!
//! The consuming crates keep their dev-deps to `serde` alone so their tests
//! run on **every** target including wasm32, with no async runtime. This crate
//! holds that line: [`block_on`] is a single-poll executor rather than a tokio
//! dependency, and the deps are `fauna-protocol` + `serde` + `serde_json`
//! ([`CapturingRequester`]'s inspectable payload — pure and runtime-free). Do
//! not add a runtime here — a double that needs one would silently make ~30
//! crates non-wasm-testable. The test is *runtime-free and wasm-clean*, not
//! *short*: a pure dep a consuming crate already carries costs nothing.

use std::sync::Mutex;

use fauna_protocol::{RpcError, RpcErrorClass, RpcRequester, Value};

/// A crate's reply table: given the request `kind`, return the **encoded**
/// bytes of a reply that the caller's `Reply` type decodes.
///
/// A plain `fn` pointer, not a generic `F: Fn(..)`, on purpose: every real
/// table is a pure `match kind` that captures nothing, and keeping the pointer
/// non-generic keeps [`RecordingRequester`] a **nameable** type. Test helpers
/// routinely write `Arc<RecordingRequester>` in their own signatures, which a
/// closure type parameter would make unspellable.
pub type ReplyTable = fn(&'static str) -> Vec<u8>;

/// An [`RpcRequester`] that records the last `(kind, payload)` it was asked to
/// send and answers from a caller-supplied [`ReplyTable`].
///
/// `Mutex` (not `RefCell`) so `Arc<RecordingRequester>` stays `Send + Sync`,
/// matching the `Arc<T: RpcRequester>` blanket impl the seam provides.
pub struct RecordingRequester {
    last: Mutex<Option<(&'static str, Vec<u8>)>>,
    /// Every `kind` sent, in order — the assertion surface for a caller whose
    /// contract is a *sequence* rather than a single request.
    ///
    /// [`Self::recorded`] answers "what did the one call look like", which is
    /// what a single-RPC client method wants. An orchestration whose whole
    /// safety property is which leg runs before which (the post-succession
    /// aftermath is the first in shared Rust) needs the other question, and a
    /// last-call-only double cannot answer it: a leg that ran too early is
    /// invisible once a later one overwrites the slot.
    kinds: Mutex<Vec<&'static str>>,
    reply: ReplyTable,
}

impl RecordingRequester {
    /// Build a double answering from `reply`.
    ///
    /// The conventional table shape — matching what it replaced in every crate
    /// — panics on an unexpected kind, so a method that starts sending a kind
    /// the table does not know fails loudly instead of decoding garbage:
    ///
    /// ```ignore
    /// fn reply(kind: &'static str) -> Vec<u8> {
    ///     match kind {
    ///         "fauna.spam.get_preferences" => {
    ///             fauna_protocol::encode_canonical(&SpamPreferences { .. })
    ///         }
    ///         other => panic!("RecordingRequester: unhandled kind {other}"),
    ///     }
    ///     .expect("encode reply")
    ///     .to_vec()
    /// }
    /// ```
    pub fn new(reply: ReplyTable) -> Self {
        Self {
            last: Mutex::new(None),
            kinds: Mutex::new(Vec::new()),
            reply,
        }
    }

    /// The recorded `(kind, payload)`, panicking if no call was made.
    ///
    /// This is the assertion surface: callers decode `payload` back into the
    /// typed request and compare, which is what pins the wire contract.
    pub fn recorded(&self) -> (&'static str, Vec<u8>) {
        self.last
            .lock()
            .unwrap()
            .clone()
            .expect("a call was recorded")
    }

    /// The recorded `(kind, payload)`, or `None` if no call was made — for the
    /// tests that assert a method sends *nothing* on some path.
    pub fn last(&self) -> Option<(&'static str, Vec<u8>)> {
        self.last.lock().unwrap().clone()
    }

    /// Every `kind` sent so far, in call order. Empty when nothing was sent.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.kinds.lock().unwrap().clone()
    }
}

impl RpcRequester for RecordingRequester {
    type Error = core::convert::Infallible;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
        *self.last.lock().unwrap() = Some((kind, bytes.to_vec()));
        self.kinds.lock().unwrap().push(kind);
        let reply = (self.reply)(kind);
        Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
    }
}

/// An [`RpcRequester`] that only proves a client type compiles over the
/// generic transport — a construction smoke test, never actually called.
///
/// Was hand-copied into 23 crates (byte-identical apart from the panic
/// message) alongside [`RecordingRequester`], for the same "one seam, one
/// double" reason: a client whose only test coverage is via
/// [`RecordingRequester`] still wants a cheap `fn constructor_builds_over_
/// generic_requester()` that needs no reply table at all.
pub struct MockRequester;

impl RpcRequester for MockRequester {
    type Error = core::convert::Infallible;

    async fn request<Req, Reply>(
        &self,
        _kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        unreachable!("mock requester is construction-only")
    }
}

/// Single-poll executor: [`RecordingRequester`] has no real suspension point,
/// so the wrapper future is `Ready` on the first poll — which keeps consuming
/// crates free of an async runtime (see the crate docs). A parked future means
/// a test bug, so it panics rather than blocking forever.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};

    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("recording-mock future parked unexpectedly"),
    }
}

/// An [`RpcRequester`] that answers a **scripted sequence** of replies — the
/// double for a contract whose whole point is what happens across successive
/// calls of the *same* kind.
///
/// [`RecordingRequester`]'s [`ReplyTable`] is keyed on `kind` alone, so it
/// necessarily answers identically every time. That is right for a
/// single-round-trip client method, and it cannot express the two shapes a
/// cursor-paging contract is made of: an RFC 6578 sync-collection poll
/// (`fauna.bridges.sync_calendar_since`) returns `more: true` with a fresh
/// `new_sync_token` and expects to be *called again* with it, and
/// `fauna.bridges.query_events` pages the same way. A last-reply-wins double
/// makes the second page unreachable, so a paging loop tested against it can
/// only ever prove its first iteration — including, silently, a loop that
/// never terminates in production.
///
/// Replies are consumed in order; the recorded [`Self::kinds`] are the
/// assertion surface for "which calls, in what order", and [`Self::payloads`]
/// gives every request body so a caller can pin what it *sent* on each turn
/// (that the second poll carried the first reply's token, say). Running out of
/// scripted replies panics rather than repeating the last one: a call the
/// script did not anticipate is a test bug, and repeating would hide it behind
/// a plausible answer.
pub struct ScriptedRequester {
    replies: Mutex<std::collections::VecDeque<Vec<u8>>>,
    kinds: Mutex<Vec<&'static str>>,
    payloads: Mutex<Vec<Vec<u8>>>,
}

impl ScriptedRequester {
    /// Build a double answering `replies` in order, each already encoded (the
    /// same `fauna_protocol::encode_canonical(&reply)` bytes a [`ReplyTable`]
    /// arm returns).
    pub fn new(replies: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().collect()),
            kinds: Mutex::new(Vec::new()),
            payloads: Mutex::new(Vec::new()),
        }
    }

    /// Every `kind` sent so far, in call order.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.kinds.lock().unwrap().clone()
    }

    /// Every request payload sent so far, in call order — decode one back into
    /// the typed request to assert what a given turn actually asked for.
    pub fn payloads(&self) -> Vec<Vec<u8>> {
        self.payloads.lock().unwrap().clone()
    }

    /// How many scripted replies remain unconsumed. `0` after a caller has
    /// taken exactly the scripted number of turns, which is what pins "the
    /// loop stopped when it was supposed to".
    pub fn remaining(&self) -> usize {
        self.replies.lock().unwrap().len()
    }
}

impl RpcRequester for ScriptedRequester {
    type Error = core::convert::Infallible;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
        self.kinds.lock().unwrap().push(kind);
        self.payloads.lock().unwrap().push(bytes.to_vec());
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("ScriptedRequester: no scripted reply left for {kind}"));
        Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
    }
}

/// A transport that fails every request, so a caller's *fail-safe* branch can
/// be pinned rather than argued about.
///
/// [`ScriptedRequester`] and [`MockRequester`] both declare
/// `type Error = Infallible`, which is exactly right for asserting what a
/// client method does with a *reply* — and structurally unable to express the
/// question "what does this caller conclude when the round trip does not
/// happen at all". That question has a wrong answer that is easy to write and
/// nearly invisible in review: an `if let Ok(reply) = …` whose implicit `else`
/// means "nothing changed". Anywhere a transport error must NOT be read as a
/// negative answer — an unchanged calendar, an empty inbox, a revoked grant
/// that still looks granted — this double is the red-verifiable witness.
///
/// The error text is caller-supplied so a test can also assert it survived
/// into whatever the caller renders.
pub struct FailingRequester {
    message: String,
    kinds: Mutex<Vec<&'static str>>,
}

impl FailingRequester {
    /// Build a transport whose every request fails with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kinds: Mutex::new(Vec::new()),
        }
    }

    /// Every `kind` attempted so far, in call order — a failing transport is
    /// still worth asking "did the caller even try, and how many times".
    pub fn kinds(&self) -> Vec<&'static str> {
        self.kinds.lock().unwrap().clone()
    }
}

/// The error [`FailingRequester`] returns. `Display` only, matching
/// [`RpcRequester::Error`]'s bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedRequest(pub String);

impl core::fmt::Display for FailedRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl RpcRequester for FailingRequester {
    type Error = FailedRequest;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.kinds.lock().unwrap().push(kind);
        Err(FailedRequest(self.message.clone()))
    }
}

/// The error a double returns when the *classification* is what a test is
/// about: did the nest **answer with a typed error**, or did the request never
/// get an answer at all?
///
/// This is the only testkit error implementing [`RpcErrorClass`], and that is
/// the whole point. [`FailedRequest`] is `Display`-only, so a client whose
/// contract is "surface a rejection to the user, but retry a transport fault"
/// cannot be tested against it — and every crate that needed the distinction
/// therefore hand-rolled its own two-variant error plus the same
/// [`RpcErrorClass`] impl. `fauna-launch-machine` and `fauna-protocol` grew
/// byte-identical copies (down to the `"rpc: {code}"` / `"transport: {s}"`
/// formatting); `fauna-media-machine` grew an isomorphic `Option<RpcError>`
/// newtype. The classification rule — **a rejection is exactly an answer
/// carrying an [`RpcError`]** — is a wire-contract claim
/// (`docs/goal/architecture/transport.md`), so it is stated once, here.
#[derive(Debug, Clone, PartialEq)]
pub enum ClassifiedError {
    /// The nest answered, and the answer was a typed error. `is_rejection()`.
    Rejected(RpcError),
    /// No answer: the request never completed. Not a rejection.
    Transport(String),
}

impl core::fmt::Display for ClassifiedError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ClassifiedError::Rejected(e) => write!(f, "rpc: {}", e.code),
            ClassifiedError::Transport(s) => write!(f, "transport: {s}"),
        }
    }
}

impl RpcErrorClass for ClassifiedError {
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            ClassifiedError::Rejected(e) => Some(e),
            ClassifiedError::Transport(_) => None,
        }
    }

    fn is_rejection(&self) -> bool {
        self.as_rpc_error().is_some()
    }
}

/// An [`RpcRequester`] answering a per-kind table whose entries may be
/// **rejections**, so one test can walk a client down both branches of its
/// error handling.
///
/// The three doubles that can fail differ in *how*, and the difference is the
/// reason each exists: [`FailingRequester`] fails **every** request at the
/// transport level (does the caller give up cleanly?); this one fails only the
/// kinds a test names, with a typed [`RpcError`] (does the caller tell a
/// rejection from a fault?); [`RecordingRequester`] cannot fail at all.
///
/// An **unmapped** kind is a [`ClassifiedError::Transport`], deliberately: a
/// client that starts sending a kind the table never anticipated should fail
/// as "no answer", not silently receive someone else's reply. (Contrast
/// [`RecordingRequester`], whose table panics — there, an unmapped kind cannot
/// mean anything else, since it has no error channel at all.)
pub struct RejectingRequester {
    responses: std::collections::HashMap<&'static str, Result<Value, RpcError>>,
    kinds: Mutex<Vec<&'static str>>,
}

impl RejectingRequester {
    /// An empty table — every kind is an unmapped transport fault until
    /// [`Self::reply`] or [`Self::reject`] names it.
    pub fn new() -> Self {
        Self {
            responses: std::collections::HashMap::new(),
            kinds: Mutex::new(Vec::new()),
        }
    }

    /// Answer `kind` with `value`, encoded the way the real connectors encode
    /// it, so a reply-shape break surfaces as a decode failure in the test
    /// rather than passing.
    pub fn reply<T: serde::Serialize>(mut self, kind: &'static str, value: &T) -> Self {
        let bytes = fauna_protocol::encode_canonical(value).expect("encode canned reply");
        let v = fauna_protocol::decode_strict(&bytes).expect("canned reply as Value");
        self.responses.insert(kind, Ok(v));
        self
    }

    /// Answer `kind` with a typed rejection.
    pub fn reject(mut self, kind: &'static str, error: RpcError) -> Self {
        self.responses.insert(kind, Err(error));
        self
    }

    /// Every `kind` requested so far, in call order.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.kinds.lock().unwrap().clone()
    }
}

impl Default for RejectingRequester {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcRequester for RejectingRequester {
    type Error = ClassifiedError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        _payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.kinds.lock().unwrap().push(kind);
        match self.responses.get(kind) {
            Some(Ok(v)) => {
                let bytes = fauna_protocol::encode_canonical(v).expect("encode canned value");
                Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
            }
            Some(Err(e)) => Err(ClassifiedError::Rejected(e.clone())),
            None => Err(ClassifiedError::Transport(format!("no mock for {kind}"))),
        }
    }
}

/// An [`RpcRequester`] that records **every** request's `(kind, payload)` in
/// an inspectable form and never answers — the double for a seam whose
/// contract is *how it builds the request*, not what it does with a reply.
///
/// It is the fifth failing/recording double here, and the distinction that
/// earns it a place is the payload: [`RecordingRequester`] keeps only the
/// **last** call and keeps it **canonically encoded**, so the assertion it
/// supports is "decode this back into the typed request and compare". That
/// cannot see the property these callers are pinning — that a *typed* field
/// rides the wire in a particular serialized shape (a `RetentionPolicy` that
/// must serialize as an opaque JSON **string** rather than a nested object,
/// say). Round-tripping through the typed request is blind to it by
/// construction: both shapes decode back to the same value. So this double
/// captures [`serde_json::Value`], which shows the shape, and it captures
/// every call rather than the last, so a seam that sends an extra request
/// cannot hide behind an overwritten slot.
///
/// Every request fails, with a caller-supplied [`ClassifiedError`], so `Reply`
/// is never constructed and the same double drives both error branches: a
/// [`ClassifiedError::Transport`] fault and a typed
/// [`ClassifiedError::Rejected`] refusal.
///
/// Hand-copied — struct, error type, doc comment and impl alike — into
/// `fauna-devices-machine`, `fauna-folders-machine`,
/// `fauna-labeler-catalog-machine` and `fauna-media-machine`, one per
/// `nest_api/ws_rpc.rs`; one of the four had already been lifted onto
/// [`ClassifiedError`] while the other three kept their own `TestError`.
///
/// Held behind an [`Arc`](std::sync::Arc) so the test keeps a handle after the
/// seam takes ownership — `RpcRequester` is implemented for `Arc<T>`:
///
/// ```ignore
/// let req = Arc::new(CapturingRequester::transport_fault());
/// let seam = WsRpcFolderNest::new(Arc::clone(&req));
/// let _ = seam.do_create(create_req()).await;
/// let (kind, payload) = &req.calls()[0];
/// ```
pub struct CapturingRequester {
    captured: Mutex<Vec<(&'static str, serde_json::Value)>>,
    err: ClassifiedError,
}

impl CapturingRequester {
    /// A double whose every request fails with `err`.
    pub fn new(err: ClassifiedError) -> Self {
        Self {
            captured: Mutex::new(Vec::new()),
            err,
        }
    }

    /// The transport-fault case: no answer ever arrives, so `is_rejection()`
    /// is false and the caller's "retry a fault" branch is the one exercised.
    ///
    /// The message matches what the four hand-rolled copies rendered, so the
    /// detail a caller folds out of `Display` is unchanged by adopting this.
    pub fn transport_fault() -> Self {
        Self::new(ClassifiedError::Transport("test transport error".into()))
    }

    /// The rejection case: the nest answered with `error`, so the caller's
    /// code-mapping arms are the ones exercised.
    pub fn rejecting(error: RpcError) -> Self {
        Self::new(ClassifiedError::Rejected(error))
    }

    /// [`Self::rejecting`] built from a wire `code` and free-form `detail`
    /// via [`RpcError::with_details_text`] — the exact construction three
    /// `*-machine` crates' own `seam_returning` test helper each hand-rolled
    /// identically (`nest_api/ws_rpc.rs`, `fauna-devices-machine` /
    /// `fauna-folders-machine` / `fauna-labeler-catalog-machine`).
    pub fn rejecting_code(code: &str, detail: &str) -> Self {
        Self::rejecting(RpcError::new(code, "error.x").with_details_text(detail))
    }

    /// Every `(kind, payload)` captured so far, in call order.
    pub fn calls(&self) -> Vec<(&'static str, serde_json::Value)> {
        self.captured.lock().unwrap().clone()
    }

    /// Every `kind` sent so far, in call order — the same question
    /// [`RecordingRequester::kinds`] answers, for callers not asserting on
    /// payloads.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.captured
            .lock()
            .unwrap()
            .iter()
            .map(|(k, _)| *k)
            .collect()
    }
}

impl RpcRequester for CapturingRequester {
    type Error = ClassifiedError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.captured.lock().unwrap().push((
            kind,
            serde_json::to_value(&payload).expect("payload as JSON"),
        ));
        Err(self.err.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table that answers every kind with the same encoded `u32`, so the
    /// tests below can drive the double without a wire type.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "test.kind" => fauna_protocol::encode_canonical(&7u32),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn it_records_the_kind_and_payload_and_answers_from_the_table() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let got: u32 = block_on(rec.request("test.kind", 42u32)).unwrap();

        assert_eq!(got, 7, "reply came from the table");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "test.kind");
        assert_eq!(
            fauna_protocol::decode_strict::<u32>(&payload).unwrap(),
            42,
            "the recorded payload round-trips back to the request"
        );
    }

    fn rpc(code: &str) -> RpcError {
        RpcError {
            code: code.into(),
            message: Box::new(fauna_protocol::LocalizedText::new("error.x")),
            details: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_rejecting_table_answers_mapped_kinds_and_records_them() {
        let r = RejectingRequester::new().reply("test.ok", &7u32);
        let got: u32 = block_on(r.request("test.ok", 42u32)).unwrap();
        assert_eq!(got, 7);
        assert_eq!(r.kinds(), vec!["test.ok"]);
    }

    #[test]
    fn a_rejected_kind_classifies_as_a_rejection_carrying_its_rpc_error() {
        let r = RejectingRequester::new().reject("test.no", rpc("fauna.denied"));
        let err = block_on(r.request::<u32, u32>("test.no", 1)).unwrap_err();
        assert!(err.is_rejection(), "an answered typed error IS a rejection");
        assert_eq!(
            err.as_rpc_error().map(|e| e.code.as_str()),
            Some("fauna.denied")
        );
    }

    #[test]
    fn an_unmapped_kind_is_a_transport_fault_not_a_rejection() {
        // The distinction the whole type exists for: a caller that retries
        // transport faults but surfaces rejections must not retry this one.
        let r = RejectingRequester::new();
        let err = block_on(r.request::<u32, u32>("test.absent", 1)).unwrap_err();
        assert!(!err.is_rejection(), "no answer is not a rejection");
        assert_eq!(err.as_rpc_error(), None);
    }

    #[test]
    fn last_is_none_before_any_call() {
        let rec = RecordingRequester::new(reply);
        assert!(rec.last().is_none());
    }

    #[test]
    #[should_panic(expected = "a call was recorded")]
    fn recorded_panics_before_any_call() {
        RecordingRequester::new(reply).recorded();
    }

    #[test]
    #[should_panic(expected = "unhandled kind")]
    fn an_unknown_kind_panics_rather_than_decoding_garbage() {
        let rec = RecordingRequester::new(reply);
        let _: u32 = block_on(rec.request("test.unknown", 1u32)).unwrap();
    }

    /// A nested struct serialized as an opaque JSON **string** and one
    /// serialized as an object decode back to the same typed value, so the
    /// round-trip assertion [`RecordingRequester`] supports cannot tell them
    /// apart. This is the property `CapturingRequester` exists for, and the
    /// one a copy switching to encoded bytes would quietly lose.
    #[test]
    fn a_captured_payload_shows_the_serialized_shape_a_round_trip_hides() {
        #[derive(serde::Serialize)]
        struct Req {
            policy: String,
            secs: u32,
        }

        let req = std::sync::Arc::new(CapturingRequester::transport_fault());
        let _: Result<u32, _> = block_on(RpcRequester::request(
            &req,
            "test.kind",
            Req {
                policy: r#"{"max":5}"#.into(),
                secs: 900,
            },
        ));

        let calls = req.calls();
        assert_eq!(calls.len(), 1);
        let (kind, payload) = &calls[0];
        assert_eq!(*kind, "test.kind");
        assert!(
            payload.get("policy").unwrap().is_string(),
            "the opaque-string shape must survive into the capture"
        );
        assert_eq!(payload.get("secs").unwrap(), 900);
    }

    /// Every call is kept, in order — a last-call-only double would let a seam
    /// that sends an extra request hide behind an overwritten slot.
    #[test]
    fn every_call_is_captured_in_order_not_just_the_last() {
        let req = CapturingRequester::transport_fault();
        let _: Result<u32, _> = block_on(RpcRequester::request(&req, "first", 1u32));
        let _: Result<u32, _> = block_on(RpcRequester::request(&req, "second", 2u32));

        assert_eq!(req.kinds(), vec!["first", "second"]);
        assert_eq!(req.calls()[0].1, serde_json::json!(1));
    }

    /// The two branches a caller's error handling has to tell apart. Both come
    /// from the same double, which is why one test can walk both.
    #[test]
    fn a_fault_is_not_a_rejection_and_a_rejection_carries_its_code() {
        let fault = CapturingRequester::transport_fault();
        let err = block_on(RpcRequester::request::<u32, u32>(&fault, "k", 1)).unwrap_err();
        assert!(!err.is_rejection());
        assert!(err.as_rpc_error().is_none());
        // The Display the four hand-rolled copies rendered, unchanged: callers
        // fold it into their `Transient { detail }` arm.
        assert_eq!(err.to_string(), "transport: test transport error");

        let rejecting =
            CapturingRequester::rejecting(RpcError::new("fauna.folders.conflict", "error.x"));
        let err = block_on(RpcRequester::request::<u32, u32>(&rejecting, "k", 1)).unwrap_err();
        assert!(err.is_rejection());
        assert_eq!(err.as_rpc_error().unwrap().code, "fauna.folders.conflict");
    }
}
