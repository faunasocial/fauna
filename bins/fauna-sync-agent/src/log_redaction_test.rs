//! The agent's log scrub (`docs/goal/behavior/path-sealing.md` § The log +
//! error-string scrub): no `tracing` macro in any production half of this
//! crate's `src/*.rs` carries a user path as a bare field — it goes through
//! `fauna_core::log_redact::log_path`. A source scan, because the pin and
//! free-up-space lines sit behind OS seams (CFAPI, FUSE, the pipe) the unit
//! fakes cannot reach, and `cfapi_host.rs` does not compile on this host at
//! all; `test_folder_location_free_space.py` proves one line end to end.
//!
//! A bound set's name (`EngineMapping.folder` and its kin, a user-chosen
//! name) is held the same way: it goes through
//! `fauna_core::log_redact::log_folder_name`, which keeps a reserved `__` name
//! literal and hashes every other.
//!
//! The cleared classes are the named `file:shape` entries in [`ALLOWED`], each
//! with its reason; anything else that matches reddens the test.
//!
//! Error strings are held the same way
//! ([`no_agent_error_string_carries_a_bare_name_or_path`]): every message an
//! error constructor builds is scanned with the engine pin's own
//! spelling-independent scanner (`fauna_core::log_redact_scan`), its cleared
//! classes in [`ERROR_STRING_ALLOWLIST`].

use std::path::Path;

/// `(file, field text, why it is cleared)`. A field is a macro's top-level
/// comma-separated argument, trimmed.
///
/// The three below are internal roots the agent itself owns — the IPC socket,
/// the custodian store and the TLS trust directory — never a user-chosen name
/// inside a bound folder. `path-sealing.md` § The log + error-string scrub
/// rules the custodian/account stores' `.display()` sites "confirmed out of
/// scope" (keyed by `actor_id_hex` or a fixed subdirectory, reaching only the
/// agent's own per-user log); the socket and trust directory are the same
/// class (fixed install-scoped subdirectories).
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "service.rs",
        "socket = %socket_path.display()",
        "the agent's own IPC socket path, an install-scoped internal root",
    ),
    (
        "custodian.rs",
        "store = %state.paths.flat_base_dir().display()",
        "custodian store root: path-sealing.md § The log + error-string scrub, out of scope",
    ),
    (
        "trust.rs",
        "trust_dir = %dir.display()",
        "the install-scoped TLS trust directory, an internal root",
    ),
];

/// Field names that carry a bound-root path or a path under one.
const PATH_NAMES: &[&str] = &[
    "rel",
    "parent_rel",
    "path",
    "abs",
    "abs_path",
    "full",
    "mount_point",
];

/// Field names that carry a bound set's name.
const NAME_NAMES: &[&str] = &["folder", "folder_name", "set_name"];

/// A set-name field: named like one, or a bare expression naming one or
/// ending in a `.folder` access. A `log_path`-wrapped value under the `folder`
/// name is a path already redacted, so any `log_redact` helper clears it.
fn name_shaped(field: &str) -> bool {
    if ["log_folder_name", "log_path", "log_hash_prefix"]
        .iter()
        .any(|h| field.contains(h))
        || field.starts_with('"')
    {
        return false;
    }
    let (name, value) = match field.split_once('=') {
        Some((n, v)) => (n.trim(), v.trim()),
        None => ("", field),
    };
    let value = value.trim_start_matches(['%', '?', '&']);
    NAME_NAMES.contains(&name) || NAME_NAMES.contains(&value) || value.ends_with(".folder")
}

fn path_shaped(field: &str) -> bool {
    if field.contains("log_path") || field.starts_with('"') {
        return false;
    }
    let name = field
        .split(['=', ' '])
        .next()
        .unwrap_or("")
        .trim_start_matches(['%', '?']);
    PATH_NAMES.contains(&name) || field.contains(".display()") || field.contains(".path")
}

/// Every top-level field of every `tracing::<level>!(…)` macro in `prod`, with
/// the 1-based line the macro starts on.
fn log_fields(prod: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (at, _) in prod.match_indices("tracing::") {
        let rest = &prod[at..];
        let Some(open) = rest.find("!(") else {
            continue;
        };
        if rest[..open].contains(char::is_whitespace) {
            continue;
        }
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in rest[open + 1..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + 1 + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &rest[open + 2..end.expect("unbalanced tracing macro")];
        let line = prod[..at].matches('\n').count() + 1;
        let (mut d, mut start) = (0usize, 0usize);
        for (i, c) in body.char_indices() {
            match c {
                '(' | '[' | '{' => d += 1,
                ')' | ']' | '}' => d = d.saturating_sub(1),
                ',' if d == 0 => {
                    out.push((line, body[start..i].trim().to_owned()));
                    start = i + 1;
                }
                _ => {}
            }
        }
        out.push((line, body[start..].trim().to_owned()));
    }
    out
}

#[test]
fn no_agent_log_macro_carries_a_bare_user_path() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut used = vec![false; ALLOWED.len()];
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let file = path.file_name().unwrap().to_str().unwrap().to_owned();
        // This file quotes the shapes it bans.
        if file == "log_redaction_test.rs" {
            continue;
        }
        let full = std::fs::read_to_string(&path).unwrap();
        let prod = match full.find("\n#[cfg(test)]\nmod tests {") {
            Some(cut) => &full[..cut],
            None => &full,
        };
        for (line, field) in log_fields(prod) {
            if !path_shaped(&field) && !name_shaped(&field) {
                continue;
            }
            if let Some(i) = ALLOWED
                .iter()
                .position(|(f, s, _)| *f == file && *s == field)
            {
                used[i] = true;
                continue;
            }
            offenders.push(format!("{file}:{line}: {field}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a tracing macro carries a bare user path or set name — wrap it in \
         `fauna_core::log_redact::log_path` / `log_folder_name` or add a reasoned `ALLOWED` entry:\n{}",
        offenders.join("\n")
    );
    let stale: Vec<_> = ALLOWED
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|((f, s, _), _)| format!("{f}: {s}"))
        .collect();
    assert!(
        stale.is_empty(),
        "stale ALLOWED entries:\n{}",
        stale.join("\n")
    );
}

/// The error constructors whose whole body is a message.
const ERROR_MACROS: &[&str] = &["anyhow!(", "bail!(", "ensure!("];

/// The error-building calls whose message is a `format!` inside them: the
/// agent's IPC handlers answer `Err(String)`, mostly built in a `map_err` /
/// `ok_or_else` closure. A bare `format!` elsewhere builds names and URLs, so
/// only one under these is scanned.
const ERROR_CALLS: &[&str] = &[
    "Err(",
    "map_err(",
    "ok_or_else(",
    "ok_or(",
    "context(",
    "with_context(",
];

/// The cleared name/path-shaped values inside an agent error string:
/// `(file, a substring of the message body, why it is not a user-chosen name
/// or path)`. One shape in one file per entry; a stale entry reddens the test.
const ERROR_STRING_ALLOWLIST: &[(&str, &str, &str)] = &[
    (
        "config.rs",
        "reading {}",
        "the agent's own config file, an install-scoped internal root",
    ),
    (
        "config.rs",
        "refusing to overwrite {}",
        "the agent's own config file, an install-scoped internal root",
    ),
    (
        "pipe_server.rs",
        "another sync-agent instance holds {name}",
        "the agent's own named pipe, an install-scoped internal name",
    ),
];

/// Every error-message body in `prod`, with its line: each [`ERROR_MACROS`]
/// body, and each `format!` body under an [`ERROR_CALLS`] call.
fn error_message_bodies(prod: &str) -> std::collections::BTreeSet<(usize, String)> {
    use fauna_core::log_redact_scan::macro_bodies;
    let mut out: std::collections::BTreeSet<_> =
        macro_bodies(prod, ERROR_MACROS).into_iter().collect();
    for (line, call) in macro_bodies(prod, ERROR_CALLS) {
        for (at, body) in macro_bodies(&call, &["format!("]) {
            out.insert((line + at - 1, body));
        }
    }
    out
}

/// The error-string half of the scrub: no agent error message carries a
/// set name or a path that has not been through a `log_*` wrapper. The IPC
/// reply is not a sealed channel — the agent logs some of these strings
/// itself (the pull-now nudge's ambiguous-name warning) and the host apps log
/// a failed request's error.
#[test]
fn no_agent_error_string_carries_a_bare_name_or_path() {
    use fauna_core::log_redact_scan::leaked_words;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut used = vec![false; ERROR_STRING_ALLOWLIST.len()];
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let file = path.file_name().unwrap().to_str().unwrap().to_owned();
        // This file quotes the shapes it bans; `lib.rs` gates every
        // `*_integration.rs` module on `cfg(test)`, so its errors are test
        // assertions, never a reply.
        if file == "log_redaction_test.rs" || file.ends_with("_integration.rs") {
            continue;
        }
        let full = std::fs::read_to_string(&path).unwrap();
        let prod = match full.find("\n#[cfg(test)]\nmod tests {") {
            Some(cut) => &full[..cut],
            None => &full,
        };
        for (line, body) in error_message_bodies(prod) {
            let leaked = leaked_words(&body);
            if leaked.is_empty() {
                continue;
            }
            match ERROR_STRING_ALLOWLIST
                .iter()
                .position(|(f, shape, _)| *f == file && body.contains(shape))
            {
                Some(i) => used[i] = true,
                None => offenders.push(format!("{file}:{line}: {leaked:?}: {}", body.trim())),
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "an agent error string carries a bare set name or path — wrap it in \
         `fauna_core::log_redact::log_folder_name` / `log_path`, or add a reasoned \
         `ERROR_STRING_ALLOWLIST` entry if it is not user-chosen:\n{}",
        offenders.join("\n")
    );
    let stale: Vec<_> = ERROR_STRING_ALLOWLIST
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|((f, s, _), _)| format!("{f}: {s:?}"))
        .collect();
    assert!(
        stale.is_empty(),
        "stale ERROR_STRING_ALLOWLIST entries:\n{}",
        stale.join("\n")
    );
}

/// The error-body finder itself: a `format!` under each call spelling and each
/// macro body is found; a wrapped value passes.
#[test]
fn the_error_scan_sees_every_constructor() {
    use fauna_core::log_redact_scan::leaked_words;
    for bad in [
        r#"fn f() { return Err(format!("folder '{folder}' is gone")); }"#,
        r#"fn f() { x.map_err(|e| match e { A => format!("pending for '{folder}'") })?; }"#,
        r#"fn f() { x.ok_or_else(|| format!("{path} is not bound"))?; }"#,
        r#"fn f() { anyhow::bail!("x {rel}"); }"#,
        r#"fn f() { r.with_context(|| format!("listing {}", dir.display()))?; }"#,
    ] {
        assert!(
            error_message_bodies(bad)
                .iter()
                .any(|(_, b)| !leaked_words(b).is_empty()),
            "{bad} must be flagged"
        );
    }
    let good = r#"fn f() { return Err(format!("'{}' gone", log_folder_name(folder))); let n = format!("{folder}"); }"#;
    assert!(
        error_message_bodies(good)
            .iter()
            .all(|(_, b)| leaked_words(b).is_empty()),
        "wrapped values, and a format! outside an error call, must pass"
    );
}
