#!/usr/bin/env -S uv run --quiet
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""Generate per-client provider registry code from i18n/providers.yaml.

Targets:
  - Rust:   libs/fauna-provisioning/src/providers_generated.rs
  - TS:     apps/fauna-web/src/lib/generated/providers.ts
  - Swift:  apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/Providers.swift
  - Kotlin: apps/fauna-android/app/src/main/kotlin/social/fauna/generated/Providers.kt
  - C#:     apps/fauna-windows/FaunaApp/FaunaApp/Generated/Providers.cs
  - Python: tests/e2e-unified/generated/providers.py

The PEP-723 header lets `uv run` resolve pyyaml automatically. The shebang
also uses `uv run --quiet` so direct invocation (`python3 providers-generate.py`)
no longer needs system-wide pyyaml — drops fresh-macOS dev-host friction.
"""
from __future__ import annotations
import argparse, sys, re, yaml, pathlib, textwrap, json

ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC  = ROOT / "i18n" / "providers.yaml"

# Relies on Python 3.7+ dict insertion order (preserved by PyYAML safe_load from
# document order); emitter output order matches providers.yaml ordering exactly.
def load() -> dict:
    with SRC.open() as f:
        return yaml.safe_load(f)

def pascal(s: str) -> str:
    return "".join(p.capitalize() for p in re.split(r'[_\-]', s) if p)


def _q(s: str) -> str:
    """Quote a Python string as a source-code string literal.

    Handles: escape backslash and double-quote, convert newline / tab to
    \\n / \\t. The generated Rust, Swift, Kotlin, and C# string-literal
    syntaxes are compatible with this escape set (JSON string syntax is a
    superset; using json.dumps gives the same result).
    """
    return json.dumps(s)  # json.dumps on a str returns a valid source-code literal


ALLOWED_CAPS   = {"dns", "vps", "registrar"}
ALLOWED_CORS   = {"open", "proxy", "native"}
ALLOWED_FIELDS = {"text", "secret", "select", "hosted-auth"}

# Per-language spelling of each field type. `hosted-auth` (the bundled
# provider's device-authorization token — registry.md § Bundled provider) is
# the one kebab-case member, so every emitter maps through here instead of
# re-casing the YAML token; Swift keeps the YAML string as the raw value so
# `Codable` round-trips it.
FIELD_TYPE_RUST   = {"text": "Text", "secret": "Secret", "select": "Select", "hosted-auth": "HostedAuth"}
FIELD_TYPE_SWIFT  = {"text": "text", "secret": "secret", "select": "select", "hosted-auth": "hostedAuth"}
FIELD_TYPE_KOTLIN = {"text": "TEXT", "secret": "SECRET", "select": "SELECT", "hosted-auth": "HOSTED_AUTH"}
FIELD_TYPE_CSHARP = {"text": "Text", "secret": "Secret", "select": "Select", "hosted-auth": "HostedAuth"}


def _validate(data: dict) -> None:
    for pid, p in data["providers"].items():
        caps = p.get("capabilities", [])
        for c in caps:
            if c not in ALLOWED_CAPS:
                raise SystemExit(f"provider {pid}: invalid capability '{c}' (allowed: {sorted(ALLOWED_CAPS)})")
        if p.get("cors_policy") not in ALLOWED_CORS:
            raise SystemExit(f"provider {pid}: invalid cors_policy '{p.get('cors_policy')}' (allowed: {sorted(ALLOWED_CORS)})")
        if not p.get("signup_url"):
            raise SystemExit(f"provider {pid}: missing required signup_url")
        for f in p.get("fields", []):
            if f.get("type") not in ALLOWED_FIELDS:
                raise SystemExit(f"provider {pid} field {f.get('id')}: invalid type '{f.get('type')}' (allowed: {sorted(ALLOWED_FIELDS)})")
            field_kinds = f.get("kinds")
            if field_kinds is not None:
                for k in field_kinds:
                    if k not in ALLOWED_CAPS:
                        raise SystemExit(f"provider {pid} field {f['id']}: invalid kind '{k}' (allowed: {sorted(ALLOWED_CAPS)})")
                    if k not in caps:
                        raise SystemExit(f"provider {pid} field {f['id']}: kind '{k}' not in provider capabilities {caps}")
                if not field_kinds:
                    raise SystemExit(f"provider {pid} field {f['id']}: kinds list cannot be empty (omit the key to default to provider.capabilities)")
        _validate_dispatch(pid, p, caps)


def _validate_dispatch(pid: str, p: dict, caps: list) -> None:
    """Every declared capability resolves to a dispatch variant, and no other.

    These three keys are consumed by the Rust emitter and pinned against the
    hand-written `libs/fauna-provisioning/src/dispatch.rs` by
    `libs/fauna-provisioning/tests/dispatch_registry_bijection.rs`, so a
    malformed declaration here is a generate-time error rather than a silent
    runtime `None` at dispatch time.
    """
    if not caps:
        return
    if not p.get("dispatch"):
        raise SystemExit(f"provider {pid}: missing required `dispatch`")
    if "dns" in caps and "vps" in caps and not p.get("dns_dispatch"):
        raise SystemExit(
            f"provider {pid}: declares both dns and vps, so `dispatch` is the VPS "
            f"variant and DNS needs an explicit `dns_dispatch`"
        )
    if "registrar" in caps and not p.get("registrar_dispatch"):
        raise SystemExit(
            f"provider {pid}: declares registrar capability but has no `registrar_dispatch`"
        )
    if "registrar" not in caps and p.get("registrar_dispatch"):
        raise SystemExit(f"provider {pid}: has `registrar_dispatch` but no registrar capability")
    if "dns" not in caps and p.get("dns_dispatch"):
        raise SystemExit(f"provider {pid}: has `dns_dispatch` but no dns capability")
    for cap, variant in _resolved_dispatch(pid, p).items():
        if not variant:
            raise SystemExit(
                f"provider {pid}: declares {cap} capability but resolves to no dispatch variant"
            )


def _resolved_field_kinds(provider_caps: list, f: dict) -> list:
    """Return the field's kinds list, defaulting to the provider's full capabilities."""
    return list(f.get("kinds") or provider_caps)


def _resolved_dispatch(pid: str, p: dict) -> dict:
    """Resolve one dispatch-enum variant name per declared capability.

    The YAML spells this awkwardly for historical reasons: `dispatch` is the
    provider's *primary* variant -- its DNS one, unless it also does VPS, in
    which case `dispatch` is the VPS variant and DNS moves to `dns_dispatch`.
    `registrar_dispatch` is always explicit. Normalize that here so every
    consumer sees a flat {capability: variant} map instead of re-deriving the
    precedence rule (`_validate_dispatch` enforces the shape this relies on).
    """
    caps = p.get("capabilities", [])
    resolved = {}
    if "vps" in caps:
        resolved["vps"] = p.get("dispatch")
    if "dns" in caps:
        resolved["dns"] = p.get("dns_dispatch") or (
            None if "vps" in caps else p.get("dispatch")
        )
    if "registrar" in caps:
        resolved["registrar"] = p.get("registrar_dispatch")
    return resolved


def _provider_entry_dict(pid: str, p: dict) -> dict:
    """Shared provider shape — used by emit_ts (json.dumps passthrough)."""
    caps = p.get("capabilities", [])
    fields = [
        {
            "id": f["id"],
            "type": f["type"],
            "labelKey": f["label_key"],
            "required": bool(f.get("required", False)),
            "kinds": _resolved_field_kinds(caps, f),
        }
        for f in p.get("fields", [])
    ]
    pvs_raw = p.get("post_verify_select")
    pvs = None if pvs_raw is None else {
        "id": pvs_raw["id"],
        "labelKey": pvs_raw["label_key"],
        "source": pvs_raw["source"],
        "required": bool(pvs_raw.get("required", False)),
    }
    return {
        "id": pid,
        "displayNameKey": p["display_name_key"],
        "helpKey": p["help_key"],
        "signupUrl": p["signup_url"],
        "capabilities": p["capabilities"],
        "corsPolicy": p["cors_policy"],
        "fields": fields,
        "postVerifySelect": pvs,
        "registrarRequiresContact": p.get("registrar_requires_contact"),
        "registrarNotesKey": p.get("registrar_notes_key"),
        "curatedOffers": p.get("curated_offers", []),
        "tldPricingEndpoint": p.get("tld_pricing_endpoint"),
    }

def emit_rust(data: dict) -> str:
    providers = data["providers"]
    out = ["// Generated by scripts/providers-generate.py. DO NOT EDIT.", ""]
    out.append("use serde::{Deserialize, Serialize};")
    out.append("")
    out.append("#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]")
    out.append("pub enum ProviderId {")
    for pid in providers:
        out.append(f"    {pascal(pid)},")
    out.append("}")
    out.append("")
    out.append("impl ProviderId {")
    out.append("    pub fn as_str(self) -> &'static str {")
    out.append("        match self {")
    for pid in providers:
        out.append(f"            ProviderId::{pascal(pid)} => {_q(pid)},")
    out.append("        }")
    out.append("    }")
    out.append("")
    out.append("    #[allow(clippy::should_implement_trait)] // returns Option, not Result")
    out.append("    pub fn from_str(s: &str) -> Option<Self> {")
    out.append("        match s {")
    for pid in providers:
        out.append(f"            {_q(pid)} => Some(ProviderId::{pascal(pid)}),")
    out.append("            _ => None,")
    out.append("        }")
    out.append("    }")
    out.append("}")
    out.append("")
    out.append("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
    out.append("pub enum Capability { Dns, Vps, Registrar }")
    out.append("")
    out.append("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
    out.append("pub enum CorsPolicy { Open, Proxy, Native }")
    out.append("")
    out.append("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
    out.append("pub enum FieldType { Text, Secret, Select, HostedAuth }")
    out.append("")
    out.append("pub struct FieldMeta {")
    out.append("    pub id: &'static str,")
    out.append("    pub field_type: FieldType,")
    out.append("    pub label_key: &'static str,")
    out.append("    pub required: bool,")
    out.append("    /// Credential flows this field applies to. The dns_config form")
    out.append("    /// renders fields where `kinds.contains(&Capability::Dns)`; when")
    out.append("    /// `same_provider_for_vps` is checked AND the provider has VPS")
    out.append("    /// capability, it also includes fields where `kinds.contains(&Capability::Vps)`.")
    out.append("    pub kinds: &'static [Capability],")
    out.append("}")
    out.append("")
    out.append("pub struct PostVerifySelect {")
    out.append("    pub id: &'static str,")
    out.append("    pub label_key: &'static str,")
    out.append("    pub source: &'static str,")
    out.append("    pub required: bool,")
    out.append("}")
    out.append("")
    # Emit PostVerifySelect statics for each provider that has one
    for pid, p in providers.items():
        pvs = p.get("post_verify_select")
        if pvs:
            static_name = f"PVS_{pid.upper().replace('-', '_')}"
            out.append(f"static {static_name}: PostVerifySelect = PostVerifySelect {{")
            out.append(f"    id: {_q(pvs['id'])},")
            out.append(f"    label_key: {_q(pvs['label_key'])},")
            out.append(f"    source: {_q(pvs['source'])},")
            out.append(f"    required: {str(bool(pvs.get('required', False))).lower()},")
            out.append("};")
            out.append("")
    out.append("// Extended from the original narrow shape to include i18n keys and")
    out.append("// post-verify-select metadata needed by native Linux/macOS UI rendering.")
    out.append("// The TS/Swift/Kotlin/C# shapes already carry these fields; this Rust")
    out.append("// extension mirrors them additively without breaking other crates.")
    out.append("pub struct ProviderMeta {")
    out.append("    pub id: ProviderId,")
    out.append("    pub display_name_key: &'static str,")
    out.append("    pub help_key: &'static str,")
    out.append("    pub signup_url: &'static str,")
    out.append("    pub capabilities: &'static [Capability],")
    out.append("    pub cors_policy: CorsPolicy,")
    out.append("    pub fields: &'static [FieldMeta],")
    out.append("    pub post_verify_select: Option<&'static PostVerifySelect>,")
    out.append("    pub registrar_requires_contact: Option<bool>,")
    out.append("    pub registrar_notes_key: Option<&'static str>,")
    out.append("    pub curated_offers: &'static [&'static str],")
    out.append("    pub tld_pricing_endpoint: Option<&'static str>,")
    out.append("    /// Name of the `DnsDispatch` variant serving this provider, or")
    out.append("    /// `None` when it has no DNS capability. Pinned against the")
    out.append("    /// hand-written `dispatch.rs` by `tests/dispatch_registry_bijection.rs`.")
    out.append("    pub dns_dispatch: Option<&'static str>,")
    out.append("    /// Name of the `VpsDispatch` variant serving this provider, or")
    out.append("    /// `None` when it has no VPS capability.")
    out.append("    pub vps_dispatch: Option<&'static str>,")
    out.append("    /// Name of the `RegistrarDispatch` variant serving this provider,")
    out.append("    /// or `None` when it has no registrar capability.")
    out.append("    pub registrar_dispatch: Option<&'static str>,")
    out.append("}")
    out.append("")
    out.append("impl ProviderMeta {")
    out.append("    /// The dispatch-enum variant name serving `cap` for this provider,")
    out.append("    /// or `None` when the provider does not declare that capability.")
    out.append("    ///")
    out.append("    /// This is the registry's half of the dispatch contract: the other")
    out.append("    /// half is `DnsDispatch::variant_name` and friends, and")
    out.append("    /// `tests/dispatch_registry_bijection.rs` asserts they agree for")
    out.append("    /// every provider x declared capability.")
    out.append("    pub fn dispatch_variant(&self, cap: Capability) -> Option<&'static str> {")
    out.append("        match cap {")
    out.append("            Capability::Dns => self.dns_dispatch,")
    out.append("            Capability::Vps => self.vps_dispatch,")
    out.append("            Capability::Registrar => self.registrar_dispatch,")
    out.append("        }")
    out.append("    }")
    out.append("}")
    out.append("")
    out.append("pub static PROVIDERS: &[ProviderMeta] = &[")
    for pid, p in providers.items():
        caps = ", ".join(f"Capability::{c.capitalize()}" for c in p["capabilities"])
        cors = {"open": "Open", "proxy": "Proxy", "native": "Native"}[p["cors_policy"]]
        fields = ", ".join(_emit_field_rust(f, p["capabilities"]) for f in p.get("fields", []))
        pvs = p.get("post_verify_select")
        pvs_expr = f"Some(&PVS_{pid.upper().replace('-', '_')})" if pvs else "None"
        rrc = p.get("registrar_requires_contact")
        rrc_expr = ("None" if rrc is None
                    else ("Some(true)" if rrc else "Some(false)"))
        rnk = p.get("registrar_notes_key")
        rnk_expr = f"Some({_q(rnk)})" if rnk is not None else "None"
        offers = p.get("curated_offers", []) or []
        offers_expr = ", ".join(_q(o) for o in offers)
        tpe = p.get("tld_pricing_endpoint")
        tpe_expr = f"Some({_q(tpe)})" if tpe is not None else "None"
        disp = _resolved_dispatch(pid, p)
        disp_exprs = {
            cap: (f"Some({_q(disp[cap])})" if disp.get(cap) else "None")
            for cap in ("dns", "vps", "registrar")
        }
        out.append(f"    ProviderMeta {{")
        out.append(f"        id: ProviderId::{pascal(pid)},")
        out.append(f"        display_name_key: {_q(p['display_name_key'])},")
        out.append(f"        help_key: {_q(p['help_key'])},")
        out.append(f"        signup_url: {_q(p['signup_url'])},")
        out.append(f"        capabilities: &[{caps}],")
        out.append(f"        cors_policy: CorsPolicy::{cors},")
        out.append(f"        fields: &[{fields}],")
        out.append(f"        post_verify_select: {pvs_expr},")
        out.append(f"        registrar_requires_contact: {rrc_expr},")
        out.append(f"        registrar_notes_key: {rnk_expr},")
        out.append(f"        curated_offers: &[{offers_expr}],")
        out.append(f"        tld_pricing_endpoint: {tpe_expr},")
        out.append(f"        dns_dispatch: {disp_exprs['dns']},")
        out.append(f"        vps_dispatch: {disp_exprs['vps']},")
        out.append(f"        registrar_dispatch: {disp_exprs['registrar']},")
        out.append(f"    }},")
    out.append("];")
    out.append("")
    return "\n".join(out)

def _emit_field_rust(f: dict, provider_caps: list) -> str:
    t = FIELD_TYPE_RUST[f["type"]]
    req = str(bool(f.get("required", False))).lower()
    kinds = _resolved_field_kinds(provider_caps, f)
    kinds_expr = ", ".join(f"Capability::{k.capitalize()}" for k in kinds)
    return (f'FieldMeta {{ id: {_q(f["id"])}, '
            f'field_type: FieldType::{t}, '
            f'label_key: {_q(f["label_key"])}, '
            f'required: {req}, '
            f'kinds: &[{kinds_expr}] }}')

def emit_ts(data: dict) -> str:
    providers = data["providers"]
    lines = [
        "// Generated by scripts/providers-generate.py. DO NOT EDIT.",
        "",
        "export type Capability = 'dns' | 'vps' | 'registrar';",
        "export type CorsPolicy = 'open' | 'proxy' | 'native';",
        "export type FieldType = 'text' | 'secret' | 'select' | 'hosted-auth';",
        "",
        "export interface FieldMeta {",
        "  id: string;",
        "  type: FieldType;",
        "  labelKey: string;",
        "  required: boolean;",
        "  /** Credential flows this field applies to. dns_config form renders",
        "   *  fields with 'dns' in kinds; vps_config renders fields with 'vps'",
        "   *  in kinds; when same_provider_for_vps is checked dns_config shows",
        "   *  the union (kinds includes 'dns' or 'vps'). */",
        "  kinds: Capability[];",
        "}",
        "",
        "export interface PostVerifySelect {",
        "  id: string;",
        "  labelKey: string;",
        "  source: string;",
        "  required: boolean;",
        "}",
        "",
        "export interface ProviderMeta {",
        "  id: string;",
        "  displayNameKey: string;",
        "  helpKey: string;",
        "  /** Literal URL the wizard surfaces as the \"open in browser\" link",
        "   *  on the per-provider section of dns_config / vps_config. */",
        "  signupUrl: string;",
        "  capabilities: Capability[];",
        "  corsPolicy: CorsPolicy;",
        "  fields: FieldMeta[];",
        "  postVerifySelect: PostVerifySelect | null;",
        "  /** Registrar-only: whether the provider's register() API accepts",
        "   *  per-domain contact info. Porkbun sets this to false (uses",
        "   *  account-level contact). Null for non-registrar providers. */",
        "  registrarRequiresContact: boolean | null;",
        "  /** Registrar-only: i18n key for a reminder message shown when",
        "   *  registrarRequiresContact is false (e.g., \"set contact on your",
        "   *  Porkbun account\"). Null when absent. */",
        "  registrarNotesKey: string | null;",
        "  /** VPS-only: provider-native server_type/plan IDs the wizard offers",
        "   *  as radio options on the VPS configuration page. Empty for",
        "   *  providers not in the handle-first VPS picker. */",
        "  curatedOffers: string[];",
        "  /** Registrar-only: public-pricing URL the registrar exposes without",
        "   *  auth, fetched on the DNS-config page for the buy-domain estimate.",
        "   *  Null when the registrar has no public pricing API. */",
        "  tldPricingEndpoint: string | null;",
        "}",
        "",
        "export const PROVIDERS: ProviderMeta[] = [",
    ]
    for pid, p in providers.items():
        entry = _provider_entry_dict(pid, p)
        pretty = json.dumps(entry, indent=2)
        lines.append("  " + pretty.replace("\n", "\n  ") + ",")
    lines.append("];")
    lines.append("")
    return "\n".join(lines)

def emit_python(data: dict) -> str:
    return textwrap.dedent('''\
        # Generated by scripts/providers-generate.py. DO NOT EDIT.
        import yaml, pathlib
        _SRC = pathlib.Path(__file__).resolve().parents[3] / "i18n" / "providers.yaml"
        with _SRC.open() as _f:
            DATA = yaml.safe_load(_f)
        PROVIDERS = DATA["providers"]
        MODES = DATA["modes"]
        ''')

def emit_swift(data: dict) -> str:
    providers = data["providers"]
    out = [
        "// Generated by scripts/providers-generate.py. DO NOT EDIT.",
        "import Foundation",
        "",
        "public enum Capability: String, Codable { case dns, vps, registrar }",
        "public enum CorsPolicy: String, Codable { case open, proxy, native }",
        "public enum FieldType: String, Codable { case text, secret, select, hostedAuth = \"hosted-auth\" }",
        "",
        "public struct FieldMeta: Codable {",
        "    public let id: String",
        "    public let type: FieldType",
        "    public let labelKey: String",
        "    public let required: Bool",
        "    /// Credential flows this field applies to (dns / vps / registrar).",
        "    public let kinds: [Capability]",
        "}",
        "",
        "public struct PostVerifySelect: Codable {",
        "    public let id: String",
        "    public let labelKey: String",
        "    public let source: String",
        "    public let required: Bool",
        "}",
        "",
        "public struct ProviderMeta {",
        "    public let id: String",
        "    public let displayNameKey: String",
        "    public let helpKey: String",
        "    public let signupUrl: String",
        "    public let capabilities: [Capability]",
        "    public let corsPolicy: CorsPolicy",
        "    public let fields: [FieldMeta]",
        "    public let postVerifySelect: PostVerifySelect?",
        "    public let curatedOffers: [String]",
        "    public let tldPricingEndpoint: String?",
        "    public let registrarNotesKey: String?",
        "}",
        "",
        "public let PROVIDERS: [ProviderMeta] = [",
    ]
    def sbool(v): return "true" if v else "false"
    for pid, p in providers.items():
        out.append("    ProviderMeta(")
        out.append(f'        id: {_q(pid)},')
        out.append(f'        displayNameKey: {_q(p["display_name_key"])},')
        out.append(f'        helpKey: {_q(p["help_key"])},')
        out.append(f'        signupUrl: {_q(p["signup_url"])},')
        caps = ", ".join(f".{c}" for c in p["capabilities"])
        out.append(f"        capabilities: [{caps}],")
        out.append(f'        corsPolicy: .{p["cors_policy"]},')
        field_items = []
        for f in p.get("fields", []):
            kinds = _resolved_field_kinds(p["capabilities"], f)
            kinds_swift = ", ".join(f".{k}" for k in kinds)
            field_items.append(
                f'FieldMeta(id: {_q(f["id"])}, type: .{FIELD_TYPE_SWIFT[f["type"]]}, '
                f'labelKey: {_q(f["label_key"])}, required: {sbool(f.get("required", False))}, '
                f'kinds: [{kinds_swift}])'
            )
        out.append(f"        fields: [{', '.join(field_items)}],")
        pvs = p.get("post_verify_select")
        if pvs:
            out.append(
                f'        postVerifySelect: PostVerifySelect('
                f'id: {_q(pvs["id"])}, labelKey: {_q(pvs["label_key"])}, '
                f'source: {_q(pvs["source"])}, required: {sbool(pvs.get("required", False))}),'
            )
        else:
            out.append("        postVerifySelect: nil,")
        offers = p.get("curated_offers", []) or []
        offers_swift = ", ".join(_q(o) for o in offers)
        out.append(f"        curatedOffers: [{offers_swift}],")
        tpe = p.get("tld_pricing_endpoint")
        out.append(f'        tldPricingEndpoint: {_q(tpe) if tpe is not None else "nil"},')
        rnk = p.get("registrar_notes_key")
        out.append(f'        registrarNotesKey: {_q(rnk) if rnk is not None else "nil"}')
        out.append("    ),")
    out.append("]")
    out.append("")
    return "\n".join(out)

def emit_kotlin(data: dict) -> str:
    providers = data["providers"]
    out = [
        "// Generated by scripts/providers-generate.py. DO NOT EDIT.",
        "package social.fauna.generated",
        "",
        "enum class Capability { DNS, VPS, REGISTRAR }",
        "enum class CorsPolicy { OPEN, PROXY, NATIVE }",
        "enum class FieldType { TEXT, SECRET, SELECT, HOSTED_AUTH }",
        "",
        "data class FieldMeta(val id: String, val type: FieldType, val labelKey: String, val required: Boolean, val kinds: List<Capability>)",
        "data class PostVerifySelect(val id: String, val labelKey: String, val source: String, val required: Boolean)",
        "data class ProviderMeta(",
        "    val id: String,",
        "    val displayNameKey: String,",
        "    val helpKey: String,",
        "    val signupUrl: String,",
        "    val capabilities: List<Capability>,",
        "    val corsPolicy: CorsPolicy,",
        "    val fields: List<FieldMeta>,",
        "    val postVerifySelect: PostVerifySelect?,",
        "    val curatedOffers: List<String>,",
        "    val tldPricingEndpoint: String?,",
        "    val registrarNotesKey: String?,",
        ")",
        "",
        "val PROVIDERS: List<ProviderMeta> = listOf(",
    ]
    def kbool(v): return "true" if v else "false"
    for pid, p in providers.items():
        out.append("    ProviderMeta(")
        out.append(f'        id = {_q(pid)},')
        out.append(f'        displayNameKey = {_q(p["display_name_key"])},')
        out.append(f'        helpKey = {_q(p["help_key"])},')
        out.append(f'        signupUrl = {_q(p["signup_url"])},')
        caps = ", ".join(f"Capability.{c.upper()}" for c in p["capabilities"])
        out.append(f"        capabilities = listOf({caps}),")
        out.append(f'        corsPolicy = CorsPolicy.{p["cors_policy"].upper()},')
        field_items = []
        for f in p.get("fields", []):
            kinds = _resolved_field_kinds(p["capabilities"], f)
            kinds_kt = ", ".join(f"Capability.{k.upper()}" for k in kinds)
            field_items.append(
                f'FieldMeta({_q(f["id"])}, FieldType.{FIELD_TYPE_KOTLIN[f["type"]]}, '
                f'{_q(f["label_key"])}, {kbool(f.get("required", False))}, '
                f'listOf({kinds_kt}))'
            )
        out.append(f"        fields = listOf({', '.join(field_items)}),")
        pvs = p.get("post_verify_select")
        if pvs:
            out.append(
                f'        postVerifySelect = PostVerifySelect('
                f'{_q(pvs["id"])}, {_q(pvs["label_key"])}, '
                f'{_q(pvs["source"])}, {kbool(pvs.get("required", False))}),'
            )
        else:
            out.append("        postVerifySelect = null,")
        offers = p.get("curated_offers", []) or []
        offers_kt = ", ".join(_q(o) for o in offers)
        out.append(f"        curatedOffers = listOf({offers_kt}),")
        tpe = p.get("tld_pricing_endpoint")
        out.append(f'        tldPricingEndpoint = {_q(tpe) if tpe is not None else "null"},')
        rnk = p.get("registrar_notes_key")
        out.append(f'        registrarNotesKey = {_q(rnk) if rnk is not None else "null"},')
        out.append("    ),")
    out.append(")")
    out.append("")
    return "\n".join(out)

def emit_csharp(data: dict) -> str:
    providers = data["providers"]
    out = [
        "// Generated by scripts/providers-generate.py. DO NOT EDIT.",
        "namespace Fauna.Generated;",
        "",
        "public enum Capability { Dns, Vps, Registrar }",
        "public enum CorsPolicy { Open, Proxy, Native }",
        "public enum FieldType { Text, Secret, Select, HostedAuth }",
        "",
        "public record FieldMeta(string Id, FieldType Type, string LabelKey, bool Required, Capability[] Kinds);",
        "public record PostVerifySelect(string Id, string LabelKey, string Source, bool Required);",
        "public record ProviderMeta(",
        "    string Id,",
        "    string DisplayNameKey,",
        "    string HelpKey,",
        "    string SignupUrl,",
        "    Capability[] Capabilities,",
        "    CorsPolicy CorsPolicy,",
        "    FieldMeta[] Fields,",
        "    PostVerifySelect? PostVerifySelect,",
        "    string[] CuratedOffers,",
        "    string? TldPricingEndpoint,",
        "    string? RegistrarNotesKey);",
        "",
        "public static class Providers",
        "{",
        "    public static readonly ProviderMeta[] All = new[]",
        "    {",
    ]
    def cbool(v): return "true" if v else "false"
    for pid, p in providers.items():
        out.append("        new ProviderMeta(")
        out.append(f'            Id: {_q(pid)},')
        out.append(f'            DisplayNameKey: {_q(p["display_name_key"])},')
        out.append(f'            HelpKey: {_q(p["help_key"])},')
        out.append(f'            SignupUrl: {_q(p["signup_url"])},')
        caps = ", ".join(f"Capability.{c.capitalize()}" for c in p["capabilities"])
        out.append(f"            Capabilities: new[] {{ {caps} }},")
        out.append(f'            CorsPolicy: CorsPolicy.{p["cors_policy"].capitalize()},')
        field_items = []
        for f in p.get("fields", []):
            kinds = _resolved_field_kinds(p["capabilities"], f)
            kinds_cs = ", ".join(f"Capability.{k.capitalize()}" for k in kinds)
            field_items.append(
                f'new FieldMeta({_q(f["id"])}, FieldType.{FIELD_TYPE_CSHARP[f["type"]]}, '
                f'{_q(f["label_key"])}, {cbool(f.get("required", False))}, '
                f'new[] {{ {kinds_cs} }})'
            )
        out.append(f"            Fields: new[] {{ {', '.join(field_items)} }},")
        pvs = p.get("post_verify_select")
        if pvs:
            out.append(
                f'            PostVerifySelect: new PostVerifySelect('
                f'{_q(pvs["id"])}, {_q(pvs["label_key"])}, '
                f'{_q(pvs["source"])}, {cbool(pvs.get("required", False))}),'
            )
        else:
            out.append("            PostVerifySelect: null,")
        offers = p.get("curated_offers", []) or []
        if offers:
            offers_cs = ", ".join(_q(o) for o in offers)
            out.append(f"            CuratedOffers: new[] {{ {offers_cs} }},")
        else:
            out.append("            CuratedOffers: System.Array.Empty<string>(),")
        tpe = p.get("tld_pricing_endpoint")
        out.append(f'            TldPricingEndpoint: {_q(tpe) if tpe is not None else "null"},')
        rnk = p.get("registrar_notes_key")
        out.append(f'            RegistrarNotesKey: {_q(rnk) if rnk is not None else "null"}')
        out.append("        ),")
    out.append("    };")
    out.append("}")
    out.append("")
    return "\n".join(out)

TARGETS = {
    "rust":   ("libs/fauna-provisioning/src/providers_generated.rs", emit_rust),
    "ts":     ("apps/fauna-web/src/lib/generated/providers.ts", emit_ts),
    "swift":  ("apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/Providers.swift", emit_swift),
    "kotlin": ("apps/fauna-android/app/src/main/kotlin/social/fauna/generated/Providers.kt", emit_kotlin),
    "csharp": ("apps/fauna-windows/FaunaApp/FaunaApp/Generated/Providers.cs", emit_csharp),
    "python": ("tests/e2e-unified/generated/providers.py", emit_python),
}

def main() -> int:
    parser = argparse.ArgumentParser(description="Generate per-client provider registry code")
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify generated files are up-to-date (exit 1 if not)",
    )
    args = parser.parse_args()

    data = load()
    _validate(data)
    all_ok = True
    for name, (path, emit) in TARGETS.items():
        out = ROOT / path
        content = emit(data)
        if args.check:
            if not out.exists():
                print(f"MISSING: {path}")
                all_ok = False
            elif out.read_text() != content:
                print(f"OUT-OF-DATE: {path}")
                all_ok = False
            else:
                print(f"OK: {path}")
        else:
            out.parent.mkdir(parents=True, exist_ok=True)
            if out.exists() and out.read_text() == content:
                print(f"unchanged {path}")
            else:
                out.write_text(content)
                print(f"wrote {path}")
    return 0 if all_ok else 1

if __name__ == "__main__":
    sys.exit(main())
