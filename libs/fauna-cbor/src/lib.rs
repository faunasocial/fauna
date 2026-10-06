//! Canonical IPLD-dag-cbor codec, CID type, and sign-over-CID envelope.
//!
//! See `docs/goal/architecture/serialization.md` for the contract.

#[cfg(debug_assertions)]
mod byte_array_guard;
mod canonical;
pub mod carried;
pub mod cid;
pub mod codec;
pub mod envelope;
pub mod error;

pub use carried::CarriedValue;
pub use cid::Cid;
pub use codec::{decode_strict, encode_canonical, encode_with_cid};
pub use envelope::SignedEnvelope;
pub use error::{DecodeError, EncodeError, VerifyError};

/// Generic dag-cbor node — the canonical generic value type for the
/// kind-agnostic positions in the protocol: the WS-RPC payload seam
/// (`Request`/`Reply`/`Push.payload`), forward-compat `extra` side-channel
/// maps, the `Unknown` fallthrough payload, and `RpcError.details`.
///
/// Re-export of `ipld_core::Ipld` — the value type `serde_ipld_dagcbor`
/// natively round-trips (incl. tag-42 CID links as [`Value::Link`]). Replaces
/// the former `ciborium::value::Value` (the CBOR-DAG-everywhere Layer-4 flip
/// that removes the `ciborium` dependency from `fauna-protocol`).
///
/// Map keys are `String` only — dag-cbor mandates string keys — so the
/// integer-keyed wire *envelope* is decoded as a typed `Frame`, not as a
/// `Value`; `Value` carries the kind *payload*, which is always a string-keyed
/// dag-cbor structure.
pub use ipld_core::ipld::Ipld as Value;
