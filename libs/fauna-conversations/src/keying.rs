//! Subject normalization and inbound-message keying rule.

use crate::address::{Rail, TypedAddress};
use crate::message::MessageId;

const STRIP_PREFIXES: &[&str] = &["re:", "fwd:", "fw:", "sv:", "vs:", "aw:"];

pub fn normalize_subject(raw: &str) -> String {
    let mut s = raw.trim().to_lowercase();
    loop {
        let mut stripped = false;
        for p in STRIP_PREFIXES {
            if s.starts_with(p) {
                s = s[p.len()..].trim_start().to_string();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    s
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ThreadKey {
    Participants {
        rail: Rail,
        participants: Vec<TypedAddress>,
    },
    SubjectKeyed {
        rail: Rail,
        participants: Vec<TypedAddress>,
        subject: String,
    },
    ByMessageReference(MessageId),
    /// Fauna-native MLS threads are identified by their MLS channel, not by
    /// participants/subject: two channels between the same actors are distinct
    /// threads, and a `ChannelMessage` carries no subject. The receiver
    /// materializes its thread under this key at welcome-ingest
    /// (`backends::fauna_mls::ingest_welcome`); routing of inbound messages
    /// still goes through the backend's thread↔channel binding.
    Channel {
        rail: Rail,
        channel_id_hex: String,
    },
}

/// Compute the lookup key for an inbound message. The store then matches
/// the key against existing threads and creates one if no match is found.
pub fn key_for_inbound(
    rail: Rail,
    participants: Vec<TypedAddress>,
    subject: Option<&str>,
    in_reply_to: Option<&MessageId>,
) -> ThreadKey {
    if let Some(parent) = in_reply_to {
        return ThreadKey::ByMessageReference(parent.clone());
    }
    let normalized = subject.map(normalize_subject).filter(|s| !s.is_empty());
    match normalized {
        Some(s) => ThreadKey::SubjectKeyed {
            rail,
            participants,
            subject: s,
        },
        None => ThreadKey::Participants { rail, participants },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_re_prefix_case_insensitive() {
        assert_eq!(normalize_subject("Re: Q4 budget"), "q4 budget");
        assert_eq!(normalize_subject("RE: Q4 budget"), "q4 budget");
        assert_eq!(normalize_subject("re: Q4 budget"), "q4 budget");
    }

    #[test]
    fn strips_repeated_prefixes() {
        assert_eq!(normalize_subject("Re: Re: Q4 budget"), "q4 budget");
        assert_eq!(normalize_subject("Fwd: Re: Q4 budget"), "q4 budget");
        assert_eq!(normalize_subject("Re: Fwd: Re: Q4 budget"), "q4 budget");
    }

    #[test]
    fn strips_locale_prefixes() {
        for prefix in &["Sv:", "Vs:", "Aw:", "VS:", "AW:"] {
            assert_eq!(normalize_subject(&format!("{} Hello", prefix)), "hello");
        }
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(normalize_subject("   Hello   "), "hello");
        assert_eq!(normalize_subject("Re:   Hello"), "hello");
    }

    #[test]
    fn empty_subject_is_empty() {
        assert_eq!(normalize_subject(""), "");
        assert_eq!(normalize_subject("  "), "");
        assert_eq!(normalize_subject("Re:"), "");
    }

    #[test]
    fn idempotent() {
        let cases = ["Re: Q4 budget", "Hello world", "Sv: Hej", "  Spaced  "];
        for s in cases {
            assert_eq!(
                normalize_subject(&normalize_subject(s)),
                normalize_subject(s)
            );
        }
    }

    fn p(_rail: Rail) -> Vec<TypedAddress> {
        vec![TypedAddress::Email {
            email_address: "alice@host.test".into(),
        }]
    }

    #[test]
    fn no_subject_keys_by_participants() {
        let key = key_for_inbound(Rail::Smtp, p(Rail::Smtp), None, None);
        assert!(matches!(key, ThreadKey::Participants { .. }));
    }

    #[test]
    fn subject_keys_by_subject() {
        let key = key_for_inbound(Rail::Smtp, p(Rail::Smtp), Some("Q4 Budget"), None);
        match key {
            ThreadKey::SubjectKeyed { subject, .. } => assert_eq!(subject, "q4 budget"),
            _ => panic!("expected SubjectKeyed"),
        }
    }

    #[test]
    fn empty_subject_falls_back_to_participants() {
        let key = key_for_inbound(Rail::Smtp, p(Rail::Smtp), Some(""), None);
        assert!(matches!(key, ThreadKey::Participants { .. }));
    }

    #[test]
    fn re_prefix_only_falls_back_to_participants() {
        let key = key_for_inbound(Rail::Smtp, p(Rail::Smtp), Some("Re:"), None);
        assert!(matches!(key, ThreadKey::Participants { .. }));
    }

    #[test]
    fn in_reply_to_overrides_subject() {
        let parent = MessageId("parent-id".into());
        let key = key_for_inbound(
            Rail::Smtp,
            p(Rail::Smtp),
            Some("Re: completely different"),
            Some(&parent),
        );
        match key {
            ThreadKey::ByMessageReference(id) => assert_eq!(id.0, "parent-id"),
            _ => panic!("expected ByMessageReference"),
        }
    }
}
