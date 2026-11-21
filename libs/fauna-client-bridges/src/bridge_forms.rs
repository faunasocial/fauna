//! The Bridges page's form-validity predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4's form case; the page's ruling is
//! `docs/goal/ui/bridges.md` § Errors & edge cases).
//!
//! Each answers [`Possibility`] for one form's submit from the draft the
//! person has typed: `Possible`, or `Blocked` with the field's reason key.
//! Every app paints its submit from the answer and the submit asks the same
//! function first, refusing with the same reason — never an empty field
//! dropped silently at the submit. The Nostr page's twins are in
//! [`crate::nostr_forms`].

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

/// `bridge-add-follow-button`: a follow needs the id to follow; the petname
/// is optional. The bridge decides whether the id names an account — the
/// client has no single shape for it to check.
pub fn bridge_follow_possibility(id: &str) -> Possibility {
    if id.trim().is_empty() {
        return Possibility::blocked(LocalizedText::key("bridges.follow_id_required"));
    }
    Possibility::Possible
}

/// `bridge-plugin-install-submit`: an install needs the plugin document's
/// URL. The nest fetches and verifies the document, so the client checks only
/// that there is one to fetch.
pub fn plugin_install_possibility(document_url: &str) -> Possibility {
    if document_url.trim().is_empty() {
        return Possibility::blocked(LocalizedText::key("bridges.plugin_install_url_required"));
    }
    Possibility::Possible
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_follow_id_or_install_url_is_blocked_with_the_fields_reason() {
        assert_eq!(
            bridge_follow_possibility("  "),
            Possibility::blocked(LocalizedText::key("bridges.follow_id_required"))
        );
        assert_eq!(bridge_follow_possibility("alice"), Possibility::Possible);
        assert_eq!(
            plugin_install_possibility(""),
            Possibility::blocked(LocalizedText::key("bridges.plugin_install_url_required"))
        );
        assert_eq!(
            plugin_install_possibility("https://example.org/plugin.json"),
            Possibility::Possible
        );
    }
}
