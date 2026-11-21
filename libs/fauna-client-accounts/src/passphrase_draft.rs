//! The credential-store unlock gate's draft-validity predicate
//! (`docs/goal/ui/README.md` § Offer only what is possible, rule 4 — *the
//! form case* — and rule 6; ui.yaml page `unlock`).
//!
//! The unlock surface asks for the passphrase that opens the sealed store, and
//! on first run it asks for a new one twice. Every app painted `unlock-submit-
//! button` enabled and refused an empty or mismatched draft only at the
//! submit; this one answer is what each paints the button from, and what each
//! submit asks again before it derives a key.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

/// `unlock-submit-button`'s answer from what has been typed. `confirm` is the
/// create mode's second entry (`unlock-confirm-input`); `None` in unlock mode,
/// which has no second field.
///
/// Blocked with `unlock.error_empty` while the passphrase is empty, then — in
/// create mode — with `unlock.error_mismatch` while the two entries differ.
/// Neither is trimmed: a passphrase is opaque bytes, and a space is part of it.
pub fn passphrase_possibility(passphrase: &str, confirm: Option<&str>) -> Possibility {
    if passphrase.is_empty() {
        return Possibility::blocked(LocalizedText::key("unlock.error_empty"));
    }
    match confirm {
        Some(confirm) if confirm != passphrase => {
            Possibility::blocked(LocalizedText::key("unlock.error_mismatch"))
        }
        _ => Possibility::Possible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: &Possibility) -> Option<&str> {
        p.reason().map(|r| r.key.as_str())
    }

    #[test]
    fn unlock_mode_blocks_only_an_empty_passphrase() {
        assert_eq!(
            key(&passphrase_possibility("", None)),
            Some("unlock.error_empty")
        );
        assert!(passphrase_possibility(" ", None).is_possible());
        assert!(passphrase_possibility("hunter2", None).is_possible());
    }

    #[test]
    fn create_mode_blocks_empty_first_then_a_mismatch() {
        assert_eq!(
            key(&passphrase_possibility("", Some(""))),
            Some("unlock.error_empty")
        );
        assert_eq!(
            key(&passphrase_possibility("", Some("x"))),
            Some("unlock.error_empty")
        );
        assert_eq!(
            key(&passphrase_possibility("hunter2", Some(""))),
            Some("unlock.error_mismatch")
        );
        assert_eq!(
            key(&passphrase_possibility("hunter2", Some("hunter2 "))),
            Some("unlock.error_mismatch")
        );
        assert!(passphrase_possibility("hunter2", Some("hunter2")).is_possible());
    }

    #[test]
    fn both_reasons_resolve() {
        for p in [
            passphrase_possibility("", None),
            passphrase_possibility("a", Some("b")),
        ] {
            let k = key(&p).unwrap();
            assert!(fauna_i18n::strings::lookup(k).is_some(), "{k}");
        }
    }
}
