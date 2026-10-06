//! Assembles the `_dmarc.<domain>` TXT record body we **publish** per RFC 7489
//! §6.3, from the deployment-wide policy + a per-domain override partial.
//!
//! This is the *publish* side of DMARC (what peers read about us); the *verify*
//! side (the alignment verdict on inbound mail, [`crate::auth::DmarcPolicy`]) is
//! a separate concern behind the `auth` feature. Kept pure (no tokio/dns/
//! reqwest) and behind the standalone `dmarc-policy` feature so both the nest
//! DNS surface (`fauna.dns.list_records` / `verify_records`) and the WASM-safe
//! onboarding provisioner (`libs/fauna-provisioning`) assemble the identical
//! body — they MUST agree or `verify_records` flags a mismatch right after the
//! provisioner publishes (priority #2: one shared builder, not two `format!`s).
//!
//! Defaults match `docs/goal/behavior/dmarc-reporting.md` § Record shape:
//! `v=DMARC1; p=reject; sp=reject; pct=100; adkim=s; aspf=s;
//! rua=mailto:dmarc-report@<primary>; ri=86400`. The deployment-wide
//! `mail.dmarc.*` catalog write-path is not built yet, so today's caller passes
//! [`DmarcPublishPolicy::default`] as the base; when the catalog lands it feeds
//! a catalog-derived base into the same assembler (no rework).

/// The DMARC tag values and the per-domain override partial-record — defined
/// beside the admin-mail wire row that carries them
/// (`fauna_protocol::bridge_routing`, `dmarc-reporting.md` § Multi-domain
/// deployments) and re-exported here, where the published record is assembled.
pub use fauna_protocol::bridge_routing::{
    DmarcAlignment, DmarcForensicOptions, DmarcMode, DmarcOverrides,
};

/// The fully-resolved DMARC policy for one domain — the deployment-wide defaults
/// with any per-domain override already overlaid. [`to_txt_body`] renders the
/// published `_dmarc.<domain>` TXT body from it.
///
/// [`to_txt_body`]: DmarcPublishPolicy::to_txt_body
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcPublishPolicy {
    pub policy_mode: DmarcMode,
    pub subdomain_policy_mode: DmarcMode,
    pub pct: u8,
    pub adkim: DmarcAlignment,
    pub aspf: DmarcAlignment,
    /// `None` ⇒ the deployment-wide default `mailto:dmarc-report@<primary>`
    /// (the nest report processor, per `mail-multidomain.md` § Per-domain DMARC).
    pub rua_destination: Option<String>,
    /// Forensic reporting is off by default (PII) — no `ruf=`/`fo=` published.
    pub ruf_publish: bool,
    /// `None` ⇒ `mailto:dmarc-report@<primary>` when `ruf_publish` is set.
    pub ruf_destination: Option<String>,
    pub fo: DmarcForensicOptions,
    pub ri_seconds: u32,
}

impl Default for DmarcPublishPolicy {
    /// `docs/goal/behavior/dmarc-reporting.md` § Record shape default-strict
    /// shipped policy.
    fn default() -> Self {
        DmarcPublishPolicy {
            policy_mode: DmarcMode::Reject,
            subdomain_policy_mode: DmarcMode::Reject,
            pct: 100,
            adkim: DmarcAlignment::Strict,
            aspf: DmarcAlignment::Strict,
            rua_destination: None,
            ruf_publish: false,
            ruf_destination: None,
            fo: DmarcForensicOptions::AnyFailure,
            ri_seconds: 86_400,
        }
    }
}

impl DmarcPublishPolicy {
    /// Render the RFC 7489 §6.3 record body (unquoted — the caller frames it as
    /// a TXT record; see [`crate::dns::per_domain::build_dmarc_txt_record`]).
    /// `primary_domain` fills the default `rua=`/`ruf=` template — every domain's
    /// reports route to the single deployment-wide processor at the primary.
    pub fn to_txt_body(&self, primary_domain: &str) -> String {
        let default_report = || format!("mailto:dmarc-report@{primary_domain}");
        let rua = self.rua_destination.clone().unwrap_or_else(default_report);
        let mut body = format!(
            "v=DMARC1; p={}; sp={}; pct={}; adkim={}; aspf={}; rua={}",
            self.policy_mode.as_tag(),
            self.subdomain_policy_mode.as_tag(),
            self.pct,
            self.adkim.as_tag(),
            self.aspf.as_tag(),
            rua,
        );
        if self.ruf_publish {
            let ruf = self.ruf_destination.clone().unwrap_or_else(default_report);
            body.push_str(&format!("; ruf={}; fo={}", ruf, self.fo.as_tag()));
        }
        body.push_str(&format!("; ri={}", self.ri_seconds));
        body
    }
}

/// Overlay a per-domain override partial onto a base policy — `Some` fields win,
/// absent fields inherit.
pub fn apply_overrides(mut base: DmarcPublishPolicy, ov: &DmarcOverrides) -> DmarcPublishPolicy {
    if let Some(v) = ov.policy_mode {
        base.policy_mode = v;
    }
    if let Some(v) = ov.subdomain_policy_mode {
        base.subdomain_policy_mode = v;
    }
    if let Some(v) = ov.pct {
        base.pct = v;
    }
    if let Some(v) = ov.adkim_mode {
        base.adkim = v;
    }
    if let Some(v) = ov.aspf_mode {
        base.aspf = v;
    }
    if ov.rua_destination.is_some() {
        base.rua_destination = ov.rua_destination.clone();
    }
    if let Some(v) = ov.ruf_publish {
        base.ruf_publish = v;
    }
    if ov.ruf_destination.is_some() {
        base.ruf_destination = ov.ruf_destination.clone();
    }
    if let Some(v) = ov.fo_mode {
        base.fo = v;
    }
    if let Some(v) = ov.ri_seconds {
        base.ri_seconds = v;
    }
    base
}

/// Parse the stored per-domain override JSON (`mail_domains.dmarc_overrides`) —
/// the column's one at-rest encoding. `None`/empty/malformed ⇒ no override:
/// the DNS surface renders a valid default record and the admin-mail reply
/// carries an empty partial rather than failing over one bad row.
pub fn parse_stored_overrides(json: Option<&str>) -> DmarcOverrides {
    match json {
        None => DmarcOverrides::default(),
        Some(s) if s.trim().is_empty() => DmarcOverrides::default(),
        Some(s) => serde_json::from_str(s).unwrap_or_default(),
    }
}

/// Apply the stored per-domain override JSON (`mail_domains.dmarc_overrides`)
/// onto `base`, through [`parse_stored_overrides`] — so malformed JSON falls
/// back to `base`.
pub fn apply_overrides_json(base: DmarcPublishPolicy, json: Option<&str>) -> DmarcPublishPolicy {
    apply_overrides(base, &parse_stored_overrides(json))
}

/// Set one domain's published DMARC policy in its stored override partial
/// (`mail_domains.dmarc_overrides`) and return the new column value — the
/// per-domain policy select's write (`dmarc-reporting.md` § Multi-domain
/// deployments). `mode` sets `policy_mode` and `subdomain_policy_mode` together
/// (softening a domain means the whole domain); the deployment default
/// (`p=reject`) removes both keys rather than storing them. Every other key in
/// the stored object survives — including one this version does not know, so a
/// newer nest's override is never lost on a round-trip. A stored value that is
/// not a JSON object already reads as no override ([`parse_stored_overrides`])
/// and is replaced. `None` = no keys left (the column's canonical empty state).
pub fn set_policy_mode_json(json: Option<&str>, mode: DmarcMode) -> Option<String> {
    use serde_json::{Map, Value};
    let mut map = match json.map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Object(map))) => map,
        _ => Map::new(),
    };
    for key in ["policy_mode", "subdomain_policy_mode"] {
        if mode == DmarcPublishPolicy::default().policy_mode {
            map.remove(key);
        } else {
            map.insert(key.to_string(), Value::from(mode.as_tag()));
        }
    }
    (!map.is_empty()).then(|| Value::Object(map).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_body_matches_goal_doc_record_shape() {
        // docs/goal/behavior/dmarc-reporting.md:49 — the canonical default record.
        let body = DmarcPublishPolicy::default().to_txt_body("example.org");
        assert_eq!(
            body,
            "v=DMARC1; p=reject; sp=reject; pct=100; adkim=s; aspf=s; \
             rua=mailto:dmarc-report@example.org; ri=86400"
        );
    }

    #[test]
    fn secondary_domain_rua_points_at_primary_processor() {
        // Every domain's reports route to the deployment-wide processor at the
        // primary (mail-multidomain.md § Per-domain DMARC). The owner name is
        // the per-domain `_dmarc.<domain>`, but the rua mailbox is the primary.
        let body = DmarcPublishPolicy::default().to_txt_body("primary.example");
        assert!(body.contains("rua=mailto:dmarc-report@primary.example"));
    }

    #[test]
    fn override_softens_policy_mode_and_pct() {
        // An admin rolling out a new domain at p=none/pct=50 while the primary
        // stays p=reject — the supported per-domain pattern.
        let json = r#"{"policy_mode":"none","pct":50}"#;
        let policy = apply_overrides_json(DmarcPublishPolicy::default(), Some(json));
        let body = policy.to_txt_body("example.org");
        assert_eq!(
            body,
            "v=DMARC1; p=none; sp=reject; pct=50; adkim=s; aspf=s; \
             rua=mailto:dmarc-report@example.org; ri=86400"
        );
    }

    #[test]
    fn override_custom_rua_and_alignment() {
        let json = r#"{"rua_destination":"mailto:agg@third.example","adkim_mode":"r"}"#;
        let policy = apply_overrides_json(DmarcPublishPolicy::default(), Some(json));
        let body = policy.to_txt_body("example.org");
        assert!(body.contains("rua=mailto:agg@third.example"));
        assert!(body.contains("adkim=r"));
        assert!(body.contains("aspf=s")); // untouched tag inherits the default
    }

    #[test]
    fn forensic_opt_in_publishes_ruf_and_fo() {
        let json = r#"{"ruf_publish":true,"fo_mode":"s"}"#;
        let policy = apply_overrides_json(DmarcPublishPolicy::default(), Some(json));
        let body = policy.to_txt_body("example.org");
        // ruf/fo land between rua and ri.
        assert_eq!(
            body,
            "v=DMARC1; p=reject; sp=reject; pct=100; adkim=s; aspf=s; \
             rua=mailto:dmarc-report@example.org; ruf=mailto:dmarc-report@example.org; \
             fo=s; ri=86400"
        );
    }

    #[test]
    fn none_empty_and_malformed_json_fall_back_to_base() {
        let base = DmarcPublishPolicy::default();
        let expected = base.to_txt_body("example.org");
        assert_eq!(
            apply_overrides_json(base.clone(), None).to_txt_body("example.org"),
            expected
        );
        assert_eq!(
            apply_overrides_json(base.clone(), Some("   ")).to_txt_body("example.org"),
            expected
        );
        assert_eq!(
            apply_overrides_json(base, Some("{not json")).to_txt_body("example.org"),
            expected
        );
    }

    #[test]
    fn set_policy_mode_softens_the_whole_domain_and_keeps_other_keys() {
        let stored = r#"{"pct":50,"future_key":"kept"}"#;
        let out = set_policy_mode_json(Some(stored), DmarcMode::Quarantine).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["policy_mode"], "quarantine");
        assert_eq!(v["subdomain_policy_mode"], "quarantine");
        assert_eq!(v["pct"], 50);
        assert_eq!(v["future_key"], "kept");
        let body = apply_overrides_json(DmarcPublishPolicy::default(), Some(&out))
            .to_txt_body("example.org");
        assert!(
            body.contains("p=quarantine; sp=quarantine; pct=50;"),
            "{body}"
        );
    }

    #[test]
    fn set_policy_mode_default_clears_both_keys() {
        let softened = set_policy_mode_json(None, DmarcMode::None).unwrap();
        assert_eq!(
            parse_stored_overrides(Some(&softened)).policy_mode,
            Some(DmarcMode::None)
        );
        // Back to the default: both keys go, and an otherwise-empty partial is
        // the column's NULL.
        assert_eq!(
            set_policy_mode_json(Some(&softened), DmarcMode::Reject),
            None
        );
        let with_pct = r#"{"policy_mode":"none","subdomain_policy_mode":"none","pct":50}"#;
        assert_eq!(
            set_policy_mode_json(Some(with_pct), DmarcMode::Reject).as_deref(),
            Some(r#"{"pct":50}"#)
        );
    }

    #[test]
    fn set_policy_mode_replaces_a_malformed_stored_value() {
        let out = set_policy_mode_json(Some("not json"), DmarcMode::None).unwrap();
        assert_eq!(
            parse_stored_overrides(Some(&out)).subdomain_policy_mode,
            Some(DmarcMode::None)
        );
        assert_eq!(set_policy_mode_json(Some("[1]"), DmarcMode::Reject), None);
    }
}
