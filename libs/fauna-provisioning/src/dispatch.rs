// Dispatch module: maps (ProviderId, Credentials) → concrete provider instance.
//
// Design choice: Option B — enum dispatch wrappers rather than Box<dyn Trait>.
// The traits use native `async fn` with #[allow(async_fn_in_trait)], which
// makes them NOT object-safe. Option B keeps traits unchanged, avoids the
// async_trait crate dependency, and incurs no per-call heap allocation.
// Downside is manual forwarding of each method, which is acceptable given the
// small and stable method surface.

use fauna_core::secret::SecretString;

use crate::ProviderId;
use crate::dns::{DnsProvider, DnsRecord, DnsZone};
use crate::error::ProvisionError;
use crate::registrar::bundled::AuthCode;
use crate::registrar::{
    ContactInfo, DomainAvailability, Registrar, RegistrarAvailability, RegistrationResult,
    TldPriceQuote,
};
use crate::vps::{ServerTypeInfo, VpsInstance, VpsLocation, VpsProvider};

// ---------------------------------------------------------------------------
// Credentials bag
// ---------------------------------------------------------------------------

/// A flat key→value bag of provider credentials, keyed by the field `id`
/// values from `providers.yaml` (e.g. `"api-token"`, `"secret-api-key"`).
///
/// The secret values are [`SecretString`] (zeroized on drop, redacted Debug) so
/// the token does not linger as a plain `String` in this transient bag; the
/// per-provider dispatch functions un-wrap to a plain `String` only at the
/// external boundary (the reqwest provider adapter, which is un-zeroizable by
/// design — see `SecretString`' module docs).
#[derive(Default)]
pub struct Credentials {
    pub entries: Vec<(String, SecretString)>,
}

impl Credentials {
    /// Look up a credential by its providers.yaml field id. Returns the borrowed
    /// text (`SecretString: AsRef<str>`); callers `.to_string()` it at the
    /// external provider-API boundary.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Create credentials from a HashMap (e.g., from the wizard's `creds` bag).
    pub fn from_map(map: std::collections::HashMap<String, SecretString>) -> Self {
        Credentials {
            entries: map.into_iter().collect(),
        }
    }
}

/// Parse a provider id from its lowercase discriminant (e.g. `"porkbun"`), as
/// sent across the FFI/wasm JSON boundary.
pub fn parse_provider_id(provider_id: &str) -> Result<ProviderId, String> {
    ProviderId::from_str(provider_id).ok_or_else(|| format!("unknown provider id: {provider_id}"))
}

/// Parse a JSON object mapping `providers.yaml` credential field ids to
/// values into a [`Credentials`] bag — the registrar/orchestrator dispatch's
/// wire format, distinct from `verify_json`'s per-provider envelope.
///
/// Deserializes the secret values straight into `SecretString` (transparent —
/// a JSON string) so the plaintext token is wrapped (zeroize-on-drop) the
/// moment it leaves the JSON boundary.
pub fn parse_credentials(creds_json: &str) -> Result<Credentials, String> {
    let raw: std::collections::BTreeMap<String, SecretString> =
        serde_json::from_str(creds_json).map_err(|e| format!("invalid creds JSON: {e}"))?;
    Ok(Credentials {
        entries: raw.into_iter().collect(),
    })
}

// ---------------------------------------------------------------------------
// DNS dispatch
// ---------------------------------------------------------------------------

pub enum DnsDispatch {
    Cloudflare(crate::dns::cloudflare::Cloudflare),
    Porkbun(crate::dns::porkbun::Porkbun),
    Namecheap(crate::dns::namecheap::Namecheap),
    Gandi(crate::dns::gandi::Gandi),
    Hetzner(crate::dns::hetzner::HetznerDns),
    Bundled(crate::dns::bundled::BundledDns),
}

impl DnsDispatch {
    /// The registry-facing name of this variant, as `i18n/providers.yaml`
    /// spells it in `dispatch` / `dns_dispatch` (`<Provider><Capability>`).
    ///
    /// The enum variants are bare provider names because the capability is
    /// already in the enum; the registry spells both because its three keys
    /// share one namespace. This match is the translation, and being
    /// exhaustive it forces an arm for every new variant --
    /// `tests/dispatch_registry_bijection.rs` then checks the arm against
    /// what the registry declares.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Cloudflare(_) => "CloudflareDns",
            Self::Porkbun(_) => "PorkbunDns",
            Self::Namecheap(_) => "NamecheapDns",
            Self::Gandi(_) => "GandiDns",
            Self::Hetzner(_) => "HetznerDns",
            Self::Bundled(_) => "BundledDns",
        }
    }
}

impl DnsProvider for DnsDispatch {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<DnsZone>, ProvisionError> {
        match self {
            Self::Cloudflare(p) => p.verify(client).await,
            Self::Porkbun(p) => p.verify(client).await,
            Self::Namecheap(p) => p.verify(client).await,
            Self::Gandi(p) => p.verify(client).await,
            Self::Hetzner(p) => p.verify(client).await,
            Self::Bundled(p) => p.verify(client).await,
        }
    }

    async fn create_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        record: &DnsRecord,
    ) -> Result<(), ProvisionError> {
        match self {
            Self::Cloudflare(p) => p.create_record(client, zone_id, record).await,
            Self::Porkbun(p) => p.create_record(client, zone_id, record).await,
            Self::Namecheap(p) => p.create_record(client, zone_id, record).await,
            Self::Gandi(p) => p.create_record(client, zone_id, record).await,
            Self::Hetzner(p) => p.create_record(client, zone_id, record).await,
            Self::Bundled(p) => p.create_record(client, zone_id, record).await,
        }
    }

    async fn find_records(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
    ) -> Result<Vec<DnsRecord>, ProvisionError> {
        match self {
            Self::Cloudflare(p) => p.find_records(client, zone_id, name, record_type).await,
            Self::Porkbun(p) => p.find_records(client, zone_id, name, record_type).await,
            Self::Namecheap(p) => p.find_records(client, zone_id, name, record_type).await,
            Self::Gandi(p) => p.find_records(client, zone_id, name, record_type).await,
            Self::Hetzner(p) => p.find_records(client, zone_id, name, record_type).await,
            Self::Bundled(p) => p.find_records(client, zone_id, name, record_type).await,
        }
    }

    async fn delete_record(
        &self,
        client: &reqwest::Client,
        zone_id: &str,
        name: &str,
        record_type: &str,
        value: &str,
    ) -> Result<(), ProvisionError> {
        match self {
            Self::Cloudflare(p) => {
                p.delete_record(client, zone_id, name, record_type, value)
                    .await
            }
            Self::Porkbun(p) => {
                p.delete_record(client, zone_id, name, record_type, value)
                    .await
            }
            Self::Namecheap(p) => {
                p.delete_record(client, zone_id, name, record_type, value)
                    .await
            }
            Self::Gandi(p) => {
                p.delete_record(client, zone_id, name, record_type, value)
                    .await
            }
            Self::Hetzner(p) => {
                p.delete_record(client, zone_id, name, record_type, value)
                    .await
            }
            Self::Bundled(p) => {
                p.delete_record(client, zone_id, name, record_type, value)
                    .await
            }
        }
    }

    fn record_names_relative_to_zone(&self) -> bool {
        // Forward to the inner provider — the default would be `true` and would
        // wrongly relativize Cloudflare's fully-qualified owner names.
        match self {
            Self::Cloudflare(p) => p.record_names_relative_to_zone(),
            Self::Porkbun(p) => p.record_names_relative_to_zone(),
            Self::Namecheap(p) => p.record_names_relative_to_zone(),
            Self::Gandi(p) => p.record_names_relative_to_zone(),
            Self::Hetzner(p) => p.record_names_relative_to_zone(),
            Self::Bundled(p) => p.record_names_relative_to_zone(),
        }
    }

    fn base_url(&self) -> &str {
        match self {
            Self::Cloudflare(p) => p.base_url(),
            Self::Porkbun(p) => p.base_url(),
            Self::Namecheap(p) => p.base_url(),
            Self::Gandi(p) => p.base_url(),
            Self::Hetzner(p) => p.base_url(),
            Self::Bundled(p) => p.base_url(),
        }
    }
}

// ---------------------------------------------------------------------------
// VPS dispatch
// ---------------------------------------------------------------------------

pub enum VpsDispatch {
    Hetzner(crate::vps::hetzner::Hetzner),
    Digitalocean(crate::vps::digitalocean::DigitalOcean),
    Vultr(crate::vps::vultr::Vultr),
    Ovh(crate::vps::ovh::Ovh),
    Linode(crate::vps::linode::Linode),
    Bundled(crate::vps::bundled::BundledVps),
}

impl VpsDispatch {
    /// The registry-facing name of this variant, as `i18n/providers.yaml`
    /// spells it in `dispatch` (`<Provider><Capability>`).
    ///
    /// The enum variants are bare provider names because the capability is
    /// already in the enum; the registry spells both because its three keys
    /// share one namespace. This match is the translation, and being
    /// exhaustive it forces an arm for every new variant --
    /// `tests/dispatch_registry_bijection.rs` then checks the arm against
    /// what the registry declares.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Hetzner(_) => "HetznerVps",
            Self::Digitalocean(_) => "DigitaloceanVps",
            Self::Vultr(_) => "VultrVps",
            Self::Ovh(_) => "OvhVps",
            Self::Linode(_) => "LinodeVps",
            Self::Bundled(_) => "BundledVps",
        }
    }
}

impl VpsProvider for VpsDispatch {
    async fn verify(&self, client: &reqwest::Client) -> Result<Vec<VpsLocation>, ProvisionError> {
        match self {
            Self::Hetzner(p) => p.verify(client).await,
            Self::Digitalocean(p) => p.verify(client).await,
            Self::Vultr(p) => p.verify(client).await,
            Self::Ovh(p) => p.verify(client).await,
            Self::Linode(p) => p.verify(client).await,
            Self::Bundled(p) => p.verify(client).await,
        }
    }

    async fn create_server(
        &self,
        client: &reqwest::Client,
        name: &str,
        location: &str,
        server_type: &str,
        user_data: &str,
        labels: &[(String, String)],
    ) -> Result<VpsInstance, ProvisionError> {
        match self {
            Self::Hetzner(p) => {
                p.create_server(client, name, location, server_type, user_data, labels)
                    .await
            }
            Self::Digitalocean(p) => {
                p.create_server(client, name, location, server_type, user_data, labels)
                    .await
            }
            Self::Vultr(p) => {
                p.create_server(client, name, location, server_type, user_data, labels)
                    .await
            }
            Self::Ovh(p) => {
                p.create_server(client, name, location, server_type, user_data, labels)
                    .await
            }
            Self::Linode(p) => {
                p.create_server(client, name, location, server_type, user_data, labels)
                    .await
            }
            Self::Bundled(p) => {
                p.create_server(client, name, location, server_type, user_data, labels)
                    .await
            }
        }
    }

    async fn delete_server(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
    ) -> Result<(), ProvisionError> {
        match self {
            Self::Hetzner(p) => p.delete_server(client, instance).await,
            Self::Digitalocean(p) => p.delete_server(client, instance).await,
            Self::Vultr(p) => p.delete_server(client, instance).await,
            Self::Ovh(p) => p.delete_server(client, instance).await,
            Self::Linode(p) => p.delete_server(client, instance).await,
            Self::Bundled(p) => p.delete_server(client, instance).await,
        }
    }

    async fn list_server_types(
        &self,
        client: &reqwest::Client,
        curated_ids: &[&str],
    ) -> Result<Vec<ServerTypeInfo>, ProvisionError> {
        match self {
            Self::Hetzner(p) => p.list_server_types(client, curated_ids).await,
            Self::Digitalocean(p) => p.list_server_types(client, curated_ids).await,
            Self::Vultr(p) => p.list_server_types(client, curated_ids).await,
            Self::Ovh(p) => p.list_server_types(client, curated_ids).await,
            Self::Linode(p) => p.list_server_types(client, curated_ids).await,
            Self::Bundled(p) => p.list_server_types(client, curated_ids).await,
        }
    }

    async fn set_ptr(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
        fqdn: &str,
    ) -> Result<(), ProvisionError> {
        match self {
            Self::Hetzner(p) => p.set_ptr(client, instance, fqdn).await,
            Self::Digitalocean(p) => p.set_ptr(client, instance, fqdn).await,
            Self::Vultr(p) => p.set_ptr(client, instance, fqdn).await,
            Self::Ovh(p) => p.set_ptr(client, instance, fqdn).await,
            Self::Linode(p) => p.set_ptr(client, instance, fqdn).await,
            Self::Bundled(p) => p.set_ptr(client, instance, fqdn).await,
        }
    }

    async fn find_server_by_name(
        &self,
        client: &reqwest::Client,
        name: &str,
    ) -> Result<Option<VpsInstance>, ProvisionError> {
        match self {
            Self::Hetzner(p) => p.find_server_by_name(client, name).await,
            Self::Digitalocean(p) => p.find_server_by_name(client, name).await,
            Self::Vultr(p) => p.find_server_by_name(client, name).await,
            Self::Ovh(p) => p.find_server_by_name(client, name).await,
            Self::Linode(p) => p.find_server_by_name(client, name).await,
            Self::Bundled(p) => p.find_server_by_name(client, name).await,
        }
    }

    async fn list_managed_servers(
        &self,
        client: &reqwest::Client,
    ) -> Result<Vec<crate::vps::ManagedServer>, ProvisionError> {
        match self {
            Self::Hetzner(p) => p.list_managed_servers(client).await,
            Self::Digitalocean(p) => p.list_managed_servers(client).await,
            Self::Vultr(p) => p.list_managed_servers(client).await,
            Self::Ovh(p) => p.list_managed_servers(client).await,
            Self::Linode(p) => p.list_managed_servers(client).await,
            Self::Bundled(p) => p.list_managed_servers(client).await,
        }
    }

    async fn get_ptr(
        &self,
        client: &reqwest::Client,
        instance: &VpsInstance,
    ) -> Result<Option<String>, ProvisionError> {
        match self {
            Self::Hetzner(p) => p.get_ptr(client, instance).await,
            Self::Digitalocean(p) => p.get_ptr(client, instance).await,
            Self::Vultr(p) => p.get_ptr(client, instance).await,
            Self::Ovh(p) => p.get_ptr(client, instance).await,
            Self::Linode(p) => p.get_ptr(client, instance).await,
            Self::Bundled(p) => p.get_ptr(client, instance).await,
        }
    }

    fn base_url(&self) -> &str {
        match self {
            Self::Hetzner(p) => p.base_url(),
            Self::Digitalocean(p) => p.base_url(),
            Self::Vultr(p) => p.base_url(),
            Self::Ovh(p) => p.base_url(),
            Self::Linode(p) => p.base_url(),
            Self::Bundled(p) => p.base_url(),
        }
    }
}

// ---------------------------------------------------------------------------
// Registrar dispatch
// ---------------------------------------------------------------------------

pub enum RegistrarDispatch {
    Porkbun(crate::registrar::porkbun::PorkbunRegistrar),
    Gandi(crate::registrar::gandi::GandiRegistrar),
    Namecheap(crate::registrar::namecheap::NamecheapRegistrar),
    Cloudflare(crate::registrar::cloudflare::CloudflareRegistrar),
    Bundled(crate::registrar::bundled::BundledRegistrar),
}

impl RegistrarDispatch {
    /// The registry-facing name of this variant, as `i18n/providers.yaml`
    /// spells it in `registrar_dispatch` (`<Provider><Capability>`).
    ///
    /// The enum variants are bare provider names because the capability is
    /// already in the enum; the registry spells both because its three keys
    /// share one namespace. This match is the translation, and being
    /// exhaustive it forces an arm for every new variant --
    /// `tests/dispatch_registry_bijection.rs` then checks the arm against
    /// what the registry declares.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Porkbun(_) => "PorkbunRegistrar",
            Self::Gandi(_) => "GandiRegistrar",
            Self::Namecheap(_) => "NamecheapRegistrar",
            Self::Cloudflare(_) => "CloudflareRegistrar",
            Self::Bundled(_) => "BundledRegistrar",
        }
    }

    /// Fetch the domain's **transfer authorization code** — the leaving user's
    /// door (`docs/goal/behavior/nest-retirement.md` § Transfer authorization
    /// code; `bundled-provider-api.md` § Exit guarantee 1).
    ///
    /// `Ok(None)` means *this registrar has no auth-code call*, not *the call
    /// failed*: the retire view then shows a one-line "get the code from your
    /// registrar's own dashboard" note instead of the button. Ask
    /// [`supports_auth_code`] **before** building a dispatcher if all you need
    /// is whether to render the affordance — that is the data lookup the page
    /// branches on, so no app ever spells a provider id.
    ///
    /// Both arms of a supporting registrar reach the caller intact:
    /// [`AuthCode::Ready`] carries the code, [`AuthCode::AvailableAfter`] the
    /// instant a registry transfer lock lifts (`202`) — never a refusal.
    pub async fn auth_code(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<Option<AuthCode>, ProvisionError> {
        match self {
            Self::Bundled(r) => r.auth_code(client, domain).await.map(Some),
            // Exhaustive by variant, not a `_` catch-all: a new registrar
            // adapter that gains an auth-code call must come here and be
            // matched by a `supports_auth_code` arm, which
            // `tests/registrar_auth_code_dispatch.rs` pins as a bijection.
            Self::Porkbun(_) | Self::Gandi(_) | Self::Namecheap(_) | Self::Cloudflare(_) => {
                Ok(None)
            }
        }
    }
}

/// Does this provider's registrar expose a **transfer authorization code**
/// call? (`docs/goal/behavior/nest-retirement.md` § Transfer authorization
/// code: *"Dispatch exposes the capability as data … so the page never
/// hard-codes a provider id"*.)
///
/// Today the bundled provider alone, where the code is a **mandatory** exit
/// guarantee of the open API (`bundled-provider-api.md` § Exit). Every other
/// registrar's code comes from its own dashboard, so the view renders a note
/// rather than a button.
///
/// Deliberately a plain data lookup here and **not** a `Capability` variant in
/// `providers_generated.rs`: that table is generated from `i18n/providers.yaml`
/// and owned by `../provisioning/registry.md`, whose three capabilities (DNS,
/// VPS, registrar) answer *which adapter do I build*. This answers *which
/// optional call does the adapter I already built have* — a finer question one
/// rung below, with one consumer. Extending the generated table would mean
/// changing the yaml, the codegen and all three of its consumers for a single
/// boolean.
///
/// Exhaustive on purpose: a new provider must decide.
pub fn supports_auth_code(id: ProviderId) -> bool {
    match id {
        ProviderId::Bundled => true,
        ProviderId::Cloudflare
        | ProviderId::Porkbun
        | ProviderId::Hetzner
        | ProviderId::Namecheap
        | ProviderId::Gandi
        | ProviderId::Digitalocean
        | ProviderId::Vultr
        | ProviderId::Ovh
        | ProviderId::Linode => false,
    }
}

impl Registrar for RegistrarDispatch {
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError> {
        match self {
            Self::Porkbun(p) => p.verify(client).await,
            Self::Gandi(p) => p.verify(client).await,
            Self::Namecheap(p) => p.verify(client).await,
            Self::Cloudflare(p) => p.verify(client).await,
            Self::Bundled(p) => p.verify(client).await,
        }
    }

    async fn check(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        match self {
            Self::Porkbun(p) => p.check(client, domain).await,
            Self::Gandi(p) => p.check(client, domain).await,
            Self::Namecheap(p) => p.check(client, domain).await,
            Self::Cloudflare(p) => p.check(client, domain).await,
            Self::Bundled(p) => p.check(client, domain).await,
        }
    }

    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        years: u32,
        agreed_price_cents: u64,
        contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        match self {
            Self::Porkbun(p) => {
                p.register(client, domain, years, agreed_price_cents, contact)
                    .await
            }
            Self::Gandi(p) => {
                p.register(client, domain, years, agreed_price_cents, contact)
                    .await
            }
            Self::Namecheap(p) => {
                p.register(client, domain, years, agreed_price_cents, contact)
                    .await
            }
            Self::Cloudflare(p) => {
                p.register(client, domain, years, agreed_price_cents, contact)
                    .await
            }
            Self::Bundled(p) => {
                p.register(client, domain, years, agreed_price_cents, contact)
                    .await
            }
        }
    }

    fn requires_contact(&self) -> bool {
        match self {
            Self::Porkbun(p) => p.requires_contact(),
            Self::Gandi(p) => p.requires_contact(),
            Self::Namecheap(p) => p.requires_contact(),
            Self::Cloudflare(p) => p.requires_contact(),
            Self::Bundled(p) => p.requires_contact(),
        }
    }

    async fn fetch_default_contact(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<ContactInfo>, ProvisionError> {
        match self {
            Self::Porkbun(p) => p.fetch_default_contact(client).await,
            Self::Gandi(p) => p.fetch_default_contact(client).await,
            Self::Namecheap(p) => p.fetch_default_contact(client).await,
            Self::Cloudflare(p) => p.fetch_default_contact(client).await,
            Self::Bundled(p) => p.fetch_default_contact(client).await,
        }
    }

    async fn list_tld_pricing(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<Vec<TldPriceQuote>>, ProvisionError> {
        match self {
            Self::Porkbun(p) => p.list_tld_pricing(client).await,
            Self::Gandi(p) => p.list_tld_pricing(client).await,
            Self::Namecheap(p) => p.list_tld_pricing(client).await,
            Self::Cloudflare(p) => p.list_tld_pricing(client).await,
            Self::Bundled(p) => p.list_tld_pricing(client).await,
        }
    }

    async fn availability(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<RegistrarAvailability, ProvisionError> {
        match self {
            Self::Porkbun(p) => p.availability(client, domain).await,
            Self::Gandi(p) => p.availability(client, domain).await,
            Self::Namecheap(p) => p.availability(client, domain).await,
            Self::Cloudflare(p) => p.availability(client, domain).await,
            Self::Bundled(p) => p.availability(client, domain).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Public dispatch functions
// ---------------------------------------------------------------------------

/// Returns a DNS dispatcher for the given provider, or `None` if:
/// - the provider has no DNS capability, or
/// - a required credential is missing from `creds`.
pub fn dns_provider(
    id: ProviderId,
    creds: Credentials,
    override_base_url: Option<String>,
) -> Option<DnsDispatch> {
    match id {
        ProviderId::Cloudflare => {
            let token = creds.get("api-token")?.to_string();
            let adapter = match override_base_url {
                Some(base) => crate::dns::cloudflare::Cloudflare::with_base_url(token, base),
                None => crate::dns::cloudflare::Cloudflare::new(token),
            };
            Some(DnsDispatch::Cloudflare(adapter))
        }
        ProviderId::Porkbun => {
            let apikey = creds.get("api-key")?.to_string();
            let secretapikey = creds.get("secret-api-key")?.to_string();
            let adapter = match override_base_url {
                Some(base) => {
                    crate::dns::porkbun::Porkbun::with_base_url(apikey, secretapikey, base)
                }
                None => crate::dns::porkbun::Porkbun::new(apikey, secretapikey),
            };
            Some(DnsDispatch::Porkbun(adapter))
        }
        ProviderId::Namecheap => {
            let api_user = creds.get("api-user")?.to_string();
            let api_key = creds.get("api-key")?.to_string();
            let adapter = match override_base_url {
                Some(base) => {
                    crate::dns::namecheap::Namecheap::with_base_url(api_user, api_key, base)
                }
                None => crate::dns::namecheap::Namecheap::new(api_user, api_key),
            };
            Some(DnsDispatch::Namecheap(adapter))
        }
        ProviderId::Gandi => {
            let token = creds.get("personal-access-token")?.to_string();
            // Precedence: `override_base_url` arg > `api-base` cred > default.
            // The `api-base` cred override (test-only; never set by the per-
            // provider field metadata in `providers.yaml`) is a legacy path
            // used by verify-time registrar tests to point the adapter at a
            // wiremock server. The new `override_base_url` argument layers on
            // top with higher precedence (more specific intent from the test
            // helper layer). Production callers leave both unset and get the
            // proxy/direct API base.
            let base = override_base_url.or_else(|| creds.get("api-base").map(|s| s.to_string()));
            let gandi = match base {
                Some(b) => crate::dns::gandi::Gandi::with_base_url(token, b),
                None => crate::dns::gandi::Gandi::new(token),
            };
            Some(DnsDispatch::Gandi(gandi))
        }
        ProviderId::Hetzner => {
            // Hetzner DNS now rides the Cloud API (api.hetzner.cloud/v1,
            // Bearer) — the same API and token as VPS — so DNS authenticates
            // with the single Cloud `api-token` (the standalone
            // dns.hetzner.com DNS API was shut down 2026-05-20). A Hetzner
            // selection without the token yields None ("no DNS for this
            // provider").
            let token = creds.get("api-token")?.to_string();
            let adapter = match override_base_url {
                Some(base) => crate::dns::hetzner::HetznerDns::with_base_url(token, base),
                None => crate::dns::hetzner::HetznerDns::new(token),
            };
            Some(DnsDispatch::Hetzner(adapter))
        }
        ProviderId::Bundled => {
            let (base, token) = bundled_creds(&creds, override_base_url)?;
            Some(DnsDispatch::Bundled(crate::dns::bundled::BundledDns::new(
                base, token,
            )))
        }
        ProviderId::Digitalocean => None, // VPS only
        ProviderId::Vultr => None,        // VPS only
        ProviderId::Ovh => None,          // VPS only
        ProviderId::Linode => None,       // VPS only
    }
}

/// The bundled provider's two credential fields (`registry.md` § Bundled
/// provider): the user-typed `base-url` and the `hosted-auth` token under
/// `api-token`. There is no canonical host to fall back to — a missing
/// `base-url` is a missing credential, exactly like a missing token. The
/// E2E `override_base_url` still wins when a test supplies one, for parity
/// with every other adapter's `with_base_url` path.
///
/// `base-url` also has to pass [`bundled_api::checked_base_url`] (spec
/// § Endpoints' `https://` requirement, apps row 493) — a scheme this base
/// already failed at `hosted_auth_begin` time should never reach a dispatcher
/// that then attaches the bearer token to it.
fn bundled_creds(
    creds: &Credentials,
    override_base_url: Option<String>,
) -> Option<(String, String)> {
    let token = creds.get("api-token")?.to_string();
    let base = match override_base_url {
        Some(b) => b,
        None => creds.get("base-url")?.to_string(),
    };
    let base = crate::bundled_api::checked_base_url(&base).ok()?;
    Some((base, token))
}

/// Returns a VPS dispatcher for the given provider, or `None` if:
/// - the provider has no VPS capability, or
/// - a required credential is missing from `creds`.
pub fn vps_provider(
    id: ProviderId,
    creds: Credentials,
    override_base_url: Option<String>,
) -> Option<VpsDispatch> {
    match id {
        ProviderId::Hetzner => {
            let token = creds.get("api-token")?.to_string();
            let adapter = match override_base_url {
                Some(base) => crate::vps::hetzner::Hetzner::with_base_url(token, base),
                None => crate::vps::hetzner::Hetzner::new(token),
            };
            Some(VpsDispatch::Hetzner(adapter))
        }
        ProviderId::Digitalocean => {
            let token = creds.get("api-token")?.to_string();
            let adapter = match override_base_url {
                Some(base) => crate::vps::digitalocean::DigitalOcean::with_base_url(token, base),
                None => crate::vps::digitalocean::DigitalOcean::new(token),
            };
            Some(VpsDispatch::Digitalocean(adapter))
        }
        ProviderId::Vultr => {
            let api_key = creds.get("api-key")?.to_string();
            let adapter = match override_base_url {
                Some(base) => crate::vps::vultr::Vultr::with_base_url(api_key, base),
                None => crate::vps::vultr::Vultr::new(api_key),
            };
            Some(VpsDispatch::Vultr(adapter))
        }
        ProviderId::Ovh => {
            let app_key = creds.get("app-key")?.to_string();
            let app_secret = creds.get("app-secret")?.to_string();
            let consumer_key = creds.get("consumer-key")?.to_string();
            let adapter = match override_base_url {
                Some(base) => {
                    crate::vps::ovh::Ovh::with_base_url(app_key, app_secret, consumer_key, base)
                }
                None => crate::vps::ovh::Ovh::new(app_key, app_secret, consumer_key),
            };
            Some(VpsDispatch::Ovh(adapter))
        }
        ProviderId::Linode => {
            let token = creds.get("api-token")?.to_string();
            let adapter = match override_base_url {
                Some(base) => crate::vps::linode::Linode::with_base_url(token, base),
                None => crate::vps::linode::Linode::new(token),
            };
            Some(VpsDispatch::Linode(adapter))
        }
        ProviderId::Bundled => {
            let (base, token) = bundled_creds(&creds, override_base_url)?;
            Some(VpsDispatch::Bundled(crate::vps::bundled::BundledVps::new(
                base, token,
            )))
        }
        ProviderId::Cloudflare => None, // DNS only
        ProviderId::Porkbun => None,    // DNS + Registrar only
        ProviderId::Namecheap => None,  // DNS only
        ProviderId::Gandi => None,      // DNS only
    }
}

/// Returns a Registrar dispatcher for the given provider, or `None` if:
/// - the provider has no Registrar capability, or
/// - a required credential is missing from `creds`.
pub fn registrar(id: ProviderId, creds: Credentials) -> Option<RegistrarDispatch> {
    match id {
        ProviderId::Bundled => {
            // The registrar seam has no `override_base_url` argument — the
            // `base-url` credential IS the base (an e2e fake's address is typed
            // into that field), so no `api-base` test override is needed either.
            let (base, token) = bundled_creds(&creds, None)?;
            Some(RegistrarDispatch::Bundled(
                crate::registrar::bundled::BundledRegistrar::new(base, token),
            ))
        }
        ProviderId::Porkbun => {
            let api_key = creds.get("api-key")?.to_string();
            let secret_api_key = creds.get("secret-api-key")?.to_string();
            // `api-base` cred override — same rationale as Gandi below.
            // Honored by tests pointing the adapter at wiremock.
            let registrar = match creds.get("api-base") {
                Some(base) => crate::registrar::porkbun::PorkbunRegistrar::with_base_url(
                    api_key,
                    secret_api_key,
                    base.to_string(),
                ),
                None => crate::registrar::porkbun::PorkbunRegistrar::new(api_key, secret_api_key),
            };
            Some(RegistrarDispatch::Porkbun(registrar))
        }
        ProviderId::Gandi => {
            let token = creds.get("personal-access-token")?.to_string();
            // `api-base` cred override — same rationale as `dns_provider`
            // above. Honored by tests pointing the adapter at wiremock.
            let registrar = match creds.get("api-base") {
                Some(base) => {
                    crate::registrar::gandi::GandiRegistrar::with_base_url(token, base.to_string())
                }
                None => crate::registrar::gandi::GandiRegistrar::new(token),
            };
            Some(RegistrarDispatch::Gandi(registrar))
        }
        ProviderId::Cloudflare => {
            let token = creds.get("api-token")?.to_string();
            let account_id = creds.get("account-id")?.to_string();
            // `api-base` cred override — same rationale as Gandi above.
            // Honored by tests pointing the adapter at wiremock.
            let registrar = match creds.get("api-base") {
                Some(base) => crate::registrar::cloudflare::CloudflareRegistrar::with_base_url(
                    token,
                    account_id,
                    base.to_string(),
                ),
                None => crate::registrar::cloudflare::CloudflareRegistrar::new(token, account_id),
            };
            Some(RegistrarDispatch::Cloudflare(registrar))
        }
        ProviderId::Hetzner => None, // VPS only
        ProviderId::Namecheap => {
            let api_user = creds.get("api-user")?.to_string();
            let api_key = creds.get("api-key")?.to_string();
            // `api-base` cred override — same rationale as Gandi above.
            // Honored by tests pointing the adapter at wiremock.
            let registrar = match creds.get("api-base") {
                Some(base) => crate::registrar::namecheap::NamecheapRegistrar::with_base_url(
                    api_user,
                    api_key,
                    base.to_string(),
                ),
                None => crate::registrar::namecheap::NamecheapRegistrar::new(api_user, api_key),
            };
            Some(RegistrarDispatch::Namecheap(registrar))
        }
        ProviderId::Digitalocean => None, // VPS only
        ProviderId::Vultr => None,        // VPS only
        ProviderId::Ovh => None,          // VPS only
        ProviderId::Linode => None,       // VPS only
    }
}

/// Resolve a [`RegistrarDispatch`] from the FFI/wasm-boundary `provider_id` +
/// `creds_json` strings — the "parse ids, look up dispatch, or error"
/// preamble `registrar_check`/`registrar_availability`/`registrar_register`/
/// `registrar_list_tld_pricing` each repeated once per binding crate
/// (`fauna-ffi`, `fauna-wasm-onboarding`).
pub fn resolve_registrar(provider_id: &str, creds_json: &str) -> Result<RegistrarDispatch, String> {
    let id = parse_provider_id(provider_id)?;
    let creds = parse_credentials(creds_json)?;
    registrar(id, creds).ok_or_else(|| {
        "provider does not support registrar capability or missing credentials".to_string()
    })
}

/// The full `registrar_register` FFI/wasm entry point, minus the platform
/// error/async wrapper — `resolve_registrar`'s "repeated preamble" analysis
/// above applied to the one call it didn't yet cover: `fauna-ffi::provisioning
/// ::registrar_register` and `fauna-wasm-onboarding::registrar_register` each
/// independently re-parsed `contact_json` with the identical
/// `"null"`/empty-string special case before calling `dispatch.register`; both
/// are now thin shells over this.
///
/// `contact_json`: either the literal string `"null"`, the empty string, or a
/// JSON-encoded [`ContactInfo`]. Registrars that use account-level WHOIS
/// contacts (Porkbun) accept `"null"`/empty; others must supply a full contact.
pub async fn register_from_json(
    provider_id: &str,
    creds_json: &str,
    domain: &str,
    years: u32,
    agreed_price_cents: u64,
    contact_json: &str,
    client: &reqwest::Client,
) -> Result<RegistrationResult, String> {
    let dispatch = resolve_registrar(provider_id, creds_json)?;
    let contact: Option<ContactInfo> = if contact_json.trim().is_empty()
        || contact_json.trim() == "null"
    {
        None
    } else {
        Some(serde_json::from_str(contact_json).map_err(|e| format!("invalid contact JSON: {e}"))?)
    };
    dispatch
        .register(client, domain, years, agreed_price_cents, contact.as_ref())
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod register_from_json_tests {
    use super::*;

    #[tokio::test]
    async fn unknown_provider_is_rejected_before_touching_the_network() {
        let client = reqwest::Client::new();
        let err = register_from_json("nope", "{}", "example.com", 1, 0, "null", &client)
            .await
            .unwrap_err();
        assert_eq!(err, "unknown provider id: nope");
    }

    #[tokio::test]
    async fn malformed_contact_json_is_rejected_before_touching_the_network() {
        let client = reqwest::Client::new();
        // Porkbun's registrar-capable credential shape — reaching the contact
        // parse (not failing earlier on a bad provider id/creds) exercises the
        // `"null"`/empty-string special case's else branch.
        let creds = r#"{"api-key":"k","secret-api-key":"s"}"#;
        let err = register_from_json(
            "porkbun",
            creds,
            "example.com",
            1,
            0,
            "not valid json",
            &client,
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("invalid contact JSON"), "{err}");
    }

    #[tokio::test]
    async fn empty_and_null_contact_json_both_take_the_account_level_contact_path() {
        // Both spellings must reach the SAME branch (no contact parse
        // attempted) — this is the exact special case the FFI and wasm
        // bindings each duplicated before this lift. A missing/invalid
        // provider surfaces the same error either way, proving neither
        // string was routed into `serde_json::from_str`.
        let client = reqwest::Client::new();
        let empty = register_from_json("nope", "{}", "example.com", 1, 0, "", &client)
            .await
            .unwrap_err();
        let null = register_from_json("nope", "{}", "example.com", 1, 0, "null", &client)
            .await
            .unwrap_err();
        assert_eq!(empty, "unknown provider id: nope");
        assert_eq!(null, "unknown provider id: nope");
    }
}
