//! Platform-registered identifiers — the strings the OS, not Fauna, keys on.
//!
//! An identifier belongs here when **the operating system or a store registers
//! it** and it therefore **persists on a user's machine** independently of our
//! code: app-group container names, launchd labels, package-receipt ids. Those
//! are exactly the strings a rename cannot fix retroactively, so they get one
//! owner instead of a literal per call site.
//!
//! Internal source namespacing is deliberately NOT here — the UniFFI Kotlin
//! `package_name` (`com.fauna.ffi`) and android's `com.fauna.app` Java package
//! stay on the legacy domain by ratified decision
//! (`docs/goal/architecture/installers/android.md` § Store identity: "renaming
//! every source file buys nothing, and the two are independent by design").
//!
//! Owner docs: `docs/goal/architecture/installers/macos.md` (the `.pkg`
//! component + launchd tier) and `docs/goal/architecture/apps/macos.md`
//! § Entitlements (the app-group tier).

use std::path::{Path, PathBuf};

/// The Apple Developer **Team ID** — the signing-identity prefix macOS keys
/// app-group authorization on. Take it from `security find-identity -v` (the
/// identities read `Fauna Social (7457N3M72H)`), NEVER from the enrollment
/// correspondence: the enrollment id there looks the part and burned
/// one measurement draft (`installers/macos.md` § Identifier domain).
pub const APPLE_TEAM_ID: &str = "7457N3M72H";

/// The shared apple app-group id — the base, UNPREFIXED spelling, used
/// verbatim by **iOS** (iOS never takes a Team-ID prefix). The ONE
/// state-unification home the app, the `Fauna-FileProvider` extension, and the
/// per-user `fauna-sync-agent` converge on, and the access group its shared
/// `FileProviderCredentialStore` keychain items live in. On macOS every
/// OS-registered surface spells it [`APPLE_MACOS_APP_GROUP`] instead.
///
/// Independent copies of the two spellings exist by necessity outside Rust —
/// the entitlements plists, the appexes' `NSExtensionFileProviderDocumentGroup`,
/// and the Swift `AppleIdentifiers` mirror — because a plist cannot reference a
/// constant. `tests/e2e-unified/tests/test_apple_identifier_pins.py` is what
/// keeps them equal; add any new site there.
pub const APPLE_APP_GROUP: &str = "group.social.fauna.shared";

/// The **macOS** app-group id: [`APPLE_APP_GROUP`] prefixed with
/// [`APPLE_TEAM_ID`] (pinned equal by test below).
///
/// Since macOS 15 the group container is TCC-protected
/// (`kTCCServiceSystemPolicyAppData`): prompt-free access requires Mac App
/// Store deployment, a Team-ID-prefixed group id, or an embedded provisioning
/// profile — and only the prefix can cover the `.pkg`'s bare
/// `/usr/local/bin/fauna-sync-agent`, since a non-bundled binary can carry no
/// profile. Decided by the first-signed-build measurement matrix
/// (`installers/macos.md` § Identifier domain, 2026-08-23): arm (a) — signed,
/// unprefixed — still prompted; arm (b) — signed, this id — is the shape.
pub const APPLE_MACOS_APP_GROUP: &str = "7457N3M72H.group.social.fauna.shared";

/// The watchOS widget/complication app group. Separate from
/// [`APPLE_APP_GROUP`] because the watch app is a distinct provisioning
/// container, not because the two ever disagree about the domain.
pub const APPLE_WATCHKIT_APP_GROUP: &str = "group.social.fauna.watchkit";

/// The **macOS account-credential keychain access group** — the group
/// `KeychainStore`'s data-protection-keychain rows (the actor secret, node
/// URL, device id, and every other account-scoped row) live in, and which
/// ONLY the app is entitled to (`Fauna-macOS.entitlements`; never an appex).
///
/// Why a second, app-only group rather than [`APPLE_MACOS_APP_GROUP`]: on
/// macOS a Developer-ID-signed, non-App-Store app reaches the data-protection
/// keychain only through an access group its entitlements grant, and an app
/// group is the one entitlement class that ad-hoc dev builds *tolerate*
/// (`keychain-access-groups` is restricted — it gets any profile-less build
/// killed at launch). ⚠ **MEASURED 2026-08-28: Developer ID does NOT honour an
/// app group as a data-protection-keychain access group without an embedded
/// provisioning profile** — the entitlement is silently ignored and the store
/// falls back to the legacy plane (the real signed app reads `write plane:
/// legacy`; `installers/macos.md` § Identifier domain owns the measurement, the
/// why, and the provisioning-profile crux). So this group is the *intended*
/// home for the DP-plane rows, but reaching that plane at all is still owed.
/// But the SHARED group is also the sandboxed File
/// Provider extension's, so rows there would be readable by the extension —
/// and the extension must never hold the identity seed (`on-demand-files.md`
/// § Apple File Provider binding, *the extension hosts the engines*: it is
/// handed `BackupKey` + a bearer, never the seed). The account rows therefore
/// get a group no appex entitlement names — least privilege by entitlement,
/// the boundary iOS gets for free from its per-app default access group.
/// Keychain-only: no process ever resolves a container directory for it.
///
/// macOS only. iOS's `KeychainStore` rows stay in the app's own default
/// (application-identifier) access group, which already excludes the appex.
/// The pin test (`test_apple_identifier_pins.py`) asserts the app names it
/// and that no extension does.
pub const APPLE_MACOS_ACCOUNT_KEYCHAIN_GROUP: &str = "7457N3M72H.group.social.fauna.account";

/// The apple app's **bundle id** — one id across the Apple family
/// (`installers/macos.md` § Identifier domain); immutable once a build is
/// uploaded to the store record.
pub const APPLE_BUNDLE_ID: &str = "social.fauna.fauna";

/// The linux app's **application id** — the GtkApplication id, the Flatpak
/// app-id, the AppStream id and the basename of the one XDG desktop entry
/// every linux channel installs (`<id>.desktop`; `apps/linux.md`
/// § Desktop Entry). The app registers under it and the sync agent launches the
/// app through that entry when a banner is tapped. `fauna-linux`'s
/// `packaging_identity_test.rs` pins the files that re-type it.
pub const LINUX_APP_ID: &str = "social.fauna.fauna";

/// The android app's Play **application id** — the permanent store identity
/// (`installers/android.md` § Store identity; the Gradle namespace
/// `com.fauna.app` is deliberately not it). Play Integrity names it as the
/// verdict's package, so it is the `application_id` the attested age claim
/// signs over (`family-safety.md` § The account age band, D5). Pinned against
/// `apps/fauna-android/app/build.gradle.kts` by test below.
pub const ANDROID_APPLICATION_ID: &str = "social.fauna.fauna";

/// The Fauna Kids android app's Play **application id** — its own Play record
/// (a Families-policy listing), never a suffix of [`ANDROID_APPLICATION_ID`]
/// (`family-safety.md` § The account age band, the kids-app bullet, item (2)).
/// The kids build signs its age claim over this id and Play names it in the
/// verdict, under the kids record's own response-encryption keys.
pub const ANDROID_KIDS_APPLICATION_ID: &str = "social.fauna.faunakids";

/// The iOS app's **App ID** — [`APPLE_TEAM_ID`] + [`APPLE_BUNDLE_ID`] (pinned
/// equal by test below): the RP-ID App Attest hashes into its `authData`, and
/// so the `application_id` the attested age claim signs over
/// (`fauna_protocol::age::FAUNA_IOS_APP_ID`, `family-safety.md` § The account
/// age band, D5).
pub const APPLE_IOS_APP_ID: &str = "7457N3M72H.social.fauna.fauna";

/// The Fauna Kids iOS app's **bundle id** — its own store record, not a
/// second spelling of [`APPLE_BUNDLE_ID`]: a Kids-Category listing cannot
/// share an identifier with the 13+ main listing (`family-safety.md` § The
/// account age band, the kids-app bullet, item (2); `installers/ios.md`).
pub const APPLE_KIDS_BUNDLE_ID: &str = "social.fauna.faunakids";

/// The Fauna Kids iOS app's **App ID** — [`APPLE_TEAM_ID`] +
/// [`APPLE_KIDS_BUNDLE_ID`] (pinned equal by test below). App Attest signs
/// under the RUNNING bundle's App ID, so this — not [`APPLE_IOS_APP_ID`] — is
/// the RP-ID a kids-app attestation hashes and the `application_id` its age
/// claim signs over (`fauna_protocol::age::FAUNA_IOS_KIDS_APP_ID`).
pub const APPLE_IOS_KIDS_APP_ID: &str = "7457N3M72H.social.fauna.faunakids";

/// Whether `p` is absolute by POSIX rules (`/`-rooted) — unlike
/// [`Path::is_absolute`], which on Windows additionally requires a drive
/// prefix and so says `false` for a plain `/Users/alice`. This crate models
/// macOS paths (always POSIX-shaped) even when compiled and unit-tested on
/// Windows, where the native check would silently reject every legitimate
/// POSIX-absolute test input.
pub fn is_posix_absolute(p: &Path) -> bool {
    p.to_str().is_some_and(|s| s.starts_with('/'))
}

/// `<home>/Library/Group Containers/<group>` — the macOS container root for
/// [`APPLE_MACOS_APP_GROUP`] (macOS resolves a Team-ID-prefixed group id to a
/// container directory of exactly that prefixed name).
///
/// ⚠ **No Rust process resolves STATE under this path any more (2026-08-25).**
/// `~/Library/Group Containers` is `kTCCServiceSystemPolicyAppData`-protected on
/// macOS 15+, and a launchd-spawned background process is prompted there on
/// every instance with no user decision ever binding the next one
/// (`installers/macos.md` § Identifier domain, record item 5) — so the sync
/// agent's data root, the account store and the pin store all live under
/// [`apple_user_domain_home`] instead. The container is the sandboxed File
/// Provider extension's root, reached only by the extension and the
/// (unsandboxed, TCC-silent) app that stewards it. This resolver is kept as
/// the one Rust spelling of that path for the pin test and for tooling.
///
/// **`None` for a missing, empty, or relative home — never a derived path.** The
/// result when present is always ABSOLUTE (every consumer runs under launchd
/// with a cwd of `/`, where a relative base dir panics the first thing that
/// touches it), and there is deliberately NO world-writable fallback such as
/// `/tmp`: another local account could pre-create it and read or plant the
/// per-user state beneath it. Callers refuse with a logged
/// reason instead. Callers pass `home` in rather than reading the
/// environment, so tests never mutate process env. Absoluteness is POSIX
/// absoluteness ([`is_posix_absolute`]), not [`Path::is_absolute`] — see there.
pub fn apple_group_container(home: Option<PathBuf>) -> Option<PathBuf> {
    Some(
        apple_absolute_home(home)?
            .join("Library/Group Containers")
            .join(APPLE_MACOS_APP_GROUP),
    )
}

/// `<home>/Library/Application Support` — the macOS **user-domain** home every
/// non-sandboxed Fauna process shares without a consent gate: the app, the
/// per-user sync agent, and fauna-tui. Per-product state hangs off it as
/// `Fauna/…` (`Fauna/sync` the platform state base, `Fauna/trust` the
/// install-scoped pin store), the exact shape linux (`<config root>/fauna`) and
/// windows (`%LOCALAPPDATA%\Fauna`) already use. Unlike the group container it
/// is NOT under `kTCCServiceSystemPolicyAppData` — measured, not assumed: the
/// 2026-08-23 matrix bisected `~/Library/Application Support/Fauna` out as a
/// confound (`installers/macos.md` § Identifier domain, record item 3).
///
/// Same absoluteness guarantee and same refusal (`None`, no `/tmp` fallback)
/// as [`apple_group_container`].
pub fn apple_user_domain_home(home: Option<PathBuf>) -> Option<PathBuf> {
    Some(apple_absolute_home(home)?.join("Library/Application Support"))
}

/// The shared absolute-home floor of the two resolvers above: the home when it
/// is POSIX-absolute, else `None`.
fn apple_absolute_home(home: Option<PathBuf>) -> Option<PathBuf> {
    home.filter(|h| is_posix_absolute(h))
}

/// The one refusal text every consumer of the floors below logs (or panics
/// with) when no absolute home exists — the no-shared-fallback rule of
/// `account-data-plane.md` § The account store → Placement.
pub const NO_HOME_REFUSAL: &str = "no usable absolute home directory (HOME is unset, empty or relative and no absolute XDG_CONFIG_HOME is set): refusing to place per-user Fauna state under a shared fallback such as /tmp";

/// `$XDG_CONFIG_HOME`, falling back to `$HOME/.config` — always absolute, and
/// `None` when neither is (a missing, empty or relative `HOME` with no usable
/// `XDG_CONFIG_HOME` is REFUSED, never mapped to `/tmp`), the
/// shared floor under linux's and other unix's `<config root>/fauna[/…]`
/// resolvers — the XDG counterpart to [`apple_absolute_home`], which plays
/// the same role for the two apple resolvers above. Unlike
/// [`xdg_app_config_dir`] below, this never returns `None`: those callers
/// need an always-usable directory to create/use, not a "no persistence
/// configured" signal.
///
/// Absoluteness is POSIX absoluteness ([`is_posix_absolute`]), not
/// [`Path::is_absolute`] — callers feed it `XDG_CONFIG_HOME`/`HOME`-shaped
/// POSIX paths that are unit-tested on every dev box including Windows,
/// where the native check rejects a plain `/home/u` for lack of a drive
/// prefix.
pub fn xdg_config_root(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let absolute_or_none =
        |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| is_posix_absolute(p));
    match absolute_or_none(xdg_config_home) {
        Some(xdg) => Some(xdg),
        None => Some(absolute_or_none(home)?.join(".config")),
    }
}

/// `$XDG_CONFIG_HOME/<app>`, falling back to `$HOME/.config/<app>` — the unix
/// counterpart to [`apple_user_domain_home`]'s macOS resolution, `home`'s
/// windows analog. `app` is the caller's own namespace (linux's `fauna`,
/// tui's `fauna-tui` — deliberately distinct so the two never contend for one
/// pin file on a box running both, per `apps/fauna-tui/src/session.rs`'s
/// `config_dir` doc comment). `None` only when neither `XDG_CONFIG_HOME` nor
/// `HOME` is set — callers decide their own no-persistence fallback, unlike
/// the apple resolvers above, which always have an OS-mandated cwd to answer
/// to. Callers pass the env values in rather than reading them here, so tests
/// never mutate process env.
pub fn xdg_app_config_dir(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    app: &str,
) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|v| !v.is_empty()) {
        Some(xdg) => PathBuf::from(xdg),
        None => PathBuf::from(home?).join(".config"),
    };
    Some(base.join(app))
}

/// The per-user, per-OS **sync state base** — the root the account store, the
/// sync agent's state and the headless credential files live under
/// (`fauna_account_store::root` has the table and the reasons):
/// `%LOCALAPPDATA%\Fauna\sync` on windows,
/// `~/Library/Application Support/Fauna/sync` on macOS,
/// `$XDG_CONFIG_HOME/fauna/sync` (else `~/.config/fauna/sync`) on linux and
/// other unix. Always absolute; `None` without an absolute home, never a
/// `/tmp` fallback ([`NO_HOME_REFUSAL`]), and `None` on a target with no
/// filesystem home (wasm). Lifted here from `fauna-account-store` so
/// `fauna-credential-store`, which must resolve the account-store slot's
/// headless file the same way for its writer and its erase, reads the same
/// path without linking the store.
pub fn sync_state_base() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| r"C:\ProgramData".into());
        Some(PathBuf::from(local).join("Fauna").join("sync"))
    }
    // `std::env::home_dir` (not `env::var("HOME")`) so an unset HOME falls
    // back to the passwd database.
    #[cfg(target_os = "macos")]
    {
        macos_sync_state_base(std::env::home_dir())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        unix_sync_state_base(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Pure user-domain resolution of [`sync_state_base`] on macOS (home passed
/// in, so tests never mutate process env):
/// `<home>/Library/Application Support/Fauna/sync`. **Never the app-group
/// container** — `~/Library/Group Containers` is TCC-protected on macOS 15+
/// and a launchd-spawned agent is prompted on every instance there
/// (`installers/macos.md` § Identifier domain, item 5).
pub fn macos_sync_state_base(home: Option<PathBuf>) -> Option<PathBuf> {
    Some(apple_user_domain_home(home)?.join("Fauna").join("sync"))
}

/// Pure XDG resolution of [`sync_state_base`] on linux and other unix (env
/// values passed in): `$XDG_CONFIG_HOME/fauna/sync`, else
/// `$HOME/.config/fauna/sync`. An empty or relative value counts as unset.
pub fn unix_sync_state_base(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    Some(
        xdg_config_root(xdg_config_home, home)?
            .join("fauna")
            .join("sync"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_sync_state_base_prefers_xdg_then_home_and_is_always_absolute() {
        // Value pin: the SAME `~/.config/fauna/sync` the in-app GTK
        // `SyncDriver` (`apps/fauna-linux/src/sync.rs::sync_state_dir`) and
        // the sync agent resolve — one root per user per machine.
        assert_eq!(
            unix_sync_state_base(Some("/run/user/1000/xdg".into()), None).unwrap(),
            Path::new("/run/user/1000/xdg/fauna/sync")
        );
        assert_eq!(
            unix_sync_state_base(None, Some("/home/alice".into())).unwrap(),
            Path::new("/home/alice/.config/fauna/sync")
        );
        // Empty and relative values count as unset.
        assert_eq!(
            unix_sync_state_base(Some("".into()), Some("/home/alice".into())).unwrap(),
            Path::new("/home/alice/.config/fauna/sync")
        );
    }

    #[test]
    fn unix_sync_state_base_refuses_a_missing_empty_or_relative_home_instead_of_tmp() {
        // The old `/tmp/.config/fauna/sync` fallback is a
        // world-writable base another local account can pre-create.
        for home in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(unix_sync_state_base(None, home.clone()), None);
            assert_eq!(unix_sync_state_base(Some("rel".into()), home), None);
        }
    }

    #[test]
    fn macos_sync_state_base_is_the_user_domain_sync_dir_and_always_absolute() {
        assert_eq!(
            macos_sync_state_base(Some("/Users/alice".into())).unwrap(),
            Path::new("/Users/alice/Library/Application Support/Fauna/sync")
        );
        // The one place the base must NEVER resolve: the TCC-protected
        // app-group container (a launchd agent is prompted per instance there,
        // and no user decision binds the next one — the 2026-08-25 record).
        assert!(
            !macos_sync_state_base(Some("/Users/alice".into()))
                .unwrap()
                .to_string_lossy()
                .contains("Group Containers")
        );
        // A missing/empty/relative home is refused — no `/tmp` base.
        for home in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(macos_sync_state_base(home), None);
        }
    }

    #[test]
    fn app_groups_are_on_the_org_domain() {
        // The whole point of the 2026-08-12 sweep: no apple identifier the OS
        // registers may sit on a domain the org does not own.
        for id in [APPLE_APP_GROUP, APPLE_WATCHKIT_APP_GROUP] {
            assert!(
                id.starts_with("group.social.fauna."),
                "{id} is not on the org domain"
            );
        }
    }

    #[test]
    fn macos_group_is_the_team_prefixed_base() {
        // The macOS spelling is a DERIVATION, not a third id: Team ID + the
        // base group, nothing else. iOS uses the base verbatim.
        assert_eq!(
            APPLE_MACOS_APP_GROUP,
            format!("{APPLE_TEAM_ID}.{APPLE_APP_GROUP}")
        );
    }

    #[test]
    fn ios_app_id_is_the_team_prefixed_bundle_id() {
        assert_eq!(
            APPLE_IOS_APP_ID,
            format!("{APPLE_TEAM_ID}.{APPLE_BUNDLE_ID}")
        );
    }

    #[test]
    fn ios_kids_app_id_is_the_team_prefixed_kids_bundle_id() {
        assert_eq!(
            APPLE_IOS_KIDS_APP_ID,
            format!("{APPLE_TEAM_ID}.{APPLE_KIDS_BUNDLE_ID}")
        );
        assert_ne!(APPLE_IOS_KIDS_APP_ID, APPLE_IOS_APP_ID);
    }

    #[test]
    fn android_application_ids_match_the_gradle_build() {
        // The nest's Play arm pins these; the Gradle build is what Play sees.
        let gradle = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../apps/fauna-android/app/build.gradle.kts"),
        )
        .expect("read the android app's build.gradle.kts");
        assert!(gradle.contains(&format!("applicationId = \"{ANDROID_APPLICATION_ID}\"")));
        assert!(gradle.contains(&format!(
            "applicationId.set(\"{ANDROID_KIDS_APPLICATION_ID}\")"
        )));
        assert_ne!(ANDROID_KIDS_APPLICATION_ID, ANDROID_APPLICATION_ID);
        assert!(!ANDROID_KIDS_APPLICATION_ID.starts_with(&format!("{ANDROID_APPLICATION_ID}.")));
    }

    #[test]
    fn account_keychain_group_is_team_prefixed_org_domain_and_not_the_shared_one() {
        // Same authorization model as the container group (a Developer-ID app
        // is entitled to a Team-ID-prefixed group), same org domain — and a
        // DIFFERENT group from the one the File Provider appex is entitled to,
        // which is the entire point: the identity seed must not be readable
        // from the extension's access group.
        assert!(APPLE_MACOS_ACCOUNT_KEYCHAIN_GROUP.starts_with(&format!("{APPLE_TEAM_ID}.")));
        assert!(
            APPLE_MACOS_ACCOUNT_KEYCHAIN_GROUP
                .trim_start_matches(&format!("{APPLE_TEAM_ID}."))
                .starts_with("group.social.fauna.")
        );
        assert_ne!(APPLE_MACOS_ACCOUNT_KEYCHAIN_GROUP, APPLE_MACOS_APP_GROUP);
    }

    #[test]
    fn container_is_refused_without_an_absolute_home() {
        // No `/tmp` fallback: a world-writable base is refused.
        for home in [
            None,
            Some(PathBuf::from("")),
            Some(PathBuf::from("rel/ative")),
        ] {
            assert_eq!(apple_group_container(home), None);
        }
    }

    #[test]
    fn container_hangs_off_the_passed_home() {
        assert_eq!(
            apple_group_container(Some(PathBuf::from("/Users/alice"))),
            Some(PathBuf::from(
                "/Users/alice/Library/Group Containers/7457N3M72H.group.social.fauna.shared"
            ))
        );
    }

    #[test]
    fn user_domain_home_is_application_support_and_never_the_container() {
        let home = apple_user_domain_home(Some(PathBuf::from("/Users/alice"))).unwrap();
        assert_eq!(
            home,
            PathBuf::from("/Users/alice/Library/Application Support")
        );
        assert!(!home.to_string_lossy().contains("Group Containers"));
        for home in [
            None,
            Some(PathBuf::from("")),
            Some(PathBuf::from("rel/ative")),
        ] {
            assert_eq!(apple_user_domain_home(home), None);
        }
    }

    #[test]
    fn xdg_config_root_prefers_absolute_xdg_then_home() {
        assert_eq!(
            xdg_config_root(Some("/x/cfg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/cfg"))
        );
        // Relative/empty XDG counts as unset.
        for xdg in [Some("rel".into()), Some("".into()), None] {
            assert_eq!(
                xdg_config_root(xdg, Some("/home/u".into())),
                Some(PathBuf::from("/home/u/.config"))
            );
        }
    }

    #[test]
    fn xdg_config_root_is_refused_without_an_absolute_home() {
        // No `/tmp/.config` fallback: nothing absolute → refusal.
        for home in [None, Some("".into()), Some("rel".into())] {
            assert_eq!(xdg_config_root(None, home.clone()), None);
            assert_eq!(xdg_config_root(Some("rel".into()), home), None);
        }
    }

    #[test]
    fn xdg_app_config_dir_prefers_xdg_config_home() {
        assert_eq!(
            xdg_app_config_dir(
                Some("/x/config".into()),
                Some("/home/alice".into()),
                "fauna-tui",
            ),
            Some(PathBuf::from("/x/config/fauna-tui"))
        );
    }

    #[test]
    fn xdg_app_config_dir_falls_back_to_home_dot_config() {
        assert_eq!(
            xdg_app_config_dir(None, Some("/home/alice".into()), "fauna"),
            Some(PathBuf::from("/home/alice/.config/fauna"))
        );
        // An empty XDG_CONFIG_HOME counts as unset, same as a missing one.
        assert_eq!(
            xdg_app_config_dir(Some("".into()), Some("/home/alice".into()), "fauna"),
            Some(PathBuf::from("/home/alice/.config/fauna"))
        );
    }

    #[test]
    fn xdg_app_config_dir_is_none_without_either_var() {
        assert_eq!(xdg_app_config_dir(None, None, "fauna"), None);
    }

    #[test]
    fn xdg_app_config_dir_namespaces_by_app() {
        // The whole reason this takes `app` rather than hard-coding one: tui
        // and linux resolve different subdirs off the same base so they never
        // contend for one pin file on a box running both.
        let base = Some("/home/alice".into());
        assert_ne!(
            xdg_app_config_dir(None, base.clone(), "fauna-tui"),
            xdg_app_config_dir(None, base, "fauna"),
        );
    }
}
