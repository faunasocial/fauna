pub mod bundled;
pub mod cloudflare;
pub mod gandi;
pub mod hetzner;
pub mod namecheap;
pub mod porkbun;

use serde::{Deserialize, Serialize};

use crate::error::ProvisionError;

/// A DNS zone returned by a DNS provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsZone {
    pub id: String,
    pub name: String,
}

/// A DNS record to create.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsRecord {
    pub record_type: String,
    pub name: String,
    pub value: String,
    pub ttl: u32,
    /// Priority for MX records (and other record types that require it).
    pub priority: Option<u32>,
}

/// Common interface for DNS providers.
#[allow(async_fn_in_trait)]
pub trait DnsProvider: Send + Sync {
    /// Verify credentials and return available zones.
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError>;

    /// Create a DNS record in the given zone.
    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError>;

    /// Look up existing records by name and type within a zone. Used by the
    /// orchestrator's pre-flight idempotency check before `create_record`:
    /// if a matching record already exists with the same value, the step
    /// is `Skipped` and the orchestrator advances. An empty `Vec` means
    /// "no matches"; a non-empty `Vec` lets the caller decide which match
    /// counts (typically by checking `value` equality).
    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError>;

    /// Delete the record at `(name, record_type)` whose value equals `value`
    /// from the given zone. **Value-based** — the impl internally looks up the
    /// matching record (resolving the provider's own record id where the API
    /// needs one) and deletes only the entry whose value matches, mirroring the
    /// value-based idempotency of [`create_record`](DnsProvider::create_record)
    /// and keeping provider record-ids off the seam surface. **Idempotent:** a
    /// record that is already absent is a no-op success (no error), so a retried
    /// `_acme-challenge` teardown after a partial run still resolves cleanly.
    /// Used to tear down the transient DNS-01 `_acme-challenge` TXT once the CA
    /// has validated (and, more generally, to retract any managed record).
    async fn delete_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
        value: &str,
    ) -> Result<(), ProvisionError>;

    /// Whether this provider's record API expects **zone-relative** owner names
    /// (`@`, `mail`, `_dmarc`) rather than fully-qualified ones
    /// (`mail.example.com`). The orchestrator relativizes
    /// [`build_records`](crate::orchestrator) owner names against the zone NAME
    /// when this is `true`. All providers are relative **except Cloudflare**,
    /// whose v4 DNS API consumes *and* returns fully-qualified names, so it
    /// overrides this to `false`.
    ///
    /// This is the seam that keeps Hetzner correct: Hetzner Cloud RRset names
    /// are zone-relative but its `DnsZone.id` is an opaque integer, so the
    /// owner names must be relativized against the zone *name* — a numeric id
    /// can't be stripped as a suffix, leaving fully-qualified owners the API
    /// double-suffixes. Gandi/Namecheap/Porkbun also want relative names and
    /// happen to use the zone name as their id, so they relativize either way.
    ///
    /// # The owner-name contract: the CALLER relativizes, adapters consume
    ///
    /// The `name` an adapter receives (`create_record`'s `record.name`,
    /// `find_records`/`delete_record`'s `name`) is **already in the form that
    /// provider's API wants** — zone-relative with `@` at the apex when this
    /// returns `true`, fully-qualified when it returns `false`. Adapters use it
    /// verbatim; the sole sanctioned transform is translating the shared `@`
    /// sentinel to a provider's own spelling (Porkbun's blank string). An
    /// adapter must **not** re-derive relativity by stripping a `.<zone>`
    /// suffix.
    ///
    /// That is not a style preference — it is the only contract this seam can
    /// express. Adapters receive `zone_id`, never the zone *name*, so Hetzner
    /// (opaque integer id) and Cloudflare (fully-qualified, opted out) cannot
    /// defend themselves even in principle. A defensive strip in the three
    /// adapters that *could* therefore buys nothing and costs detection: it
    /// makes a caller that forgets to relativize look correct on those three
    /// while silently corrupting the two that can't. That asymmetry cost a
    /// production outage on 2026-07-24, when `fauna-client-dns` passed
    /// fully-qualified owners and Hetzner stored every record at a doubled name
    /// (`_acme-challenge.example.com.example.com.`) — API-visible, never resolvable,
    /// failing ACME twice
    /// (`docs/goal/architecture/nest/tls-certificates.md` § the 2026-07-24 dual
    /// defect; `docs/goal/architecture/provisioning/registry.md` § The
    /// owner-name contract).
    ///
    /// Both callers hold up their end: the orchestrator via
    /// [`dns_record_name`](crate::orchestrator::dns_record_name) in
    /// `to_provider_record`, and `fauna-client-dns`'s managed-DNS seam via its
    /// own `owner_name` (which calls the same function). A new caller must do
    /// likewise; a new adapter may assume it.
    fn record_names_relative_to_zone(&self) -> bool {
        true
    }

    /// Returns the configured base URL for this provider. Production builds
    /// see the canonical API host; integration tests with `with_base_url(...)`
    /// see the wiremock server URL. Used by the E2E fake-cloud bridge to
    /// route provider calls through a local stub.
    fn base_url(&self) -> &str;
}
