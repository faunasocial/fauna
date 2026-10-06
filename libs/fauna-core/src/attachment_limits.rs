//! **The attachment blob's two shared bounds** — the numbers every writer and
//! every reader of a sealed attachment blob agrees on, owned here once so the
//! nest's upload door, the nest's per-record pin, the labeler's facet loop and
//! every app's receive loop cannot drift apart
//! (`conversation-rooms.md` § The home nest → *Attachment bytes*).
//!
//! Constants, never knobs (`principles.md` § One configuration surface): each
//! follows from the wire, not from anything a deployment would choose.

/// How many attachments one record may carry. The nest pins at most this many
/// plaintext `attachment_refs` per send; a reader walks at most this many
/// entries of a message's **sealed** attachment list, because that list is its
/// author's alone — no send-time check can bound it — and an entry past this
/// count names a blob its own record never pinned.
pub const MAX_ATTACHMENTS_PER_RECORD: usize = 64;

/// The largest body the nest's inline blob door accepts (`POST /api/v1/blob`
/// multipart, `PUT /api/v1/blob/{cid}` octet-stream). A sealed attachment blob
/// comes to rest only through that door, so no legitimate one is larger: a
/// reader refuses an attachment whose declared plaintext size is over it before
/// fetching, and stops reading any response that runs past it — a nest serving
/// more than its own door accepts is not serving an attachment.
pub const INLINE_BLOB_BODY_LIMIT: usize = 10 * 1024 * 1024;
