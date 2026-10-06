//! Where an app's region comes from — **declared, never detected**
//! (`region-blocking.md` § Region determination; § The content plane → *How an
//! app obtains its region's policy*).
//!
//! Each platform shell hands shared Rust one [`DeclaredRegion`] from ONE
//! function — the only platform-divergent leaf of the plane. A store build reads
//! its storefront, a sideloaded or self-built one the OS's user-set region
//! setting. There is no in-app override: an override is a knob that would make
//! the declaration a choice rather than a fact.
//!
//! The parsing a leaf needs is shared here too, so two shells reading the same
//! OS setting (tui and linux both read the POSIX locale) cannot disagree about
//! what it says.

use fauna_core::region_authority::RegionCode;
use serde::Serialize;

/// Which OS or store fact the declared region was read from — shown beside the
/// region on the transparency surface, with the change path it names.
///
/// A view type, closed on purpose: the device record stores it through its own
/// carrying twin (`plane::StoredRegionSource`), so a source a newer build added
/// is held there and never becomes a case an app must render. `Serialize`
/// only, for the web shell's view JSON — nothing decodes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionSource {
    /// The app store's storefront region (a store-distributed build).
    Storefront,
    /// The operating system's user-set region setting.
    SystemRegion,
    /// The territory of the POSIX locale (`LC_ALL`, else `LANG`) — tui and
    /// linux, whose OS region setting *is* the locale.
    SystemLocale,
    /// The region subtag of the browser's user-set language (web): a page can
    /// read no OS region setting, and the browser's language list is the one
    /// user-set, visible, network-free declaration it can read.
    BrowserLocale,
}

impl RegionSource {
    /// The i18n key naming this source on the settings surface.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Storefront => "region.source_storefront",
            Self::SystemRegion => "region.source_system_region",
            Self::SystemLocale => "region.source_system_locale",
            Self::BrowserLocale => "region.source_browser_locale",
        }
    }
}

/// The app's declared region and where the declaration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredRegion {
    pub code: RegionCode,
    pub source: RegionSource,
}

impl DeclaredRegion {
    /// A region an OS or store API hands over as a bare code (Windows'
    /// `RegionInfo`, Apple's `Locale.region` and storefront, Android's locale
    /// country and billing country) — `None` when the platform reports none,
    /// or something that is not a region code (a UN M.49 numeric like `419`).
    /// Never case-folds, like [`RegionCode::parse`] everywhere else.
    pub fn from_os_code(code: &str, source: RegionSource) -> Option<Self> {
        Some(Self {
            code: RegionCode::parse(code).ok()?,
            source,
        })
    }

    /// A store build's declaration from its storefront's ISO 3166-1
    /// **alpha-3** code (Apple's StoreKit `Storefront.countryCode`) — source
    /// [`RegionSource::Storefront`]; `None` for no storefront or a code that is
    /// not in [`crate::iso3166`]. A store build with no storefront declares
    /// nothing: it never falls back to the OS region, which is the self-built
    /// build's declaration, not a store build's.
    pub fn from_storefront_alpha3(alpha3: Option<&str>) -> Option<Self> {
        let alpha2 = crate::iso3166::alpha2_for_alpha3(alpha3?)?;
        Self::from_os_code(alpha2, RegionSource::Storefront)
    }
}

/// The declared region of a browser: the region subtag of one BCP 47 language
/// tag (`navigator.language`) — `language[-script][-REGION]…`. `None` when the
/// tag carries no region subtag (two letters, or a three-digit UN M.49 area,
/// RFC 5646 § 2.2.4): a region is **never inferred** from the language (`en`
/// is not `US`). Whether anyone administers the region is the registry's
/// question, never the parser's.
///
/// BCP 47 subtags are case-insensitive (RFC 5646 § 2.1.1) and the region's
/// canonical form is upper case, so — unlike a POSIX territory — the subtag
/// is canonicalized before it is parsed.
pub fn declared_from_bcp47(tag: &str) -> Option<DeclaredRegion> {
    let mut subtags = tag.split(['-', '_']).skip(1);
    let mut next = subtags.next()?;
    if next.len() == 4 && next.chars().all(|c| c.is_ascii_alphabetic()) {
        next = subtags.next()?;
    }
    let alpha2 = next.len() == 2 && next.chars().all(|c| c.is_ascii_alphabetic());
    let digit3 = next.len() == 3 && next.chars().all(|c| c.is_ascii_digit());
    if !(alpha2 || digit3) {
        return None;
    }
    DeclaredRegion::from_os_code(&next.to_ascii_uppercase(), RegionSource::BrowserLocale)
}

/// The environment variable carrying the e2e declared-region override.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub const E2E_DECLARED_ENV: &str = "FAUNA_E2E_REGION_DECLARED";

/// The leaf's declaration — or, **in a test-capable build only**, the region
/// [`E2E_DECLARED_ENV`] names, declared from the leaf's own `source`.
///
/// How a journey declares the synthetic region on a platform whose region a
/// test cannot set (a store build's storefront, Windows' user geo, a mobile
/// OS's region): the override replaces the *code* only, so the settings
/// surface still names the platform's real source. The same gate and posture
/// as the registry seed ([`crate::registry`], convention 15): compiled out of
/// release artifacts, where this is the identity.
pub fn with_e2e_override(
    leaf: Option<DeclaredRegion>,
    #[allow(unused_variables)] source: RegionSource,
) -> Option<DeclaredRegion> {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if let Ok(code) = std::env::var(E2E_DECLARED_ENV)
        && !code.is_empty()
    {
        return DeclaredRegion::from_os_code(&code, source);
    }
    leaf
}

/// The territory of one POSIX locale value (`language[_TERRITORY][.codeset][@modifier]`),
/// as a [`RegionCode`] — or `None` when the value names no territory (`C`,
/// `POSIX`, `C.UTF-8`, a bare `en`) or a territory that is not a region code.
///
/// Never case-folds: a lower-case territory is not a region code, the same rule
/// [`RegionCode::parse`] holds everywhere else.
pub fn region_from_posix_locale(value: &str) -> Option<RegionCode> {
    let without_modifier = value.split('@').next().unwrap_or_default();
    let without_codeset = without_modifier.split('.').next().unwrap_or_default();
    let (_language, territory) = without_codeset.split_once('_')?;
    RegionCode::parse(territory).ok()
}

/// The declared region of a POSIX-locale platform: the first **set, non-empty**
/// of `LC_ALL` then `LANG` — the precedence the C library applies — and its
/// territory. A set value naming no territory declares nothing (it does not fall
/// through to `LANG`: the locale in force is the one `LC_ALL` names).
pub fn declared_from_posix_locale(
    lc_all: Option<&str>,
    lang: Option<&str>,
) -> Option<DeclaredRegion> {
    let in_force = [lc_all, lang]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty())?;
    Some(DeclaredRegion {
        code: region_from_posix_locale(in_force)?,
        source: RegionSource::SystemLocale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(s: &str) -> RegionCode {
        RegionCode::parse(s).unwrap()
    }

    #[test]
    fn a_storefront_declares_its_alpha2_region_and_nothing_else_declares() {
        let d = DeclaredRegion::from_storefront_alpha3(Some("NOR")).unwrap();
        assert_eq!(d.code, code("NO"));
        assert_eq!(d.source, RegionSource::Storefront);
        // No storefront, or one the table does not know, declares nothing — a
        // store build never falls back to the OS region.
        assert_eq!(DeclaredRegion::from_storefront_alpha3(None), None);
        assert_eq!(DeclaredRegion::from_storefront_alpha3(Some("")), None);
        assert_eq!(DeclaredRegion::from_storefront_alpha3(Some("XXX")), None);
        assert_eq!(DeclaredRegion::from_storefront_alpha3(Some("NO")), None);
    }

    #[test]
    fn a_locale_territory_is_the_region() {
        assert_eq!(region_from_posix_locale("nb_NO.UTF-8"), Some(code("NO")));
        assert_eq!(region_from_posix_locale("de_DE@euro"), Some(code("DE")));
        assert_eq!(region_from_posix_locale("en_US"), Some(code("US")));
        assert_eq!(
            region_from_posix_locale("sr_RS.UTF-8@latin"),
            Some(code("RS"))
        );
    }

    #[test]
    fn a_locale_without_a_territory_declares_nothing() {
        for value in [
            "C", "POSIX", "C.UTF-8", "en", "en.UTF-8", "", "en_", "en_us",
        ] {
            assert_eq!(region_from_posix_locale(value), None, "{value:?}");
        }
    }

    #[test]
    fn lc_all_wins_over_lang_and_an_empty_value_is_unset() {
        let d = declared_from_posix_locale(Some("fr_FR.UTF-8"), Some("nb_NO.UTF-8")).unwrap();
        assert_eq!(d.code, code("FR"));
        assert_eq!(d.source, RegionSource::SystemLocale);
        let d = declared_from_posix_locale(Some(""), Some("nb_NO.UTF-8")).unwrap();
        assert_eq!(d.code, code("NO"));
        // The locale in force names no territory: nothing is declared, and LANG
        // is NOT consulted behind it.
        assert_eq!(
            declared_from_posix_locale(Some("C.UTF-8"), Some("nb_NO")),
            None
        );
        assert_eq!(declared_from_posix_locale(None, None), None);
    }
}
