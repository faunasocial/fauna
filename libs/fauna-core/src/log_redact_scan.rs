//! The source scanner behind every crate's [`crate::log_redact`] regression
//! pin (`docs/goal/behavior/path-sealing.md` § The log + error-string scrub):
//! find the bodies of log macros and error constructors in a `.rs` file and
//! report every name/path-shaped value left in one once each `log_*` wrapper is
//! cut out. Spelling-independent — a `field = %x`, a shorthand `x,`, a `{x}`
//! interpolation, a positional argument or `x.display()` all count.
//!
//! Test-only (`test-helpers`): the crates' pins read their own `src/` with it.

/// Identifiers that, left bare inside a log macro or an error string, carry a
/// user-chosen folder name or folder-relative path.
pub const SENSITIVE_WORDS: &[&str] = &[
    "folder",
    "path",
    "paths",
    "relative_path",
    "rel",
    "rel_path",
    "name",
    "names",
    "display_name",
    "file_name",
    "filename",
    "dir",
    "set_name",
];

/// The redaction wrappers; a call to one is cut out whole, arguments included
/// (a bare mention, as in `.map(log_folder_name)`, is cut alone).
pub const WRAPPERS: &[&str] = &["log_path", "log_folder_name", "log_hash_prefix"];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Index just past the `)` closing the paren that opens at `open` (the byte
/// index of `(`), string/char/comment aware. `None` when unbalanced.
pub fn close_of(src: &str, open: usize) -> Option<usize> {
    let b: Vec<(usize, char)> = src[open..].char_indices().collect();
    let (mut depth, mut i) = (0usize, 0usize);
    while i < b.len() {
        match b[i].1 {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + b[i].0 + 1);
                }
            }
            '"' => {
                i += 1;
                while i < b.len() && b[i].1 != '"' {
                    if b[i].1 == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '\'' => {
                // A char literal (`'"'`, `'\n'`); a lifetime has no closing quote nearby.
                let w: String = b[i..b.len().min(i + 5)].iter().map(|x| x.1).collect();
                if let Some(end) = w[1..].find('\'').filter(|e| *e <= 3) {
                    i += end + 1;
                }
            }
            '/' if b.get(i + 1).is_some_and(|x| x.1 == '/') => {
                while i < b.len() && b[i].1 != '\n' {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// A macro body reduced to its code: comments dropped, each string literal
/// reduced to the expressions it interpolates (`{x:?}` → ` x `), so prose
/// words inside a message never look like a value.
fn residue(body: &str) -> String {
    let c: Vec<char> = body.chars().collect();
    let (mut out, mut i) = (String::new(), 0usize);
    while i < c.len() {
        match c[i] {
            '"' => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    match c[i] {
                        '\\' => i += 1,
                        '{' if c.get(i + 1) == Some(&'{') => i += 1,
                        '{' => {
                            let mut j = i + 1;
                            let mut expr = String::new();
                            while j < c.len() && c[j] != '}' {
                                if c[j] == ':' && c.get(j + 1) != Some(&':') && !expr.ends_with(':')
                                {
                                    while j < c.len() && c[j] != '}' {
                                        j += 1;
                                    }
                                    break;
                                }
                                expr.push(c[j]);
                                j += 1;
                            }
                            out.push_str(&format!(" {expr} "));
                            i = j;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                out.push(' ');
            }
            '/' if c.get(i + 1) == Some(&'/') => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ch => out.push(ch),
        }
        i += 1;
    }
    out
}

/// Cut every redaction-wrapper call out of `s`.
fn without_wrappers(mut s: String) -> String {
    loop {
        let hit = WRAPPERS
            .iter()
            .filter_map(|w| s.find(w).map(|at| (at, w.len())))
            .min();
        let Some((at, wlen)) = hit else { return s };
        let name_end = at + wlen;
        let end = if s[name_end..].starts_with('(') {
            close_of(&s, name_end).unwrap_or(s.len())
        } else {
            name_end
        };
        s.replace_range(at..end, " ");
    }
}

/// The [`SENSITIVE_WORDS`] left in a macro body once wrappers,
/// interpolation-free prose and `field =` keys are removed.
pub fn leaked_words(body: &str) -> Vec<String> {
    let code = without_wrappers(residue(body));
    let c: Vec<char> = code.chars().collect();
    let mut leaked = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if !is_ident(c[i]) {
            i += 1;
            continue;
        }
        // A dotted path `a.b.c` is one unit: it is a field KEY when `=` follows it.
        let start = i;
        while i < c.len()
            && (is_ident(c[i]) || (c[i] == '.' && c.get(i + 1).is_some_and(|x| is_ident(*x))))
        {
            i += 1;
        }
        let unit: String = c[start..i].iter().collect();
        let mut j = i;
        while matches!(c.get(j), Some(' ' | '\n')) {
            j += 1;
        }
        let is_key = c.get(j) == Some(&'=')
            && c.get(j + 1) != Some(&'=')
            && start
                .checked_sub(1)
                .is_none_or(|p| !matches!(c[p], '%' | '?' | '&' | '.' | ':'));
        if is_key {
            continue;
        }
        for word in unit.split('.') {
            if SENSITIVE_WORDS.contains(&word) {
                leaked.push(word.to_owned());
            }
        }
    }
    leaked
}

/// Every body of one of `openers` (each ending in `(`, e.g. `"warn!("` or
/// `"map_err("`) in `src`, with its 1-based line. An opener glued to a
/// preceding identifier character (`xwarn!(`) is not a match.
pub fn macro_bodies(src: &str, openers: &[&str]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for opener in openers {
        for (at, _) in src.match_indices(opener) {
            if src[..at].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let open = at + opener.len() - 1;
            if let Some(end) = close_of(src, open) {
                let line = src[..at].matches('\n').count() + 1;
                out.push((line, src[open + 1..end - 1].to_owned()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One body per spelling a fixed-shape list could not see.
    #[test]
    fn the_scanner_sees_every_spelling() {
        for bad in [
            r#"folder = %folder, "x""#,
            r#"folder, "x""#,
            r#""pull {folder}""#,
            r#"folder = %req.folder, "x""#,
            r#"path = %path.display(), "x""#,
            r#""({}) failed", folder"#,
        ] {
            assert!(!leaked_words(bad).is_empty(), "{bad} must be flagged");
        }
        for good in [
            r#"folder = %fauna_core::log_redact::log_folder_name(&req.folder), "x""#,
            r#"folder = %folder_r, "folder {folder_r}: done""#,
            r#"path = %log_path(&path.to_string_lossy()), "x""#,
            r#""the folder name was not found in the path list""#,
        ] {
            assert!(leaked_words(good).is_empty(), "{good} must pass");
        }
    }

    #[test]
    fn bodies_are_found_whole_and_glued_openers_skipped() {
        let src = "a\nwarn!(\"x {}\", f(y)); xwarn!(z);";
        assert_eq!(
            macro_bodies(src, &["warn!("]),
            [(2, "\"x {}\", f(y)".to_owned())]
        );
    }
}
