//! `LocalizedText` — the shared i18n-aware text carrier for shared-Rust state
//! machines (onboarding, folder wizard, …).
//!
//! A machine returns `(key, args)` rather than a finished English string so each
//! app can route the key through its own localization pipeline (Apple
//! `Bundle.main.localizedString`, Android `getString`, Web `L()`, Linux/GTK
//! `fauna_i18n` lookup). `resolve()` is the fallback renderer for native Rust
//! callers that don't go through a per-platform pipeline.
//!
//! This type lives in `fauna-core` (not in any one machine crate) so every
//! machine shares the *same* type — a single `uniffi::Record` registration, no
//! name collision in the generated bindings when a client links more than one
//! machine. Mirrors how [`crate::nat_mode::NodeMode`] is defined here and
//! re-exported by the onboarding machine.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// An i18n key plus a flat substitution map. `key` is the string key from
/// `i18n/strings/en.yaml`; `args` is `placeholder name → value`. Clients pass
/// `(key, args)` through their platform's localization pipeline. An empty key
/// means "no text".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LocalizedText {
    pub key: String,
    pub args: HashMap<String, String>,
}

impl LocalizedText {
    /// Construct from a static key with no substitutions.
    pub fn key(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            args: HashMap::new(),
        }
    }

    /// Construct from a key plus a single `{name}` substitution.
    pub fn key_arg(
        key: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let mut args = HashMap::new();
        args.insert(name.into(), value.into());
        Self {
            key: key.into(),
            args,
        }
    }

    /// Construct from a key plus several `{name}` substitutions.
    pub fn key_args<K, N, V>(key: K, pairs: impl IntoIterator<Item = (N, V)>) -> Self
    where
        K: Into<String>,
        N: Into<String>,
        V: Into<String>,
    {
        Self {
            key: key.into(),
            args: pairs
                .into_iter()
                .map(|(n, v)| (n.into(), v.into()))
                .collect(),
        }
    }

    /// Resolve into a display string using `lookup` to translate the key.
    /// Substitutes `{name}` placeholders in the looked-up template with
    /// `args[name]`. An empty key returns an empty string. A missing key
    /// falls back to using the key itself as the template (so the surface
    /// text is at least diagnostic instead of blank). For native Rust
    /// clients that don't go through a per-platform i18n pipeline.
    pub fn resolve<F, S>(&self, lookup: F) -> String
    where
        F: Fn(&str) -> Option<S>,
        S: AsRef<str>,
    {
        if self.key.is_empty() {
            return String::new();
        }
        let template = lookup(&self.key)
            .map(|s| s.as_ref().to_string())
            .unwrap_or_else(|| self.key.clone());
        let mut out = template;
        for (k, v) in &self.args {
            out = out.replace(&format!("{{{}}}", k), v);
        }
        out
    }

    /// [`resolve`](Self::resolve), but each **argument** is first put through
    /// `lookup` as well — for templates whose substitution is itself a
    /// translatable term rather than data.
    ///
    /// Some machines deliberately pass a *key* as an argument value, because the
    /// substituted word has to be translated too: the feature plane's exhaustion
    /// sentence carries `window = "features.window_day"` so *"you've used up the
    /// limit for this {window}"* reads "…for this day"
    /// (`fauna_client_features::view_model::restriction_text`). Plain [`resolve`]
    /// substitutes literally, so that sentence would render with the raw key in
    /// it — visible to the user, on every app that renders the row.
    ///
    /// An argument that is *not* a key simply misses the lookup and is
    /// substituted verbatim, which is what makes this safe to call on a mixed
    /// template. The one thing to know: a data value that happens to equal an
    /// i18n key would be translated, so prefer plain [`resolve`] for templates
    /// whose arguments are user-supplied strings (handles, filenames, error
    /// details).
    pub fn resolve_nested<F, S>(&self, lookup: F) -> String
    where
        F: Fn(&str) -> Option<S>,
        S: AsRef<str>,
    {
        if self.key.is_empty() {
            return String::new();
        }
        let template = lookup(&self.key)
            .map(|s| s.as_ref().to_string())
            .unwrap_or_else(|| self.key.clone());
        let mut out = template;
        for (k, v) in &self.args {
            let value = lookup(v)
                .map(|s| s.as_ref().to_string())
                .unwrap_or_else(|| v.clone());
            out = out.replace(&format!("{{{}}}", k), &value);
        }
        out
    }

    /// A compact, redaction-safe rendering for logs: the i18n `key` followed by
    /// any substitution `args` (which carry error *metadata* — an `e.detail()`
    /// string — never plaintext bodies or secrets). Unlike [`resolve`], it needs
    /// no per-platform i18n lookup, so the producer-side error logging in shared
    /// state machines (observability.md § Log on the *event*, not the *paint*)
    /// can call it where no localization pipeline is available. Args are emitted
    /// in sorted key order so the line is deterministic.
    pub fn log_line(&self) -> String {
        if self.args.is_empty() {
            return self.key.clone();
        }
        let mut pairs: Vec<(&String, &String)> = self.args.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        let rendered = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ({rendered})", self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_resolves_to_empty() {
        let lt = LocalizedText::default();
        assert_eq!(lt.resolve(|_| None::<&str>), "");
    }

    #[test]
    fn missing_key_falls_back_to_key_as_template() {
        let lt = LocalizedText::key("some.key");
        assert_eq!(lt.resolve(|_| None::<&str>), "some.key");
    }

    #[test]
    fn substitutes_args() {
        let lt = LocalizedText::key_arg("greeting", "name", "Ada");
        assert_eq!(
            lt.resolve(|k| if k == "greeting" {
                Some("Hi {name}!")
            } else {
                None
            }),
            "Hi Ada!"
        );
    }

    /// The feature plane's exhaustion sentence is the reason `resolve_nested`
    /// exists: its `{window}` argument is a key, so plain `resolve` leaves the
    /// raw key in a sentence the user reads.
    #[test]
    fn resolve_nested_translates_arguments_that_are_keys() {
        let lookup = |k: &str| match k {
            "features.exhausted_admin" => Some("You've used up the limit for this {window}."),
            "features.window_week" => Some("week"),
            _ => None,
        };
        let lt =
            LocalizedText::key_arg("features.exhausted_admin", "window", "features.window_week");

        assert_eq!(
            lt.resolve(lookup),
            "You've used up the limit for this features.window_week.",
            "plain resolve is literal — this is the leak resolve_nested fixes"
        );
        assert_eq!(
            lt.resolve_nested(lookup),
            "You've used up the limit for this week."
        );
    }

    /// A non-key argument must survive untouched, or every template with a
    /// data substitution would break the moment it went through this path.
    #[test]
    fn resolve_nested_leaves_data_arguments_alone() {
        let lt = LocalizedText::key_arg("greeting", "name", "Ada");
        assert_eq!(
            lt.resolve_nested(|k| if k == "greeting" {
                Some("Hi {name}!")
            } else {
                None
            }),
            "Hi Ada!"
        );
    }

    #[test]
    fn log_line_renders_key_and_sorted_args() {
        assert_eq!(
            LocalizedText::key("devices.error_refresh").log_line(),
            "devices.error_refresh"
        );
        let lt =
            LocalizedText::key_arg("devices.error_remove_device", "message", "nest unreachable");
        assert_eq!(
            lt.log_line(),
            "devices.error_remove_device (message=nest unreachable)"
        );
        // Multiple args render in sorted key order (deterministic).
        let mut multi = LocalizedText::key("k");
        multi.args.insert("b".into(), "2".into());
        multi.args.insert("a".into(), "1".into());
        assert_eq!(multi.log_line(), "k (a=1, b=2)");
    }
}
