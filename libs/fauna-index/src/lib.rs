//! `fauna-index` — per-user content search index for the fauna project.
//!
//! Wraps Tantivy 0.26 (index format v4) with a domain schema (kind / content_id / timestamp /
//! sender + title/body/tags text fields) and exposes:
//!
//! * [`Index::create_in_ram`] / [`Index::add_doc`] / [`Index::commit`] /
//!   [`Index::query`] — in-memory build and search.
//! * [`Index::seal_to_bytes`] / [`Index::open_from_bytes`] — plaintext
//!   round-trip of the underlying Tantivy `RamDirectory` (Plan 1).
//! * [`Index::seal_encrypted`] / [`Index::open_encrypted`] — AEAD-encrypted
//!   round-trip under an [`IndexMasterKey`] using the segment wire format
//!   (Plan 2).
//! * [`rewrap_segment_master_key`] — master-key rotation that rewrites only
//!   the 80-byte header region; bodies stay byte-identical.
//! * [`seal_under_master`] / [`open_under_master`] — master-direct AEAD for
//!   small mutable files (Plan 3 manifest, Plan 7 classifier ledger).
//! * [`IndexManifest`] / [`KindManifest`] — typed payload of
//!   `__index/manifest.idx` (per-kind live/tombstoned segments + next id),
//!   serialized as a single-block CARv2 file (canonical dag-cbor payload,
//!   root CID = `Cid::of_dag_cbor(payload)`). Plaintext or sealed under an
//!   [`IndexMasterKey`] (Plan 3 / Layer 3).
//! * [`INDEX_FOLDER`] / [`manifest_path`] / [`segment_path`] /
//!   [`parse_segment_path`] — virtual-path conventions for the `__index/`
//!   reserved folder on the nest (Plan 3).
//!
//! Encryption is XChaCha20-Poly1305 with random 24-byte nonces (design
//! tracked internally, § D4 + § D10, for the file layout and encryption
//! rationale).

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_index");

pub mod index;
mod key;
mod manifest;
mod paths;
mod seal;
pub mod tokenizer_adapter;
pub mod types;
#[cfg(feature = "uniffi")]
mod uniffi_handle;
pub mod version;

pub use index::Index;
pub use key::{ClassKey, IndexMasterKey, IndexSegmentKey};
pub use manifest::{IndexManifest, KindManifest, MANIFEST_FORMAT_VERSION};
pub use paths::{
    INDEX_FOLDER, MAILCAL_MANIFEST_FILE_NAME, MANIFEST_FILE_NAME, mailcal_manifest_path,
    manifest_path, parse_segment_path, path_key_class, segment_path,
};
pub use tokenizer_adapter::{FAUNA_TOKENIZER_NAME, TOKENIZER_PIPELINE_VERSION, query_tokens};

pub use seal::{
    open_segment_bytes, open_segment_bytes_mailcal, open_under_mailcal_key, open_under_master,
    peek_sealed_stamp, rewrap_segment_mailcal_key, rewrap_segment_master_key, seal_segment_bytes,
    seal_segment_bytes_mailcal, seal_under_mailcal_key, seal_under_master,
};
pub use types::{
    ContentId, ContentKind, DocIdentity, FieldKind, IndexError, IndexedDoc, IndexedField,
    KindClass, QueryHit, TimeRange,
};
#[cfg(feature = "uniffi")]
pub use uniffi_handle::IndexHandle;
pub use version::{
    CURRENT_INDEX_FORMAT_VERSION, IndexFormatStamp, IndexFormatVerdict,
    MIN_READER_INDEX_FORMAT_VERSION, check_index_format_compatibility,
};
