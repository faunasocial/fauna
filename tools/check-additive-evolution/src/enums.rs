//! Rule 3 for enums — the per-enum ledger and the two checks that hold it
//! (transport.md § Schema and forward-compat discipline → *Rule 3 in full*,
//! "The ledger and the gate"; the answers live in
//! `tools/check-additive-evolution/enum_ledger.txt`).
//!
//! - **State** ([`check_enum_ledger`]), over the whole head tree: every
//!   `Deserialize` enum — deriving it, or implementing it by hand — has a
//!   ledger line (its own, or its crate's
//!   `local <crate>::*`), every line names an enum that exists, and an enum
//!   listed `open` carries a recognised arm. A new enum therefore cannot ship
//!   closed by omission.
//! - **Diff** ([`diff_enum_maps`]), against the merge base: a variant removed
//!   (or its wire name or an `alias` dropped) on an enum the base ledger puts
//!   in rule 3's scope — any answer but `local`, `locked` or `foreign` — is a
//!   finding, excused only by a `ratified-breaks.txt` line
//!   `rust <crate>::<module>::<Enum>::<Variant> removed`; a variant added to
//!   an enum closed on the consensus, ladder, extension-point or fixed ground
//!   is a finding unless that enum's ledger line changed in the same change.
//!
//! Keys are `<crate>::<module path>::<Enum>` — the struct check's
//! module-qualified key with the crate name in front, since this scan spans
//! every crate rather than four fixed roots. The module path is the file's
//! path under `src/` (`lib`/`mod`/`main` dropped) plus any inline `mod`, so a
//! move between files that keeps the module path is not flagged, like the
//! struct check's.
//!
//! `#[cfg(test)]` enums, inline modules and out-of-line `#[cfg(test)] mod x;`
//! files are not scanned: test code is never on a wire.

use std::collections::{BTreeMap, BTreeSet};

use syn::punctuated::Punctuated;
use syn::{Attribute, Fields, Item, ItemEnum, Meta, Token};

use crate::{
    RatifiedBreaks, RenameRule, Transition, Violation, has_cfg_test, has_named_derive, qualify,
    serde_metas, str_lit,
};

/// One enum's decode-relevant shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnumShape {
    /// Every decodable variant, by resolved **wire name** (`rename` /
    /// `rename_all` applied; `skip`/`skip_deserializing` variants excluded).
    pub variants: BTreeSet<String>,
    /// Every spelling the decoder accepts: [`Self::variants`] plus each
    /// `alias`. A removal is judged on these (dropping an alias stops reading
    /// what an older writer wrote), an addition on `variants` (an alias is
    /// never written, so adding one reaches no older reader).
    pub accepted: BTreeSet<String>,
    /// Carries a recognised unknown arm: a `#[serde(other)]` unit variant, a
    /// last `#[serde(untagged)]` variant, or a hand-written `impl Deserialize`.
    pub has_arm: bool,
}

/// Enums keyed `<crate>::<module>::<Enum>`.
pub type EnumMap = BTreeMap<String, EnumShape>;

/// One crate's raw scan, before the crate name is applied and the
/// `#[cfg(test)]` file modules are dropped ([`EnumScan::finish`]).
#[derive(Debug, Default)]
pub struct EnumScan {
    /// Keyed `<module>::<Enum>` (crate-relative).
    enums: EnumMap,
    /// Enums that derive no `Deserialize`, same keys: one becomes a scanned
    /// enum when the crate implements `Deserialize` for it by hand
    /// ([`Self::finish`]) — a hand-written decoder decodes stored or wire
    /// bytes exactly as a derived one does.
    plain_enums: EnumMap,
    /// Idents with a hand-written `impl Deserialize` anywhere in the crate.
    hand_written: BTreeSet<String>,
    /// Module paths declared `#[cfg(test)] mod x;` (out-of-line).
    test_modules: BTreeSet<String>,
}

impl EnumScan {
    /// Parse one source file at `module_prefix` into the scan. `Err` only on
    /// a syntax error.
    pub fn add_file(&mut self, source: &str, module_prefix: &str) -> Result<(), String> {
        let file = syn::parse_file(source).map_err(|e| e.to_string())?;
        self.add_items(&file.items, module_prefix);
        Ok(())
    }

    fn add_items(&mut self, items: &[Item], module_prefix: &str) {
        for item in items {
            match item {
                Item::Enum(e) if !has_cfg_test(&e.attrs) => {
                    let into = if derives_deserialize(&e.attrs) {
                        &mut self.enums
                    } else {
                        &mut self.plain_enums
                    };
                    into.insert(qualify(module_prefix, &e.ident.to_string()), enum_shape(e));
                }
                // A decode-shape helper declared inside a function (typically a
                // hand-written `deserialize`) decodes stored or wire bytes like
                // any other; it is keyed at its enclosing module.
                Item::Fn(f) if !has_cfg_test(&f.attrs) => {
                    self.add_block(&f.block, module_prefix);
                }
                Item::Impl(i) if !has_cfg_test(&i.attrs) => {
                    for impl_item in &i.items {
                        if let syn::ImplItem::Fn(f) = impl_item
                            && !has_cfg_test(&f.attrs)
                        {
                            self.add_block(&f.block, module_prefix);
                        }
                    }
                    let is_deserialize = i
                        .trait_
                        .as_ref()
                        .and_then(|(_, p, _)| p.segments.last())
                        .is_some_and(|s| s.ident == "Deserialize");
                    if is_deserialize
                        && let syn::Type::Path(tp) = &*i.self_ty
                        && let Some(seg) = tp.path.segments.last()
                    {
                        self.hand_written.insert(seg.ident.to_string());
                    }
                }
                Item::Mod(m) => {
                    let path = qualify(module_prefix, &m.ident.to_string());
                    if has_cfg_test(&m.attrs) {
                        if m.content.is_none() {
                            self.test_modules.insert(path);
                        }
                    } else if let Some((_, inner)) = &m.content {
                        self.add_items(inner, &path);
                    }
                }
                _ => {}
            }
        }
    }

    /// The items declared directly in a function body (one level: an enum in
    /// a nested block is not reached).
    fn add_block(&mut self, block: &syn::Block, module_prefix: &str) {
        let items: Vec<Item> = block
            .stmts
            .iter()
            .filter_map(|s| match s {
                syn::Stmt::Item(item) => Some(item.clone()),
                _ => None,
            })
            .collect();
        self.add_items(&items, module_prefix);
    }

    /// The crate's enums keyed `<crate_name>::<module>::<Enum>`, with every
    /// enum under a `#[cfg(test)]` file module dropped and a hand-written
    /// `impl Deserialize` counted as an arm (matched by ident: the impl's
    /// module is not resolved, which errs toward accepting an arm). An enum
    /// that derives no `Deserialize` is scanned when such an impl names it,
    /// its variants read off the declaration like any other's.
    pub fn finish(self, crate_name: &str) -> EnumMap {
        let in_test_module = |key: &str| {
            self.test_modules
                .iter()
                .any(|m| key.starts_with(&format!("{m}::")))
        };
        let hand_decoded = self.plain_enums.into_iter().filter(|(key, _)| {
            self.hand_written
                .contains(key.rsplit("::").next().unwrap_or(key))
        });
        self.enums
            .into_iter()
            .chain(hand_decoded)
            .filter(|(key, _)| !in_test_module(key))
            .map(|(key, mut shape)| {
                let ident = key.rsplit("::").next().unwrap_or(&key);
                if self.hand_written.contains(ident) {
                    shape.has_arm = true;
                }
                (format!("{crate_name}::{key}"), shape)
            })
            .collect()
    }
}

/// `#[derive(.., Deserialize, ..)]`, bare, path-qualified or under `cfg_attr`.
fn derives_deserialize(attrs: &[Attribute]) -> bool {
    has_named_derive(attrs, "Deserialize")
}

fn enum_shape(e: &ItemEnum) -> EnumShape {
    let enum_metas = serde_metas(&e.attrs);
    let rename_all = enum_metas
        .iter()
        .find_map(|m| deserialize_name(m, "rename_all"));
    let rule = rename_all.as_deref().and_then(RenameRule::from_str);

    let mut shape = EnumShape::default();
    let live: Vec<_> = e
        .variants
        .iter()
        .filter(|v| !has_cfg_test(&v.attrs))
        .collect();
    for (i, v) in live.iter().enumerate() {
        let metas = serde_metas(&v.attrs);
        let flag = |name: &str| {
            metas
                .iter()
                .any(|m| matches!(m, Meta::Path(p) if p.is_ident(name)))
        };
        if flag("skip") || flag("skip_deserializing") {
            continue;
        }
        if (flag("other") && matches!(v.fields, Fields::Unit))
            || (flag("untagged") && i + 1 == live.len())
        {
            shape.has_arm = true;
        }
        let ident = v.ident.to_string();
        let wire = metas
            .iter()
            .find_map(|m| deserialize_name(m, "rename"))
            .unwrap_or_else(|| apply_variant_rule(rule, &ident));
        shape.accepted.insert(wire.clone());
        shape.accepted.extend(metas.iter().filter_map(|m| match m {
            Meta::NameValue(nv) if nv.path.is_ident("alias") => str_lit(&nv.value),
            _ => None,
        }));
        shape.variants.insert(wire);
    }
    shape
}

/// The decode-side value of `key = "x"` or `key(deserialize = "x", ..)`.
fn deserialize_name(meta: &Meta, key: &str) -> Option<String> {
    match meta {
        Meta::NameValue(nv) if nv.path.is_ident(key) => str_lit(&nv.value),
        Meta::List(list) if list.path.is_ident(key) => list
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            .ok()?
            .iter()
            .find_map(|m| match m {
                Meta::NameValue(nv) if nv.path.is_ident("deserialize") => str_lit(&nv.value),
                _ => None,
            }),
        _ => None,
    }
}

/// serde's `rename_all` as applied to VARIANT idents (PascalCase input) —
/// serde's `RenameRule::apply_to_variant`, not the field transform
/// `apply_rename_all` does for struct fields (snake_case input).
fn apply_variant_rule(rule: Option<RenameRule>, ident: &str) -> String {
    let snake = || {
        let mut out = String::new();
        for (i, c) in ident.char_indices() {
            if c.is_uppercase() && i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        }
        out
    };
    match rule {
        None | Some(RenameRule::Pascal) => ident.to_string(),
        Some(RenameRule::Lower) => ident.to_ascii_lowercase(),
        Some(RenameRule::Upper) => ident.to_ascii_uppercase(),
        Some(RenameRule::Camel) => {
            let mut c = ident.chars();
            c.next()
                .map(|f| f.to_ascii_lowercase().to_string() + c.as_str())
                .unwrap_or_default()
        }
        Some(RenameRule::Snake) => snake(),
        Some(RenameRule::ScreamingSnake) => snake().to_ascii_uppercase(),
        Some(RenameRule::Kebab) => snake().replace('_', "-"),
        Some(RenameRule::ScreamingKebab) => snake().replace('_', "-").to_ascii_uppercase(),
    }
}

// ── The ledger ──────────────────────────────────────────────────────────────

/// A ledger line's answer token (the ledger header owns their meanings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Answer {
    Open,
    OwedCarry,
    OwedCollapse,
    Skip,
    OwedSkip,
    ClosedRequest,
    ClosedConsensus,
    ClosedLadder,
    ClosedExtension,
    ClosedFixed,
    Locked,
    Local,
    Foreign,
}

impl Answer {
    const ALL: [(Self, &'static str); 13] = [
        (Self::Open, "open"),
        (Self::OwedCarry, "owed-carry"),
        (Self::OwedCollapse, "owed-collapse"),
        (Self::Skip, "skip"),
        (Self::OwedSkip, "owed-skip"),
        (Self::ClosedRequest, "closed-request"),
        (Self::ClosedConsensus, "closed-consensus"),
        (Self::ClosedLadder, "closed-ladder"),
        (Self::ClosedExtension, "closed-extension"),
        (Self::ClosedFixed, "closed-fixed"),
        (Self::Locked, "locked"),
        (Self::Local, "local"),
        (Self::Foreign, "foreign"),
    ];

    fn parse(token: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, s)| *s == token).map(|(a, _)| *a)
    }

    pub fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(a, _)| *a == self)
            .map(|(_, s)| *s)
            .unwrap_or("?")
    }

    /// Ruled and not yet built — a debt none may carry into the public repo.
    pub fn is_owed(self) -> bool {
        matches!(self, Self::OwedCarry | Self::OwedCollapse | Self::OwedSkip)
    }

    /// Closed on a ground where a new variant reaches an older reader: every
    /// closed ground but `request`, whose new variant ships as a new kind.
    fn refuses_silent_addition(self) -> bool {
        matches!(
            self,
            Self::ClosedConsensus | Self::ClosedLadder | Self::ClosedExtension | Self::ClosedFixed
        )
    }
}

/// One ledger line: its answer and its whole text (answer, key and reason),
/// the unit "the line changed" compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerLine {
    pub answer: Answer,
    pub text: String,
}

/// The parsed ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnumLedger {
    /// Per-enum lines, keyed `<crate>::<module>::<Enum>`.
    pub enums: BTreeMap<String, LedgerLine>,
    /// `local <crate>::*` lines, keyed by crate name.
    pub crates: BTreeMap<String, LedgerLine>,
}

impl EnumLedger {
    /// The answer that covers `key`: its own line, else its crate's.
    pub fn answer_for(&self, key: &str) -> Option<Answer> {
        self.enums.get(key).map(|l| l.answer).or_else(|| {
            let krate = key.split("::").next()?;
            self.crates.get(krate).map(|l| l.answer)
        })
    }

    /// Every `owed-` line, `(key, answer)`, in key order.
    pub fn owed(&self) -> Vec<(&str, Answer)> {
        self.enums
            .iter()
            .filter(|(_, l)| l.answer.is_owed())
            .map(|(k, l)| (k.as_str(), l.answer))
            .collect()
    }
}

/// Parse `enum_ledger.txt`: `<answer> <key>  # <reason>` per line, `#`
/// comment lines and blanks ignored. A malformed line is an `Err` (the gate
/// exits 2, as for a bad `ratified-breaks.txt` token): an unknown answer, a
/// missing reason, a key that names no enum, a crate wildcard on any answer
/// but `local`, or a key listed twice.
pub fn parse_enum_ledger(text: &str) -> Result<EnumLedger, String> {
    let mut ledger = EnumLedger::default();
    for (n, raw) in text.lines().enumerate() {
        let n = n + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (entry, reason) = line.split_once('#').unwrap_or((line, ""));
        if reason.trim().is_empty() {
            return Err(format!("line {n}: no `# reason`: {raw:?}"));
        }
        let parts: Vec<&str> = entry.split_whitespace().collect();
        let [answer, key] = parts[..] else {
            return Err(format!(
                "line {n}: want `<answer> <key>  # <reason>`: {raw:?}"
            ));
        };
        let Some(answer) = Answer::parse(answer) else {
            return Err(format!("line {n}: unknown answer {answer:?}: {raw:?}"));
        };
        let entry = LedgerLine {
            answer,
            text: line.to_string(),
        };
        let duplicate = if let Some(krate) = key.strip_suffix("::*") {
            if answer != Answer::Local {
                return Err(format!(
                    "line {n}: a crate wildcard is only ever `local` (a crate holding any enum \
                     on the wire, at rest or on the IPC lists every enum): {raw:?}"
                ));
            }
            ledger.crates.insert(krate.to_string(), entry).is_some()
        } else {
            if !key.contains("::") {
                return Err(format!(
                    "line {n}: key {key:?} names no `<crate>::…::<Enum>`: {raw:?}"
                ));
            }
            ledger.enums.insert(key.to_string(), entry).is_some()
        };
        if duplicate {
            return Err(format!("line {n}: {key} is listed twice: {raw:?}"));
        }
    }
    Ok(ledger)
}

// ── The checks ──────────────────────────────────────────────────────────────

/// The state check: `head` (every scanned crate's enums) against the ledger.
pub fn check_enum_ledger(head: &EnumMap, ledger: &EnumLedger) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (key, shape) in head {
        match ledger.enums.get(key) {
            Some(line) if line.answer == Answer::Open && !shape.has_arm => {
                violations.push(Violation::new(
                    key,
                    "listed `open` in enum_ledger.txt but carries no arm (`#[serde(other)]` on a \
                     unit variant, a last `#[serde(untagged)]` variant, or a hand-written \
                     `impl Deserialize`)"
                        .to_string(),
                ));
            }
            Some(_) => {}
            None if ledger.answer_for(key).is_some() => {} // the crate's `local` wildcard
            None => violations.push(Violation::new(
                key,
                "deserializable enum with no enum_ledger.txt line — give it its rule-3 answer \
                 (transport.md § Schema and forward-compat discipline → Rule 3 in full)"
                    .to_string(),
            )),
        }
    }
    for key in ledger.enums.keys() {
        if !head.contains_key(key) {
            violations.push(Violation::new(
                key,
                "enum_ledger.txt line names no deserializable enum in the tree — drop it, or \
                 re-key it to where the enum moved"
                    .to_string(),
            ));
        }
    }
    for krate in ledger.crates.keys() {
        let prefix = format!("{krate}::");
        if !head.keys().any(|k| k.starts_with(&prefix)) {
            violations.push(Violation::new(
                &format!("{krate}::*"),
                "enum_ledger.txt crate wildcard covers no deserializable enum — drop it"
                    .to_string(),
            ));
        }
    }
    violations.sort();
    violations
}

/// The `--no-owed` check: every `owed-` line is a finding.
pub fn check_no_owed(ledger: &EnumLedger) -> Vec<Violation> {
    ledger
        .owed()
        .into_iter()
        .map(|(key, answer)| {
            Violation::new(
                key,
                format!(
                    "enum_ledger.txt still owes this enum (`{}`) — an `owed-` line is a \
                     debt: `--no-owed` refuses any, and none may land on main",
                    answer.as_str()
                ),
            )
        })
        .collect()
}

/// The diff check, `base` → `head`; a whole enum appearing or disappearing is
/// the state check's business, not this one's.
///
/// A **removal** is judged against the contract the BASE committed to: the
/// base ledger's answer must be in rule 3's scope (anything but `local`,
/// `locked` or `foreign` — transport.md's (d)–(f), where a removed variant
/// meets no reader of another release). So the check binds from the commit
/// that lists an enum, and a base older than the ledger itself (the async
/// merge-gate check's last-green tip can be) has promised nothing about its
/// enums. An enum flipped out of scope in the same change as a removal is
/// still judged by its base line. A **revival** of a ratified removal is a
/// finding whichever ledger puts the enum in scope; an **addition** follows
/// the head ledger's answer and the base-to-head change of the enum's line.
pub fn diff_enum_maps(
    base: &EnumMap,
    head: &EnumMap,
    base_ledger: &EnumLedger,
    head_ledger: &EnumLedger,
    allow: &RatifiedBreaks,
) -> Vec<Violation> {
    let in_scope = |ledger: &EnumLedger, key: &str| {
        ledger
            .answer_for(key)
            .is_some_and(|a| !matches!(a, Answer::Local | Answer::Locked | Answer::Foreign))
    };
    let ratified = |key: &str, variant: &str| {
        allow.contains(&(format!("{key}::{variant}"), Transition::Removed))
    };
    let mut violations = Vec::new();
    for (key, hs) in head {
        if !in_scope(head_ledger, key) && !in_scope(base_ledger, key) {
            continue;
        }
        for variant in &hs.accepted {
            if ratified(key, variant) {
                violations.push(Violation::new(
                    key,
                    format!(
                        "revives variant `{variant}`, a ratified removal (ratified-breaks.txt) \
                         — a removed name never comes back"
                    ),
                ));
            }
        }
        let Some(bs) = base.get(key) else { continue };
        let removals = in_scope(base_ledger, key).then(|| bs.accepted.difference(&hs.accepted));
        for variant in removals.into_iter().flatten() {
            if !ratified(key, variant) {
                violations.push(Violation::new(
                    key,
                    format!(
                        "removed variant `{variant}` (a rename is a remove+add — the removal is \
                         the break)"
                    ),
                ));
            }
        }
        let closed = head_ledger
            .answer_for(key)
            .is_some_and(Answer::refuses_silent_addition);
        let line_changed = base_ledger.enums.get(key) != head_ledger.enums.get(key);
        if closed && !line_changed {
            for variant in hs.variants.difference(&bs.variants) {
                violations.push(Violation::new(
                    key,
                    format!(
                        "added variant `{variant}` to an enum closed by design — change its \
                         enum_ledger.txt line in the same change to name the variant and what \
                         keeps it from older readers"
                    ),
                ));
            }
        }
    }
    violations.sort();
    violations
}

#[cfg(test)]
#[path = "enums_tests.rs"]
mod tests;
