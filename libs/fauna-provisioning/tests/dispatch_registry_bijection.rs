//! A declared capability is a promise `dispatch.rs` has to keep.
//!
//! `i18n/providers.yaml` declares, per provider, which capabilities it has and
//! which dispatch-enum variant serves each one (`dispatch`, `dns_dispatch`,
//! `registrar_dispatch` — normalized into one variant per capability by
//! `scripts/providers-generate.py` and emitted as `ProviderMeta::{dns,vps,
//! registrar}_dispatch`). Honouring that declaration is `dispatch.rs`'s job,
//! and until this test nothing enforced it.
//!
//! Rust's exhaustiveness covers less of this than it looks. A new `ProviderId`
//! does force an arm in all three constructor functions — but the arm is free
//! to be `None`, and every credential lookup inside it is a *string* lookup
//! against the registry's field ids:
//!
//! ```ignore
//! let token = creds.get("api-token")?;   // renamed in providers.yaml -> None
//! ```
//!
//! So renaming a field id in the YAML, or adding a capability to a provider
//! without wiring its adapter, degrades to a silent `None` at runtime — which
//! the wizard reads as "this provider has no DNS", indistinguishable from a
//! genuine capability boundary. No compile error, no panic, just a provider
//! that quietly stops being offered.
//!
//! Two properties are pinned, per (provider, declared capability):
//!
//! 1. **Constructible.** With a complete credentials bag — built *from the
//!    registry's own `FieldMeta`*, which is what makes the field-id rename
//!    visible — the matching constructor returns `Some`.
//! 2. **The right adapter.** The returned variant's `variant_name()` equals
//!    the variant the registry declares for that capability. A copy-paste arm
//!    handing DNS credentials to the wrong provider's adapter fails here.
//!
//! And the converse, so the table can't rot in the other direction: a provider
//! that does *not* declare a capability has no dispatch variant recorded for
//! it, and its constructor returns `None`.

use fauna_provisioning::dispatch::{
    Credentials, dns_provider, parse_credentials, registrar, vps_provider,
};
use fauna_provisioning::{Capability, PROVIDERS, ProviderId};

/// A credentials bag holding every field the registry lists for `id`, keyed by
/// the registry's own field ids.
///
/// Sourcing the keys from `FieldMeta` rather than hard-coding them here is the
/// point: if `providers.yaml` renames `api-token`, this bag renames with it
/// while `dispatch.rs`'s `creds.get("api-token")` does not — and property 1
/// turns that mismatch into a failure instead of a silent `None`.
///
/// Built through `parse_credentials` — the same JSON path the FFI/WASM
/// boundary uses to hand the wizard's bag to dispatch — rather than by
/// assembling `Credentials` directly, so the test exercises the production
/// entry point. Field ids are kebab-case ASCII and the values are ours, so the
/// hand-rolled JSON needs no escaping.
fn full_creds(id: ProviderId) -> Credentials {
    let meta = PROVIDERS
        .iter()
        .find(|p| p.id == id)
        .expect("every ProviderId has a PROVIDERS row");
    let body = meta
        .fields
        .iter()
        .map(|f| {
            // `base-url` is the one field with a shape requirement rather than
            // a mere presence one — `bundled_creds` rejects a blank or
            // whitespace base URL as a missing credential (there is no
            // canonical host to fall back to), so a placeholder token would
            // read as absent.
            let value = if f.id == "base-url" {
                "https://provider.test".to_string()
            } else {
                format!("test-{}", f.id)
            };
            format!("\"{}\":\"{}\"", f.id, value)
        })
        .collect::<Vec<_>>()
        .join(",");
    parse_credentials(&format!("{{{body}}}")).expect("registry field ids form valid JSON keys")
}

/// The variant name each constructor actually yields for `id`, or `None` when
/// it refuses to build one.
fn actual_variant(id: ProviderId, cap: Capability) -> Option<&'static str> {
    let creds = full_creds(id);
    match cap {
        Capability::Dns => dns_provider(id, creds, None).map(|d| d.variant_name()),
        Capability::Vps => vps_provider(id, creds, None).map(|d| d.variant_name()),
        Capability::Registrar => registrar(id, creds).map(|d| d.variant_name()),
    }
}

const ALL_CAPS: [Capability; 3] = [Capability::Dns, Capability::Vps, Capability::Registrar];

#[test]
fn every_declared_capability_dispatches_to_the_variant_the_registry_names() {
    let mut failures = Vec::new();

    for meta in PROVIDERS {
        for cap in ALL_CAPS {
            let declared = meta.dispatch_variant(cap);
            let has_cap = meta.capabilities.contains(&cap);

            // The registry's own two halves must agree before we ask the code:
            // a capability with no declared variant (or a variant with no
            // capability) is a generator bug, and `_validate_dispatch` should
            // already have refused to emit it.
            if has_cap != declared.is_some() {
                failures.push(format!(
                    "{}: capabilities say {:?} for {:?} but dispatch_variant says {:?} \
                     — providers.yaml and its codegen disagree",
                    meta.id.as_str(),
                    has_cap,
                    cap,
                    declared,
                ));
                continue;
            }

            let actual = actual_variant(meta.id, cap);
            match (declared, actual) {
                (Some(want), Some(got)) if want == got => {}
                (None, None) => {}
                (Some(want), None) => failures.push(format!(
                    "{} declares {:?} -> {} in providers.yaml, but dispatch.rs built \
                     nothing from a complete credentials bag. Either the capability is \
                     declared without an adapter, or a `creds.get(\"…\")` in dispatch.rs \
                     names a field id the registry no longer has.",
                    meta.id.as_str(),
                    cap,
                    want,
                )),
                (None, Some(got)) => failures.push(format!(
                    "{} does not declare {:?}, but dispatch.rs built {} for it — add the \
                     capability to providers.yaml or drop the arm.",
                    meta.id.as_str(),
                    cap,
                    got,
                )),
                (Some(want), Some(got)) => failures.push(format!(
                    "{} declares {:?} -> {} in providers.yaml, but dispatch.rs built {}.",
                    meta.id.as_str(),
                    cap,
                    want,
                    got,
                )),
            }
        }
    }

    assert!(
        failures.is_empty(),
        "registry/dispatch disagreement:\n  {}",
        failures.join("\n  "),
    );
}

/// A missing credential must still yield `None` — property 1 asserts the bag
/// *works*, and would be satisfied by a constructor that ignored credentials
/// entirely. This is the other side: an empty bag builds nothing anywhere.
#[test]
fn no_capability_dispatches_without_credentials() {
    for meta in PROVIDERS {
        for cap in ALL_CAPS {
            let empty = Credentials::default();
            let built = match cap {
                Capability::Dns => dns_provider(meta.id, empty, None).map(|d| d.variant_name()),
                Capability::Vps => vps_provider(meta.id, empty, None).map(|d| d.variant_name()),
                Capability::Registrar => registrar(meta.id, empty).map(|d| d.variant_name()),
            };
            assert!(
                built.is_none(),
                "{} built {:?} for {:?} from an EMPTY credentials bag — the constructor is \
                 not reading its credentials, so property 1 above proves nothing for it",
                meta.id.as_str(),
                built,
                cap,
            );
        }
    }
}
