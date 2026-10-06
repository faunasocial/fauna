//! The packaging identity pin: **one app id, spelled the same in every file
//! that ships it.**
//!
//! `APP_ID` is not just a Rust constant — the same string is independently
//! re-typed into the Flatpak manifest's `app-id`, the AppStream metainfo
//! `<id>`, that metainfo's `<launchable>`, and the installed desktop entry's
//! basename. Nothing at build time makes those agree, and nothing at runtime
//! complains when they do not: the app launches, the window opens, and only
//! the *shell* notices — a metainfo whose `<launchable>` names a desktop file
//! the package never installs is a software-centre entry with no launch
//! button, and a GApplication id matching no desktop-entry basename is a
//! window the shell cannot associate with its icon.
//!
//! That is exactly the class this test was written for. Before the
//! `social.fauna.fauna` convergence (2026-08-22) the tree carried **two**
//! metainfo files declaring **different** ids, and the deb channel shipped a
//! metainfo whose `<launchable>` pointed at `social.fauna.fauna.desktop`
//! while the package installed `fauna.desktop` — a launchable that had never
//! resolved on that channel. Both survived review for months because every
//! individual file reads correctly on its own; only the *agreement* is wrong,
//! and agreement is what nobody re-checks.
//!
//! Goal doc: `installers/macos.md` § Identifier domain → *How the LEAF is
//! spelled* (owner of the ratified string) and `installers/linux-desktop.md`
//! § Flatpak (owner of the sandbox grants derived from it).

use std::path::{Path, PathBuf};

/// `apps/fauna-linux/` — this crate's own root, resolved at compile time so
/// the test does not depend on the working directory cargo was invoked from.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The one metainfo file, by its ratified path.
const METAINFO: &str = "packaging/social.fauna.fauna.metainfo.xml";
/// The one Flatpak manifest.
const MANIFEST: &str = "packaging/flatpak/social.fauna.fauna.yml";

/// Read a shipped packaging file, failing with the path when it is missing —
/// a renamed file must break this test loudly rather than silently skip it.
fn read(relative: &str) -> String {
    let path = crate_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("packaging file {} is unreadable: {e}", path.display()))
}

/// The document with every XML comment span removed.
///
/// Both readers below scan for a literal tag, and this file's own header
/// comment talks *about* those tags — so without this the first `<id>` found
/// is the one inside the prose. Stripping is the fix rather than rewording the
/// comment: the next person to document a tag must not silently break the pin.
fn without_comments(xml: &str) -> String {
    const OPEN: &str = "<!--";
    const CLOSE: &str = "--";
    let close_tag = format!("{CLOSE}>");
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(&close_tag) {
            Some(end) => rest = &rest[start + end + close_tag.len()..],
            // Unterminated comment: everything left of it is commented out.
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The single `<id>` of the AppStream metainfo, extracted without an XML dep.
fn metainfo_id(xml: &str) -> String {
    let xml = without_comments(xml);
    let open = xml.find("<id>").expect("metainfo declares an <id>");
    let close = xml.find("</id>").expect("metainfo closes its <id>");
    xml[open + "<id>".len()..close].trim().to_owned()
}

/// The value of a top-level `key: value` line in the Flatpak manifest.
fn manifest_value(yml: &str, key: &str) -> String {
    yml.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}:")))
        .unwrap_or_else(|| panic!("the Flatpak manifest declares `{key}:`"))
        .trim()
        .to_owned()
}

/// Exactly ONE metainfo file may exist under this crate.
///
/// The two-file state is not a tidiness problem: Flathub requires the manifest
/// id to equal the metainfo `<id>`, so a second file declaring a second id
/// means one of them is already unpublishable — and which one is canonical
/// gets decided by whichever build recipe happens to stage it.
#[test]
fn exactly_one_metainfo_file_ships() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `target/` can hold staged copies of the real thing.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&path, out);
            } else if path.to_string_lossy().ends_with(".metainfo.xml") {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&crate_root(), &mut found);
    found.sort();
    assert_eq!(
        found.len(),
        1,
        "exactly one AppStream metainfo may ship, found {}: {:#?}",
        found.len(),
        found
    );
}

/// The Rust constant, the Flatpak app-id and the metainfo `<id>` are ONE
/// string.
#[test]
fn the_app_id_is_spelled_identically_in_rust_flatpak_and_appstream() {
    let manifest = read(MANIFEST);
    let metainfo = read(METAINFO);

    assert_eq!(
        manifest_value(&manifest, "app-id"),
        crate::APP_ID,
        "the Flatpak app-id and the GtkApplication APP_ID must be one string — \
         when they differ the sandbox refuses g_application_register() unless \
         the manifest carries an explicit --own-name grant for the difference"
    );
    assert_eq!(
        metainfo_id(&metainfo),
        crate::APP_ID,
        "Flathub requires the manifest id to equal the metainfo <id>, and both \
         to be the app's own id"
    );
}

/// The desktop entry the packages install is named `<APP_ID>.desktop`, and the
/// metainfo's `<launchable>` names that same file.
///
/// This is the leg the deb channel failed silently: `<launchable>` named a
/// file the package did not install, so the software-centre entry had no
/// launch button and nothing anywhere reported it.
#[test]
fn the_desktop_entry_basename_and_the_launchable_are_the_app_id() {
    let expected = format!("{}.desktop", crate::APP_ID);

    let entry = crate_root().join("packaging").join(&expected);
    assert!(
        entry.is_file(),
        "the shipped desktop entry must be named after the app id: {} is missing",
        entry.display()
    );

    let metainfo = without_comments(&read(METAINFO));
    let launchable = metainfo
        .lines()
        .find_map(|l| l.trim().strip_prefix("<launchable type=\"desktop-id\">"))
        .and_then(|l| l.strip_suffix("</launchable>"))
        .expect("the metainfo declares a desktop-id launchable");
    assert_eq!(
        launchable, expected,
        "the metainfo <launchable> must name the desktop entry the packages \
         actually install"
    );
}

/// The shell associates a window with its icon by matching the GApplication id
/// against a desktop entry — by basename, or by `StartupWMClass` against the
/// window's own WM_CLASS. GTK sets WM_CLASS from the binary name, which is
/// *not* the app id, so the entry must carry `StartupWMClass` explicitly.
#[test]
fn the_desktop_entry_declares_startup_wm_class() {
    let entry = read(&format!("packaging/{}.desktop", crate::APP_ID));
    let wm_class = entry
        .lines()
        .find_map(|l| l.trim().strip_prefix("StartupWMClass="))
        .map(str::trim);
    assert_eq!(
        wm_class,
        Some("fauna-desktop"),
        "without StartupWMClass matching the binary's WM_CLASS, the shell \
         cannot associate the window with this entry's icon"
    );
}

/// A trailing `.desktop`, `.app` or `.linux` leaf is unpublishable.
///
/// Flathub bans them outright, and AppStream additionally *strips* a trailing
/// `.desktop`, reading it as the legacy desktop-entry suffix — so an id
/// spelled that way is silently rewritten in software centres
/// (`org.telegram.desktop` → `org.telegram` is the canonical casualty).
/// Ratified 2026-08-22: `installers/macos.md` § Identifier domain.
#[test]
fn the_app_id_leaf_is_not_a_banned_generic_term() {
    let leaf = crate::APP_ID
        .rsplit('.')
        .next()
        .expect("the app id has a leaf");
    assert!(
        !matches!(leaf, "desktop" | "app" | "linux"),
        "Flathub bans a `.desktop`/`.app`/`.linux` leaf and AppStream strips a \
         trailing `.desktop`: {} is unpublishable",
        crate::APP_ID
    );
}

/// The leaf is lowercase.
///
/// Ratified 2026-08-22 (`installers/macos.md` § Identifier domain): the leaf is
/// the product name, lowercase, on every registry we get to choose on. It rests
/// on the written guidance of the two layers that have any — [Flatpak
/// conventions](https://docs.flatpak.org/en/latest/conventions.html) recommend a
/// lowercase leaf, and the AppStream spec *"strongly encourage[s]"* lowercase
/// component-IDs — against which stand only unwritten habits (GNOME's CamelCase,
/// Apple's capitalized doc examples). Both cases are *legal* everywhere, which is
/// exactly why this needs a test rather than a compiler: a capitalized leaf would
/// build, install and run, and only drift the id away from what humans and
/// third-party catalogs normalize to when they re-type it.
#[test]
fn the_app_id_is_lowercase() {
    assert_eq!(
        crate::APP_ID,
        crate::APP_ID.to_ascii_lowercase(),
        "the ratified leaf is lowercase (installers/macos.md § Identifier domain); \
         uppercase is legal everywhere and therefore fails silently"
    );
}

/// The per-account raise channel is a SUBNAME of the app id, so once the ids
/// agree the Flatpak manifest needs no grants for them at all.
///
/// Before the convergence the GtkApplication id and the Flatpak app-id
/// differed, which is why the manifest carried three explicit grants. With one
/// id, Flatpak's default session-bus policy already lets an app own
/// `$FLATPAK_ID` and its subnames — so any surviving `--own-name`/
/// `--talk-name` line naming the app id or its wildcard is dead weight that
/// hides the real policy from the next reader.
#[test]
fn the_manifest_carries_no_redundant_grants_for_its_own_id() {
    let manifest = read(MANIFEST);
    let redundant: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- --own-name=") || l.starts_with("- --talk-name="))
        .filter(|l| {
            l.contains(&format!("={}", crate::APP_ID))
                || l.contains(&format!("={}.", crate::APP_ID))
        })
        .collect();
    assert!(
        redundant.is_empty(),
        "Flatpak's default policy already grants $FLATPAK_ID and its subnames; \
         these lines are redundant now that the ids agree: {redundant:#?}"
    );
}
