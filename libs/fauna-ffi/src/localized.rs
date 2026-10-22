//! UniFFI façade for the shared single-pass placeholder fill
//! ([`fauna_core::localized::fill_placeholders`]).
//!
//! The apps resolve a `LocalizedText`'s template through their own i18n catalog,
//! then fill its `{name}` placeholders here — one shared implementation, so an
//! arg holding `{other}` (a stranger-chosen display name) is never expanded as
//! another arg, whatever the dictionary order. Replaces the per-arg
//! `replacingOccurrences` loop in apple's `renderLocalizedText`. See
//! `docs/goal/behavior/notifications.md` § Localized body.

use std::collections::HashMap;

/// UniFFI face of [`fauna_core::localized::fill_placeholders`]: fills `{name}`
/// placeholders of `template` from `args` in one pass; an unknown `{name}` is
/// left as written.
#[uniffi::export]
pub fn fill_placeholders(template: String, args: HashMap<String, String>) -> String {
    fauna_core::localized::fill_placeholders(&template, &args)
}

#[cfg(test)]
mod tests {
    use super::fill_placeholders;
    use std::collections::HashMap;

    #[test]
    fn an_arg_containing_a_placeholder_is_not_expanded() {
        for _ in 0..64 {
            let args: HashMap<String, String> = [("a", "{b}"), ("b", "second")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            assert_eq!(fill_placeholders("{a} / {b}".into(), args), "{b} / second");
        }
    }
}
