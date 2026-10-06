//! Archive path components — the one place a user-supplied name becomes part of
//! a path in the export archive.
//!
//! Authority: `docs/goal/behavior/mail-export.md` § Format choices → *Mailbox
//! names in archive paths* (the injective encoding) and → *Archive paths on the
//! extracting filesystem* (everything [`ComponentNamer`] adds to it).
//!
//! # Two layers
//!
//! [`encode_path_component`] is the context-free encoding: safe, injective, and
//! the identity on every ordinary name. It guarantees two mailboxes never share
//! a path **inside the archive**.
//!
//! [`ComponentNamer`] guarantees they never share one **once extracted**. A zip
//! holds `Work.mbox` beside `work.mbox` happily; default APFS, HFS+, NTFS and
//! FAT then write the second over the first. No context-free rule can prevent
//! that and leave ordinary names alone — of `Work` and `work` at most one can
//! be its own path always, and a rule that picks without seeing the other
//! mangles `INBOX` and `Sent` in every archive to defend against a pair almost
//! no user has. So the namer is per-run state: it remembers the
//! [`extraction_key`] of every component it has handed out and walks a name up
//! an **escape ladder** until the key is free —
//!
//! 1. the plain encoding;
//! 2. the plain encoding with its first verbatim character escaped as well
//!    (`work` → `%77ork` — still readable, and the shape the Maildir++
//!    `subscriptions` exception always had);
//! 3. every character escaped but ASCII digits and the folded `/`.
//!
//! Input arrives in the § Container shape total order, so which mailbox of a
//! colliding pair keeps its plain name is a function of the exported *set*,
//! never of timing: same input, same scope → the same bytes. What it costs is
//! that a mailbox's path depends on what it was exported beside.
//!
//! **Decoding stays context-free** on every rung — `.` is `/`, `%XX` is that
//! byte — because the ladder only ever escapes *more* characters. That is why
//! escaping beats suffixing (`work-2`): the name is still recovered exactly.
//!
//! The same acceptance test carries the two sub-rules. A component whose stem
//! is a Windows device name is never acceptable, so `Aux` climbs to `%41ux`
//! whatever else is exported; and every rung is first cut to
//! [`MAX_COMPONENT_BYTES`] — the one step that is *not* decodable, marked
//! `%~` so no reader mistakes the result for a whole name.

use std::collections::HashSet;

use unicode_normalization::UnicodeNormalization;

use super::ExportError;

/// Longest component the namer hands out, in UTF-8 bytes. Most filesystems cap
/// a component at 255 bytes (NTFS at 255 UTF-16 units, which is never more);
/// the margin is for the `.mbox` the mbox serializer appends and for the
/// sidecar suffixes MUAs add beside an imported file (`.msf`, `.sbd`).
pub(super) const MAX_COMPONENT_BYTES: usize = 240;

/// What a cut component ends in: this marker, then [`CUT_DIGEST_HEX`] hex
/// characters of BLAKE3 over the raw name. `%` followed by a non-hex character
/// occurs nowhere else in the encoding, so a cut name can never be read as a
/// whole one.
const CUT_MARKER: &str = "%~";
/// 128 bits, not the 64 of [`super::helpers::message_digest16`]: the names are chosen by
/// whoever named the source mailboxes — a migration auto-creates a source
/// server's names — and a 64-bit digest yields a chosen pair at a birthday
/// cost of about 2^32 hashes. At 2^64 the refusal in
/// [`ComponentNamer::name`] stays out of reach.
const CUT_DIGEST_HEX: usize = 32;
const CUT_SUFFIX_BYTES: usize = CUT_MARKER.len() + CUT_DIGEST_HEX;

/// How much of a name [`encode`] escapes — the rungs of the ladder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rung {
    Plain,
    FirstEscaped,
    AllEscaped,
}

/// Encode one user-supplied name as a single archive path component that is
/// both **safe** and **injective** (§ Format choices → *Mailbox names in
/// archive paths*).
///
/// The rules, applied per character:
///
/// - `/`, the IMAP hierarchy delimiter, becomes `.` — the Maildir++ nesting
///   fold (`Work/Reports` → `Work.Reports`) — except as the name's first or
///   last character or right after another `/`, where it is `%2F`, so the
///   output never holds `..` whatever names the nest lets through;
/// - `%` (the escape character itself), a literal `.` (otherwise
///   indistinguishable from a folded `/`), `\`, the reserved `:` `*` `?`
///   `"` `<` `>` `|`, and every ASCII control byte (`0x00`–`0x1F`, `0x7F`)
///   become `%XX`, the byte in uppercase hex;
/// - every non-ASCII character whose compatibility decomposition holds `/` or
///   one of those characters — the fullwidth twins (`／` `％` `．` `＼` `：` …),
///   the small forms (`﹒` `﹨` `﹕` …), the dot leaders (`․` `‥` `…`) — becomes
///   `%XX` per UTF-8 byte, because a best-fit extractor narrows it back to the
///   ASCII it decomposes to, and so do the separator look-alikes no
///   decomposition names (`¥` `₩` `∕` `⁄` `∖` `⧵`);
/// - a space as the name's last character becomes `%20`;
/// - everything else, non-ASCII included, is kept verbatim;
/// - the empty name becomes a lone `%`.
///
/// **Injective** because decoding is unambiguous — `.` is `/`, `%XX` is that
/// byte, a lone `%` is the empty name, anything else is itself — so two
/// distinct names can never share a path. This replaced a many-to-one fold
/// (`/`,`\` → `.`, reserved → `_`, controls dropped, trailing `.`/space
/// stripped) under which two mailboxes could land on one path and the
/// container refused the whole export.
///
/// **Safe** by construction, with no separate prefixing or stripping step:
/// the output never contains `/` or `\` (nor a look-alike an extractor
/// could narrow to one), is never empty, and never begins or ends with `.` nor
/// holds two in a row (a `.` is only ever one *interior* folded `/`, and no
/// kept character decomposes to one), so no component — nor any piece of one
/// an unforeseen separator mapping splits off — can read as `..`, before or
/// after compatibility narrowing; and it never ends with a space or a `.`,
/// which Windows would silently drop on extraction.
///
/// Context-free, so it says nothing about what two components do to each other
/// once *extracted* — mailbox names go through [`ComponentNamer`] for that.
/// Called directly for the archive root, whose handle is `[a-z0-9-]`, and for
/// the same handle inside § Compression wrapper's download file name — one
/// encoding for both, so a handle that is not a safe path component cannot be
/// safe in the archive and unsaveable as the file holding it.
pub fn encode_path_component(name: &str) -> String {
    encode(name, Rung::Plain)
}

fn encode(name: &str, rung: Rung) -> String {
    if name.is_empty() {
        return "%".to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut first_escape_owed = rung == Rung::FirstEscaped;
    let mut after_slash = false;
    for (at, ch) in name.char_indices() {
        let first = at == 0;
        let last = at + ch.len_utf8() == name.len();
        let must_escape = match ch {
            '/' => first || last || after_slash,
            ' ' => last,
            c if ALWAYS_ESCAPED.contains(&c) => true,
            c => c.is_ascii_control() || narrows_to_an_escape(c) || is_separator_look_alike(c),
        };
        after_slash = ch == '/';
        // An interior `/` is the hierarchy fold on every rung: it is a `.`, and
        // never the "first verbatim character" the second rung spends.
        if ch == '/' && !must_escape {
            out.push('.');
            continue;
        }
        let ladder_escape = !must_escape
            && match rung {
                Rung::Plain => false,
                Rung::FirstEscaped => std::mem::take(&mut first_escape_owed),
                Rung::AllEscaped => !ch.is_ascii_digit(),
            };
        if must_escape || ladder_escape {
            push_escaped(&mut out, ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// The ASCII characters the plain encoding escapes wherever they stand: the
/// escape character, a literal `.` (which would read as a folded `/`), `\`,
/// and the characters Windows reserves in a path.
const ALWAYS_ESCAPED: [char; 10] = ['%', '.', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Whether `ch` is non-ASCII and its compatibility decomposition (NFKD) holds
/// `/` or a character in [`ALWAYS_ESCAPED`] — the whole class a best-fit
/// extractor may narrow back to one, rather than a list of its instances.
///
/// An extractor that narrows a UTF-16 path to a Windows ANSI code page with
/// best-fit mapping turns `／` into `/`, `．` and `﹒` into `.`, `…` into `...`
/// and `：` into `:`, so kept verbatim they would let a name that is one safe
/// component in the archive become several — `a／．．／b` or `﹒﹒` extracting
/// as `a/../b` or `..`, outside its directory — name a stream, or forge an
/// escape. Escaped, every extractor sees the same `%XX` the ASCII character
/// gets. A character whose decomposition holds none of them (fullwidth letters,
/// digits and brackets, `ü`) narrows only to characters that are safe where
/// they land, so it stays verbatim like any other non-ASCII.
fn narrows_to_an_escape(ch: char) -> bool {
    !ch.is_ascii()
        && std::iter::once(ch)
            .nfkd()
            .any(|d| d == '/' || ALWAYS_ESCAPED.contains(&d))
}

/// Whether `ch` is a character a best-fit extractor may narrow to a path
/// separator though its compatibility decomposition does not say so, so
/// [`narrows_to_an_escape`] cannot see it: `¥` is byte 0x5C — `\` — in code
/// page 932 (Japanese) and `₩` is in 949 (Korean), and the slash and backslash
/// operators (`∕` `⁄` `∖` `⧵`) look like the separator they may become.
/// Escaped on sight, like the decomposing look-alikes, without measuring which
/// code page narrows which: kept, `a¥b` would extract as two nested directories.
fn is_separator_look_alike(ch: char) -> bool {
    matches!(ch, '¥' | '₩' | '∕' | '⁄' | '∖' | '⧵')
}

/// `%XX` per UTF-8 byte of `ch`, uppercase hex. The plain encoding escapes ASCII
/// and its look-alikes above; the ladder's rungs escape whatever comes first.
fn push_escaped(out: &mut String, ch: char) {
    let mut utf8 = [0u8; 4];
    for byte in ch.encode_utf8(&mut utf8).bytes() {
        out.push_str(&format!("%{byte:02X}"));
    }
}

/// What a case- and normalization-insensitive filesystem makes of a component:
/// two components with one key may extract to one file.
///
/// Deliberately **coarser than any one filesystem** — canonical decomposition,
/// uppercase then lowercase (so both the upcase-table folders and the
/// case-folding ones are covered: `ß`/`SS`, `ſ`/`s`, final sigma), decomposed
/// again because case mapping can undo it. Too coarse costs a rare name one
/// avoidable escape; too fine loses a mailbox. Canonical, not compatibility,
/// equivalence: no filesystem this archive is extracted onto folds `％` to `%`
/// (a best-fit *extractor* does, which is why [`encode`] escapes `％` itself),
/// and a key that did would let a fullwidth name forge an escape.
pub(super) fn extraction_key(component: &str) -> String {
    component
        .nfd()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .nfd()
        .collect()
}

/// Whether Windows would open a device instead of creating this component.
///
/// Windows reserves these names **with any extension** and ignoring trailing
/// spaces, so the test reads the part before the first `.` — which matters
/// here twice over: the mbox serializer appends `.mbox`, and the `/` fold turns
/// `Aux/Reports` into `Aux.Reports`. The superscript digits are reserved too.
pub(super) fn is_windows_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches(' ');
    let numbered = |prefix: &str| {
        let mut rest = stem.chars().skip(prefix.len());
        stem.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            && matches!(
                (rest.next(), rest.next()),
                (Some('0'..='9' | '¹' | '²' | '³'), None)
            )
    };
    ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
        || numbered("COM")
        || numbered("LPT")
}

/// Cut an over-long component to [`MAX_COMPONENT_BYTES`]: the longest prefix
/// that splits neither a character nor a `%XX` escape, then [`CUT_MARKER`] and
/// a digest of the **raw** name, which keeps two names with one long common
/// prefix apart. The only step of the encoding that does not decode — a name
/// this long keeps its messages and loses the tail of its own spelling.
fn fit(component: String, raw_name: &str) -> String {
    if component.len() <= MAX_COMPONENT_BYTES {
        return component;
    }
    let mut cut = MAX_COMPONENT_BYTES - CUT_SUFFIX_BYTES;
    while !component.is_char_boundary(cut) {
        cut -= 1;
    }
    // Every escape is exactly `%XX`, so a `%` in the last two bytes kept is an
    // escape the cut would split.
    if let Some(split) = component.as_bytes()[..cut]
        .iter()
        .rposition(|&b| b == b'%')
        .filter(|at| cut - at < 3)
    {
        cut = split;
    }
    let digest = &blake3::hash(raw_name.as_bytes()).to_hex()[..CUT_DIGEST_HEX];
    format!("{}{CUT_MARKER}{digest}", &component[..cut])
}

/// Hands out one archive path component per name such that no two of them —
/// and none of them and a reserved sibling — can land on one file when the
/// archive is extracted. See the module docs for the ladder.
///
/// One namer per directory of the archive whose entries come from user-supplied
/// names (the mailbox files of mbox, the mailbox directories of Maildir++).
pub(super) struct ComponentNamer {
    taken: HashSet<String>,
}

impl ComponentNamer {
    /// A namer for a directory that already holds `reserved` siblings the
    /// serializer writes itself (Maildir++'s `subscriptions` index).
    pub(super) fn reserving(reserved: &[&str]) -> Self {
        Self {
            taken: reserved.iter().map(|r| extraction_key(r)).collect(),
        }
    }

    /// The component for `raw_name`, which must not have been named before by
    /// this namer (the serializers name a mailbox once, when it opens).
    ///
    /// The error is unreachable short of a 128-bit digest collision between two
    /// over-long names — about 2^64 hashes even for a source that chooses its
    /// names ([`CUT_DIGEST_HEX`]), and it fails closed: nothing is inserted
    /// into `taken` twice, so no mailbox is written over another. The last rung keeps nothing verbatim but digits and the
    /// folded `.`, so the fold can only lowercase its hex — a one-to-one image
    /// of a component that is itself an injective image of the name. A lower
    /// rung's component can share that key only by being the same string
    /// (nothing folds *to* a digit, a `.` or a `%`), hence the same name. An
    /// error rather than a panic because the alternative to refusing is
    /// writing one mailbox over another.
    pub(super) fn name(&mut self, raw_name: &str) -> Result<String, ExportError> {
        for rung in [Rung::Plain, Rung::FirstEscaped, Rung::AllEscaped] {
            let candidate = fit(encode(raw_name, rung), raw_name);
            if is_windows_device_name(&candidate) {
                continue;
            }
            let key = extraction_key(&candidate);
            if self.taken.insert(key) {
                return Ok(candidate);
            }
        }
        Err(ExportError::PathCollision {
            name: raw_name.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_component_fits_on_every_rung_and_ends_in_a_128_bit_digest() {
        // 255 bytes is the nest's cap on a mailbox name; `:` escapes to three
        // bytes and the last rung escapes every letter, so each rung overflows.
        for name in [
            format!("{}a", "n".repeat(254)),
            ":".repeat(255),
            "日".repeat(85),
        ] {
            let digest = blake3::hash(name.as_bytes()).to_hex()[..32].to_string();
            for rung in [Rung::Plain, Rung::FirstEscaped, Rung::AllEscaped] {
                let component = fit(encode(&name, rung), &name);
                assert!(component.len() <= MAX_COMPONENT_BYTES, "{component}");
                let (_, suffix) = component
                    .rsplit_once(CUT_MARKER)
                    .expect("an over-long component is cut");
                assert_eq!(suffix, digest, "{component}");
            }
        }
    }
}
