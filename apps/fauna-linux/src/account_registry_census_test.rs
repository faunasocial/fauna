//! The registry-construction census: `main.rs::account_registry()` is the
//! **only** place linux builds an [`fauna_client_accounts::AccountRegistry`].
//!
//! This is a source-level test on purpose, because the failure it guards is
//! invisible at runtime. Mutation serialization is decided **at construction**
//! (`long-term-store.md` § Multi-account evolution → Cross-process mutation
//! lock: *"one construction site per client decides locking for all of its
//! writers"*): a registry built with `AccountRegistry::new` carries the no-op
//! lock, so a writer minted that way silently skips the file lock every other
//! writer takes. Nothing observable fails — the write succeeds, the UI is
//! happy, and two concurrent instances quietly lose an update to the shared
//! `fauna/index` blob. There is no assertion a normal unit test could make
//! about a bypass, because the bypass *is* the absence of behavior.
//!
//! That class already bit once: 19 direct constructions had accumulated behind
//! a rustdoc claiming the choke point was "THE one place" — caught by a
//! security review in 2026-07, not by anyone reading the code. Adopting the
//! lock by editing the choke point alone would have left every one of them
//! Noop-locked. So the census is the pin: add a writer that mints its own
//! registry and this test names your file and line.
//!
//! **Since 2026-09-01 the choke point decides a second invisible thing: how
//! WIDE the erase is.** A registry built through
//! `fauna_credential_store::account_registry{,_with_lock}` also sweeps the
//! shared `fauna-account-store` namespace, where this machine's store writer
//! key and each account's principal bundle live; one built through
//! `AccountRegistry::new` deletes those keys in linux's own `fauna-desktop`
//! namespace, where they were never written, and the real rows survive the
//! sign-out (`long-term-store.md` § Cleanup contract). That failure is even
//! quieter than the lock one — the deletes all "succeed". So the census counts
//! both constructor families: the bare ones are the offence, the choke point's
//! are the census's own subject.

/// The type whose construction is being counted. Assembled from parts at
/// runtime so this file never contains the literal it searches for — otherwise
/// the census would flag itself.
const REGISTRY_TYPE: &str = "AccountRegistry";

/// The crate owning the erase-complete constructors the choke point uses.
/// Same self-flagging dodge as [`REGISTRY_TYPE`]: the needles are assembled, so
/// no full literal appears in this file.
const ERASE_CTOR_CRATE: &str = "fauna_credential_store";

/// The file allowed to construct one: linux's choke point.
const CHOKE_POINT_FILE: &str = "main.rs";

/// Every `.rs` file under this crate's `src/`, recursively.
fn crate_sources() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&src, &mut out);
    out.sort();
    out
}

/// `file.rs:LINE` for every constructing call, comments stripped so a rustdoc
/// mention of a constructor is documentation, not a call site.
fn construction_sites() -> Vec<(std::path::PathBuf, usize, String)> {
    let needles = [
        format!("{REGISTRY_TYPE}::new("),
        format!("{REGISTRY_TYPE}::with_mutation_lock("),
        format!("{ERASE_CTOR_CRATE}::account_registry("),
        format!("{ERASE_CTOR_CRATE}::account_registry_with_lock("),
    ];
    let mut sites = Vec::new();
    for path in crate_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (idx, line) in text.lines().enumerate() {
            let code = match line.find("//") {
                Some(at) => &line[..at],
                None => line,
            };
            for needle in &needles {
                if code.contains(needle.as_str()) {
                    sites.push((path.clone(), idx + 1, needle.clone()));
                }
            }
        }
    }
    sites
}

/// No file but the choke point may construct a registry.
///
/// Fix a failure by calling `crate::account_registry()` instead of minting
/// your own — reads cost nothing extra (a read never locks), writers get the
/// cross-process lock for free, and the erase gets the account-store namespace
/// for free. If you genuinely need a registry over a *different* store, add the
/// variant beside `account_registry()` so both decisions stay in one file.
#[test]
fn only_the_choke_point_constructs_the_account_registry() {
    let offenders: Vec<String> = construction_sites()
        .into_iter()
        .filter(|(path, _, _)| path.file_name().and_then(|n| n.to_str()) != Some(CHOKE_POINT_FILE))
        .map(|(path, line, needle)| {
            let shown = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(&path)
                .display()
                .to_string();
            format!("  {shown}:{line} — {needle}…)")
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "{} construction(s) bypass main.rs::account_registry(), so their writers \
         are silently no-op-locked while every other writer takes the file lock, \
         and their erase never reaches the account-store namespace \
         (long-term-store.md § Cross-process mutation lock, § Cleanup contract). \
         Route them through crate::account_registry():\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

/// …and inside the choke point's own file, construction stays confined to the
/// choke point *function* — the locked build plus its degrade fallback, and
/// nothing else.
///
/// The file-scoped rule above cannot see a `main.rs` writer that mints its own
/// registry, and `main.rs` is exactly where the launch/switch writers live, so
/// without this the census would have passed while five of them stayed
/// unlocked. Two is the budget because `account_registry()` picks between the
/// file lock and the no-op lock; a third means someone added a builder beside
/// it.
#[test]
fn the_choke_point_is_the_only_builder_in_its_own_file_too() {
    let at_choke_point: Vec<usize> = construction_sites()
        .into_iter()
        .filter(|(path, _, _)| path.file_name().and_then(|n| n.to_str()) == Some(CHOKE_POINT_FILE))
        .map(|(_, line, _)| line)
        .collect();
    assert!(
        !at_choke_point.is_empty(),
        "no registry construction left in {CHOKE_POINT_FILE} — if the choke point moved, \
         point CHOKE_POINT_FILE at its new home rather than deleting this census"
    );
    assert!(
        at_choke_point.len() <= 2,
        "{CHOKE_POINT_FILE} builds {} registries (lines {at_choke_point:?}); only \
         account_registry() may — the locked build and its degrade fallback. Anything \
         else is a writer that skips the cross-process lock.",
        at_choke_point.len()
    );
}
