//! A small, WASM-safe vCard (RFC 6350 / 2426) reader for the Address Book view.
//!
//! Deliberately lenient: unknown properties are ignored and malformed lines
//! skipped — the goal is a best-effort render of the fields the detail pane shows
//! (`FN` / `N` / `EMAIL` / `TEL` / `ADR` / `ORG` / `TITLE` / `NOTE` / `BDAY`),
//! not RFC validation (the MDA + MUA already validate on PUT). Handles §3.2 line
//! unfolding, `;`-separated params with `TYPE`/`PREF`, `;`-separated structured
//! values (`N` / `ADR` / `ORG`), and §3.4 text escaping (`\n \, \; \\`).
//!
//! Lifted here from `fauna-client-carddav` so the nest's materializer
//! (`encryption-at-rest.md` § Capability tiering → *The materialization
//! grant*) and the apps' own index derive a card's searchable text through
//! one function, [`search_text`]; `fauna_client_carddav::vcard` re-exports
//! this module.

/// A property value paired with its `TYPE` parameters and preference flag —
/// e.g. a `TEL;TYPE=work,voice;PREF=1:+1-555-0100`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypedValue {
    /// The unescaped property value (the phone number, email, URL, …).
    pub value: String,
    /// Lower-cased `TYPE` parameter values (e.g. `work`, `home`, `cell`), with
    /// the `pref` pseudo-type removed (surfaced via [`Self::pref`] instead).
    pub types: Vec<String>,
    /// `true` when the property is marked preferred (vCard 4.0 `PREF=1` or the
    /// 3.0 `TYPE=pref` idiom).
    pub pref: bool,
}

/// The structured `N` property: `family;given;additional;prefixes;suffixes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuredName {
    pub family: String,
    pub given: String,
    pub additional: String,
    pub prefixes: String,
    pub suffixes: String,
}

/// The structured `ADR` property: a mailing address plus its `TYPE`s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Address {
    pub types: Vec<String>,
    pub pref: bool,
    pub po_box: String,
    pub extended: String,
    pub street: String,
    pub locality: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}

impl Address {
    /// The non-empty components joined into a single human line — `po_box,
    /// extended, street, locality, region postal_code, country` (region + postal
    /// code read as one token, e.g. `IL 62704`), skipping empties. The single
    /// source of truth for the `vcard-detail-adr` render, so every app (native
    /// via UniFFI, web via wasm) shows byte-identical address text (priority #2).
    #[must_use]
    pub fn one_line(&self) -> String {
        let region_zip = [self.region.as_str(), self.postal_code.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        [
            self.po_box.as_str(),
            self.extended.as_str(),
            self.street.as_str(),
            self.locality.as_str(),
            region_zip.as_str(),
            self.country.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// The fields the Address Book renders, parsed from a decrypted vCard body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedVCard {
    /// `UID` (plaintext; the sealed body's own identifier).
    pub uid: String,
    /// `FN` — the display name (RFC 6350 requires exactly one; first wins).
    pub formatted_name: String,
    /// `N` — the structured name, when present.
    pub name: Option<StructuredName>,
    pub emails: Vec<TypedValue>,
    pub tels: Vec<TypedValue>,
    pub addresses: Vec<Address>,
    pub urls: Vec<TypedValue>,
    /// `ORG` components (`organization;unit;subunit`).
    pub org: Vec<String>,
    /// `TITLE`.
    pub title: String,
    /// `NOTE`.
    pub note: String,
    /// `BDAY`, verbatim (a date or date-time as written).
    pub bday: String,
    /// `X-FAUNA-ACTOR-ID` — the optional link back to a social contact
    /// (carddav-server.md § What a CardDAV address book is). Parsed here so
    /// the post-v1 cross-link slice has it; unused by the v1 UI.
    pub fauna_actor_id: Option<String>,
}

impl ParsedVCard {
    /// The non-empty `ORG` components joined with ` · `, skipping empties —
    /// the single source of truth for the `vcard-detail-org` render, the twin
    /// of [`Address::one_line`] and here for the same reason (priority #2): so
    /// every app shows byte-identical organization text. The separator is
    /// the one web and linux already render, so adopting this changes no
    /// client's output; an all-empty `ORG` yields `""`, which every app
    /// treats as "hide the row".
    #[must_use]
    pub fn org_line(&self) -> String {
        self.org
            .iter()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Parse a decrypted vCard body into the fields the Address Book renders.
/// Never fails — an unrecognized or malformed line is skipped.
#[must_use]
pub fn parse_vcard(text: &str) -> ParsedVCard {
    let mut out = ParsedVCard::default();
    for logical in unfold_lines(text).lines() {
        let Some(line) = ContentLine::parse(logical) else {
            continue;
        };
        match line.name.as_str() {
            "FN" if out.formatted_name.is_empty() => {
                out.formatted_name = unescape_text(&line.value);
            }
            "N" if out.name.is_none() => {
                let c = split_components(&line.value);
                out.name = Some(StructuredName {
                    family: comp(&c, 0),
                    given: comp(&c, 1),
                    additional: comp(&c, 2),
                    prefixes: comp(&c, 3),
                    suffixes: comp(&c, 4),
                });
            }
            "EMAIL" => out.emails.push(line.typed_value()),
            "TEL" => out.tels.push(line.typed_value()),
            "URL" => out.urls.push(line.typed_value()),
            "ADR" => {
                let c = split_components(&line.value);
                out.addresses.push(Address {
                    types: line.type_params(),
                    pref: line.is_pref(),
                    po_box: comp(&c, 0),
                    extended: comp(&c, 1),
                    street: comp(&c, 2),
                    locality: comp(&c, 3),
                    region: comp(&c, 4),
                    postal_code: comp(&c, 5),
                    country: comp(&c, 6),
                });
            }
            "ORG" => {
                out.org = split_components(&line.value)
                    .iter()
                    .map(|s| unescape_text(s))
                    .collect();
            }
            "TITLE" if out.title.is_empty() => out.title = unescape_text(&line.value),
            "NOTE" if out.note.is_empty() => out.note = unescape_text(&line.value),
            "BDAY" if out.bday.is_empty() => out.bday = unescape_text(&line.value),
            "UID" if out.uid.is_empty() => out.uid = unescape_text(&line.value),
            "X-FAUNA-ACTOR-ID" if out.fauna_actor_id.is_none() => {
                out.fauna_actor_id = Some(unescape_text(&line.value));
            }
            _ => {}
        }
    }
    out
}

/// One parsed content line: `[group "."] NAME *(";" param) ":" value`. The
/// value is kept raw (unescaping is applied by the caller per property).
struct ContentLine {
    /// Upper-cased property name (group prefix stripped).
    name: String,
    /// `(UPPER-name, values)` params — values already comma-split + dequoted.
    params: Vec<(String, Vec<String>)>,
    /// Raw property value (post-colon, pre-unescape).
    value: String,
}

impl ContentLine {
    fn parse(logical: &str) -> Option<ContentLine> {
        if logical.trim().is_empty() {
            return None;
        }
        let colon = find_value_colon(logical)?;
        let head = &logical[..colon];
        let value = &logical[colon + 1..];
        let mut parts = split_unquoted(head, ';');
        if parts.is_empty() {
            return None;
        }
        let name_with_group = parts.remove(0);
        let name = name_with_group
            .rsplit('.')
            .next()
            .unwrap_or(&name_with_group)
            .to_ascii_uppercase();
        if name.is_empty() {
            return None;
        }
        let mut params = Vec::new();
        for p in parts {
            if p.is_empty() {
                continue;
            }
            let (pn, pv) = match p.split_once('=') {
                Some((n, v)) => (n.to_ascii_uppercase(), parse_param_values(v)),
                // vCard 2.1 bare type param (e.g. `;WORK`): treat as `TYPE=work`.
                None => ("TYPE".to_string(), vec![p]),
            };
            params.push((pn, pv));
        }
        Some(ContentLine {
            name,
            params,
            value: value.to_string(),
        })
    }

    /// Lower-cased `TYPE` values with the `pref` pseudo-type removed.
    fn type_params(&self) -> Vec<String> {
        self.params
            .iter()
            .filter(|(n, _)| n == "TYPE")
            .flat_map(|(_, v)| v.iter())
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| s != "pref")
            .collect()
    }

    /// vCard 4.0 `PREF=…` param or 3.0 `TYPE=pref` idiom.
    fn is_pref(&self) -> bool {
        self.params.iter().any(|(n, v)| {
            n == "PREF" || (n == "TYPE" && v.iter().any(|x| x.eq_ignore_ascii_case("pref")))
        })
    }

    fn typed_value(&self) -> TypedValue {
        TypedValue {
            value: unescape_text(&self.value),
            types: self.type_params(),
            pref: self.is_pref(),
        }
    }
}

/// Comma-split a param value string (respecting DQUOTE), trimming and
/// stripping surrounding double-quotes from each value.
fn parse_param_values(v: &str) -> Vec<String> {
    split_unquoted(v, ',')
        .into_iter()
        .map(|s| {
            let t = s.trim();
            t.strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(t)
                .to_string()
        })
        .collect()
}

// `find_value_colon` / `split_unquoted` / `unfold_lines` — the content
// line's DQUOTE quoting rule and its line-folding rule — are
// `crate::content_line`'s, shared with the iCalendar parser: RFC 6350
// § 3.2/3.3 and RFC 5545 § 3.1 define the same grammar, so a private copy
// here is a second place vCard and iCalendar can come to disagree about
// one syntax. Pure `&str` work, no deps — the "fully WASM-safe, no
// native-only slice" property above is untouched.
use crate::content_line::{find_value_colon, split_unquoted, unfold_lines};

/// Split a structured value on unescaped `;` component separators, keeping
/// backslash-escape pairs intact for [`unescape_text`] to resolve.
fn split_components(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            cur.push(c);
            if let Some(&next) = chars.peek() {
                cur.push(next);
                chars.next();
            }
        } else if c == ';' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// The `i`-th structured component, unescaped, or empty if absent.
fn comp(components: &[String], i: usize) -> String {
    components
        .get(i)
        .map(|s| unescape_text(s))
        .unwrap_or_default()
}

/// RFC 6350 §3.4 text unescaping: `\n`/`\N` → newline, `\,` → `,`,
/// `\;` → `;`, `\\` → `\`; any other `\x` yields `x`.
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The searchable text of one card: every field a user would plausibly
/// recall, joined by newlines — the **parsed** fields rather than the raw
/// vCard, so an index holds what a person would search for and not the
/// format's scaffolding (`BEGIN:VCARD`, property names and encoding
/// parameters would otherwise be tokens every card matches on). The one
/// derivation both a device's own index and the nest's materialized view
/// read, so the two hold the same document.
#[must_use]
pub fn search_text(card: &ParsedVCard) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() {
            parts.push(s);
        }
    };
    push(card.formatted_name.clone());
    if let Some(n) = card.name.as_ref() {
        push(
            [
                n.prefixes.as_str(),
                n.given.as_str(),
                n.additional.as_str(),
                n.family.as_str(),
                n.suffixes.as_str(),
            ]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
        );
    }
    for e in &card.emails {
        push(e.value.clone());
    }
    for t in &card.tels {
        push(t.value.clone());
    }
    for u in &card.urls {
        push(u.value.clone());
    }
    for a in &card.addresses {
        push(a.one_line());
    }
    push(card.org_line());
    push(card.title.clone());
    push(card.note.clone());
    parts.join("\n")
}
