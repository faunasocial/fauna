//! Spaceship as a DNS provider, for the domains registered on the account
//! (`docs/goal/architecture/provisioning/registry.md` § Delegation doors, the
//! Spaceship paragraph). Spaceship has no zone create, so it declares no zone
//! door: a zone exists for every domain the account holds.
//!
//! - `verify` — the account's domains (`GET /domains`, paged) as zones, id and
//!   name both the A-label domain.
//! - `create_record` — `PUT /dns/records/{domain}` with one item, which adds
//!   the record or updates the one it matches.
//! - `find_records` / `delete_record` — `GET /dns/records/{domain}` paged and
//!   filtered here; `DELETE /dns/records/{domain}` takes a JSON array of the
//!   items to remove, matched by their fields.
//!
//! Spaceship's records are typed objects, not RDATA strings, so this adapter
//! translates the shared [`DnsRecord`] — the owner name verbatim (the caller
//! relativizes, § The owner-name contract), the value in zone-file RDATA form —
//! to each type's fields and back. The two the shared form packs into the
//! owner name are split out: an SRV `_service._proto[.host]` and a TLSA
//! `_port._proto[.host]` become Spaceship's `service`/`port` + `protocol`
//! fields over the remaining `name` (`@` when nothing remains).

use serde_json::{Value, json};

use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::{ProvisionError, ensure_success};
use crate::proxy::BuildEnv;
use crate::spaceship_api::SpaceshipApi;

/// The largest page `GET /dns/records` answers (its items cap at 100).
const RECORDS_PAGE: u32 = 100;
/// Spaceship's TTL bounds (seconds).
const TTL_MIN: u32 = 60;
const TTL_MAX: u32 = 3600;

pub struct SpaceshipDns {
    api: SpaceshipApi,
}

impl SpaceshipDns {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self {
            api: SpaceshipApi::new(api_key, api_secret),
        }
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing (`tests/cors_policy_bijection.rs`).
    pub fn new_for_env(api_key: String, api_secret: String, env: BuildEnv) -> Self {
        Self {
            api: SpaceshipApi::new_for_env(api_key, api_secret, env),
        }
    }

    /// Point the adapter at a wiremock server or the e2e fake.
    pub fn with_base_url(api_key: String, api_secret: String, api_base: String) -> Self {
        Self {
            api: SpaceshipApi::with_base_url(api_key, api_secret, api_base),
        }
    }

    /// The API base this adapter calls.
    pub fn api_base(&self) -> &str {
        self.api.api_base()
    }

    /// Every record of the zone, as Spaceship's typed items.
    async fn list_items(
        &self,
        client: &reqwest::Client,
        zone: &str,
    ) -> Result<Vec<Value>, ProvisionError> {
        let mut out = Vec::new();
        let mut skip: u32 = 0;
        loop {
            let resp = self
                .api
                .request(
                    client,
                    reqwest::Method::GET,
                    &format!("/dns/records/{zone}?take={RECORDS_PAGE}&skip={skip}"),
                )
                .send()
                .await?;
            let page: Value = ensure_success(resp)
                .await?
                .json()
                .await
                .map_err(ProvisionError::parse)?;
            let items = page
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let total = page.get("total").and_then(Value::as_u64).unwrap_or(0);
            let got = items.len() as u64;
            out.extend(items);
            skip += got as u32;
            if got == 0 || u64::from(skip) >= total {
                return Ok(out);
            }
        }
    }

    /// The zone's records at `(name, record_type)`, in the shared form.
    async fn matching(
        &self,
        client: &reqwest::Client,
        zone: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        Ok(self
            .list_items(client, zone)
            .await?
            .iter()
            .filter_map(from_item)
            .filter(|r| {
                r.record_type.eq_ignore_ascii_case(record_type) && r.name.eq_ignore_ascii_case(name)
            })
            .collect())
    }
}

/// A hostname as Spaceship's patterns take it: no trailing root dot.
fn host(v: &str) -> String {
    v.trim().trim_end_matches('.').to_string()
}

/// Split an owner `_a._b[.rest]` into (`_a`, `_b`, `rest` or `@`).
fn split_underscored(name: &str, what: &str) -> Result<(String, String, String), ProvisionError> {
    let mut parts = name.splitn(3, '.');
    let (a, b) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if !(a.starts_with('_') && b.starts_with('_')) {
        return Err(ProvisionError::Other(format!(
            "a {what} record's name must start `_x._proto` for Spaceship, got {name:?}"
        )));
    }
    let rest = parts.next().filter(|r| !r.is_empty()).unwrap_or("@");
    Ok((a.to_string(), b.to_string(), rest.to_string()))
}

/// Join Spaceship's split fields back into the shared owner name.
fn join_underscored(a: &str, b: &str, rest: &str) -> String {
    if rest == "@" || rest.is_empty() {
        format!("{a}.{b}")
    } else {
        format!("{a}.{b}.{rest}")
    }
}

fn parse_u16(s: &str, what: &str) -> Result<u16, ProvisionError> {
    s.parse().map_err(|_| {
        ProvisionError::Other(format!("Spaceship needs a number for {what}, got {s:?}"))
    })
}

/// The typed item for `record`, without its TTL (a delete item carries none).
fn item_fields(record: &DnsRecord) -> Result<Value, ProvisionError> {
    let t = record.record_type.to_ascii_uppercase();
    let v = record.value.trim();
    let malformed = || {
        ProvisionError::Other(format!(
            "malformed {t} value for Spaceship: {:?}",
            record.value
        ))
    };
    Ok(match t.as_str() {
        "A" | "AAAA" => json!({ "type": t, "name": record.name, "address": v }),
        "CNAME" => json!({ "type": t, "name": record.name, "cname": host(v) }),
        "NS" => json!({ "type": t, "name": record.name, "nameserver": host(v) }),
        "TXT" => json!({ "type": t, "name": record.name, "value": record.value }),
        "MX" => {
            // The priority rides `priority`; a value that folds it in
            // (`"10 mail.example.com."`) is read either way.
            let (pref, exchange) = match (record.priority, v.split_once(char::is_whitespace)) {
                (Some(p), _) => (p, v),
                (None, Some((p, h))) => (u32::from(parse_u16(p, "an MX preference")?), h),
                (None, None) => return Err(malformed()),
            };
            json!({ "type": t, "name": record.name, "exchange": host(exchange), "preference": pref })
        }
        "CAA" => {
            let mut it = v.splitn(3, char::is_whitespace);
            let (flag, tag, value) = (
                it.next().ok_or_else(malformed)?,
                it.next().ok_or_else(malformed)?,
                it.next().ok_or_else(malformed)?,
            );
            json!({
                "type": t, "name": record.name,
                "flag": parse_u16(flag, "a CAA flag")?,
                "tag": tag,
                "value": value.trim().trim_matches('"'),
            })
        }
        "SRV" => {
            let (service, protocol, name) = split_underscored(&record.name, "SRV")?;
            let parts: Vec<&str> = v.split_whitespace().collect();
            let (prio, rest) = match (parts.len(), record.priority) {
                (4, _) => (parse_u16(parts[0], "an SRV priority")?, &parts[1..]),
                (3, Some(p)) => (u16::try_from(p).map_err(|_| malformed())?, &parts[..]),
                _ => return Err(malformed()),
            };
            json!({
                "type": t, "name": name, "service": service, "protocol": protocol,
                "priority": prio,
                "weight": parse_u16(rest[0], "an SRV weight")?,
                "port": parse_u16(rest[1], "an SRV port")?,
                "target": host(rest[2]),
            })
        }
        "TLSA" => {
            let (port, protocol, name) = split_underscored(&record.name, "TLSA")?;
            let parts: Vec<&str> = v.split_whitespace().collect();
            let [usage, selector, matching, data] = parts[..] else {
                return Err(malformed());
            };
            json!({
                "type": t, "name": name, "port": port, "protocol": protocol,
                "usage": parse_u16(usage, "a TLSA usage")?,
                "selector": parse_u16(selector, "a TLSA selector")?,
                "matching": parse_u16(matching, "a TLSA matching type")?,
                "associationData": data.to_ascii_lowercase(),
            })
        }
        other => {
            return Err(ProvisionError::Other(format!(
                "this Fauna build does not write {other} records to Spaceship"
            )));
        }
    })
}

/// The typed item `PUT /dns/records` takes for `record`.
fn create_item(record: &DnsRecord) -> Result<Value, ProvisionError> {
    let mut item = item_fields(record)?;
    item["ttl"] = json!(record.ttl.clamp(TTL_MIN, TTL_MAX));
    Ok(item)
}

/// A listed item back in the shared form; `None` for a type this adapter does
/// not translate (it is then simply not matched).
fn from_item(item: &Value) -> Option<DnsRecord> {
    let s = |k: &str| item.get(k).and_then(Value::as_str).map(str::to_string);
    let n = |k: &str| item.get(k).and_then(Value::as_u64);
    let t = s("type")?.to_ascii_uppercase();
    let name = s("name").unwrap_or_else(|| "@".to_string());
    let ttl = n("ttl").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    let (name, value, priority) = match t.as_str() {
        "A" | "AAAA" => (name, s("address")?, None),
        "CNAME" => (name, s("cname")?, None),
        "NS" => (name, s("nameserver")?, None),
        "TXT" => (name, s("value")?, None),
        "MX" => (
            name,
            s("exchange")?,
            n("preference").and_then(|p| u32::try_from(p).ok()),
        ),
        "CAA" => (
            name,
            format!("{} {} \"{}\"", n("flag")?, s("tag")?, s("value")?),
            None,
        ),
        "SRV" => (
            join_underscored(&s("service")?, &s("protocol")?, &name),
            format!(
                "{} {} {} {}",
                n("priority")?,
                n("weight")?,
                n("port")?,
                s("target")?
            ),
            None,
        ),
        "TLSA" => (
            join_underscored(&s("port")?, &s("protocol")?, &name),
            format!(
                "{} {} {} {}",
                n("usage")?,
                n("selector")?,
                n("matching")?,
                s("associationData")?
            ),
            None,
        ),
        _ => return None,
    };
    Some(DnsRecord {
        record_type: t,
        name,
        value,
        ttl,
        priority,
    })
}

/// Whether a listed record carries `value` — the zone-file comparison
/// `dns_plan` uses, so a trailing root dot, the case of a hostname or TXT
/// quoting never makes a present record read as absent.
fn same_value(record: &DnsRecord, value: &str) -> bool {
    let norm = |v: &str| -> String {
        let v = v.trim();
        match record.record_type.as_str() {
            "TXT" => v.trim_matches('"').to_string(),
            "MX" => {
                let h = v
                    .split_once(char::is_whitespace)
                    .filter(|(p, _)| p.chars().all(|c| c.is_ascii_digit()))
                    .map(|(_, h)| h)
                    .unwrap_or(v);
                h.trim().trim_end_matches('.').to_ascii_lowercase()
            }
            _ => v.trim_end_matches('.').to_ascii_lowercase(),
        }
    };
    norm(&record.value) == norm(value)
}

impl DnsProvider for SpaceshipDns {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        Ok(self
            .api
            .list_domains(client)
            .await?
            .into_iter()
            .map(|d| DnsZone {
                id: d.clone(),
                name: d,
                nameservers: Vec::new(),
            })
            .collect())
    }

    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        // `force` turns Spaceship's conflicts checker off, so a record the
        // run asks for replaces a conflicting default (a fresh domain's
        // parking records) instead of being refused.
        let body = json!({ "force": true, "items": [create_item(record)?] });
        let resp = self
            .api
            .request(
                client,
                reqwest::Method::PUT,
                &format!("/dns/records/{zone_id}"),
            )
            .json(&body)
            .send()
            .await?;
        ensure_success(resp).await?;
        Ok(())
    }

    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        self.matching(client, zone_id, name, record_type).await
    }

    async fn delete_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
        value: &str,
    ) -> Result<(), ProvisionError> {
        // Idempotent: nothing at this value means it is already gone.
        let Some(found) = self
            .matching(client, zone_id, name, record_type)
            .await?
            .into_iter()
            .find(|r| same_value(r, value))
        else {
            return Ok(());
        };
        // Delete what Spaceship listed, not the caller's spelling of it, so the
        // item's fields match the stored record exactly.
        let mut target = found;
        target.name = name.to_string();
        let resp = self
            .api
            .request(
                client,
                reqwest::Method::DELETE,
                &format!("/dns/records/{zone_id}"),
            )
            .json(&json!([item_fields(&target)?]))
            .send()
            .await?;
        ensure_success(resp).await?;
        Ok(())
    }

    fn base_url(&self) -> &str {
        self.api.api_base()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t: &str, name: &str, value: &str, priority: Option<u32>) -> DnsRecord {
        DnsRecord {
            record_type: t.into(),
            name: name.into(),
            value: value.into(),
            ttl: 300,
            priority,
        }
    }

    #[test]
    fn mx_takes_the_priority_and_drops_the_root_dot() {
        let item = create_item(&rec("MX", "@", "mail.example.com.", Some(10))).unwrap();
        assert_eq!(item["exchange"], "mail.example.com");
        assert_eq!(item["preference"], 10);
        assert_eq!(item["ttl"], 300);
        // A folded `"<pref> <host>"` value reads the same.
        let folded = create_item(&rec("MX", "@", "20 mx.example.com", None)).unwrap();
        assert_eq!(folded["preference"], 20);
        assert_eq!(folded["exchange"], "mx.example.com");
    }

    #[test]
    fn srv_splits_service_and_protocol_out_of_the_owner_name() {
        let item = create_item(&rec(
            "SRV",
            "_caldavs._tcp",
            "0 1 443 mail.example.com.",
            None,
        ))
        .unwrap();
        assert_eq!(item["service"], "_caldavs");
        assert_eq!(item["protocol"], "_tcp");
        assert_eq!(item["name"], "@");
        assert_eq!(item["port"], 443);
        assert_eq!(item["target"], "mail.example.com");
        let back = from_item(&item).unwrap();
        assert_eq!(back.name, "_caldavs._tcp");
        assert_eq!(back.value, "0 1 443 mail.example.com");
    }

    #[test]
    fn tlsa_splits_port_and_protocol_and_round_trips() {
        let hex = "ab".repeat(32);
        let item =
            create_item(&rec("TLSA", "_25._tcp.mail", &format!("3 1 1 {hex}"), None)).unwrap();
        assert_eq!(item["port"], "_25");
        assert_eq!(item["name"], "mail");
        assert_eq!(item["associationData"], hex);
        let back = from_item(&item).unwrap();
        assert_eq!(back.name, "_25._tcp.mail");
        assert_eq!(back.value, format!("3 1 1 {hex}"));
    }

    #[test]
    fn ttl_is_clamped_to_spaceships_bounds() {
        let mut r = rec("A", "@", "192.0.2.1", None);
        r.ttl = 30;
        assert_eq!(create_item(&r).unwrap()["ttl"], 60);
        r.ttl = 86_400;
        assert_eq!(create_item(&r).unwrap()["ttl"], 3600);
    }

    #[test]
    fn an_untranslated_type_is_refused_before_any_request() {
        assert!(create_item(&rec("HINFO", "@", "x y", None)).is_err());
        assert!(create_item(&rec("SRV", "caldav", "0 1 443 h", None)).is_err());
    }

    #[test]
    fn values_compare_as_zone_file_values() {
        let listed = rec("MX", "@", "mail.example.com", Some(10));
        assert!(same_value(&listed, "Mail.Example.com."));
        let txt = rec("TXT", "@", "v=spf1 mx -all", None);
        assert!(same_value(&txt, "\"v=spf1 mx -all\""));
    }
}
