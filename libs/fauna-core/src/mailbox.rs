//! Mailbox namespace and entry ID derivation for email bridge.

use serde::{Deserialize, Serialize};

/// Derive the BLAKE3 namespace hash for a mailbox.
pub fn mailbox_namespace(actor_id: &[u8], mailbox_name: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(actor_id);
    hasher.update(b"fauna.mailbox.v1");
    hasher.update(mailbox_name.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Derive a deterministic entry ID from a namespace and Message-ID header.
pub fn mail_entry_id(namespace: &[u8], message_id: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace);
    hasher.update(message_id.as_bytes());
    *hasher.finalize().as_bytes()
}

/// IMAP flags for a mail message. Used for bidirectional sync between nests.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MailFlags {
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub deleted: bool,
    pub draft: bool,
}

impl MailFlags {
    /// Merge two flag sets as a union — if either has a flag, the result has it.
    pub fn merge(&self, other: &MailFlags) -> MailFlags {
        MailFlags {
            seen: self.seen || other.seen,
            answered: self.answered || other.answered,
            flagged: self.flagged || other.flagged,
            deleted: self.deleted || other.deleted,
            draft: self.draft || other.draft,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_namespace_is_deterministic() {
        let actor_id = vec![1u8; 32];
        let ns1 = mailbox_namespace(&actor_id, "INBOX");
        let ns2 = mailbox_namespace(&actor_id, "INBOX");
        assert_eq!(ns1, ns2);

        let ns3 = mailbox_namespace(&actor_id, "Sent");
        assert_ne!(ns1, ns3);
    }

    #[test]
    fn mailbox_namespace_varies_by_actor() {
        // The whole point of hashing the actor_id in is per-actor isolation:
        // the same mailbox name under two actors must not collide.
        let ns_a = mailbox_namespace(&[1u8; 32], "INBOX");
        let ns_b = mailbox_namespace(&[2u8; 32], "INBOX");
        assert_ne!(ns_a, ns_b, "different actors must get distinct namespaces");
    }

    #[test]
    fn mail_entry_id_deterministic() {
        let actor_id = vec![1u8; 32];
        let ns = mailbox_namespace(&actor_id, "INBOX");
        let eid1 = mail_entry_id(&ns, "<abc@example.com>");
        let eid2 = mail_entry_id(&ns, "<abc@example.com>");
        assert_eq!(eid1, eid2);
    }

    #[test]
    fn mail_entry_id_varies_by_message_id_and_namespace() {
        let ns = mailbox_namespace(&[1u8; 32], "INBOX");
        let eid = mail_entry_id(&ns, "<abc@example.com>");

        // Different Message-ID under the same namespace must not collide.
        assert_ne!(
            eid,
            mail_entry_id(&ns, "<xyz@example.com>"),
            "distinct Message-IDs must derive distinct entry IDs"
        );

        // Same Message-ID under a different namespace must not collide either
        // (e.g. the same message filed under a different mailbox/actor).
        let ns_other = mailbox_namespace(&[1u8; 32], "Sent");
        assert_ne!(
            eid,
            mail_entry_id(&ns_other, "<abc@example.com>"),
            "same Message-ID under a different namespace must not collide"
        );
    }

    #[test]
    fn mail_flags_merge_as_union() {
        let flags_a = MailFlags {
            seen: true,
            answered: false,
            flagged: false,
            deleted: false,
            draft: false,
        };
        let flags_b = MailFlags {
            seen: false,
            answered: true,
            flagged: false,
            deleted: false,
            draft: false,
        };
        let merged = flags_a.merge(&flags_b);
        assert!(merged.seen, "seen from A");
        assert!(merged.answered, "answered from B");
        assert!(!merged.flagged, "neither had flagged");
    }

    #[test]
    fn mail_flags_merge_unions_every_flag_from_either_side() {
        // The previous test only exercised `seen`/`answered`. Assert each of
        // the five flags survives the union from EITHER side — the property the
        // bidirectional sync merge relies on (a set flag is never cleared by a
        // peer that has it false).
        let none = MailFlags::default();

        let seen = MailFlags {
            seen: true,
            ..Default::default()
        };
        assert!(seen.merge(&none).seen && none.merge(&seen).seen, "seen");

        let answered = MailFlags {
            answered: true,
            ..Default::default()
        };
        assert!(
            answered.merge(&none).answered && none.merge(&answered).answered,
            "answered"
        );

        let flagged = MailFlags {
            flagged: true,
            ..Default::default()
        };
        assert!(
            flagged.merge(&none).flagged && none.merge(&flagged).flagged,
            "flagged"
        );

        let deleted = MailFlags {
            deleted: true,
            ..Default::default()
        };
        assert!(
            deleted.merge(&none).deleted && none.merge(&deleted).deleted,
            "deleted"
        );

        let draft = MailFlags {
            draft: true,
            ..Default::default()
        };
        assert!(
            draft.merge(&none).draft && none.merge(&draft).draft,
            "draft"
        );
    }

    #[test]
    fn mail_flags_merge_with_default_is_identity() {
        let flags = MailFlags {
            seen: true,
            answered: false,
            flagged: true,
            deleted: false,
            draft: true,
        };
        let merged = flags.merge(&MailFlags::default());
        assert_eq!(merged.seen, flags.seen);
        assert_eq!(merged.answered, flags.answered);
        assert_eq!(merged.flagged, flags.flagged);
        assert_eq!(merged.deleted, flags.deleted);
        assert_eq!(merged.draft, flags.draft);
    }
}
