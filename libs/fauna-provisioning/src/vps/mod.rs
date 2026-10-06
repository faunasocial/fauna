pub mod bundled;
pub mod digitalocean;
pub mod hetzner;
pub mod linode;
pub mod ovh;
pub mod vultr;

use serde::{Deserialize, Serialize};

use crate::error::{ProvisionError, ensure_success};

/// The stable marker every fauna-provisioned box carries as a provider-side
/// label/tag (`docs/goal/architecture/installers/vps.md` § Uninstall). The
/// orchestrator unions it with caller labels at the create chokepoint
/// (`run_server_step`), so a future client "list / decommission my nests" view
/// can filter the user's cloud account to fauna boxes (Hetzner
/// `label_selector=managed-by=fauna`; tag providers match the `managed-by:fauna`
/// tag string) instead of listing every server in the account. A hard-coded
/// constant, never a config surface; value chosen to mirror the
/// `app.kubernetes.io/managed-by` convention. Key and value stay within every
/// provider's tag charset (colon-separated on tag providers — see
/// `create_server`'s labels doc).
pub const MANAGED_BY_LABEL: (&str, &str) = ("managed-by", "fauna");

/// Shared response handling for `delete_server`: a 2xx OR a 404 (already gone)
/// is a success, matching the idempotent-decommission contract on the trait
/// method; any other status maps to `ProvisionError::Provider`. All six
/// providers funnel their delete response through this so the "404 is a no-op"
/// semantic is single-sourced.
pub(crate) async fn finish_delete(resp: reqwest::Response) -> Result<(), ProvisionError> {
    let status = resp.status();
    if status.is_success() || status.as_u16() == 404 {
        return Ok(());
    }
    let code = status.as_u16();
    let body = resp.text().await.unwrap_or_default();
    Err(ProvisionError::provider(code, body))
}

/// `delete_server` for the four bearer-token providers (Linode, Hetzner,
/// DigitalOcean, Vultr) whose delete call is nothing but a bare
/// `DELETE <url>` with a bearer header — each provider's own doc-comment-free
/// `delete_server` body was this exact sequence, hand-copied four times,
/// varying only in the URL it builds. OVH (a signed request needing a
/// timestamp) and the bundled provider (routed through its own `self.api`
/// client wrapper) genuinely differ and are not candidates for this helper.
pub(crate) async fn delete_server_bearer(
    client: &reqwest::Client,
    url: String,
    token: &str,
) -> Result<(), ProvisionError> {
    let resp = client.delete(url).bearer_auth(token).send().await?;
    finish_delete(resp).await
}

/// Authenticated `<VERB> <url>` with a bearer header and a JSON body, for the
/// four bearer-token providers (Linode, DigitalOcean, Hetzner, Vultr),
/// returning the already-`ensure_success`-checked response. `set_ptr`
/// (discarding the response — see [`set_ptr_bearer`]) and `create_server`
/// (parsing the created instance back out) both hand-copied this same
/// five-line request-building shape, varying only in HTTP verb, URL, and body
/// — the response *parsing* genuinely differs per provider/method and stays
/// with each caller. OVH (a signed request needing a timestamp) and the
/// bundled provider (routed through its own `self.api` client wrapper)
/// genuinely differ and are not candidates for this helper.
pub(crate) async fn json_bearer(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: String,
    token: &str,
    body: serde_json::Value,
) -> Result<reqwest::Response, ProvisionError> {
    let resp = client
        .request(method, url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    ensure_success(resp).await
}

/// `set_ptr` for the same four bearer-token providers — [`json_bearer`],
/// discarding the response on success.
pub(crate) async fn set_ptr_bearer(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: String,
    token: &str,
    body: serde_json::Value,
) -> Result<(), ProvisionError> {
    json_bearer(client, method, url, token, body).await?;
    Ok(())
}

/// Authenticated GET for the four bearer-token providers, returning the
/// already-`ensure_success`-checked response for the caller to `.json()`
/// into its own response type. `verify`, `list_server_types`, `get_ptr`, and
/// three of four `find_server_by_name` bodies (DigitalOcean/Hetzner/Vultr —
/// Linode's needs an `X-Filter` header instead of a query param, so it stays
/// bespoke) each hand-copied this exact two-statement shape, varying only in
/// the URL and an optional query filter. `query` is `&[]` for the plain GETs.
/// Mirrors `delete_server_bearer` / `set_ptr_bearer` above for the same four
/// providers.
pub(crate) async fn get_bearer(
    client: &reqwest::Client,
    url: String,
    token: &str,
    query: &[(&str, &str)],
) -> Result<reqwest::Response, ProvisionError> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .query(query)
        .send()
        .await?;
    ensure_success(resp).await
}

/// Encode `labels` as the flat `k:v` tag array three providers share
/// (Linode, DigitalOcean, Vultr) — `None` when `labels` is empty, so a
/// caller assigns it into `body["tags"]` only under `if let Some(...)` and
/// omits the field entirely rather than sending an empty array. Hetzner
/// takes a native string→string `labels` map instead (round 165's
/// `create_server` split already keeps that per-provider) and stays out of
/// this helper. DigitalOcean's tag charset — letters, numbers, colons,
/// dashes, underscores; no `=` — is the narrowest of the three and sets the
/// encoding for all (round 168 of the shared-Rust harvest sweep).
pub(crate) fn kv_tags(labels: &[(String, String)]) -> Option<serde_json::Value> {
    if labels.is_empty() {
        return None;
    }
    Some(serde_json::Value::Array(
        labels
            .iter()
            .map(|(k, v)| serde_json::Value::String(format!("{k}:{v}")))
            .collect(),
    ))
}

/// `find_server_by_name`'s response-parsing shape for the four providers
/// (DigitalOcean, Vultr, Linode, OVH) whose server-list endpoint isn't
/// guaranteed to filter server-side to an exact name match: walk the list,
/// skip entries whose name/label doesn't match, and take the first one whose
/// provider-specific extraction (`to_instance`) actually yields a usable
/// `VpsInstance` (a droplet with no `public` network, or an instance with no
/// IP yet, is skipped rather than treated as a match) — each provider hand-
/// copied this exact loop, varying only the field names and the extraction
/// closure. Hetzner and the bundled provider trust their single-result query
/// filter and just take the first list entry outright (no name re-check, no
/// fallback to a later entry), a genuinely simpler shape that stays out of
/// this helper.
pub(crate) fn find_matching_server<T>(
    items: impl IntoIterator<Item = T>,
    matches: impl Fn(&T) -> bool,
    to_instance: impl Fn(T) -> Option<VpsInstance>,
) -> Option<VpsInstance> {
    for item in items {
        if !matches(&item) {
            continue;
        }
        if let Some(instance) = to_instance(item) {
            return Some(instance);
        }
    }
    None
}

/// A region/datacenter returned by a VPS provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct VpsLocation {
    pub id: String,
    pub name: String,
    pub city: String,
    pub country: String,
}

/// Result of creating a VPS instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpsInstance {
    pub server_id: String,
    pub ipv4: String,
}

/// One row of [`VpsProvider::list_managed_servers`] — a fauna-provisioned box
/// as the retire view shows it (`docs/goal/architecture/installers/vps.md`
/// § Uninstall → *Listing primitive*).
///
/// Richer than [`VpsInstance`], which carries only what `create_server`
/// returns: a list has to be *read* by a person, so it needs the provider-side
/// name, the addresses, the labels and the age. Not a UniFFI record — the
/// binding-facing type is the machine's own `ManagedServerRow`, which adds the
/// attributed domain and the transfer-code state the crate knows nothing
/// about; this stays the crate's wire-shaped type, like `VpsInstance`.
///
/// `marked` is `false` only where the provider has no label facility at all
/// (OVH), so the row could not be *proven* fauna-provisioned; the view shows
/// those behind its unmarked note and the same typed per-box confirm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedServer {
    pub server_id: String,
    pub name: String,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub labels: Vec<(String, String)>,
    pub created_at: Option<String>,
    pub marked: bool,
}

/// A listed row is directly the `delete_server` / `get_ptr` take — retiring a
/// box must never need a second lookup by name (the name is exactly what
/// cannot be trusted to identify a box: `my-site.example.com` and
/// `my.site.example.com` share one provisioning name).
impl From<&ManagedServer> for VpsInstance {
    fn from(s: &ManagedServer) -> Self {
        VpsInstance {
            server_id: s.server_id.clone(),
            // A listed box with no public IPv4 is still deletable (the id is
            // what `delete_server` takes); only the PTR read would be moot.
            ipv4: s.ipv4.clone().unwrap_or_default(),
        }
    }
}

/// `managed-by=fauna` — the `key=value` selector spelling Hetzner's
/// `label_selector` and the bundled provider's `?label=` filter both take.
pub fn marker_selector() -> String {
    format!("{}={}", MANAGED_BY_LABEL.0, MANAGED_BY_LABEL.1)
}

/// `managed-by:fauna` — the flat `key:value` tag spelling the three tag
/// providers carry (colon, not `=`; see [`kv_tags`]).
pub fn marker_tag() -> String {
    format!("{}:{}", MANAGED_BY_LABEL.0, MANAGED_BY_LABEL.1)
}

/// Decode the flat `k:v` tag array three providers use back into label pairs
/// — the inverse of [`kv_tags`]. A tag carrying no `:` keeps the whole string
/// as the key with an empty value rather than being dropped: the labels are
/// shown to a person, and silently eating a tag would misreport the box.
pub(crate) fn tags_to_labels(tags: &[String]) -> Vec<(String, String)> {
    tags.iter()
        .map(|t| match t.split_once(':') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (t.clone(), String::new()),
        })
        .collect()
}

/// Does this label set carry [`MANAGED_BY_LABEL`]?
///
/// Every marker-capable adapter re-checks this client-side **after** sending
/// its server-side filter. Trusting the query alone would mean a provider that
/// silently ignores an unknown filter parameter hands the retire view the
/// user's entire cloud account — and the view's whole safety property is that
/// a non-fauna server is never shown, let alone deletable
/// (`vps.md` § Uninstall → *Listing primitive*).
pub(crate) fn has_marker(labels: &[(String, String)]) -> bool {
    labels
        .iter()
        .any(|(k, v)| k == MANAGED_BY_LABEL.0 && v == MANAGED_BY_LABEL.1)
}

/// Pagination stop-loss for [`VpsProvider::list_managed_servers`]. Every
/// adapter walks until its provider says "no more"; this bounds a provider
/// that misreports its own cursor so a retire view can never spin forever.
/// Far above any real account: at the page sizes the adapters request it is
/// tens of thousands of servers.
pub(crate) const MAX_LIST_PAGES: u32 = 100;

/// Normalized server-type info returned by `list_server_types()`. The UI
/// renders these as radio options on the VPS configuration page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ServerTypeInfo {
    pub id: String,
    pub vcpu: u32,
    pub mem_gb: f32,
    pub disk_gb: u32,
    pub price_monthly_cents: u64,
    pub currency: String,
}

/// Common interface for VPS providers.
#[allow(async_fn_in_trait)]
pub trait VpsProvider: Send + Sync {
    /// Verify the API token and return available locations.
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<VpsLocation>, ProvisionError>;

    /// Create a server with the given cloud-init user data.
    ///
    /// `labels` are provider-side key/value tags attached to the new server at
    /// create time. Callers going through the orchestrator never need to pass
    /// [`MANAGED_BY_LABEL`] — `run_server_step` unions it in, so every
    /// fauna-provisioned box carries it; the live-provision e2e additionally
    /// passes `[("fauna-e2e","1")]` so its throwaway boxes are sweepable by a
    /// provider `label_selector` rather than a name-prefix + age heuristic. Each
    /// provider maps them to its native tagging where the API supports it
    /// (Hetzner `labels` map; DigitalOcean/Linode/Vultr string `tags` of
    /// `key:value` — colon, not `=`, because DigitalOcean's tag charset allows
    /// only letters, numbers, colons, dashes, and underscores); OVH
    /// instance-create has no label field, so it ignores them.
    async fn create_server(
        &self,
        client: &reqwest::Client,
        name: &str,
        location: &str,
        server_type: &str,
        user_data: &str,
        labels: &[(String, String)],
    ) -> Result<VpsInstance, ProvisionError>;

    /// Delete a server by its provider-side id (the decommission primitive).
    ///
    /// Backs a client-side Uninstall/decommission flow
    /// (`docs/goal/architecture/installers/vps.md` § Uninstall) and lets the
    /// live-provision e2e teardown exercise the crate path. Mirrors the DNS
    /// side's `DnsProvider::delete_record`, closing the trait asymmetry.
    ///
    /// **Idempotent.** Deleting an already-absent server (the provider replies
    /// `404`) is a success: a decommission that finds the box already gone has
    /// achieved its goal, and a double-teardown must not raise. Any other
    /// non-success status maps to `ProvisionError::Provider`.
    async fn delete_server(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError>;

    /// Return the curated server-types this provider offers, intersected
    /// with what the API currently lists. Curation list is sourced from
    /// `ProviderMeta::curated_offers`. Returned vec preserves curation
    /// order so the UI shows radio options in that order.
    async fn list_server_types(
        &self,
        client: &reqwest::Client,
        curated_ids: &[&str],
    ) -> Result<Vec<ServerTypeInfo>, ProvisionError>;

    /// Set the IPv4 reverse-DNS (PTR) record for an instance to `fqdn`.
    ///
    /// Called by the orchestrator immediately after the nest comes
    /// online, as the last mutation before `Complete`. Implementations
    /// may key off `instance.server_id`, `instance.ipv4`, or both —
    /// whichever the underlying provider's API requires.
    ///
    /// **Idempotent.** All six current implementations call PUT/POST
    /// endpoints that overwrite the existing PTR value rather than
    /// appending; calling `set_ptr` twice with the same `fqdn` is safe
    /// and is the basis of the retry path.
    async fn set_ptr(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
        fqdn: &str,
    ) -> Result<(), ProvisionError>;

    /// Look up an existing server by its provider-side name. Used by the
    /// orchestrator's pre-flight idempotency check before `create_server`:
    /// re-running provisioning on the same domain finds the previously
    /// created server and skips creation rather than failing on a duplicate.
    /// Returns `None` when no matching server exists.
    async fn find_server_by_name(
        &self,
        client: &reqwest::Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError>;

    /// List the fauna-provisioned servers in this account — the listing
    /// primitive behind the retire view
    /// (`docs/goal/architecture/installers/vps.md` § Uninstall → *Listing
    /// primitive*; the app half is `docs/goal/behavior/nest-retirement.md`).
    ///
    /// **Filtered to [`MANAGED_BY_LABEL`].** Each implementation sends its
    /// provider's own server-side filter (Hetzner `label_selector`,
    /// DigitalOcean `tag_name`, Linode's `X-Filter`, Vultr `tag`, the bundled
    /// provider's `?label=`) **and re-checks the marker client-side** — a
    /// provider that ignores an unknown filter parameter must not be able to
    /// hand the view the user's whole cloud account. Filtering is what keeps a
    /// person from ever being shown, let alone able to delete, a non-fauna
    /// server living in the same account.
    ///
    /// **OVH is the exception:** its instance-create API has no label field at
    /// all (see `create_server`), so it returns the project list *unfiltered*
    /// with every row `marked: false`, and the view's mandatory typed per-box
    /// confirm is what discharges the risk there.
    ///
    /// **Pagination is followed to the end** — a truncated list that hides a
    /// fauna box is a bug (`nest-retirement.md` § Errors & edge cases), and a
    /// box the view cannot show is a box the person cannot retire from the
    /// app. Bounded by [`MAX_LIST_PAGES`].
    ///
    /// Boxes provisioned before the marker landed (2026-07-08) do not appear;
    /// the provider dashboard stays their teardown path.
    async fn list_managed_servers(
        &self,
        client: &reqwest::Client,
    ) -> Result<Vec<ManagedServer>, ProvisionError>;

    /// Returns the IPv4 reverse-DNS (PTR) currently set on the instance, if
    /// any. Used as the pre-flight idempotency check for the PTR substep
    /// of step 3: when this matches the desired FQDN, `set_ptr` is skipped.
    async fn get_ptr(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
    ) -> Result<Option<String>, ProvisionError>;

    /// Returns the configured base URL for this provider. Production builds
    /// see the canonical API host; integration tests with `with_base_url(...)`
    /// see the wiremock server URL. Used by the E2E fake-cloud bridge to
    /// route provider calls through a local stub.
    fn base_url(&self) -> &str;
}
