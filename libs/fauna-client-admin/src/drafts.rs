//! Draft-validity predicates for the admin forms (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4 — the form case; `admin.md`
//! § Errors & edge cases → *A form's submit is live only for a draft the nest
//! would accept*).
//!
//! Each answers whether a form's typed drafts can be sent as they stand, in the
//! one shared type every app paints from: [`Possibility::Possible`], or
//! [`Possibility::Blocked`] with the sentence naming what to fix. The app paints
//! its submit from the answer, the submit asks the same predicate again, and a
//! refusal shows that reason on the page's error line — so a malformed draft is
//! never sent, never silently replaced, and never discovered only at the press.
//! These answer only the DRAFT: an app folds each with the read gate its page
//! already has (`Possibility::and`), never in place of it.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::{TierCreateRefusal, parse_region_code, tier_create_request};

fn blocked(key: &str) -> Possibility {
    Possibility::blocked(LocalizedText::key(key))
}

/// Parse the registration form's free-tier ceiling: blank is "no limit"
/// (`None`), a whole number is the ceiling, anything else is refused with the
/// sentence the page shows (`admin.md` § 2 → registration).
pub fn parse_registration_ceiling(raw: &str) -> Result<Option<u64>, LocalizedText> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    raw.parse::<u64>()
        .map(Some)
        .map_err(|_| LocalizedText::key("admin.users_page.max_free_users_invalid"))
}

/// Whether `admin-users-registration-save-button` can send its ceiling draft.
pub fn registration_ceiling_possibility(raw: &str) -> Possibility {
    Possibility::from_blocked_reason(parse_registration_ceiling(raw).err())
}

/// Parse the admit form's account key: exactly 64 hex characters, the 32-byte
/// actor id. `None` for anything else (blank included).
pub fn parse_admit_actor(raw: &str) -> Option<Vec<u8>> {
    let raw = raw.trim();
    if raw.len() != 64 {
        return None;
    }
    (0..raw.len())
        .step_by(2)
        // `.get` (not a slice): a multi-byte char fails the parse and cannot
        // panic on a non-boundary index.
        .map(|i| u8::from_str_radix(raw.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Whether the admit button can send its account-key draft.
pub fn admit_actor_possibility(raw: &str) -> Possibility {
    if parse_admit_actor(raw).is_some() {
        Possibility::Possible
    } else {
        blocked("admin.users_page.admit_actor_hint")
    }
}

/// Whether a tier row's save can send its five cap drafts (`admin.md` § 3 →
/// in-place tier-cap editing; `value-formatting.md` § Tier cap validation):
/// every cap must parse through `fauna_core::format::parse_cap`.
pub fn tier_caps_possibility(caps: [&str; 5]) -> Possibility {
    if caps
        .iter()
        .all(|c| fauna_core::format::parse_cap(c).is_some())
    {
        Possibility::Possible
    } else {
        blocked("admin.settings_page.save_tier_error_invalid_cap")
    }
}

/// Whether the add-a-tier form can send its drafts — the same rule as
/// [`tier_create_request`], worded per refusal.
pub fn tier_create_possibility(name: &str, caps: [&str; 5]) -> Possibility {
    match tier_create_request(name, caps) {
        Ok(_) => Possibility::Possible,
        Err(TierCreateRefusal::EmptyName) => {
            blocked("admin.settings_page.add_tier_error_empty_name")
        }
        Err(TierCreateRefusal::InvalidCap) => {
            blocked("admin.settings_page.add_tier_error_invalid_cap")
        }
    }
}

/// Whether a membership row's save can send its drafts: it needs the
/// admitted-at quota tier picked.
pub fn membership_save_possibility(admin_tier: &str) -> Possibility {
    if admin_tier.trim().is_empty() {
        blocked("admin.settings_page.save_membership_tier_error_no_tier")
    } else {
        Possibility::Possible
    }
}

/// Whether `admin-nest-serving-port-save-button` can send its port draft: a
/// listener port in `[1, 65535]` (`fauna_core::format::parse_port`).
pub fn serving_port_possibility(raw: &str) -> Possibility {
    if fauna_core::format::parse_port(raw).is_some() {
        Possibility::Possible
    } else {
        blocked("admin.nest_page.serving_port_invalid")
    }
}

/// Whether the region save can send its code draft ([`parse_region_code`]).
pub fn region_possibility(raw: &str) -> Possibility {
    match parse_region_code(raw) {
        Ok(_) => Possibility::Possible,
        Err(key) => blocked(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(p: &Possibility) -> &str {
        p.reason().map(|r| r.key.as_str()).unwrap_or("")
    }

    #[test]
    fn registration_ceiling_takes_blank_or_a_whole_number_only() {
        assert_eq!(parse_registration_ceiling("  "), Ok(None));
        assert_eq!(parse_registration_ceiling(" 25 "), Ok(Some(25)));
        assert!(registration_ceiling_possibility("").is_possible());
        assert!(registration_ceiling_possibility("12").is_possible());
        for bad in ["ten", "-1", "1.5", "1e3"] {
            let p = registration_ceiling_possibility(bad);
            assert_eq!(
                reason(&p),
                "admin.users_page.max_free_users_invalid",
                "{bad}"
            );
        }
    }

    #[test]
    fn admit_actor_needs_exactly_64_hex_characters() {
        let good = "ab".repeat(32);
        assert_eq!(parse_admit_actor(&good), Some(vec![0xab; 32]));
        assert!(admit_actor_possibility(&format!(" {good} ")).is_possible());
        let multibyte = format!("{}é", "a".repeat(62));
        for bad in [
            String::new(),
            "ab".repeat(31),
            "ab".repeat(33),
            format!("{}zz", "ab".repeat(31)),
            multibyte,
        ] {
            assert_eq!(
                reason(&admit_actor_possibility(&bad)),
                "admin.users_page.admit_actor_hint",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn tier_caps_refuse_any_unparseable_cap() {
        assert!(tier_caps_possibility(["1", "2", "3", "0", "-4"]).is_possible());
        for i in 0..5 {
            let mut caps = ["1"; 5];
            caps[i] = "lots";
            assert_eq!(
                reason(&tier_caps_possibility(caps)),
                "admin.settings_page.save_tier_error_invalid_cap"
            );
            caps[i] = "";
            assert!(!tier_caps_possibility(caps).is_possible(), "blank cap {i}");
        }
    }

    #[test]
    fn tier_create_names_the_refusal() {
        assert!(tier_create_possibility("pro", ["1"; 5]).is_possible());
        assert_eq!(
            reason(&tier_create_possibility("  ", ["1"; 5])),
            "admin.settings_page.add_tier_error_empty_name"
        );
        assert_eq!(
            reason(&tier_create_possibility("pro", ["1", "x", "1", "1", "1"])),
            "admin.settings_page.add_tier_error_invalid_cap"
        );
    }

    #[test]
    fn membership_needs_an_admitted_tier() {
        assert!(membership_save_possibility("free").is_possible());
        assert_eq!(
            reason(&membership_save_possibility("")),
            "admin.settings_page.save_membership_tier_error_no_tier"
        );
    }

    #[test]
    fn serving_port_and_region_refuse_malformed_drafts() {
        assert!(serving_port_possibility("8443").is_possible());
        for bad in ["0", "65536", "port", ""] {
            assert_eq!(
                reason(&serving_port_possibility(bad)),
                "admin.nest_page.serving_port_invalid",
                "{bad}"
            );
        }
        assert!(region_possibility("NO").is_possible());
        for bad in ["no", "", "TOOLONGCODE", "N"] {
            assert_eq!(
                reason(&region_possibility(bad)),
                "admin.nest_page.region_invalid",
                "{bad}"
            );
        }
    }
}
