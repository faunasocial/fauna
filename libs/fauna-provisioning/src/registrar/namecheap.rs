//! Namecheap registrar.
//!
//! Rides the same transport as `dns/namecheap.rs` ([`crate::namecheap_api`]):
//! same credentials, same single query-driven endpoint, so the global
//! parameters, the HTTP-200-with-`Status="ERROR"` envelope and the IP-allowlist
//! self-heal all come for free and are never re-derived here.
//!
//! `list_tld_pricing` is deliberately **not** overridden. Namecheap's
//! `users.getPricing` requires authentication, and that trait method exists to
//! quote a price *before* credentials are entered, so the default `Ok(None)`
//! is the honest answer. Per-domain pricing still works — [`Registrar::check`]
//! calls `getPricing` with the credentials it already holds.

use super::{
    ContactInfo, DomainAvailability, Registrar, RegistrarAvailability, RegistrationResult,
};
use crate::error::ProvisionError;
use crate::namecheap_api::{NamecheapApi, attr, elements};
use crate::proxy::BuildEnv;

/// The four WHOIS contact roles Namecheap requires on `domains.create`. Every
/// role is mandatory and they may all be the same person, so the wizard's
/// single [`ContactInfo`] is submitted four times.
const CONTACT_ROLES: [&str; 4] = ["Registrant", "Tech", "Admin", "AuxBilling"];

pub struct NamecheapRegistrar {
    api: NamecheapApi,
}

impl NamecheapRegistrar {
    pub fn new(api_user: String, api_key: String) -> Self {
        Self {
            api: NamecheapApi::new(api_user, api_key),
        }
    }

    /// [`Self::new`] with the build env supplied explicitly — lets a native
    /// test assert the browser routing. See `tests/cors_policy_bijection.rs`.
    pub fn new_for_env(api_user: String, api_key: String, env: BuildEnv) -> Self {
        Self {
            api: NamecheapApi::new_for_env(api_user, api_key, env),
        }
    }

    /// Test-only constructor that lets the conformance harness point the
    /// adapter at a wiremock server.
    pub fn with_base_url(api_user: String, api_key: String, api_base: String) -> Self {
        Self {
            api: NamecheapApi::with_base_url(api_user, api_key, api_base),
        }
    }

    /// The API base this adapter will call.
    pub fn api_base(&self) -> &str {
        self.api.api_base()
    }

    /// Override the `ClientIp` the transport claims — see
    /// [`NamecheapApi::set_client_ip`]. Only an optimisation.
    pub fn set_client_ip(&mut self, client_ip: String) {
        self.api.set_client_ip(client_ip);
    }

    /// `domains.check` for one name, returning its `<DomainCheckResult>`
    /// fragment.
    async fn check_result(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<CheckResult, ProvisionError> {
        let xml = self
            .api
            .call(
                client,
                "namecheap.domains.check",
                &[("DomainList".to_string(), domain.to_string())],
            )
            .await?;
        let el = elements(&xml, "DomainCheckResult")
            .into_iter()
            .next()
            .ok_or_else(|| {
                ProvisionError::Parse(
                    "Namecheap domains.check returned no DomainCheckResult".into(),
                )
            })?;
        Ok(CheckResult {
            available: attr(el, "Available").as_deref() == Some("true"),
            is_premium: attr(el, "IsPremiumName").as_deref() == Some("true"),
            premium_registration_cents: attr(el, "PremiumRegistrationPrice")
                .as_deref()
                .and_then(parse_decimal_to_cents),
            icann_fee_cents: attr(el, "IcannFee")
                .as_deref()
                .and_then(parse_decimal_to_cents),
        })
    }

    /// First-year registration price for a TLD, via `users.getPricing`.
    ///
    /// `Ok(None)` means Namecheap quoted no 1-year REGISTER price for the TLD —
    /// which is how this adapter recognises a TLD Namecheap doesn't sell (see
    /// [`Registrar::availability`]). Namecheap has no documented, verified
    /// "unsupported TLD" error code, so the absence of a price is used as the
    /// signal rather than a guessed number.
    async fn tld_price(
        &self,
        client: &reqwest::Client,
        tld: &str,
    ) -> Result<Option<Price>, ProvisionError> {
        let xml = self
            .api
            .call(
                client,
                "namecheap.users.getPricing",
                &[
                    ("ProductType".to_string(), "DOMAIN".to_string()),
                    ("ProductCategory".to_string(), "DOMAINS".to_string()),
                    ("ActionName".to_string(), "REGISTER".to_string()),
                    ("ProductName".to_string(), tld.to_string()),
                ],
            )
            .await?;
        Ok(parse_first_year_price(&xml))
    }
}

/// The fields this adapter consumes from a `<DomainCheckResult>`.
struct CheckResult {
    available: bool,
    is_premium: bool,
    /// Premium names carry their price inline; the standard `getPricing`
    /// table does not apply to them.
    premium_registration_cents: Option<u64>,
    /// The ICANN fee Namecheap bills on top of the registration price.
    icann_fee_cents: Option<u64>,
}

/// A resolved first-year registration price.
struct Price {
    cents: u64,
    currency: Option<String>,
}

/// Parse a Namecheap decimal money string ("10.98", "9", "0.00") into integer
/// cents. Returns `None` on unparseable input, and `None` for a bare "0" —
/// Namecheap uses zero to mean "not applicable" (a non-premium name's
/// `PremiumRegistrationPrice`), never a free domain.
fn parse_decimal_to_cents(s: &str) -> Option<u64> {
    let total = crate::money::parse_decimal_to_cents(s)?;
    (total > 0).then_some(total)
}

/// Pull the 1-year REGISTER price out of a `users.getPricing` response.
///
/// Namecheap nests `<Price Duration="1" DurationType="YEAR" Price="…"
/// YourPrice="…" Currency="USD" AdditionalCost="…" YourAdditonalCost="…"/>`
/// under `ProductType`/`ProductCategory`/`Product`. `YourPrice` is the
/// account's actual price (it reflects tier discounts) so it wins over the
/// list `Price`.
///
/// The additional cost is the ICANN fee, which Namecheap bills on top; it is
/// added so the quoted number is what the user is actually charged. Namecheap
/// really does spell one of those attributes `YourAdditonalCost` — the typo is
/// in their API, so both spellings are tried.
fn parse_first_year_price(xml: &str) -> Option<Price> {
    let el = elements(xml, "Price").into_iter().find(|el| {
        attr(el, "Duration").as_deref() == Some("1")
            && attr(el, "DurationType")
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case("YEAR"))
    })?;

    let base = attr(el, "YourPrice")
        .as_deref()
        .and_then(parse_decimal_to_cents)
        .or_else(|| {
            attr(el, "Price")
                .as_deref()
                .and_then(parse_decimal_to_cents)
        })?;
    let extra = attr(el, "YourAdditonalCost")
        .as_deref()
        .and_then(parse_decimal_to_cents)
        .or_else(|| {
            attr(el, "AdditionalCost")
                .as_deref()
                .and_then(parse_decimal_to_cents)
        })
        .unwrap_or(0);

    Some(Price {
        cents: base + extra,
        currency: attr(el, "Currency"),
    })
}

/// The TLD portion of a domain — everything after the first label, so
/// `example.co.uk` yields `co.uk` (Namecheap's `ProductName` for pricing).
fn tld_of(domain: &str) -> Result<&str, ProvisionError> {
    let (sld, tld) = domain
        .split_once('.')
        .ok_or_else(|| ProvisionError::Parse(format!("Invalid domain: {domain}")))?;
    if sld.is_empty() || tld.is_empty() {
        return Err(ProvisionError::Parse(format!("Invalid domain: {domain}")));
    }
    Ok(tld)
}

/// Namecheap requires WHOIS phone numbers as `+CC.NNNNNNNNNN` and rejects
/// anything else with an opaque parameter error, so a free-text number from the
/// wizard's contact form is normalised here rather than sent as typed.
///
/// A number that is already in the required shape passes through. Otherwise the
/// country code is taken from the digits between the leading `+` and the first
/// separator, and the remainder becomes the subscriber number. When the split
/// can't be determined — no `+`, or no separator to split on, so the country
/// code would have to be guessed — this returns an error telling the user the
/// exact expected format. Guessing is the worse failure: a wrong country code
/// registers a domain against an unreachable contact.
fn normalize_phone(raw: &str) -> Result<String, ProvisionError> {
    let bad_format = || {
        ProvisionError::Other(format!(
            "Namecheap needs the phone number as +CountryCode.Number (for example \
             +1.5551234567). Enter it with the country code separated from the rest, \
             e.g. \"+1 555 123 4567\". Got: {raw:?}"
        ))
    };

    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix('+').ok_or_else(bad_format)?;

    // Already `+CC.NNNN`? Accept as-is once both halves are digits.
    if let Some((cc, number)) = rest.split_once('.')
        && !cc.is_empty()
        && cc.len() <= 3
        && cc.chars().all(|c| c.is_ascii_digit())
        && number.len() >= 4
        && number.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(format!("+{cc}.{number}"));
    }

    // Otherwise split at the first non-digit: those leading digits are the
    // country code, everything else contributes its digits to the number.
    let split = rest
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(bad_format)?;
    let (cc, tail) = rest.split_at(split);
    let number: String = tail.chars().filter(char::is_ascii_digit).collect();
    if cc.is_empty() || cc.len() > 3 || number.len() < 4 {
        return Err(bad_format());
    }
    Ok(format!("+{cc}.{number}"))
}

/// The nine `domains.create` parameters for one WHOIS contact role.
fn contact_params(role: &str, contact: &ContactInfo, phone: &str) -> Vec<(String, String)> {
    vec![
        (format!("{role}FirstName"), contact.first_name.clone()),
        (format!("{role}LastName"), contact.last_name.clone()),
        (format!("{role}Address1"), contact.address1.clone()),
        (format!("{role}City"), contact.city.clone()),
        (format!("{role}StateProvince"), contact.state.clone()),
        (format!("{role}PostalCode"), contact.postal_code.clone()),
        (format!("{role}Country"), contact.country.clone()),
        (format!("{role}Phone"), phone.to_string()),
        (format!("{role}EmailAddress"), contact.email.clone()),
    ]
}

impl Registrar for NamecheapRegistrar {
    /// `users.getBalances` — the cheapest safe command that proves the
    /// credentials work *and* that the account can be billed, which is what a
    /// registrar's verify is really asserting.
    async fn verify(&self, client: &reqwest::Client) -> Result<(), ProvisionError> {
        self.api
            .call(client, "namecheap.users.getBalances", &[])
            .await?;
        Ok(())
    }

    async fn check(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<DomainAvailability, ProvisionError> {
        let result = self.check_result(client, domain).await?;

        // A premium name's price is quoted inline and the standard TLD price
        // table does not apply to it — quoting the table price here would show
        // the user a number far below what Namecheap actually bills.
        let price = if result.is_premium {
            result.premium_registration_cents.map(|cents| Price {
                cents: cents + result.icann_fee_cents.unwrap_or(0),
                currency: None,
            })
        } else if result.available {
            self.tld_price(client, tld_of(domain)?).await?
        } else {
            // Unavailable names get no price — matching the trait's contract
            // and saving a round-trip.
            None
        };

        Ok(DomainAvailability {
            domain: domain.to_string(),
            available: result.available,
            price_first_year_cents: price.as_ref().map(|p| p.cents),
            // Renewal pricing is a second `getPricing` call with
            // `ActionName=RENEW`; not fetched here, matching Gandi's
            // single-round-trip `check`.
            price_renewal_cents: None,
            currency: price.and_then(|p| p.currency),
        })
    }

    /// Overrides the default so an unsold TLD reads as `TldNotSupported`
    /// rather than "already taken".
    ///
    /// The signal is structural, not a guessed error code: if Namecheap's own
    /// pricing table quotes no 1-year REGISTER price for the TLD, Namecheap
    /// does not sell it.
    async fn availability(
        &self,
        client: &reqwest::Client,
        domain: &str,
    ) -> Result<RegistrarAvailability, ProvisionError> {
        let result = self.check_result(client, domain).await?;

        if result.is_premium {
            return Ok(match result.premium_registration_cents {
                Some(cents) if result.available => RegistrarAvailability::Buyable {
                    price_cents: cents + result.icann_fee_cents.unwrap_or(0),
                    currency: None,
                    renewal_cents: None,
                },
                _ => RegistrarAvailability::Unavailable,
            });
        }

        match self.tld_price(client, tld_of(domain)?).await? {
            None => Ok(RegistrarAvailability::TldNotSupported),
            Some(price) if result.available => Ok(RegistrarAvailability::Buyable {
                price_cents: price.cents,
                currency: price.currency,
                renewal_cents: None,
            }),
            Some(_) => Ok(RegistrarAvailability::Unavailable),
        }
    }

    async fn register(
        &self,
        client: &reqwest::Client,
        domain: &str,
        years: u32,
        // Namecheap's `domains.create` has no price-acknowledgement field
        // (unlike Porkbun's `cost`); the price the user confirmed is enforced
        // by the wizard, not the API.
        _agreed_price_cents: u64,
        contact: Option<&ContactInfo>,
    ) -> Result<RegistrationResult, ProvisionError> {
        let contact = contact.ok_or_else(|| {
            ProvisionError::Other(
                "Namecheap requires WHOIS contact (requires_contact = true)".into(),
            )
        })?;
        let phone = normalize_phone(&contact.phone)?;

        let mut params = vec![
            ("DomainName".to_string(), domain.to_string()),
            ("Years".to_string(), years.to_string()),
            // Namecheap's WHOIS privacy is free and off by default. Turning it
            // on is a hard-coded constant, not a choice surface: there is no
            // deployment in which publishing the user's home address in WHOIS
            // is the behaviour Fauna wants, and the product invariants put any
            // real user choice in the client UI, never a flag.
            ("AddFreeWhoisguard".to_string(), "yes".to_string()),
            ("WGEnabled".to_string(), "yes".to_string()),
        ];
        for role in CONTACT_ROLES {
            params.extend(contact_params(role, contact, &phone));
        }

        let xml = self
            .api
            .call(client, "namecheap.domains.create", &params)
            .await?;

        // `call` has already rejected an `Status="ERROR"` body. Check the
        // result element too: a `Registered="false"` under an OK envelope is
        // the same class of lying success that made every Namecheap API
        // failure read as a win before 2026-07-22 — never assume the envelope
        // is the whole answer.
        let registered = elements(&xml, "DomainCreateResult")
            .into_iter()
            .next()
            .and_then(|el| attr(el, "Registered"));
        match registered.as_deref() {
            Some("true") => {}
            Some(other) => {
                return Err(ProvisionError::Other(format!(
                    "Namecheap did not register {domain} (Registered=\"{other}\")"
                )));
            }
            None => {
                return Err(ProvisionError::Parse(
                    "Namecheap domains.create returned no DomainCreateResult".into(),
                ));
            }
        }

        Ok(RegistrationResult {
            domain: domain.to_string(),
            // `domains.create` reports no nameservers. A fresh Namecheap
            // registration lands on their BasicDNS, and the wizard discovers
            // the zone through the `dns_verify(<registrar>, <same creds>)`
            // step that follows registration (`registry.md` § Why `purchase`
            // doesn't show a separate DNS step) — same as Gandi.
            nameservers: Vec::new(),
        })
    }

    /// Namecheap accepts per-registration WHOIS contacts, so the wizard must
    /// render the contact form (unlike Porkbun's account-level model).
    fn requires_contact(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tld_of_handles_multi_label_tlds() {
        assert_eq!(tld_of("example.com").unwrap(), "com");
        assert_eq!(tld_of("example.co.uk").unwrap(), "co.uk");
        assert!(tld_of("nodot").is_err());
        assert!(tld_of(".nosld").is_err());
        assert!(tld_of("notld.").is_err());
    }

    #[test]
    fn parses_money_strings() {
        assert_eq!(parse_decimal_to_cents("10.98"), Some(1098));
        assert_eq!(parse_decimal_to_cents("9"), Some(900));
        assert_eq!(parse_decimal_to_cents("9.7"), Some(970));
        assert_eq!(parse_decimal_to_cents("0.18"), Some(18));
        // Namecheap's "not applicable" zero must not read as a free domain.
        assert_eq!(parse_decimal_to_cents("0.00"), None);
        assert_eq!(parse_decimal_to_cents("0"), None);
        assert_eq!(parse_decimal_to_cents("abc"), None);
        assert_eq!(parse_decimal_to_cents(""), None);
    }

    /// `YourPrice` (the account's real price) wins over the list `Price`, and
    /// the ICANN fee is included so the quote matches the charge.
    #[test]
    fn pricing_prefers_your_price_and_adds_the_icann_fee() {
        let xml = r#"<ApiResponse Status="OK">
  <CommandResponse Type="namecheap.users.getPricing">
    <UserGetPricingResult>
      <ProductType Name="domains">
        <ProductCategory Name="register">
          <Product Name="com">
            <Price Duration="1" DurationType="YEAR" Price="10.98" RegularPrice="13.98" YourPrice="9.98" Currency="USD" AdditionalCost="0.18" YourAdditonalCost="0.18"/>
            <Price Duration="2" DurationType="YEAR" Price="21.96" YourPrice="19.96" Currency="USD"/>
          </Product>
        </ProductCategory>
      </ProductType>
    </UserGetPricingResult>
  </CommandResponse>
</ApiResponse>"#;
        let price = parse_first_year_price(xml).expect("1-year price must parse");
        assert_eq!(price.cents, 998 + 18);
        assert_eq!(price.currency.as_deref(), Some("USD"));
    }

    /// Namecheap's own typo'd `YourAdditonalCost` is the documented spelling,
    /// but the correctly-spelled `AdditionalCost` must work too.
    #[test]
    fn pricing_accepts_either_additional_cost_spelling() {
        let xml = r#"<Price Duration="1" DurationType="YEAR" Price="10.00" Currency="USD" AdditionalCost="0.18"/>"#;
        assert_eq!(parse_first_year_price(xml).unwrap().cents, 1018);
    }

    /// No 1-year REGISTER price means Namecheap doesn't sell the TLD — the
    /// signal `availability()` turns into `TldNotSupported`.
    #[test]
    fn pricing_absent_when_no_one_year_row() {
        let xml = r#"<ApiResponse Status="OK"><Product Name="zzz">
            <Price Duration="2" DurationType="YEAR" Price="21.96" Currency="USD"/>
        </Product></ApiResponse>"#;
        assert!(parse_first_year_price(xml).is_none());
    }

    #[test]
    fn normalizes_phone_numbers_namecheap_will_accept() {
        // Already in the required shape.
        assert_eq!(normalize_phone("+1.5551234567").unwrap(), "+1.5551234567");
        // Common free-text shapes.
        assert_eq!(normalize_phone("+1 555 123 4567").unwrap(), "+1.5551234567");
        assert_eq!(normalize_phone("+47 21 22 23 24").unwrap(), "+47.21222324");
        assert_eq!(
            normalize_phone("+44 (0)20 7946 0958").unwrap(),
            "+44.02079460958"
        );
        assert_eq!(
            normalize_phone("  +1-555-123-4567 ").unwrap(),
            "+1.5551234567"
        );
    }

    /// A number whose country code would have to be guessed is refused with an
    /// actionable message — registering a domain against an unreachable WHOIS
    /// contact is worse than making the user retype it.
    #[test]
    fn refuses_phone_numbers_it_would_have_to_guess_at() {
        for raw in ["5551234567", "+15551234567", "", "+", "+1 555"] {
            let err = normalize_phone(raw)
                .expect_err("must not guess a country code from {raw:?}")
                .to_string();
            assert!(
                err.contains("+1.5551234567"),
                "the error must show the expected format: {err}"
            );
        }
    }

    /// All four WHOIS roles must be submitted — Namecheap rejects a create
    /// that is missing any of them, and the wizard only collects one contact.
    #[test]
    fn register_submits_every_contact_role() {
        let contact = ContactInfo {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: "ada@example.com".into(),
            phone: "+44 20 7946 0958".into(),
            address1: "1 Mill Lane".into(),
            city: "London".into(),
            state: "London".into(),
            postal_code: "NW1 1AA".into(),
            country: "GB".into(),
        };
        let phone = normalize_phone(&contact.phone).unwrap();
        let mut params = Vec::new();
        for role in CONTACT_ROLES {
            params.extend(contact_params(role, &contact, &phone));
        }
        assert_eq!(params.len(), 36, "4 roles x 9 fields");
        for role in ["Registrant", "Tech", "Admin", "AuxBilling"] {
            assert!(
                params
                    .iter()
                    .any(|(k, v)| k == &format!("{role}Phone") && v == "+44.2079460958"),
                "{role} must carry the normalised phone"
            );
            assert!(
                params
                    .iter()
                    .any(|(k, v)| k == &format!("{role}EmailAddress") && v == "ada@example.com"),
                "{role} must carry the email"
            );
        }
    }
}
