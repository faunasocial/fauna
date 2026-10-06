//! Client-facing label maps for the bridges surface — backend enum/mode
//! values → a [`LocalizedText`] each app resolves through its own i18n
//! pipeline (`.resolve(lookup)`), so the value→label contract lives once in
//! shared Rust and can't drift per client (priorities #2/#4). Mirrors the
//! `fauna_client_mail_settings::member_status_label` / `fauna_core::ical::
//! reminder_label` pattern.

use fauna_core::localized::LocalizedText;
use fauna_protocol::bridges_ui::BridgeFollow;

/// Map a linked Nostr account's stored **signing mode** (the `mode` field a
/// `fauna.bridges.list` reply carries for `bridge_id:"nostr"`, sourced from the
/// nest's `users.signing_mode` column) to its human label.
///
/// The nest stores one of four values when an account is linked
/// (`bins/fauna-nest/src/nostr/bridge_provider.rs::link`): `generated` (nest
/// generated + custodies the key), `imported` (user pasted an nsec, nest
/// custodies), `remote` (NIP-46 bunker — key stays in the user's external
/// signer), `nip07` (browser-extension signer). Each carries its
/// `nostr.account.mode_*` key; any unrecognized value (or an empty/None mode,
/// which clients pass as their not-linked placeholder) falls back to the raw
/// string rendered verbatim — preserving the prior per-app "show the raw
/// mode" behavior for anything off-list.
///
/// Shared so the four apps with a Nostr page (web/linux/apple — windows when
/// it builds one) stop hand-rendering the raw enum (`docs/goal/ui/nostr.md`
/// § Account linking). Note `generated`/`imported`/`remote`/`nip07` are the
/// **stored** values, distinct from the link-*request* modes
/// `generate`/`import`/`remote`/`nip07` the picker sends.
pub fn nostr_key_source_label(mode: &str) -> LocalizedText {
    match mode {
        "generated" => LocalizedText::key("nostr.account.mode_generated"),
        "imported" => LocalizedText::key("nostr.account.mode_imported"),
        "remote" => LocalizedText::key("nostr.account.mode_remote"),
        "nip07" => LocalizedText::key("nostr.account.mode_nip07"),
        // The keyless serving box's auto-provisioned account (Phase-2 proxy
        // delegation, `nostr.md` § The bridging gate → Phase 2).
        "proxied" => LocalizedText::key("nostr.account.mode_proxied"),
        // Unknown / not-linked placeholder (e.g. "—", "") → render verbatim:
        // `resolve` returns the key as-is when no i18n entry matches it.
        other => LocalizedText::key(other),
    }
}

/// The three native link-*request* modes, in picker order — `nip07` is web's
/// own fourth, browser-only option (§ Architectural rules 4) and isn't part
/// of this catalog. tui's `nostr::LINK_MODES` and linux's
/// `settings::nostr_tab::LINK_MODES` each hand-copied this exact array (the
/// linux site's own doc comment already said "Mirrors apple `NostrVM.LinkMode`
/// / android `NostrScreen.LINK_MODES` exactly") — collapsed here since both
/// are Rust-native and already depend on this crate; apple/android stay
/// per-platform (a different language, same as [`nostr_link_mode_label`]'s
/// own web/apple/android carve-out below).
pub const NOSTR_LINK_MODE_GENERATE: &str = "generate";
pub const NOSTR_LINK_MODE_IMPORT: &str = "import";
pub const NOSTR_LINK_MODE_REMOTE: &str = "remote";
pub const NOSTR_LINK_MODES: [&str; 3] = [
    NOSTR_LINK_MODE_GENERATE,
    NOSTR_LINK_MODE_IMPORT,
    NOSTR_LINK_MODE_REMOTE,
];

/// Map a Nostr link-*request* mode (`generate`/`import`/`remote`/`nip07` —
/// what the `nostr-link-mode` picker sends to `fauna.bridges.link`) to its
/// human label — the request-mode twin of [`nostr_key_source_label`] (the
/// *stored*-mode label). `remote`'s label reuses the stored-mode
/// `nostr.account.mode_remote` key rather than mint a duplicate — the two
/// vocabularies want the same English text for that one option.
///
/// Shared so linux/tui/android/windows/apple/web — every app with a
/// `nostr-link-mode` picker (`docs/goal/ui/nostr.md` § Account linking) —
/// stop each carrying its own `generate`/`import`/`remote` (native five) or
/// `t.nostr.link_account.*` (web) match. `nip07` is web's own fourth,
/// web-only option (§ Architectural rules 4), also covered here. Unrecognized
/// input renders verbatim, mirroring [`nostr_key_source_label`].
pub fn nostr_link_mode_label(mode: &str) -> LocalizedText {
    match mode {
        "generate" => LocalizedText::key("nostr.link_account.generate"),
        "import" => LocalizedText::key("nostr.link_account.import_nsec"),
        "remote" => LocalizedText::key("nostr.account.mode_remote"),
        "nip07" => LocalizedText::key("nostr.link_account.nip07"),
        other => LocalizedText::key(other),
    }
}

/// A follow's display name — the petname if set and non-blank, else the raw
/// external id (the Nostr `follow_display` convention). Never falls back to a
/// `handle`: the typed [`BridgeFollow`] wire type carries no such field.
/// Shared so linux and tui — both of which render a follows list — can't
/// drift on the "trim before trusting" rule (priority #1: same concept, same
/// fallback, everywhere).
pub fn follow_display(f: &BridgeFollow) -> String {
    match &f.petname {
        Some(p) if !p.trim().is_empty() => p.clone(),
        _ => f.id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn follow(id: &str, petname: Option<&str>) -> BridgeFollow {
        BridgeFollow {
            id: id.to_string(),
            petname: petname.map(str::to_string),
            created_at: None,
            extra: None,
            unknown_keys: Default::default(),
        }
    }

    #[test]
    fn follow_display_prefers_a_non_blank_petname_over_the_raw_id() {
        assert_eq!(
            follow_display(&follow("did:plc:abc", Some("Alice"))),
            "Alice"
        );
        assert_eq!(follow_display(&follow("did:plc:abc", None)), "did:plc:abc");
        // A whitespace-only petname is treated as unset, not as a blank label.
        assert_eq!(
            follow_display(&follow("did:plc:abc", Some("   "))),
            "did:plc:abc"
        );
    }

    #[test]
    fn nostr_key_source_label_maps_every_stored_mode() {
        // Every value the nest's `link` handler stores into `users.signing_mode`
        // (bridge_provider.rs) carries its `nostr.account.mode_*` key.
        assert_eq!(
            nostr_key_source_label("generated").key,
            "nostr.account.mode_generated"
        );
        assert_eq!(
            nostr_key_source_label("imported").key,
            "nostr.account.mode_imported"
        );
        assert_eq!(
            nostr_key_source_label("remote").key,
            "nostr.account.mode_remote"
        );
        assert_eq!(
            nostr_key_source_label("nip07").key,
            "nostr.account.mode_nip07"
        );
        assert_eq!(
            nostr_key_source_label("proxied").key,
            "nostr.account.mode_proxied"
        );
        // Unknown / placeholder → verbatim (no i18n entry, `resolve` echoes it),
        // preserving the prior raw-display behavior.
        let dash = nostr_key_source_label("—");
        assert_eq!(dash.key, "—");
        assert_eq!(dash.resolve(|_| None::<&str>), "—");
        // A would-be link-*request* mode is NOT a stored value → also verbatim,
        // guarding against keying the wrong (input) vocabulary.
        assert_eq!(nostr_key_source_label("generate").key, "generate");
    }

    #[test]
    fn nostr_link_mode_label_maps_every_request_mode() {
        assert_eq!(
            nostr_link_mode_label("generate").key,
            "nostr.link_account.generate"
        );
        assert_eq!(
            nostr_link_mode_label("import").key,
            "nostr.link_account.import_nsec"
        );
        // Reuses the *stored*-mode key deliberately — see the doc comment.
        assert_eq!(
            nostr_link_mode_label("remote").key,
            "nostr.account.mode_remote"
        );
        assert_eq!(
            nostr_link_mode_label("nip07").key,
            "nostr.link_account.nip07"
        );
        // Unknown → verbatim, same fallback shape as nostr_key_source_label.
        assert_eq!(nostr_link_mode_label("bogus").key, "bogus");
        // A would-be *stored* mode is NOT a request value → also verbatim,
        // guarding against keying the wrong (output) vocabulary.
        assert_eq!(nostr_link_mode_label("generated").key, "generated");
    }
}
