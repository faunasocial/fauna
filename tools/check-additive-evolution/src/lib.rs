//! Additive-only schema-evolution lint for Rust wire + at-rest struct shapes.
//!
//! The type-level analogue of [`scripts/check-cddl-evolution.py`], which gates
//! the CDDL *schema* files. This gates the Rust **struct shapes** the CDDL gate
//! doesn't cover — every `Serialize`/`Deserialize` struct in `fauna-protocol`
//! (the client↔nest↔nest wire payloads), the at-rest serde types in
//! `fauna-segment-store` (`Manifest`, `SegmentSidecar`, `KindManifest`), and
//! the FFI-crossing/at-rest strict types in `fauna-core`. (`fauna-sync-engine`
//! was in scope for the legacy daemon's at-rest `SyncConfig`, and left with
//! it.)
//!
//! ## Contract
//!
//! Within a major version (version-compatibility.md I4 / transport.md § Schema
//! and forward-compat discipline): for each struct that survives under the same
//! module-qualified name, **no wire field may be removed, renamed, retyped, or
//! tightened optional→required**; adding fields is fine. This mirrors the
//! ratified CDDL gate's blocked set exactly — same rules, a real parser.
//!
//! ## Design rationale (the cold-read a future session needs)
//!
//! - **`syn`, not regex.** Regex struct-parsing is fragile — a Python regex pass
//!   tripped on destructuring, multi-line braces, and `extra` name collisions
//!   (proven on the 2026-06-15 catch-all sweep). `syn` parses the real grammar.
//! - **Crate-level, not file-level, diff.** Structs are keyed by
//!   `<module>::<Name>` and unioned across every file in the crate, so moving a
//!   struct between files — a permitted internal refactor (version-
//!   compatibility.md § 2) — is not flagged. Module qualification also resolves
//!   the handful of same-named structs in different modules (e.g. `email`'s vs
//!   `inbox`'s `InboxFetchRequest`).
//! - **Removed/renamed *structs* are NOT flagged.** A struct's Rust name/path is
//!   not on the wire (serde encodes field *names*, not the struct ident), so
//!   renaming or deleting a whole struct is either an internal refactor or the
//!   documented `.v2`-kind breaking-change escape hatch (Dim 2) — caught at the
//!   *kind* level by the CDDL gate, not here. Only *field-level* drift within a
//!   stably-keyed struct is a violation.
//! - **Wire name, not Rust ident.** Field keys resolve serde `rename` + struct
//!   `rename_all`, so renaming a Rust ident while keeping the wire key is not
//!   flagged, and changing the wire key (incl. flipping `rename_all`) is.
//! - **optional = `Option<T>` or any serde `default`** (incl. the `flatten`
//!   catch-all). Tightening any of those to required breaks old encoders that
//!   omit the key. (Like the CDDL gate, the *looser* direction required→optional
//!   is allowed — it's the recommended additive pattern.)
//! - **`skip`/`skip_deserializing` fields are off-wire** and excluded.
//! - **Enums are held by the ledger, not by this struct contract** ([`enums`]).
//!   The note this bullet once carried — that an `Unknown` fallthrough covers
//!   union evolution — was refuted by the 2026-10-01 census (409
//!   deserializable enums, nine with an arm): every enum's rule-3 answer is a
//!   line in `enum_ledger.txt`, checked as a whole-tree state (listed, and
//!   `open` means an arm exists) and as a diff (no variant removed or renamed
//!   on a listed non-`local` enum; no variant added to a closed-by-design one
//!   without its ledger line changing). transport.md § Schema and
//!   forward-compat discipline → *Rule 3 in full*.
//!
//! [`scripts/check-cddl-evolution.py`]: ../../../scripts/check-cddl-evolution.py

use std::collections::{BTreeMap, BTreeSet};

pub mod enums;

use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::{Attribute, Expr, ExprLit, Fields, Item, ItemStruct, Lit, Meta, Path, Token, Type};

/// One field's wire-relevant shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldShape {
    /// Tolerated-absent on decode: `Option<T>`, or any serde `default`
    /// (`#[serde(default)]`, `default = "..."`, or `flatten`).
    pub optional: bool,
    /// The underlying type with a single outer `Option<>` stripped, normalized
    /// (whitespace-free token string). Isolating optionality from the inner type
    /// keeps an optional↔required flip from also reading as a retype.
    pub inner_type: String,
    /// Carries `#[serde(flatten)]` — the forward-compat `extra` catch-all
    /// signal. A plain domain field merely *named* `extra` (no `flatten`) is not
    /// a catch-all, so this distinguishes the two.
    pub flatten: bool,
}

/// One struct's wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructShape {
    /// Fields keyed by resolved **wire name**.
    pub fields: BTreeMap<String, FieldShape>,
    /// Opts out of forward-compat tolerance via `#[serde(deny_unknown_fields)]`.
    pub strict: bool,
    /// Derives `Deserialize` (as opposed to `Serialize`-only). A struct with
    /// no decode side has no unknown-key hazard to guard against, so neither
    /// catch-all annotation the rule-4 check offers is anything but a
    /// cosmetic no-op on it — [`check_catch_all_violations`] skips these.
    pub derives_deserialize: bool,
}

impl StructShape {
    /// Whether any field is the `#[serde(flatten)]` catch-all.
    pub fn has_catch_all(&self) -> bool {
        self.fields.values().any(|f| f.flatten)
    }
}

/// A crate's tracked structs, keyed by `<module>::<Name>`.
pub type StructMap = BTreeMap<String, StructShape>;

/// A single additive-only violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub key: String,
    pub message: String,
}

impl Violation {
    fn new(key: &str, message: String) -> Self {
        Self {
            key: key.to_string(),
            message,
        }
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// Parse one source file's tracked structs into `map`, keyed
/// `<module_prefix>::<Name>` (or bare `<Name>` at the crate root). Only
/// named-field structs deriving `Serialize` or `Deserialize` are tracked;
/// `#[cfg(test)]` items and inline modules are skipped.
///
/// Returns `Err` only on a syntax error (caller decides whether to warn+skip).
///
/// A single file cannot know which of its out-of-line `mod x;` declarations
/// are `#[cfg(test)]`; a caller walking a whole crate uses [`StructScan`],
/// which drops those modules' files.
pub fn collect_structs(
    source: &str,
    module_prefix: &str,
    map: &mut StructMap,
) -> Result<(), String> {
    let file = syn::parse_file(source).map_err(|e| e.to_string())?;
    collect_items(&file.items, module_prefix, map, &mut BTreeSet::new());
    Ok(())
}

/// One root's struct scan: every file's tracked structs, plus the module
/// paths declared `#[cfg(test)] mod x;` (out-of-line), whose files
/// [`Self::finish`] drops — test code is never on a wire. The struct twin of
/// [`enums::EnumScan`].
#[derive(Debug, Default)]
pub struct StructScan {
    structs: StructMap,
    test_modules: BTreeSet<String>,
}

impl StructScan {
    /// Parse one source file at `module_prefix` into the scan. `Err` only on
    /// a syntax error.
    pub fn add_file(&mut self, source: &str, module_prefix: &str) -> Result<(), String> {
        let file = syn::parse_file(source).map_err(|e| e.to_string())?;
        collect_items(
            &file.items,
            module_prefix,
            &mut self.structs,
            &mut self.test_modules,
        );
        Ok(())
    }

    /// The scanned structs, every one under a `#[cfg(test)]` file module
    /// dropped.
    pub fn finish(self) -> StructMap {
        let in_test_module = |key: &str| {
            self.test_modules
                .iter()
                .any(|m| key.starts_with(&format!("{m}::")))
        };
        self.structs
            .into_iter()
            .filter(|(key, _)| !in_test_module(key))
            .collect()
    }
}

fn collect_items(
    items: &[Item],
    module_prefix: &str,
    map: &mut StructMap,
    test_modules: &mut BTreeSet<String>,
) {
    for item in items {
        match item {
            Item::Struct(s) if !has_cfg_test(&s.attrs) => {
                if let Some(shape) = struct_shape(s) {
                    map.insert(qualify(module_prefix, &s.ident.to_string()), shape);
                }
            }
            Item::Mod(m) => {
                let path = qualify(module_prefix, &m.ident.to_string());
                if has_cfg_test(&m.attrs) {
                    if m.content.is_none() {
                        test_modules.insert(path);
                    }
                } else if let Some((_, inner)) = &m.content {
                    collect_items(inner, &path, map, test_modules);
                }
            }
            _ => {}
        }
    }
}

fn qualify(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

/// Build a [`StructShape`] for a named-field struct that derives serde, else
/// `None` (tuple/unit structs and non-serde structs aren't wire shapes).
fn struct_shape(s: &ItemStruct) -> Option<StructShape> {
    if !derives_serde(&s.attrs) {
        return None;
    }
    let Fields::Named(named) = &s.fields else {
        return None;
    };

    let strict = struct_has_deny_unknown_fields(&s.attrs);
    let derives_deserialize = has_named_derive(&s.attrs, "Deserialize");
    let rename_all = struct_rename_all(&s.attrs);

    let mut fields = BTreeMap::new();
    for field in &named.named {
        let Some(ident) = &field.ident else { continue };
        let attrs = parse_serde_field_attrs(&field.attrs);
        if attrs.skip {
            continue; // off-wire — never decoded
        }
        let wire_name = match &attrs.rename {
            Some(r) => r.clone(),
            None => apply_rename_all(rename_all, &ident.to_string()),
        };
        let (inner_type, is_option) = strip_option(&field.ty);
        fields.insert(
            wire_name,
            FieldShape {
                optional: is_option || attrs.has_default || attrs.flatten,
                inner_type,
                flatten: attrs.flatten,
            },
        );
    }
    Some(StructShape {
        fields,
        strict,
        derives_deserialize,
    })
}

/// `#[cfg(test)]` or `#[cfg(all(.., test, ..))]` — compiled only into tests.
/// Exact, so `cfg(feature = "test-hooks")` (a real build's wire) and
/// `cfg(not(test))` are still scanned. The one matcher both the struct and
/// the [`enums`] scan use.
pub(crate) fn has_cfg_test(attrs: &[Attribute]) -> bool {
    fn is_test(meta: &Meta) -> bool {
        match meta {
            Meta::Path(p) => p.is_ident("test"),
            Meta::List(l) if l.path.is_ident("all") => l
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .is_ok_and(|inner| inner.iter().any(is_test)),
            _ => false,
        }
    }
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .filter_map(|a| a.parse_args::<Meta>().ok())
        .any(|m| is_test(&m))
}

fn derives_serde(attrs: &[Attribute]) -> bool {
    has_named_derive(attrs, "Serialize") || has_named_derive(attrs, "Deserialize")
}

/// `#[derive(.., <derive_name>, ..)]` — bare or path-qualified
/// (`serde::Deserialize`, matched on the last segment) — or the same inside a
/// `#[cfg_attr(.., derive(..))]`. The one derive matcher both the struct and
/// the [`enums`] scan use.
pub(crate) fn has_named_derive(attrs: &[Attribute], derive_name: &str) -> bool {
    attrs.iter().any(|a| {
        if a.path().is_ident("derive") {
            a.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
                .unwrap_or_default()
                .iter()
                .any(|p| p.segments.last().is_some_and(|s| s.ident == derive_name))
        } else if a.path().is_ident("cfg_attr") {
            // Idents split on any non-ident character: the token stream's
            // rendering glues a closing `)` onto the last derive.
            let tokens = a.to_token_stream().to_string();
            let mut words = tokens.split(|c: char| !(c.is_alphanumeric() || c == '_'));
            tokens.contains("derive") && words.any(|w| w == derive_name)
        } else {
            false
        }
    })
}

fn struct_has_deny_unknown_fields(attrs: &[Attribute]) -> bool {
    serde_metas(attrs)
        .iter()
        .any(|m| matches!(m, Meta::Path(p) if p.is_ident("deny_unknown_fields")))
}

fn struct_rename_all(attrs: &[Attribute]) -> Option<RenameRule> {
    serde_metas(attrs).iter().find_map(|m| match m {
        Meta::NameValue(nv) if nv.path.is_ident("rename_all") => {
            RenameRule::from_str(&str_lit(&nv.value)?)
        }
        _ => None,
    })
}

#[derive(Default)]
struct SerdeFieldAttrs {
    rename: Option<String>,
    has_default: bool,
    flatten: bool,
    /// `skip` or `skip_deserializing` — not present on the decode wire.
    skip: bool,
}

fn parse_serde_field_attrs(attrs: &[Attribute]) -> SerdeFieldAttrs {
    let mut out = SerdeFieldAttrs::default();
    for meta in serde_metas(attrs) {
        match &meta {
            Meta::Path(p) if p.is_ident("flatten") => out.flatten = true,
            Meta::Path(p) if p.is_ident("default") => out.has_default = true,
            Meta::Path(p) if p.is_ident("skip") || p.is_ident("skip_deserializing") => {
                out.skip = true
            }
            // `default = "path"` is still a default.
            Meta::NameValue(nv) if nv.path.is_ident("default") => out.has_default = true,
            Meta::NameValue(nv) if nv.path.is_ident("rename") => out.rename = str_lit(&nv.value),
            _ => {}
        }
    }
    out
}

/// Flatten every `#[serde(...)]` attribute on a node into its comma-separated
/// inner [`Meta`] items. `parse_terminated` consumes each `key = value`
/// uniformly, so a key we don't care about can't abort the parse.
fn serde_metas(attrs: &[Attribute]) -> Vec<Meta> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("serde"))
        .filter_map(|a| {
            a.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .ok()
        })
        .flat_map(|p| p.into_iter())
        .collect()
}

/// Extract a string-literal value from a `key = "lit"` meta expression.
fn str_lit(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Some(s.value()),
        _ => None,
    }
}

/// If `ty` is `Option<Inner>`, return (`Inner` normalized, true); else
/// (`ty` normalized, false).
fn strip_option(ty: &Type) -> (String, bool) {
    if let Type::Path(tp) = ty
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return (normalize_type(inner), true);
    }
    (normalize_type(ty), false)
}

/// Peel wire-transparent `Box<…>` wrappers.
///
/// serde's `Serialize`/`Deserialize` impls for `Box<T>` delegate straight
/// through to `T`, so the canonical encoding is byte-for-byte identical and
/// boxing a field is **not** a wire change. It is this codebase's established
/// way to shrink a hot `Result` payload without touching the wire
/// (`transport.md` § Wire format → In-memory representation: `RpcError`'s
/// `details` in 2026-07-30, then its `message` in 2026-09-11, each pinned
/// byte-exact by `rpc_error_wire_bytes_pin`) — so a purely syntactic type
/// comparison would block the technique forever.
///
/// Peeling is **top-level only** and deliberately so: a `Box` nested inside
/// another generic (`Vec<Box<T>>`) still compares literally. That errs toward
/// flagging a change the gate cannot prove transparent, never toward missing
/// one, which is the safe direction for a wire gate.
fn strip_box(ty: &Type) -> &Type {
    let mut cur = ty;
    while let Type::Path(tp) = cur
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Box"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        cur = inner;
    }
    cur
}

/// Whitespace-free token rendering, so both versions stringify identically.
/// Wire-transparent `Box` wrappers are peeled first ([`strip_box`]).
fn normalize_type(ty: &Type) -> String {
    strip_box(ty)
        .to_token_stream()
        .to_string()
        .split_whitespace()
        .collect()
}

// ── serde rename_all ──────────────────────────────────────────────────────────

/// The serde `rename_all` rules. Field idents in this codebase are already
/// `snake_case`, so the transform assumes snake_case input (serde does too).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RenameRule {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl RenameRule {
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "lowercase" => Self::Lower,
            "UPPERCASE" => Self::Upper,
            "PascalCase" => Self::Pascal,
            "camelCase" => Self::Camel,
            "snake_case" => Self::Snake,
            "SCREAMING_SNAKE_CASE" => Self::ScreamingSnake,
            "kebab-case" => Self::Kebab,
            "SCREAMING-KEBAB-CASE" => Self::ScreamingKebab,
            _ => return None,
        })
    }
}

fn apply_rename_all(rule: Option<RenameRule>, ident: &str) -> String {
    let Some(rule) = rule else {
        return ident.to_string();
    };
    let words: Vec<&str> = ident.split('_').filter(|w| !w.is_empty()).collect();
    match rule {
        RenameRule::Snake => ident.to_string(),
        RenameRule::Lower => ident.replace('_', ""),
        RenameRule::Upper => ident.replace('_', "").to_uppercase(),
        RenameRule::ScreamingSnake => ident.to_uppercase(),
        RenameRule::Kebab => ident.replace('_', "-"),
        RenameRule::ScreamingKebab => ident.replace('_', "-").to_uppercase(),
        RenameRule::Pascal => words.iter().map(|w| capitalize(w)).collect(),
        RenameRule::Camel => {
            let mut it = words.iter();
            let first = it.next().map(|w| w.to_string()).unwrap_or_default();
            std::iter::once(first)
                .chain(it.map(|w| capitalize(w)))
                .collect()
        }
    }
}

fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

// ── Diffing ───────────────────────────────────────────────────────────────────

/// The additive-only contract for every struct present under the same key in
/// both `base` and `head`. Structs only in `base` (removed/renamed/moved) or
/// only in `head` (new) are intentionally NOT flagged here — see the module
/// doc. Returns violations sorted for stable output.
pub fn diff_struct_maps(base: &StructMap, head: &StructMap) -> Vec<Violation> {
    diff_struct_maps_allowing(base, head, &RatifiedBreaks::new())
}

/// `diff_struct_maps` minus the user-ratified in-place breaks: `allow` holds
/// the `rust` entries of `libs/fauna-protocol/schemas/ratified-breaks.txt`,
/// `(module::Struct.wire_field, transition)`, each excusing ONLY the finding
/// of its own transition on its own field — a `Removed` entry the removal, an
/// `OptionalToRequired` entry the tightening and never a later removal or
/// retype, a `Retyped(T)` entry a retype TO `T` and no other. And since a removed name never comes
/// back, a head carrying a field the list records as `Removed` is itself a
/// violation, whether or not its struct exists in `base`. The list is the
/// single sanctioned way past the gate, and every line on it is a
/// ratification recorded in version-compatibility.md § Dimension 2.
pub fn diff_struct_maps_allowing(
    base: &StructMap,
    head: &StructMap,
    allow: &RatifiedBreaks,
) -> Vec<Violation> {
    let ratified =
        |key: &str, fname: &str, t: Transition| allow.contains(&(format!("{key}.{fname}"), t));
    let retype_ratified = |key: &str, fname: &str, to: &str| {
        ratified(key, fname, Transition::Retyped(to.to_string()))
    };
    let mut violations = Vec::new();
    for (key, head_struct) in head {
        for fname in head_struct.fields.keys() {
            if ratified(key, fname, Transition::Removed) {
                violations.push(Violation::new(
                    key,
                    format!(
                        "revives wire field `{fname}`, a ratified removal (ratified-breaks.txt) \
                         — a removed name never comes back"
                    ),
                ));
            }
        }
    }
    for (key, base_struct) in base {
        let Some(head_struct) = head.get(key) else {
            continue;
        };
        for (fname, bf) in &base_struct.fields {
            match head_struct.fields.get(fname) {
                None if ratified(key, fname, Transition::Removed) => {}
                None => violations.push(Violation::new(
                    key,
                    format!("removed wire field `{fname}` (a rename is a remove+add — the removal is the break)"),
                )),
                Some(hf) => {
                    if bf.optional
                        && !hf.optional
                        && !ratified(key, fname, Transition::OptionalToRequired)
                    {
                        violations.push(Violation::new(
                            key,
                            format!("field `{fname}` was optional, now required"),
                        ));
                    }
                    if bf.inner_type != hf.inner_type
                        && !retype_ratified(key, fname, &hf.inner_type)
                    {
                        violations.push(Violation::new(
                            key,
                            format!(
                                "field `{fname}` type changed: `{}` -> `{}`",
                                bf.inner_type, hf.inner_type
                            ),
                        ));
                    }
                }
            }
        }
    }
    violations.sort();
    violations
}

/// Self-perpetuation check (wire crate only): a non-strict, decodable struct
/// with at least one field must carry the `#[serde(flatten)]` catch-all (or
/// opt into `#[serde(deny_unknown_fields)]`), so the universal-catch-all
/// property the 2026-06-15 sweep established can't silently regress.
///
/// A **state** check over the whole `head` tree, not a diff against a base: the prior diff-based form only ever saw
/// a struct on the one commit that introduced it, so a violation on a struct
/// already older than the merge-base was permanently invisible to the merge
/// gate — `push_events::StaleSurfaces` (2026-08-22) is the
/// measured instance. `baseline` is the explicit, committed set of struct
/// keys grandfathered out of the rule (`load_catch_all_baseline`,
/// mechanically regenerated by `--update-catch-all-baseline`, never a
/// blanket per-module skip: a per-module exemption would also swallow future
/// violations in that module, which is the same silent-regression failure
/// one level up — transport.md:2225's 2026-08-27 ruling specifically pulled
/// `bridge_routing`'s app-callable `admin-mail` kinds OUT of that module's
/// grandfathering, so the exclusion set must be precise enough to say so).
///
/// A struct that derives `Serialize` only (no `Deserialize`) has no decode
/// side, so neither annotation this check offers is anything but a cosmetic
/// no-op on it — skipped regardless of baseline membership, which is how
/// `StaleSurfaces` is resolved without adding either annotation to it.
pub fn check_catch_all_violations(head: &StructMap, baseline: &BTreeSet<String>) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (key, hs) in head {
        if baseline.contains(key) {
            continue; // explicitly grandfathered
        }
        if hs.strict || hs.fields.is_empty() || hs.has_catch_all() || !hs.derives_deserialize {
            continue; // strict opt-out / empty marker / already compliant / encode-only
        }
        violations.push(Violation::new(
            key,
            "non-strict wire struct lacks the `#[serde(flatten)] extra` catch-all \
             (add it, or `#[serde(deny_unknown_fields)]` if the struct is version-locked) \
             — transport.md § Schema and forward-compat discipline rule 4"
                .to_string(),
        ));
    }
    violations.sort();
    violations
}

/// The transition a `ratified-breaks.txt` entry ratifies. Each excuses
/// exactly the finding of the same kind on its key and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Transition {
    /// `removed` — the field is gone, and its name never comes back.
    Removed,
    /// `optional→required` — the tightening only, never a later removal or
    /// retype of the same field.
    OptionalToRequired,
    /// `retyped→<Type>` — a retype TO the named type (the tool's
    /// whitespace-free rendering, outer `Option<>` stripped) and no other, so
    /// a later retype of the same field away from it is a fresh finding. The
    /// first ratified: the 2026-09-24 baseline reset's two credential secrets,
    /// `SecretBytes` → `SecretByteBuf` (user ruling 2026-09-30).
    Retyped(String),
}

/// The grammar prefix of [`Transition::Retyped`]'s token.
const RETYPED_PREFIX: &str = "retyped→";

impl Transition {
    fn parse(token: &str) -> Option<Self> {
        match token {
            "removed" => Some(Self::Removed),
            "optional→required" => Some(Self::OptionalToRequired),
            _ => token
                .strip_prefix(RETYPED_PREFIX)
                .filter(|ty| !ty.is_empty())
                .map(|ty| Self::Retyped(ty.to_string())),
        }
    }

    /// The grammar token this transition parses from — the inverse of
    /// [`Transition::parse`], used to render a violation message in the same
    /// vocabulary as `ratified-breaks.txt` itself.
    fn as_str(&self) -> String {
        match self {
            Self::Removed => "removed".to_string(),
            Self::OptionalToRequired => "optional→required".to_string(),
            Self::Retyped(ty) => format!("{RETYPED_PREFIX}{ty}"),
        }
    }
}

/// The ratified `(key, transition)` entries: a struct's
/// `module::Struct.wire_field`, or an enum variant's
/// `<crate>::<module>::<Enum>::<Variant>` (only ever `removed`; [`enums`]).
pub type RatifiedBreaks = BTreeSet<(String, Transition)>;

/// The `rust` entries of `ratified-breaks.txt` — `<gate> <key> <transition>
/// <ratified-on> <ratification…>` per line, `#` comments; `cddl` entries
/// belong to the CDDL gate and are skipped here WITHOUT validation — its own
/// parser (`scripts/check-cddl-evolution.py`) validates those. Any OTHER
/// first token is malformed: an entry the gate cannot read must not silently
/// excuse (or silently stop excusing) anything, so an unrecognized gate name
/// is an error exactly like a short or invalid line is.
pub fn parse_ratified_breaks(text: &str) -> Result<RatifiedBreaks, String> {
    let mut entries = RatifiedBreaks::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(&gate) = parts.first() else {
            continue; // blank / comment-only line
        };
        let n = n + 1;
        if gate == "cddl" {
            continue;
        }
        if gate != "rust" {
            return Err(format!(
                "line {n}: unknown gate {gate:?} — want `cddl` or `rust`: {raw:?}"
            ));
        }
        let [_, key, transition, _ratified_on, _ratification, ..] = parts[..] else {
            return Err(format!(
                "line {n}: want `rust <key> <transition> <ratified-on> <ratification…>`: {raw:?}"
            ));
        };
        let Some(transition) = Transition::parse(transition) else {
            return Err(format!(
                "line {n}: transition {transition:?} is not `removed`, `optional→required` or \
                 `retyped→<Type>`: {raw:?}"
            ));
        };
        // A struct key names its wire field after a `.`; an enum key —
        // `<crate>::<module>::<Enum>::<Variant>` — has none, and only a
        // variant's removal is ever ratified.
        if !key.contains('.') && (transition != Transition::Removed || key.split("::").count() < 3)
        {
            return Err(format!(
                "line {n}: key {key:?} names no wire field (`module::Struct.field`) and is \
                 not an enum variant's removal (`<crate>::<module>::<Enum>::<Variant> \
                 removed`): {raw:?}"
            ));
        }
        entries.insert((key.to_string(), transition));
    }
    Ok(entries)
}

/// The `rust` entries present in `base` but missing from `head` — the
/// monotonic half of "the list only grows" (transport.md § Schema and
/// forward-compat discipline). The `revives`-on-lookup check in
/// [`diff_struct_maps_allowing`] catches a *revived name*; this catches the
/// *deletion itself*, so the two-step attack (delete the entry, then revive
/// the name in the same change) can't slip through by leaving the revived
/// key un-excused — deleting the entry alone, with no revival at all, is
/// caught here too. One synthetic [`Violation`] per dropped entry, keyed on
/// the allowlist file rather than a struct, since the break is in the list,
/// not in any wire type.
pub fn check_ratified_breaks_monotonic(
    base: &RatifiedBreaks,
    head: &RatifiedBreaks,
) -> Vec<Violation> {
    base.difference(head)
        .map(|(key, transition)| {
            Violation::new(
                "ratified-breaks.txt",
                format!(
                    "entry `rust {key} {}` at the merge base is missing from HEAD — the list \
                     only grows",
                    transition.as_str()
                ),
            )
        })
        .collect()
}

/// Parse a committed catch-all baseline: one `module::Struct` key per line,
/// blank lines and `#`-prefixed comment lines ignored. Shared by the runtime
/// gate (reads the committed file) and `--update-catch-all-baseline`
/// (writes it) so the two can never drift on format.
pub fn parse_catch_all_baseline(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

impl PartialOrd for Violation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Violation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.key, &self.message).cmp(&(&other.key, &other.message))
    }
}

#[cfg(test)]
mod tests;
