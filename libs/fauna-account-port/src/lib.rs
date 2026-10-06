//! The **account port** — how a web wasm chunk other than the core chunk
//! reaches the tab's account runtime
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*).
//!
//! The runtime's `AccountStoreHandle` lives in the core chunk's linear memory,
//! and every other chunk is its own module with its own memory, so no Rust
//! value crosses between them. What crosses is a **call, as bytes, through one
//! JS method** (decision (a)): a door name, the call's arguments as canonical
//! DAG-CBOR, and its return the same way. A door is one method of a consumer
//! seam — a trait the consumer's own crate declares — never a bare handle
//! method (decision (b)); both halves of a seam's crossing, the forwarder and
//! `serve`, live in a `port` module beside that trait (decision (c)).
//!
//! This crate is what every seam shares, and it names neither the plane nor
//! any seam, so a consumer chunk links no store code:
//!
//! - [`PortTransport`] — the one-method transport a forwarder calls through;
//! - [`PortFault`] — every way a crossing fails, which a forwarder turns into
//!   its seam method's own failure value, never a success (decision (f));
//! - [`forward`] and [`answer`] — the encode → call → decode and
//!   decode → run → encode halves a seam's `port` module is built from;
//! - on wasm32, [`JsAccountTransport`] over the SPA's `SharedAccountPort`
//!   (the TypeScript interface this crate declares), and the tagged-object
//!   form a fault crosses JS as ([`PortFault::to_js`] / [`PortFault::from_js`]);
//! - behind `test-helpers`, [`loopback::Loopback`], which feeds a `serve`
//!   directly — the native proof of a seam's crossing, with no browser.
//!
//! The port is not a wire surface, not an at-rest one, and not a principal
//! (decision (g)): both ends are one build served together, so its byte shapes
//! carry no compatibility duty — a strict decode fails two mismatched builds
//! closed — and it carries arguments, never authority.

use std::fmt;
use std::future::Future;

use serde::Serialize;
use serde::de::DeserializeOwned;

#[cfg(target_arch = "wasm32")]
mod js;
#[cfg(target_arch = "wasm32")]
pub use js::{JsAccountPort, JsAccountTransport};

#[cfg(any(test, feature = "test-helpers"))]
pub mod loopback;

/// Every way a crossing fails (decision (f)). A forwarder answers each one
/// with its seam method's own refusal; none is ever read as a success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortFault {
    /// No runtime serves this account — none runs in the tab, or the one that
    /// runs serves another account (decision (e)).
    NoRuntime(String),
    /// The core side knows no door by this name.
    UnknownDoor(String),
    /// The bytes did not encode or did not decode — two chunks from different
    /// builds fail here, closed.
    Codec(String),
    /// The JS glue between the chunks failed: a method threw, or a promise
    /// rejected with a value this crate did not write.
    Glue(String),
}

impl fmt::Display for PortFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRuntime(why) => write!(f, "account port: no runtime for this account: {why}"),
            Self::UnknownDoor(door) => write!(f, "account port: unknown door `{door}`"),
            Self::Codec(why) => write!(f, "account port: codec: {why}"),
            Self::Glue(why) => write!(f, "account port: glue: {why}"),
        }
    }
}

impl std::error::Error for PortFault {}

/// The transport a seam's forwarder calls through: one door, canonical bytes
/// in, canonical bytes out. On web it is the SPA's `SharedAccountPort`
/// ([`JsAccountTransport`]); in a native test, a loopback into `serve`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait PortTransport: fauna_core::MaybeSendSync {
    async fn call(&self, door: &'static str, payload: Vec<u8>) -> Result<Vec<u8>, PortFault>;
}

/// Encode `args` canonically.
pub fn encode<T: Serialize>(args: &T) -> Result<Vec<u8>, PortFault> {
    fauna_protocol::encode_canonical(args)
        .map(|b| b.to_vec())
        .map_err(|e| PortFault::Codec(format!("encode: {e}")))
}

/// Decode canonical bytes strictly.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, PortFault> {
    fauna_protocol::decode_strict(bytes).map_err(|e| PortFault::Codec(format!("decode: {e}")))
}

/// A forwarder's half: encode `args`, call `door`, decode the reply.
pub async fn forward<Args, Reply>(
    transport: &dyn PortTransport,
    door: &'static str,
    args: &Args,
) -> Result<Reply, PortFault>
where
    Args: Serialize,
    Reply: DeserializeOwned,
{
    let payload = encode(args)?;
    let reply = transport.call(door, payload).await?;
    decode(&reply)
}

/// A `serve`'s half: decode the door's arguments, run the seam method, encode
/// what it answered — the method's own failure arm included, which crosses as
/// data, not as a fault.
pub async fn answer<Args, Reply, F, Fut>(payload: &[u8], run: F) -> Result<Vec<u8>, PortFault>
where
    Args: DeserializeOwned,
    Reply: Serialize,
    F: FnOnce(Args) -> Fut,
    Fut: Future<Output = Reply>,
{
    let args: Args = decode(payload)?;
    encode(&run(args).await)
}

// Native: the codec halves need no browser (`js`'s own tests run under
// `wasm-pack test`).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn a_reply_round_trips_canonically() {
        let bytes = encode(&("row".to_string(), Some(7u64))).unwrap();
        let back: (String, Option<u64>) = decode(&bytes).unwrap();
        assert_eq!(back, ("row".to_string(), Some(7)));
    }

    #[test]
    fn bytes_that_do_not_decode_are_a_codec_fault() {
        let bytes = encode(&"a string").unwrap();
        assert!(matches!(decode::<u64>(&bytes), Err(PortFault::Codec(_))));
    }

    #[tokio::test]
    async fn answer_encodes_what_the_method_answered() {
        let payload = encode(&(2u64, 3u64)).unwrap();
        let reply = answer(&payload, |(a, b): (u64, u64)| async move { a * b })
            .await
            .unwrap();
        assert_eq!(decode::<u64>(&reply).unwrap(), 6);
    }
}
