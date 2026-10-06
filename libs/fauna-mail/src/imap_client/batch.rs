//! § Batching: pack fetched messages into `import_message_batch` calls.
//!
//! > "up to **32 messages or 16 MiB** per `import_message_batch` call … the
//! > client decides per-batch based on the running byte total."
//!
//! Nest enforces both ceilings as a **whole-call rejection**
//! (`bridge_import_handlers.rs`), not a per-message error, so an over-long
//! batch loses all 32 messages — the packer must never emit one.
//!
//! # Why a message can outgrow every batch
//!
//! § Scope & limits allows a single message up to
//! [`crate::imap_client::DEFAULT_MAX_MESSAGE_BYTES`] (50 MiB), while a *batch*
//! caps at [`MAX_BATCH_BYTES`] (16 MiB). A 20 MiB message therefore fits no batch at
//! all. Nest's single-message `import_message` handler enforces no byte
//! ceiling — only the batch handler does — so such a message rides alone as
//! [`ImportUnit::Single`]. This is what the goal doc means by "batching
//! reduces WS round-trips **without blocking on a single 50 MiB message**".
//!
//! # ⚠ The transport ceiling is lower than either of these
//!
//! `fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE` caps *every* WS-RPC message
//! at **2 MiB**, symmetrically, and `architecture/transport.md` (the owner of
//! that constant) states there is "no streaming/chunking at the protocol
//! level". A 16 MiB batch — and any single message over ~2 MiB — therefore
//! cannot cross the wire at all, which makes nest's own `MAX_BATCH_BYTES`
//! check unreachable. This contradiction predates the import client and is not
//! this module's to settle (it governs the Go MDA/MTA ingest path identically);
//! see `mailbox-migration.md` § Implementation status today.
//!
//! [`BatchPacker::with_limits`] exists so the transport-aware layer can clamp
//! to whatever the wire actually accepts, without this module hard-coding a
//! number that a pending design pass may change.

use fauna_protocol::bridge_routing::ImportMessageItem;

use super::{FetchedMessage, MAX_BATCH_BYTES, MAX_BATCH_MESSAGES};

/// One WS-RPC call's worth of import work.
///
/// Not `Eq`: the wire type `ImportMessageItem` is only `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportUnit {
    /// Send as `fauna.bridges.import_message_batch`. Never empty, never over
    /// either ceiling.
    Batch(Vec<ImportMessageItem>),
    /// Send as `fauna.bridges.import_message`. A body larger than
    /// [`MAX_BATCH_BYTES`] that no batch can carry.
    Single(Box<ImportMessageItem>),
}

impl ImportUnit {
    /// Messages carried, for the wizard's progress arithmetic.
    pub fn len(&self) -> usize {
        match self {
            Self::Batch(items) => items.len(),
            Self::Single(_) => 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Accumulates [`FetchedMessage`]s and emits [`ImportUnit`]s as ceilings fill.
///
/// **Source order is preserved.** The nest-side resume cursor advances for
/// every processed message, so reordering across a pause would make the cursor
/// skip messages that were never sent.
#[derive(Debug)]
pub struct BatchPacker {
    items: Vec<ImportMessageItem>,
    bytes: usize,
    max_messages: usize,
    max_bytes: usize,
}

impl Default for BatchPacker {
    fn default() -> Self {
        Self::new()
    }
}

impl BatchPacker {
    /// The ratified § Batching ceilings: 32 messages, 16 MiB.
    pub fn new() -> Self {
        Self::with_limits(MAX_BATCH_MESSAGES, MAX_BATCH_BYTES)
    }

    /// Pack to caller-chosen ceilings.
    ///
    /// For the transport-aware layer, which must not emit a unit larger than
    /// the wire accepts (see the module docs: the 2 MiB WS-RPC cap currently
    /// binds below both ratified ceilings). Both must be non-zero.
    pub fn with_limits(max_messages: usize, max_bytes: usize) -> Self {
        debug_assert!(max_messages > 0 && max_bytes > 0);
        Self {
            items: Vec::new(),
            bytes: 0,
            max_messages: max_messages.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    /// Number of messages currently buffered (not yet emitted).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Convert `msg` into its wire item and buffer it, returning whatever
    /// became sendable — in source order.
    ///
    /// Yields at most two units: the batch this message displaced, and (when
    /// the message alone exceeds [`MAX_BATCH_BYTES`]) the oversized single.
    pub fn push(&mut self, msg: FetchedMessage) -> Vec<ImportUnit> {
        let item = to_item(msg);
        let size = item.body.len();
        let mut out = Vec::new();

        // A body no batch can hold: flush what precedes it, then send it alone.
        // Flushing first is what keeps the emitted order equal to source order.
        if size > self.max_bytes {
            out.extend(self.flush());
            out.push(ImportUnit::Single(Box::new(item)));
            return out;
        }

        if self.items.len() >= self.max_messages || self.bytes + size > self.max_bytes {
            out.extend(self.flush());
        }
        self.bytes += size;
        self.items.push(item);
        out
    }

    /// Emit the buffered messages, if any. Call at end-of-mailbox and before
    /// pausing — a half-full batch still has to reach nest.
    pub fn flush(&mut self) -> Option<ImportUnit> {
        if self.items.is_empty() {
            return None;
        }
        self.bytes = 0;
        Some(ImportUnit::Batch(std::mem::take(&mut self.items)))
    }
}

/// Build the wire item, deriving the two fields nest cannot compute for itself.
///
/// - `dedup_key` + `envelope_key`: § Dedup — computed here because nest holds
///   only the sealed body. Calls [`crate::dedup_key::mail_dedup_keys_from_slice`];
///   **never a reimplementation**, since the Go MDA and MTA and the nest's
///   in-domain delivery compute the same pair over the same bytes and dedup
///   silently stops working if any two disagree. The envelope key is what lets a hit skip only a faithful
///   duplicate (§ The envelope key confirms a Message-ID hit).
/// - `sender_domain`: populates `from_norm` for IMAP `SEARCH FROM`.
///
/// The body itself goes to the wire **unmodified**, in both storage modes —
/// nest seals it and derives the search-index hint at ingest (§ Per-message
/// flow step 4). This client never encrypts.
fn to_item(msg: FetchedMessage) -> ImportMessageItem {
    let keys = crate::dedup_key::mail_dedup_keys_from_slice(&msg.body);
    let sender_domain = crate::envelope::sender_domain(&msg.body);
    ImportMessageItem {
        mailbox: msg.mailbox,
        flags: msg.flags,
        // `validate_item` rejects a mismatch, and a >4 GiB message cannot
        // exist (`DEFAULT_MAX_MESSAGE_BYTES` is 50 MiB), so the cast is exact.
        body_size: msg.body.len() as u32,
        body: msg.body,
        timestamp: msg.internal_date_epoch,
        sender_domain,
        source_uid: msg.uid,
        source_uid_validity: msg.uid_validity,
        dedup_key: keys.dedup_key,
        envelope_key: keys.envelope_key,
        // The producer always emits an inline body; the staged-envelope leg (an
        // over-inline-ceiling body crossing on the byte plane) is a client-send-path
        // concern that no shipping producer wires yet (`mailbox-migration.md` § RPC
        // surface — the send path itself is unbuilt).
        staged_body: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(uid: u32, body_len: usize) -> FetchedMessage {
        // A parseable RFC 5322 message padded to `body_len`.
        let head = format!("Message-ID: <m{uid}@example.com>\r\nFrom: A <a@Example.COM>\r\n\r\n");
        let mut body = head.into_bytes();
        assert!(body.len() <= body_len, "body_len too small for headers");
        body.resize(body_len, b'x');
        FetchedMessage {
            mailbox: "INBOX".into(),
            uid,
            uid_validity: 1,
            flags: vec!["\\Seen".into()],
            internal_date_epoch: 837_596_665,
            body,
        }
    }

    fn uids(unit: &ImportUnit) -> Vec<u32> {
        match unit {
            ImportUnit::Batch(items) => items.iter().map(|i| i.source_uid).collect(),
            ImportUnit::Single(i) => vec![i.source_uid],
        }
    }

    #[test]
    fn a_partial_batch_is_only_emitted_on_flush() {
        let mut p = BatchPacker::new();
        for uid in 1..=5 {
            assert!(p.push(msg(uid, 100)).is_empty(), "nothing is full yet");
        }
        assert_eq!(p.len(), 5);
        let unit = p.flush().expect("half-full batch must still be sent");
        assert_eq!(uids(&unit), (1..=5).collect::<Vec<_>>());
        assert!(p.flush().is_none(), "flush is idempotent");
    }

    #[test]
    fn the_thirty_third_message_starts_a_new_batch() {
        let mut p = BatchPacker::new();
        for uid in 1..=32 {
            assert!(p.push(msg(uid, 100)).is_empty(), "uid {uid} fits");
        }
        let emitted = p.push(msg(33, 100));
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].len(), MAX_BATCH_MESSAGES);
        assert_eq!(uids(&emitted[0]), (1..=32).collect::<Vec<_>>());
        // 33 is buffered, not lost.
        assert_eq!(uids(&p.flush().unwrap()), vec![33]);
    }

    #[test]
    fn the_byte_ceiling_closes_a_batch_before_it_is_rejected() {
        // Nest rejects the *whole call* over 16 MiB, so the packer must close
        // the batch at the message that would cross it.
        let half = MAX_BATCH_BYTES / 2;
        let mut p = BatchPacker::new();
        assert!(p.push(msg(1, half)).is_empty());
        assert!(p.push(msg(2, half)).is_empty(), "exactly 16 MiB still fits");

        let emitted = p.push(msg(3, 1024));
        assert_eq!(uids(&emitted[0]), vec![1, 2]);
        match &emitted[0] {
            ImportUnit::Batch(items) => {
                let total: usize = items.iter().map(|i| i.body.len()).sum();
                assert!(total <= MAX_BATCH_BYTES, "emitted {total} bytes");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_message_larger_than_a_batch_rides_alone_and_keeps_source_order() {
        // 20 MiB: legal (§ Scope & limits allows 50 MiB) but fits no batch.
        // The two messages ahead of it must be sent *first*, or the nest-side
        // resume cursor would skip them.
        let mut p = BatchPacker::new();
        assert!(p.push(msg(1, 1024)).is_empty());
        assert!(p.push(msg(2, 1024)).is_empty());

        let emitted = p.push(msg(3, 20 * 1024 * 1024));
        assert_eq!(emitted.len(), 2, "the pending batch, then the single");
        assert_eq!(uids(&emitted[0]), vec![1, 2]);
        assert!(matches!(emitted[0], ImportUnit::Batch(_)));
        assert_eq!(uids(&emitted[1]), vec![3]);
        assert!(
            matches!(emitted[1], ImportUnit::Single(_)),
            "a 20 MiB body must not ride import_message_batch"
        );
        assert!(p.is_empty());
    }

    #[test]
    fn an_oversized_message_with_nothing_pending_emits_only_the_single() {
        let mut p = BatchPacker::new();
        let emitted = p.push(msg(1, 20 * 1024 * 1024));
        assert_eq!(emitted.len(), 1);
        assert!(matches!(emitted[0], ImportUnit::Single(_)));
    }

    #[test]
    fn exactly_max_batch_bytes_in_one_message_still_batches() {
        // The boundary is `>`, not `>=`: a 16 MiB message is the largest a
        // batch can carry, and nest's check is `total_bytes > MAX_BATCH_BYTES`.
        let mut p = BatchPacker::new();
        assert!(p.push(msg(1, MAX_BATCH_BYTES)).is_empty());
        assert!(matches!(p.flush(), Some(ImportUnit::Batch(_))));
    }

    #[test]
    fn no_emitted_unit_can_be_rejected_by_the_nest_handler() {
        // The packer's whole contract, asserted against nest's two checks:
        //   req.messages.len() > 32  ||  sum(body.len()) > 16 MiB  => rejected.
        let mut p = BatchPacker::new();
        let mut units = Vec::new();
        let sizes = [1024, MAX_BATCH_BYTES / 3, 100, 20 * 1024 * 1024, 4096];
        for (i, size) in sizes.iter().cycle().take(80).enumerate() {
            units.extend(p.push(msg(i as u32 + 1, *size)));
        }
        units.extend(p.flush());

        assert!(!units.is_empty());
        for unit in &units {
            if let ImportUnit::Batch(items) = unit {
                assert!(!items.is_empty(), "an empty batch is a wasted round-trip");
                assert!(items.len() <= MAX_BATCH_MESSAGES, "{} msgs", items.len());
                let total: usize = items.iter().map(|i| i.body.len()).sum();
                assert!(total <= MAX_BATCH_BYTES, "{total} bytes");
            }
        }
        // Every message is accounted for exactly once, in source order.
        let seen: Vec<u32> = units.iter().flat_map(uids).collect();
        assert_eq!(seen, (1..=80).collect::<Vec<_>>());
    }

    #[test]
    fn with_limits_lets_the_transport_layer_clamp_below_the_ratified_ceilings() {
        // The 2 MiB WS-RPC cap binds below § Batching's 16 MiB. A packer told
        // about the real wire ceiling must never emit a unit that exceeds it.
        let wire = 2 * 1024 * 1024;
        let mut p = BatchPacker::with_limits(MAX_BATCH_MESSAGES, wire);

        // Two 1.5 MiB messages cannot share a 2 MiB frame.
        let emitted = [p.push(msg(1, 1_500_000)), p.push(msg(2, 1_500_000))];
        assert!(emitted[0].is_empty());
        assert_eq!(uids(&emitted[1][0]), vec![1], "uid 2 must not join uid 1");

        // A message over the wire ceiling rides alone rather than in a batch.
        let over = p.push(msg(3, wire + 1));
        assert!(matches!(over.last().unwrap(), ImportUnit::Single(_)));

        for unit in emitted.iter().flatten().chain(over.iter()) {
            if let ImportUnit::Batch(items) = unit {
                let total: usize = items.iter().map(|i| i.body.len()).sum();
                assert!(total <= wire, "{total} exceeds the wire ceiling");
            }
        }
    }

    #[test]
    fn the_wire_item_carries_the_shared_dedup_key_not_a_reimplementation() {
        let mut p = BatchPacker::new();
        let m = msg(7, 200);
        let expected = crate::dedup_key::mail_dedup_keys(m.body.clone());
        p.push(m);
        let ImportUnit::Batch(items) = p.flush().unwrap() else {
            panic!("expected a batch")
        };
        assert_eq!(items[0].dedup_key, expected.dedup_key);
        assert_eq!(items[0].envelope_key, expected.envelope_key);
        assert!(
            items[0].dedup_key.starts_with("msgid:v1:"),
            "{}",
            items[0].dedup_key
        );
    }

    #[test]
    fn the_wire_item_passes_nest_validate_item() {
        // Mirrors `bridge_import_handlers.rs::validate_item` exactly.
        let mut p = BatchPacker::new();
        let mut m = msg(9, 500);
        m.flags = vec!["\\Seen".into(), "\\Answered".into()];
        p.push(m);
        let ImportUnit::Batch(items) = p.flush().unwrap() else {
            panic!("expected a batch")
        };
        let it = &items[0];
        assert!(!it.body.is_empty());
        assert_eq!(it.body_size as usize, it.body.len());
        assert!(!it.flags.iter().any(|f| f == "\\Recent"));
        assert!(!it.dedup_key.is_empty());
    }

    #[test]
    fn sender_domain_is_lowercased_from_the_from_header() {
        let mut p = BatchPacker::new();
        p.push(msg(1, 200));
        let ImportUnit::Batch(items) = p.flush().unwrap() else {
            panic!("expected a batch")
        };
        assert_eq!(items[0].sender_domain, "example.com");
    }

    #[test]
    fn a_message_with_no_from_header_still_imports() {
        // Nest accepts `sender_domain = ""` on the import path.
        let mut p = BatchPacker::new();
        p.push(FetchedMessage {
            mailbox: "INBOX".into(),
            uid: 1,
            uid_validity: 1,
            flags: vec![],
            internal_date_epoch: 0,
            body: b"Subject: no from\r\n\r\nbody".to_vec(),
        });
        let ImportUnit::Batch(items) = p.flush().unwrap() else {
            panic!("expected a batch")
        };
        assert_eq!(items[0].sender_domain, "");
        assert!(
            !items[0].dedup_key.is_empty(),
            "the envelope fallback still keys it"
        );
    }

    #[test]
    fn source_coordinates_ride_the_wire_for_the_resume_cursor() {
        let mut p = BatchPacker::new();
        let mut m = msg(4242, 200);
        m.uid_validity = 3_857_529_045;
        p.push(m);
        let ImportUnit::Batch(items) = p.flush().unwrap() else {
            panic!("expected a batch")
        };
        assert_eq!(items[0].source_uid, 4242);
        assert_eq!(items[0].source_uid_validity, 3_857_529_045);
        assert_eq!(items[0].timestamp, 837_596_665);
    }
}
