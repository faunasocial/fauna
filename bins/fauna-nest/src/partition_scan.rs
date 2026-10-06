//! The scanner behind this crate's **partition gates** — the tests that turn a
//! standing "enrol the next caller" duty into something that fails on the day
//! the caller is written.
//!
//! A partition gate names one or more **primitives** (a flag-blind read, a
//! handle onto the blob store) and a table classifying every production call
//! site of them. This module walks `src/` the way those tables must be read —
//! `#[cfg(test)]` items cut, the enclosing `fn` resolved — and asserts the two
//! directions that make the table a gate rather than a comment: nothing found
//! that the table does not classify, and nothing classified that no longer
//! calls the primitive.
//!
//! It exists because there are two such gates (`segments::post`'s over the
//! flag-blind post-body reads, `backup::service`'s over the blob store) and a
//! second hand-rolled scanner is a second set of rules for reading the tree —
//! a difference the gates would express as a disagreement about which callers
//! exist.
//!
//! A table that only lists callers proves a decision was recorded, not that it
//! is true: an entry saying "gated by X" stayed green with the call of X
//! deleted, and only the door's own witness went red. So a serving entry also names its [`Gate`], and
//! [`assert_gates_hold`] holds the code to it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How a serving partition entry proves its gate is in the code, not only in
/// the table.
pub(crate) enum Gate {
    /// The entry's own production body calls one of these — each a literal
    /// including its opening paren, matched the way [`callers_of`] matches.
    Calls(&'static [&'static str]),
    /// The entry calls no gate itself: it is a private helper reached ONLY
    /// from these fns of its own file, each of which must be a
    /// [`Gate::Calls`] entry of the same table.
    OnlyFrom(&'static [&'static str]),
    /// The entry calls no gate itself, and its callers are no entries of the
    /// table: it is reached ONLY from these `(path under src/, fn)` callers,
    /// anywhere in the crate, and each of those callers' production bodies
    /// calls one of `gates` — a door decided one fn up, in another file.
    Upstream {
        callers: &'static [(&'static str, &'static str)],
        gates: &'static [&'static str],
    },
    /// The entry calls no predicate itself: its own body names a **seam** —
    /// a fn that does not withhold anything but reaches, one or more hops
    /// down, the fn that does. `hops` is that chain, nearest-to-the-entry
    /// first, each a `(path under src/, fn)`: the entry's own production
    /// body must call `hops[0]`'s fn (matched the way [`Gate::Calls`]
    /// matches), each `hops[i]`'s production body must in turn call
    /// `hops[i + 1]`'s fn the same way, and the LAST hop's production body
    /// must call one of `gates`.
    ///
    /// A `Gate::Calls` literal naming a seam proves only that the seam was
    /// reached, not that anything downstream still withholds — a named seam
    /// held to nothing let one hop's predicate go unchecked and unwitnessed
    /// . Unlike [`Gate::Upstream`], which
    /// holds a caller one fn UP, this holds the chain one or more fns DOWN.
    Via {
        hops: &'static [(&'static str, &'static str)],
        gates: &'static [&'static str],
    },
}

/// The crate's `src/`, the root every partition key is relative to.
fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `dir`, recursively.
pub(crate) fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A source file's lines with every `#[cfg(test)]` item blanked out
/// (indices preserved), so callers inside test code are not counted.
///
/// An item gated on `#[cfg(test)]` — a `mod …_tests {`, a helper `fn`, an
/// `impl` — runs from the attribute (plus any further attributes) to the
/// closing `}` at the item's own indentation, which rustfmt guarantees and
/// the merge gate enforces; a one-line item (`mod tests;`, a `use`) is just
/// that line. Files keep several such modules, interleaved with production
/// code, so this cannot simply cut at the first one.
pub(crate) fn production_lines(src: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "#[cfg(test)]" {
            i += 1;
            continue;
        }
        let mut item = i + 1;
        while item < lines.len() && lines[item].trim_start().starts_with("#[") {
            item += 1;
        }
        let Some(head) = lines.get(item).copied() else {
            break;
        };
        // A signature may span lines: the item opens (`{`) or ends (`;`)
        // on the first line that closes it.
        let opener = (item..lines.len())
            .find(|&j| {
                let t = lines[j].trim_end();
                t.ends_with('{') || t.ends_with(';')
            })
            .unwrap_or_else(|| panic!("#[cfg(test)] item at line {} never opens", i + 1));
        let end = if lines[opener].trim_end().ends_with('{') {
            let indent = &head[..head.len() - head.trim_start().len()];
            let close = format!("{indent}}}");
            // Never guess: blanking past a missing close would hide
            // production callers from the gate, the one unsafe direction.
            (opener + 1..lines.len())
                .find(|&j| lines[j].trim_end() == close)
                .unwrap_or_else(|| panic!("#[cfg(test)] item at line {} never closes", i + 1))
        } else {
            opener
        };
        for line in &mut lines[i..=end] {
            *line = "";
        }
        i = end + 1;
    }
    lines
}

/// The name of the `fn` whose body line `at` sits in: the nearest
/// non-comment line at or above it that declares one.
pub(crate) fn enclosing_fn(lines: &[&str], at: usize) -> Option<String> {
    lines[..=at].iter().rev().find_map(|line| {
        let t = line.trim_start();
        if t.starts_with("//") {
            return None;
        }
        let bytes = line.as_bytes();
        line.match_indices("fn ").find_map(|(i, _)| {
            if i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                return None;
            }
            let name: String = line[i + 3..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
    })
}

/// Every production call site of `primitives`, as `(path under src/, enclosing
/// fn)` — the key both partition tables use.
///
/// A `primitive` is matched as a literal including its opening paren. A match
/// whose preceding token is `fn` is the **definition**, not a call, and a
/// match continuing a longer identifier is another fn's call — `restore_post(`
/// never calls `store_post(`.
pub(crate) fn callers_of(primitives: &[&str]) -> BTreeSet<(String, String)> {
    let src_root = src_root();
    let mut files = Vec::new();
    rs_files(&src_root, &mut files);

    let mut found = BTreeSet::new();
    for path in &files {
        let rel = path
            .strip_prefix(&src_root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(path).expect("read source");
        let lines = production_lines(&src);
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            if !calls_any(line, primitives) {
                continue;
            }
            let func = enclosing_fn(&lines, i)
                .unwrap_or_else(|| panic!("{rel}:{}: no enclosing fn found", i + 1));
            found.insert((rel.clone(), func));
        }
    }
    found
}

/// Whether `line` calls one of `primitives`, by [`callers_of`]'s rules.
fn calls_any(line: &str, primitives: &[&str]) -> bool {
    fn ident(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_'
    }
    primitives.iter().any(|p| {
        let opens_on_ident = p.starts_with(ident);
        line.match_indices(p).any(|(at, _)| {
            let before = &line[..at];
            !before.trim_end().ends_with("fn") && !(opens_on_ident && before.ends_with(ident))
        })
    })
}

/// The two directions that make a partition table a gate.
///
/// `unclassified_help` is the gate's own guidance — what the author of a new
/// caller has to decide — appended to the failure that names them.
pub(crate) fn assert_partitioned(
    found: &BTreeSet<(String, String)>,
    table: &BTreeSet<(String, String)>,
    unclassified_help: &str,
) {
    let unclassified: Vec<_> = found.difference(table).collect();
    assert!(
        unclassified.is_empty(),
        "a partitioned primitive has a caller nobody has classified: \
         {unclassified:?}. {unclassified_help}"
    );
    let stale: Vec<_> = table.difference(found).collect();
    assert!(
        stale.is_empty(),
        "the partition lists callers that no longer call a primitive: {stale:?} — \
         remove them, so the table only ever describes the code as it is"
    );
}

/// The production body of each `fn func` declared in `rel` (a path under
/// `src/`), comment lines dropped — one entry per declaration, so a name
/// declared twice in one file is held to its gate in both places.
fn fn_bodies(rel: &str, func: &str) -> Vec<Vec<String>> {
    let src =
        std::fs::read_to_string(src_root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let lines = production_lines(&src);
    let decl = format!("fn {func}");
    let mut bodies = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let declares = line.match_indices(decl.as_str()).any(|(at, _)| {
            (at == 0 || line.as_bytes()[at - 1].is_ascii_whitespace())
                && matches!(line[at + decl.len()..].chars().next(), Some('(' | '<'))
        });
        if !declares {
            continue;
        }
        // Read the way `production_lines` reads an item: it opens on the
        // first line ending `{` (a signature may span lines) and closes at `}`
        // on its own indentation; a `;` first is a declaration with no body.
        let Some(opener) = (i..lines.len()).find(|&j| {
            let t = lines[j].trim_end();
            t.ends_with('{') || t.ends_with(';')
        }) else {
            continue;
        };
        if lines[opener].trim_end().ends_with(';') {
            continue;
        }
        let close = format!("{}}}", &line[..line.len() - line.trim_start().len()]);
        let end = (opener + 1..lines.len())
            .find(|&j| lines[j].trim_end() == close)
            .unwrap_or_else(|| panic!("{rel}:{}: fn {func} never closes", i + 1));
        bodies.push(
            lines[opener + 1..end]
                .iter()
                .filter(|l| !l.trim_start().starts_with("//"))
                .map(|l| l.to_string())
                .collect(),
        );
    }
    bodies
}

/// The half of a partition gate that checks each serving entry against the
/// code rather than against its own table.
///
/// A [`Gate::Calls`] entry's production body must call one of its gates; a
/// [`Gate::OnlyFrom`] entry must be called from exactly the fns it names, each
/// of them a `Calls` entry of `served`. **Presence, not order**: that the gate
/// is called somewhere in the fn is what a line scanner can prove, and its
/// order against the store read stays each door's own witness (the chunk
/// door's relay-arm test is the reference). The grain is the fn, as it is for
/// the census — a classified fn that grows a second read is caught by review
/// of its entry, not here.
pub(crate) fn assert_gates_hold(served: &[(&str, &str, &Gate)]) {
    for (file, func, gate) in served {
        match gate {
            Gate::Calls(gates) => {
                assert!(!gates.is_empty(), "{file}::{func} names no gate");
                let bodies = fn_bodies(file, func);
                assert!(
                    !bodies.is_empty(),
                    "{file}: no production `fn {func}` to hold to its gate"
                );
                for body in &bodies {
                    assert!(
                        body.iter()
                            .any(|line| gates.iter().any(|g| line.contains(g))),
                        "{file}::{func} is classified as gated by {gates:?}, but its production \
                         body calls none of them. Restore the gate call — deleting it is the \
                         bypass this check exists to catch — or, if this fn truly no longer \
                         serves a read a caller can aim, reclassify its entry and say why"
                    );
                }
            }
            Gate::OnlyFrom(callers) => {
                let needle = format!("{func}(");
                let reached = callers_of(&[needle.as_str()]);
                let expected: BTreeSet<(String, String)> = callers
                    .iter()
                    .map(|c| (file.to_string(), c.to_string()))
                    .collect();
                assert_eq!(
                    reached, expected,
                    "{file}::{func} is classified as gated by its callers {callers:?}, but it is \
                     reached from {reached:?} — a new caller carries its own gate, and this \
                     entry must name it"
                );
                for caller in *callers {
                    assert!(
                        served.iter().any(|(f, c, g)| f == file
                            && c == caller
                            && matches!(g, Gate::Calls(_))),
                        "{file}::{func} leans on {caller}'s gate, but {caller} is not an entry \
                         that calls one — a chain of callers-of-callers proves nothing"
                    );
                }
            }
            Gate::Upstream { callers, gates } => {
                assert!(!gates.is_empty(), "{file}::{func} names no gate");
                let needle = format!("{func}(");
                let reached = callers_of(&[needle.as_str()]);
                let expected: BTreeSet<(String, String)> = callers
                    .iter()
                    .map(|(f, c)| (f.to_string(), c.to_string()))
                    .collect();
                assert_eq!(
                    reached, expected,
                    "{file}::{func} is classified as gated by its callers {callers:?}, but it is \
                     reached from {reached:?} — a new caller carries its own gate, and this \
                     entry must name it"
                );
                for (caller_file, caller) in *callers {
                    let bodies = fn_bodies(caller_file, caller);
                    assert!(
                        !bodies.is_empty(),
                        "{caller_file}: no production `fn {caller}` to hold to its gate"
                    );
                    for body in &bodies {
                        assert!(
                            body.iter()
                                .any(|line| gates.iter().any(|g| line.contains(g))),
                            "{file}::{func} leans on {caller_file}::{caller} calling one of \
                             {gates:?}, but that fn's production body calls none of them. \
                             Restore the gate call, or reclassify this entry and say why"
                        );
                    }
                }
            }
            Gate::Via { hops, gates } => {
                assert!(!hops.is_empty(), "{file}::{func} names no seam");
                assert!(!gates.is_empty(), "{file}::{func} names no gate");
                let mut holder_file = *file;
                let mut holder_func = *func;
                for (hop_file, hop_func) in *hops {
                    let needle = format!("{hop_func}(");
                    let bodies = fn_bodies(holder_file, holder_func);
                    assert!(
                        !bodies.is_empty(),
                        "{holder_file}: no production `fn {holder_func}` to hold to its seam"
                    );
                    for body in &bodies {
                        assert!(
                            body.iter().any(|line| line.contains(needle.as_str())),
                            "{file}::{func} is classified as gated via the seam \
                             {holder_file}::{holder_func} -> {hop_file}::{hop_func}, but \
                             {holder_file}::{holder_func}'s production body calls no `{needle}`. \
                             Restore the call — deleting it is the bypass this check exists to \
                             catch — or reclassify this entry and say why"
                        );
                    }
                    holder_file = hop_file;
                    holder_func = hop_func;
                }
                let bodies = fn_bodies(holder_file, holder_func);
                assert!(
                    !bodies.is_empty(),
                    "{holder_file}: no production `fn {holder_func}` to hold to its gate"
                );
                for body in &bodies {
                    assert!(
                        body.iter()
                            .any(|line| gates.iter().any(|g| line.contains(g))),
                        "{file}::{func} leans on {holder_file}::{holder_func}, the end of its \
                         seam, calling one of {gates:?}, but that fn's production body calls \
                         none of them. Restore the gate call, or reclassify this entry and say \
                         why"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::calls_any;

    #[test]
    fn a_definition_is_not_a_call() {
        assert!(!calls_any("pub async fn store_post(", &["store_post("]));
        assert!(calls_any(
            "    crate::segments::post::store_post(&mgr, &db, &id, body, None)",
            &["store_post("]
        ));
    }

    /// `restore_post(` ends in `store_post(`: without the boundary, the
    /// backup restore's handler and its caller read as two writers of
    /// `content.created_at` that write nothing.
    #[test]
    fn a_match_continuing_a_longer_identifier_is_not_a_call() {
        assert!(!calls_any(
            "    restore_post(&state, &owner, blob).await",
            &["store_post("]
        ));
        assert!(!calls_any("async fn restore_post(", &["store_post("]));
        assert!(calls_any(
            "        self.db.put_post(&post_id, &payload, None).await",
            &["put_post("]
        ));
        assert!(calls_any(
            "        \"INSERT OR REPLACE INTO content (id, schema)",
            &["INTO content ("]
        ));
    }

    /// A primitive opening on punctuation names its own boundary, so it
    /// matches right after an identifier.
    #[test]
    fn a_primitive_opening_on_punctuation_matches_after_an_identifier() {
        assert!(calls_any("let body = db.get_post(&id)", &[".get_post("]));
    }
}
