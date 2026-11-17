//! The possibility answer of the new-thread composer's gated controls — Send,
//! attach and topic (`docs/goal/ui/README.md` § Offer only what is possible,
//! rules 4–7; the page's ruling is `docs/goal/ui/conversations.md` § Errors &
//! edge cases → *The new-thread composer offers only what it can send*).
//!
//! An existing thread's controls answer from its
//! [`crate::capabilities::ThreadCapabilities`] ([`crate::thread_controls`]). A
//! thread not yet created has none: its rail is decided by who it goes to. The
//! recipient that decides is the first committed chip — the one
//! `send_new_thread` routes on — or, before any chip, the typed recipient the
//! picker's probe resolved, which is the chip Send's flush would commit.
//!
//! Attach and topic follow that rail: blocked, with the reason, on a rail that
//! carries no files or no topic. **Before any recipient decides a rail they are
//! possible** — nothing is unknown there, only not yet chosen: the draft holds
//! the file and the topic whatever the rail turns out to be (a message is
//! written first and addressed later, and survives a restart that way —
//! `conversations.md` § Persistence), so the input is never spent on something
//! that cannot be saved (rule 4), and choosing a recipient whose rail cannot
//! carry them blocks Send's use of them at that point, with the reason.
//!
//! Send itself is possible as soon as there is anyone to send to: a committed
//! chip, or typed text Send will flush into one (`conversations.md` § User
//! actions, the `dm-send-button` row: Send never silently drops a pending
//! recipient). `send_new_thread` asks [`send_possibility`] before it acts and
//! stamps the refusal on the draft's `send_state`, so the paint and the action
//! cannot disagree (rule 6) and a refusal is never silent (rule 7).

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;
use serde::{Deserialize, Serialize};

use crate::address::TypedAddress;
use crate::capabilities::derive_capabilities;
use crate::compose::{ComposeState, ResolveState};
use crate::thread::ThreadFlavor;

/// No recipient chosen yet: what the control does depends on who it goes to.
pub const NEEDS_RECIPIENT_KEY: &str = "conversations.compose.needs_recipient";
/// The deciding recipient is of a kind this build has no rail for (a chip a
/// newer device's drafts carried in).
pub const RECIPIENT_UNSUPPORTED_KEY: &str = "conversations.compose.recipient_unsupported";
/// The recipient's rail carries no files.
pub const NO_ATTACHMENTS_KEY: &str = "conversations.compose.no_attachments_here";
/// The recipient's rail carries no topic.
pub const NO_TOPIC_KEY: &str = "conversations.compose.no_topic_here";

/// A new-thread composer control whose possibility follows the recipient.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum NewThreadControl {
    /// `dm-send-button`.
    Send,
    /// `attachment-button`.
    Attach,
    /// `topic-toggle-button`.
    Topic,
}

/// Whether `control` in the new-thread composer `compose` can succeed now.
/// `None` (no composer open) is absent: nothing is painted.
pub fn new_thread_possibility(
    compose: Option<&ComposeState>,
    control: NewThreadControl,
) -> Possibility {
    let Some(compose) = compose else {
        return Possibility::Absent;
    };
    match control {
        NewThreadControl::Send => send_possibility(compose),
        NewThreadControl::Attach => rail_control_possibility(compose, |caps| {
            caps.supports_attachments
                .then_some(())
                .ok_or(NO_ATTACHMENTS_KEY)
        }),
        NewThreadControl::Topic => {
            // An open topic can always be closed again.
            if compose.subject_draft.is_some() {
                return Possibility::Possible;
            }
            rail_control_possibility(compose, |caps| {
                caps.supports_subject.then_some(()).ok_or(NO_TOPIC_KEY)
            })
        }
    }
}

/// Send: possible with a committed chip of a kind this build can send to, or
/// with typed text Send will flush; blocked otherwise.
pub fn send_possibility(compose: &ComposeState) -> Possibility {
    let Some(picker) = compose.recipient_picker.as_ref() else {
        return blocked(NEEDS_RECIPIENT_KEY);
    };
    match picker.chips.first() {
        Some(first) if first.rail().is_none() => blocked(RECIPIENT_UNSUPPORTED_KEY),
        Some(_) => Possibility::Possible,
        None if !picker.raw_input.trim().is_empty() => Possibility::Possible,
        None => blocked(NEEDS_RECIPIENT_KEY),
    }
}

/// The recipient that decides the new thread's rail: the first committed
/// chip, else the typed recipient the probe resolved (what Send's flush would
/// commit). `None` while no recipient has decided one.
fn deciding_recipient(compose: &ComposeState) -> Option<&TypedAddress> {
    let picker = compose.recipient_picker.as_ref()?;
    if let Some(first) = picker.chips.first() {
        return Some(first);
    }
    if picker.raw_input.trim().is_empty() {
        return None;
    }
    match (&picker.resolve_state, &picker.resolved) {
        (ResolveState::Resolved, Some(addr)) => Some(addr),
        _ => None,
    }
}

fn rail_control_possibility(
    compose: &ComposeState,
    allows: impl Fn(&crate::capabilities::ThreadCapabilities) -> Result<(), &'static str>,
) -> Possibility {
    // No rail decided yet: the draft holds the file or topic whatever it turns
    // out to be.
    let Some(recipient) = deciding_recipient(compose) else {
        return Possibility::Possible;
    };
    let Some(rail) = recipient.rail() else {
        return blocked(RECIPIENT_UNSUPPORTED_KEY);
    };
    let chips = compose
        .recipient_picker
        .as_ref()
        .map_or(0, |p| p.chips.len());
    // The flavor `send_new_thread` would found: one recipient is a one-to-one
    // thread, more a subject-keyed one (the rail decides these two controls the
    // same way for either today, but the answer follows what would be sent).
    let flavor = if chips > 1 {
        ThreadFlavor::SubjectKeyed
    } else {
        ThreadFlavor::OneToOne
    };
    match allows(&derive_capabilities(rail, flavor)) {
        Ok(()) => Possibility::Possible,
        Err(key) => blocked(key),
    }
}

fn blocked(key: &str) -> Possibility {
    Possibility::blocked(LocalizedText::key(key))
}

/// [`new_thread_possibility`] over FFI and wasm: the app families hand in the
/// snapshot's `new_thread_compose` (the `thread_control_possibility` shape).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn new_thread_control_possibility(
    compose: Option<ComposeState>,
    control: NewThreadControl,
) -> Possibility {
    new_thread_possibility(compose.as_ref(), control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::RecipientPickerState;

    fn fauna() -> TypedAddress {
        TypedAddress::Fauna {
            handle: "a".to_string(),
            actor_id: fauna_core::identity::ActorId([7u8; 32]),
        }
    }

    fn email() -> TypedAddress {
        TypedAddress::Email {
            email_address: "someone@example.test".to_string(),
        }
    }

    fn compose_with(picker: RecipientPickerState) -> ComposeState {
        ComposeState {
            recipient_picker: Some(picker),
            ..Default::default()
        }
    }

    fn key_of(p: &Possibility) -> Option<String> {
        match p {
            Possibility::Blocked { reason } => Some(reason.key.clone()),
            _ => None,
        }
    }

    #[test]
    fn no_composer_is_absent() {
        for c in [
            NewThreadControl::Send,
            NewThreadControl::Attach,
            NewThreadControl::Topic,
        ] {
            assert_eq!(new_thread_possibility(None, c), Possibility::Absent);
        }
    }

    #[test]
    fn no_recipient_blocks_send_naming_the_recipient_but_lets_the_draft_take_files_and_topic() {
        let compose = compose_with(RecipientPickerState::default());
        let send = new_thread_possibility(Some(&compose), NewThreadControl::Send);
        assert_eq!(key_of(&send).as_deref(), Some(NEEDS_RECIPIENT_KEY));
        // Written first, addressed later: nothing about the rail is decided yet.
        assert!(new_thread_possibility(Some(&compose), NewThreadControl::Attach).is_possible());
        assert!(new_thread_possibility(Some(&compose), NewThreadControl::Topic).is_possible());
    }

    #[test]
    fn typed_recipient_makes_send_possible_because_send_flushes_it() {
        let compose = compose_with(RecipientPickerState {
            raw_input: "someone@example.test".into(),
            resolve_state: ResolveState::Resolving,
            ..Default::default()
        });
        assert!(new_thread_possibility(Some(&compose), NewThreadControl::Send).is_possible());
    }

    #[test]
    fn resolved_typed_recipient_decides_attach_and_topic() {
        let compose = compose_with(RecipientPickerState {
            raw_input: "someone@example.test".into(),
            resolve_state: ResolveState::Resolved,
            resolved: Some(email()),
            ..Default::default()
        });
        assert!(new_thread_possibility(Some(&compose), NewThreadControl::Attach).is_possible());
        assert!(new_thread_possibility(Some(&compose), NewThreadControl::Topic).is_possible());
    }

    #[test]
    fn committed_chip_makes_everything_possible_on_a_rail_that_carries_it() {
        let compose = compose_with(RecipientPickerState {
            chips: vec![fauna()],
            ..Default::default()
        });
        for c in [
            NewThreadControl::Send,
            NewThreadControl::Attach,
            NewThreadControl::Topic,
        ] {
            assert!(
                new_thread_possibility(Some(&compose), c).is_possible(),
                "{c:?}"
            );
        }
    }

    #[test]
    fn unknown_kind_chip_blocks_send_attach_and_topic() {
        let unknown = TypedAddress::Unknown {
            canonical: vec![0xa0],
        };
        assert!(unknown.rail().is_none(), "precondition: no rail for it");
        let compose = compose_with(RecipientPickerState {
            chips: vec![unknown],
            ..Default::default()
        });
        for c in [
            NewThreadControl::Send,
            NewThreadControl::Attach,
            NewThreadControl::Topic,
        ] {
            let p = new_thread_possibility(Some(&compose), c);
            assert_eq!(
                key_of(&p).as_deref(),
                Some(RECIPIENT_UNSUPPORTED_KEY),
                "{c:?}"
            );
        }
    }

    #[test]
    fn a_rail_without_files_or_topic_blocks_them_naming_why() {
        let compose = compose_with(RecipientPickerState {
            chips: vec![TypedAddress::Bridged {
                bridge_id: "bridge".to_string(),
                address: "far".to_string(),
            }],
            ..Default::default()
        });
        assert!(new_thread_possibility(Some(&compose), NewThreadControl::Send).is_possible());
        let attach = new_thread_possibility(Some(&compose), NewThreadControl::Attach);
        assert_eq!(key_of(&attach).as_deref(), Some(NO_ATTACHMENTS_KEY));
        let topic = new_thread_possibility(Some(&compose), NewThreadControl::Topic);
        assert_eq!(key_of(&topic).as_deref(), Some(NO_TOPIC_KEY));
    }

    #[test]
    fn an_open_topic_can_always_be_closed() {
        let compose = ComposeState {
            subject_draft: Some(String::new()),
            recipient_picker: Some(RecipientPickerState::default()),
            ..Default::default()
        };
        assert!(new_thread_possibility(Some(&compose), NewThreadControl::Topic).is_possible());
    }
}
