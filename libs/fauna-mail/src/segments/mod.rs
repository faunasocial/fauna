//! Mail segment-record framing for the kind-agnostic segment store.
//!
//! Split by dependency weight (mirrors `fauna_mls::segments` for the conv rail):
//!   * The **pure codec** — `envelope` (`MailRecordEnvelope`), `floor`
//!     (`MailFloorMetadata`), `paths` (bucket/path helpers) — pulls only
//!     `fauna-cbor` and compiles under the light `segments-codec` feature, so a
//!     client can decode the OUTER `sealed_envelope` the `fauna.email.inbox.fetch`
//!     feed ships without the nest stack.
//!   * The **`SegmentManager` coordination** — `ops`, `placement` — needs the
//!     nest-only `fauna-segment-store` and stays gated behind `nest-segments`.
//!   * `receive::open_inbound_record` (the `segments-receive` feature) bridges the
//!     codec to `fauna_mls`'s inner HPKE unseal for client-side decrypt.
//!
//! Used by `bins/fauna-nest/src/segments/mail.rs` to drive the per-actor
//! `__mail/<actor>/` segment store.
//!
//! See `docs/goal/architecture/message-segment-store.md` for the
//! mechanism doc and `docs/goal/behavior/file-sync.md` § `__mail` Sync
//! for the at-rest tiering authority.

pub mod envelope;
pub mod floor;
#[cfg(feature = "nest-segments")]
pub mod ops;
pub mod paths;
#[cfg(feature = "nest-segments")]
pub mod placement;
#[cfg(feature = "segments-receive")]
pub mod receive;

pub use envelope::{
    MAIL_ENVELOPE_FORMAT_VERSION_V2, MAIL_ENVELOPE_FORMAT_VERSION_V3, MAIL_ENVELOPE_WRITE_FORMAT,
    MailContinuationHead, MailRecord, MailRecordEnvelope, peek_format_version,
};
pub use floor::{
    CONTINUATION_ROLE_HEAD, CONTINUATION_ROLE_NORMAL, CONTINUATION_ROLE_PART, FloorScoreEntry,
    MAIL_FLOOR_FORMAT_VERSION, MailFloorMetadata,
};
pub use paths::{bucket_for, mail_manifest_path, mail_segments_root};
