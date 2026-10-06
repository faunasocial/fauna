//! This crate's error type over `fauna_cbor`'s canonical dag-cbor pair.
//!
//! `fauna_cbor::{encode_canonical, decode_strict}` return their own
//! `EncodeError`/`DecodeError`; every at-rest caller in this crate's orbit
//! then maps them into [`SegmentStoreError::Encoding`] with the same
//! `e.to_string()`. That mapping was written **17 times across the three
//! placement modules** (`fauna-mail`, `fauna-calendar`, `fauna-contacts`) and
//! owned nowhere, so it lives here now — the same lift, and for the same
//! reason, as [`crate::atomic`]'s write and read halves before it.
//!
//! Deliberately *not* named `encode_canonical`/`decode_strict`: a call site
//! reading `codec::encode(self)` should not have to work out whether it is
//! looking at `fauna_cbor`'s pair or this one.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::SegmentStoreError;

/// Canonical dag-cbor, with an encode failure surfaced as
/// [`SegmentStoreError::Encoding`].
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, SegmentStoreError> {
    fauna_cbor::encode_canonical(value).map_err(|e| SegmentStoreError::Encoding(e.to_string()))
}

/// Strict dag-cbor decode, with a decode failure surfaced as
/// [`SegmentStoreError::Encoding`].
///
/// ⚠ `Encoding`, never `SchemaMismatch` — the distinction is load-bearing at
/// rest: undecodable bytes mean *this file is damaged*, which a caller may
/// answer by rebuilding from the durable journal, whereas a schema mismatch
/// means *this binary is too old to read an intact file*, which it must refuse
/// loudly. [`crate::versioned::VersionedManifest::decode_any_version`] draws
/// exactly that line.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, SegmentStoreError> {
    fauna_cbor::decode_strict(bytes).map_err(|e| SegmentStoreError::Encoding(e.to_string()))
}
