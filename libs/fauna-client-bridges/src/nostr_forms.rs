//! The Nostr page's form-validity predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4's form case; the page's ruling is
//! `docs/goal/ui/nostr.md` § Errors & edge cases).
//!
//! Each answers [`Possibility`] for one form's submit from the draft the
//! person has typed: `Possible`, or `Blocked` with the field's reason key.
//! Every app paints its submit from the answer (disabled, the reason within
//! eyeshot), and the submit asks the same function first and refuses with the
//! same reason — so no app refuses an empty field only at the submit, and none
//! drops it silently. The relay list's twin is
//! `fauna_protocol::nostr_relay::add_relay_possibility`, beside the URL rule it
//! wraps.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::labels::{NOSTR_LINK_MODE_IMPORT, NOSTR_LINK_MODE_REMOTE};

/// `nostr-link-button`: import mode needs an nsec, remote mode a bunker URL;
/// generate mode needs nothing typed. `mode` is the link-*request* mode the
/// picker holds (`NOSTR_LINK_MODES`).
pub fn link_possibility(mode: &str, nsec: &str, bunker_url: &str) -> Possibility {
    if mode == NOSTR_LINK_MODE_IMPORT && nsec.trim().is_empty() {
        return Possibility::blocked(LocalizedText::key("nostr.link_account.enter_nsec"));
    }
    if mode == NOSTR_LINK_MODE_REMOTE && bunker_url.trim().is_empty() {
        return Possibility::blocked(LocalizedText::key("nostr.link_account.enter_bunker_url"));
    }
    Possibility::Possible
}

/// `nostr-add-follow`: a follow needs the pubkey (npub or hex); the petname is
/// optional. The nest decides whether the id is a well-formed key — the client
/// has no single shape for it to check.
pub fn add_follow_possibility(pubkey: &str) -> Possibility {
    if pubkey.trim().is_empty() {
        return Possibility::blocked(LocalizedText::key("nostr.follows.pubkey_required"));
    }
    Possibility::Possible
}

/// Whether a trimmed signer pubkey is the 64-hex shape the nest stores.
fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `nostr-zap-signer-add-btn`'s draft half: empty → the "enter it" reason, not
/// 64 hex → the shape reason. The registry-feature gate (the `designate`
/// courtesy layer) is a separate precondition the page folds in beside it.
pub fn zap_signer_add_possibility(pubkey: &str) -> Possibility {
    let pubkey = pubkey.trim();
    if pubkey.is_empty() {
        return Possibility::blocked(LocalizedText::key("nostr.zap_signers.pubkey_required"));
    }
    if !is_hex64(pubkey) {
        return Possibility::blocked(LocalizedText::key("nostr.zap_signers.invalid_pubkey"));
    }
    Possibility::Possible
}

/// The *Connected apps* (NIP-46 bunker) section: offered only to an account
/// whose key is on the box (`generated`/`imported`, the stored signing mode).
/// A `remote` or `nip07` account can never sign there, so the section is
/// absent, not disabled (rule 2); with no linked account at all it is absent
/// too, the linked surface not being painted.
pub fn connected_apps_possibility(signing_mode: Option<&str>) -> Possibility {
    match signing_mode {
        Some("generated") | Some("imported") => Possibility::Possible,
        _ => Possibility::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::NOSTR_LINK_MODE_GENERATE;

    fn blocked(key: &str) -> Possibility {
        Possibility::blocked(LocalizedText::key(key))
    }

    #[test]
    fn link_import_needs_an_nsec() {
        assert_eq!(
            link_possibility(NOSTR_LINK_MODE_IMPORT, "  ", ""),
            blocked("nostr.link_account.enter_nsec")
        );
        assert_eq!(
            link_possibility(NOSTR_LINK_MODE_IMPORT, "nsec1abc", ""),
            Possibility::Possible
        );
    }

    #[test]
    fn link_remote_needs_a_bunker_url() {
        assert_eq!(
            link_possibility(NOSTR_LINK_MODE_REMOTE, "", " "),
            blocked("nostr.link_account.enter_bunker_url")
        );
        assert_eq!(
            link_possibility(NOSTR_LINK_MODE_REMOTE, "", "bunker://x"),
            Possibility::Possible
        );
    }

    #[test]
    fn link_generate_needs_nothing_typed() {
        assert_eq!(
            link_possibility(NOSTR_LINK_MODE_GENERATE, "", ""),
            Possibility::Possible
        );
    }

    #[test]
    fn a_follow_needs_its_pubkey() {
        assert_eq!(
            add_follow_possibility(" "),
            blocked("nostr.follows.pubkey_required")
        );
        assert_eq!(add_follow_possibility("npub1x"), Possibility::Possible);
    }

    #[test]
    fn a_zap_signer_needs_64_hex() {
        assert_eq!(
            zap_signer_add_possibility(""),
            blocked("nostr.zap_signers.pubkey_required")
        );
        assert_eq!(
            zap_signer_add_possibility("abc"),
            blocked("nostr.zap_signers.invalid_pubkey")
        );
        assert_eq!(
            zap_signer_add_possibility(&"g".repeat(64)),
            blocked("nostr.zap_signers.invalid_pubkey")
        );
        assert_eq!(
            zap_signer_add_possibility(&format!(" {} ", "aB".repeat(32))),
            Possibility::Possible
        );
    }

    #[test]
    fn connected_apps_only_for_a_key_on_the_box() {
        for (mode, want) in [
            (Some("generated"), Possibility::Possible),
            (Some("imported"), Possibility::Possible),
            (Some("remote"), Possibility::Absent),
            (Some("nip07"), Possibility::Absent),
            (None, Possibility::Absent),
        ] {
            assert_eq!(connected_apps_possibility(mode), want, "{mode:?}");
        }
    }
}
