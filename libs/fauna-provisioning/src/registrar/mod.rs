pub mod bundled;
pub mod cloudflare;
pub mod gandi;
pub mod namecheap;
pub mod porkbun;

use crate::error::ProvisionError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainAvailability {
    pub domain: String,
    pub available: bool,
    /// First-year registration price in integer cents of `currency`.
    /// Used as the `agreed_price_cents` argument to `register()` for
    /// registrars (e.g., Porkbun) that require the price in the register
    /// request body. `None` if the API didn't return a price (e.g., when
    /// `available == false`).
    pub price_first_year_cents: Option<u64>,
    pub price_renewal_cents: Option<u64>,
    pub currency: Option<String>, // e.g. "USD"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationResult {
    pub domain: String,
    pub nameservers: Vec<String>,
}

/// Public TLD pricing quote returned by `Registrar::list_tld_pricing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TldPriceQuote {
    pub tld: String, // lowercase, no leading dot
    pub registration_cents: u64,
    pub renewal_cents: u64,
    pub currency: String, // e.g. "USD"
}

/// Outcome of `Registrar::availability` — the structured answer the wizard's
/// `dns-status-text` consumes. Distinct from `DomainAvailability` (the raw
/// API response shape) because it folds the "registrar can't quote this TLD"
/// case into its own variant, which `DomainAvailability` can't express.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RegistrarAvailability {
    /// Registrar will sell this domain. `price_cents` is what the wizard
    /// shows in the price-confirm step and passes back as `agreed_price_cents`
    /// to `register()`.
    Buyable {
        price_cents: u64,
        currency: Option<String>,
        /// The registrar's renewal price per year in the same currency, when
        /// it quoted one — disclosed on the wizard's BoM before the charge
        /// (`docs/goal/behavior/onboarding.md` § 6). `None` for a registrar
        /// whose availability call carries no renewal figure.
        #[serde(default)]
        renewal_cents: Option<u64>,
    },
    /// Domain isn't available at this registrar — either it's already
    /// registered (anywhere) or this registrar's API said `available: false`
    /// without further detail.
    Unavailable,
    /// This registrar doesn't carry the domain's TLD. Distinct from
    /// `Unavailable` so the wizard can surface "this registrar can't sell
    /// you a .example domain — try another" rather than "already taken."
    TldNotSupported,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContactInfo {
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: String,
    pub address1: String,
    pub city: String,
    pub state: String,
    pub postal_code: String,
    pub country: String, // ISO 3166-1 alpha-2
}

#[allow(async_fn_in_trait)]
pub trait Registrar: Send + Sync {
    /// Verify credentials. Returns Ok(()) if the API accepts the token.
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError>;

    /// Check availability of a domain name. The returned `DomainAvailability`
    /// carries the price the caller passes to `register()` below, preventing
    /// price-change races between UI confirmation and the register call.
    async fn check(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError>;

    /// Register a domain. Bills the registrar account's prepaid balance or
    /// default payment method.
    ///
    /// - `agreed_price_cents`: the price the user confirmed in the UI. Some
    ///   registrars (Porkbun) require this in the request body; others may
    ///   ignore it.
    /// - `contact`: WHOIS contact fields. `None` for registrars whose API
    ///   uses pre-configured account-level contacts (see
    ///   `requires_contact()`); `Some` for registrars that accept per-
    ///   registration contacts (Gandi, Namecheap).
    ///
    /// The caller MUST confirm via UI that the user accepts the displayed
    /// price before calling this.
    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        years: u32,
        agreed_price_cents: u64,
        contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError>;

    /// Whether this registrar's API accepts WHOIS contact fields per
    /// registration. Default true. Porkbun overrides to false — Porkbun's
    /// API uses account-level contacts set via their web dashboard. UIs
    /// consult this to decide whether to render the contact form.
    fn requires_contact(&self) -> bool {
        true
    }

    /// Optional pre-fill source for the wizard's WHOIS contact form.
    /// Returns the user's account-default contact if the registrar's API
    /// exposes one. The wizard pre-populates the form; the user still
    /// confirms/edits before submission. `None` = no prefill (Porkbun,
    /// Namecheap). Errors are non-fatal at the call site — the wizard
    /// treats them as `None`.
    async fn fetch_default_contact(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<ContactInfo>, ProvisionError> {
        let _ = client;
        Ok(None)
    }

    /// Public TLD pricing — for registrars that expose a no-auth pricing
    /// endpoint, returns the full TLD table so the wizard can show a
    /// "buy domain for $X" estimate before credentials are entered.
    /// Returns `Ok(None)` for registrars without a public price list.
    async fn list_tld_pricing(
        &self,
        client: &reqwest::Client,
    ) -> Result<Option<Vec<TldPriceQuote>>, ProvisionError> {
        let _ = client;
        Ok(None)
    }

    /// Structured availability outcome consumed by the wizard's
    /// `dns-status-text`. Default impl wraps `check()`; impls override to
    /// surface `TldNotSupported` when their API distinguishes "TLD not
    /// carried" from "already registered."
    async fn availability(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<RegistrarAvailability, ProvisionError> {
        let avail = self.check(client, domain).await?;
        if avail.available {
            if let Some(price_cents) = avail.price_first_year_cents {
                Ok(RegistrarAvailability::Buyable {
                    price_cents,
                    currency: avail.currency,
                    renewal_cents: avail.price_renewal_cents,
                })
            } else {
                // Available but no price: can't quote, so the wizard
                // can't offer a confirmable Buyable. Surface as Unavailable.
                Ok(RegistrarAvailability::Unavailable)
            }
        } else {
            Ok(RegistrarAvailability::Unavailable)
        }
    }
}
