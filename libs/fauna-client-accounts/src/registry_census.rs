//! The registry-construction census every Rust app pins its choke point with
//! (`long-term-store.md` § Multi-account evolution → Cross-process mutation
//! lock: *"one construction site per client decides locking for all of its
//! writers"*). Each app's own `account_registry_census_test` names its choke
//! file and calls [`assert_only_the_choke_point_constructs`]; the scan and the
//! rule live here once, so linux and tui cannot drift on what counts.
//!
//! This is a source-level check on purpose, because the failure it guards is
//! invisible at runtime: a registry built with `AccountRegistry::new` carries
//! the no-op lock, so a writer minted that way silently skips the file lock
//! every other writer takes. Nothing observable fails — the write succeeds,
//! the UI is happy, and two concurrent instances quietly lose an update to the
//! shared `fauna/index` blob. There is no assertion a normal unit test could
//! make about a bypass, because the bypass *is* the absence of behavior.
//!
//! That class already bit once: 19 direct constructions had accumulated in
//! linux behind a rustdoc claiming the choke point was "THE one place" —
//! caught by a security review in 2026-07, not by anyone reading the code.
//!
//! **The choke point also decides how WIDE the erase is.** A registry built
//! through `fauna_credential_store::account_registry{,_with_lock}` also sweeps
//! the shared `fauna-account-store` namespace (the store writer key, each
//! account's principal bundle); one built through `AccountRegistry::new`
//! deletes those keys in the app's own namespace, where they were never
//! written, and the real rows survive the sign-out (`long-term-store.md`
//! § Cleanup contract). So the census counts both constructor families: the
//! bare ones are the offence, the choke point's are the census's own subject.
//!
//! Test code is counted too, deliberately: a test that mints its own registry
//! exercises a construction production never makes. Route tests through the
//! app's choke point (or a store-taking variant beside it) like production.

use std::path::{Path, PathBuf};

/// One constructing call found by the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConstructionSite {
    /// The file, as found under the scanned `src/`.
    pub path: PathBuf,
    /// 1-based line.
    pub line: usize,
    /// The constructor matched (`AccountRegistry::new(`, …).
    pub needle: String,
}

/// The four constructing calls. Assembled from parts so a file holding this
/// census never contains the literals it searches for.
fn needles() -> [String; 4] {
    let ty = "AccountRegistry";
    let erase = "fauna_credential_store";
    [
        format!("{ty}::new("),
        format!("{ty}::with_mutation_lock("),
        format!("{erase}::account_registry("),
        format!("{erase}::account_registry_with_lock("),
    ]
}

/// Every `.rs` file under `src`, recursively, sorted.
fn rust_sources(src: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
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
    let mut out = Vec::new();
    walk(src, &mut out);
    out.sort();
    out
}

/// Every constructing call under `src`, comments stripped so a rustdoc mention
/// of a constructor is documentation, not a call site.
pub fn registry_construction_sites(src: &Path) -> Vec<RegistryConstructionSite> {
    let needles = needles();
    let mut sites = Vec::new();
    for path in rust_sources(src) {
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
                    sites.push(RegistryConstructionSite {
                        path: path.clone(),
                        line: idx + 1,
                        needle: needle.clone(),
                    });
                }
            }
        }
    }
    sites
}

/// The census rule, both halves: no file under `src` but `choke_file` (a file
/// name, e.g. `"main.rs"`) constructs a registry, and inside `choke_file` at
/// most two constructions stand — the choke point's locked build and its
/// degrade fallback. A third means someone added a writer beside the choke
/// point that skips the lock, which the file-scoped half cannot see.
///
/// `choke_point` names the function to route through, for the failure text.
///
/// # Panics
/// With the offending `file:line`s when either half fails — this IS the test.
pub fn assert_only_the_choke_point_constructs(src: &Path, choke_file: &str, choke_point: &str) {
    let sites = registry_construction_sites(src);
    let in_choke_file = |s: &RegistryConstructionSite| {
        s.path.file_name().and_then(|n| n.to_str()) == Some(choke_file)
    };
    let root = src.parent().unwrap_or(src);

    let offenders: Vec<String> = sites
        .iter()
        .filter(|s| !in_choke_file(s))
        .map(|s| {
            let shown = s.path.strip_prefix(root).unwrap_or(&s.path).display();
            format!("  {shown}:{} — {}…)", s.line, s.needle)
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{} construction(s) bypass {choke_point}, so their writers are silently \
         no-op-locked while every other writer takes the file lock, and their erase \
         never reaches the account-store namespace (long-term-store.md § Cross-process \
         mutation lock, § Cleanup contract). Route them through {choke_point} — reads \
         cost nothing extra (a read never locks); if you need a registry over a \
         different store, add the variant beside it so both decisions stay in one \
         file:\n{}",
        offenders.len(),
        offenders.join("\n")
    );

    let at_choke_point: Vec<usize> = sites
        .iter()
        .filter(|s| in_choke_file(s))
        .map(|s| s.line)
        .collect();
    assert!(
        !at_choke_point.is_empty(),
        "no registry construction left in {choke_file} — if the choke point moved, \
         point the census at its new home rather than deleting it"
    );
    assert!(
        at_choke_point.len() <= 2,
        "{choke_file} builds {} registries (lines {at_choke_point:?}); only \
         {choke_point} may — the locked build and its degrade fallback. Anything \
         else is a writer that skips the cross-process lock.",
        at_choke_point.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src_with(files: &[(&str, &str)]) -> tempfile_free::Dir {
        tempfile_free::Dir::new(files)
    }

    /// A scratch `src/` under the target-local temp dir, removed on drop —
    /// this crate carries no `tempfile` dependency.
    mod tempfile_free {
        use std::path::PathBuf;

        pub struct Dir(pub PathBuf);

        impl Dir {
            pub fn new(files: &[(&str, &str)]) -> Self {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let root = std::env::temp_dir()
                    .join(format!("registry-census-{}-{n}", std::process::id()));
                let _ = std::fs::remove_dir_all(&root);
                let src = root.join("src");
                for (name, body) in files {
                    let path = src.join(name);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, body).unwrap();
                }
                Self(src)
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                if let Some(root) = self.0.parent() {
                    let _ = std::fs::remove_dir_all(root);
                }
            }
        }
    }

    fn bare() -> String {
        format!("let r = {}::new(store);\n", "AccountRegistry")
    }

    fn locked() -> String {
        format!(
            "Some(b) => {}::account_registry_with_lock(s, l),\nNone => {}::account_registry(s),\n",
            "fauna_credential_store", "fauna_credential_store"
        )
    }

    #[test]
    fn the_choke_point_alone_passes() {
        let dir = src_with(&[("session.rs", &locked()), ("other.rs", "fn f() {}\n")]);
        assert_only_the_choke_point_constructs(&dir.0, "session.rs", "registry()");
    }

    #[test]
    #[should_panic(expected = "nested/w.rs:1")]
    fn a_bypass_in_another_file_is_named_by_file_and_line() {
        let dir = src_with(&[("session.rs", &locked()), ("nested/w.rs", &bare())]);
        assert_only_the_choke_point_constructs(&dir.0, "session.rs", "registry()");
    }

    #[test]
    #[should_panic(expected = "builds 3 registries")]
    fn a_third_builder_inside_the_choke_file_fails() {
        let dir = src_with(&[("session.rs", &format!("{}{}", locked(), bare()))]);
        assert_only_the_choke_point_constructs(&dir.0, "session.rs", "registry()");
    }

    #[test]
    fn a_commented_mention_is_not_a_call_site() {
        let dir = src_with(&[
            ("session.rs", &locked()),
            ("doc.rs", &format!("// {}", bare())),
        ]);
        assert_only_the_choke_point_constructs(&dir.0, "session.rs", "registry()");
    }
}
