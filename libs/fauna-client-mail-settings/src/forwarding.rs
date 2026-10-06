//! The `mail-settings` page's Forwarding section — the draft checks every app
//! runs before the round trip (`docs/goal/behavior/mail-forwarding.md`
//! § Per-account "forward all", § Per-account forward rate-limit).
//!
//! Two fields, both committed as a whole value: the "forward all incoming mail
//! to" address and the user's own hourly forwarding limit. Each parser turns
//! the field's draft text into the value the `fauna.bridges.set_*` door takes,
//! or into the sentence the page shows instead — a [`LocalizedText`] each app
//! resolves through its own string table, the [`crate::settings_status_label`]
//! shape. They wrap the same `fauna_mail` validators the nest runs at the doors
//! (priority #2: one rule), so a refusal an app shows here is exactly one the
//! nest would have made. The one check an app cannot make is "is this address on
//! a domain the nest hosts" — the app does not know the hosted domains; the nest
//! refuses that with its own code (`fauna.bridges.forward_target_on_local_domain`),
//! whose sentence names the alias remedy.

use fauna_core::localized::LocalizedText;
use fauna_mail::forward_config::{
    ForwardPerHourError, validate_forward_per_hour, validate_forward_target,
};

/// Parse the "forward all incoming mail to" draft. Blank (after trimming)
/// clears forwarding — `Ok(None)`, the doc's "clearing it disables". A
/// non-blank draft must pass the RFC 5321 syntactic check
/// (`mail-forwarding.md` § Per-account "forward all" → Address validation) and
/// is returned trimmed.
pub fn parse_forward_all_to_draft(draft: &str) -> Result<Option<String>, LocalizedText> {
    let address = draft.trim();
    if address.is_empty() {
        return Ok(None);
    }
    // No hosted domains here — the nest owns that half (module docs).
    validate_forward_target(address, &[])
        .map(|()| Some(address.to_string()))
        .map_err(|_| LocalizedText::key("mail_settings.forward_all_to_invalid"))
}

/// Parse the hourly forwarding limit draft against the `ceiling` the nest
/// returned beside the value (`GetForwardPerHourReply::forward_per_hour_ceiling`
/// — the app never hard-codes the admin tier). There is no blank: the setting
/// always has a value (default 100), so an empty draft is not a number.
pub fn parse_forward_per_hour_draft(draft: &str, ceiling: u32) -> Result<u32, LocalizedText> {
    let value: u32 = draft
        .trim()
        .parse()
        .map_err(|_| LocalizedText::key("mail_settings.forward_per_hour_not_a_number"))?;
    validate_forward_per_hour(value, ceiling).map_err(|e| match e {
        ForwardPerHourError::Zero => LocalizedText::key("mail_settings.forward_per_hour_zero"),
        ForwardPerHourError::AboveCeiling { ceiling } => LocalizedText::key_arg(
            "mail_settings.forward_per_hour_above_ceiling",
            "ceiling",
            ceiling.to_string(),
        ),
    })?;
    Ok(value)
}

/// The limit field's explainer, naming the ceiling the value may not pass.
pub fn forward_per_hour_hint(ceiling: u32) -> LocalizedText {
    LocalizedText::key_arg(
        "mail_settings.forward_per_hour_subtitle",
        "ceiling",
        ceiling.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_address_draft_clears_forwarding() {
        assert_eq!(parse_forward_all_to_draft(""), Ok(None));
        assert_eq!(parse_forward_all_to_draft("   "), Ok(None));
    }

    #[test]
    fn an_address_draft_is_trimmed_and_kept() {
        assert_eq!(
            parse_forward_all_to_draft("  me@elsewhere.example "),
            Ok(Some("me@elsewhere.example".to_string()))
        );
    }

    #[test]
    fn a_malformed_address_draft_is_refused_with_one_sentence() {
        for bad in [
            "not-an-email",
            "two@@at.com",
            "bob@",
            "@nodomain",
            "bob@nodot",
        ] {
            assert_eq!(
                parse_forward_all_to_draft(bad),
                Err(LocalizedText::key("mail_settings.forward_all_to_invalid")),
                "bad={bad:?}"
            );
        }
    }

    #[test]
    fn a_limit_draft_parses_within_one_and_the_ceiling() {
        assert_eq!(parse_forward_per_hour_draft("1", 500), Ok(1));
        assert_eq!(parse_forward_per_hour_draft(" 40 ", 500), Ok(40));
        assert_eq!(parse_forward_per_hour_draft("500", 500), Ok(500));
    }

    #[test]
    fn a_limit_draft_outside_the_range_names_why() {
        assert_eq!(
            parse_forward_per_hour_draft("0", 500),
            Err(LocalizedText::key("mail_settings.forward_per_hour_zero"))
        );
        assert_eq!(
            parse_forward_per_hour_draft("501", 500),
            Err(LocalizedText::key_arg(
                "mail_settings.forward_per_hour_above_ceiling",
                "ceiling",
                "500"
            ))
        );
    }

    #[test]
    fn a_limit_draft_that_is_not_a_whole_number_says_so() {
        for bad in ["", "  ", "ten", "-3", "2.5"] {
            assert_eq!(
                parse_forward_per_hour_draft(bad, 500),
                Err(LocalizedText::key(
                    "mail_settings.forward_per_hour_not_a_number"
                )),
                "bad={bad:?}"
            );
        }
    }

    /// Every key the parsers emit resolves in the shared string table — a typo
    /// here would paint the raw key on every app.
    #[test]
    fn every_key_resolves() {
        for key in [
            "mail_settings.forward_all_to_invalid",
            "mail_settings.forward_per_hour_not_a_number",
            "mail_settings.forward_per_hour_zero",
            "mail_settings.forward_per_hour_above_ceiling",
            "mail_settings.forward_per_hour_subtitle",
        ] {
            assert!(fauna_i18n::strings::lookup(key).is_some(), "{key}");
        }
    }
}
