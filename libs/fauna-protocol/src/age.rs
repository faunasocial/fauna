//! The account **age band** — wire vocabulary and the attested-claim payload
//! (`docs/goal/behavior/family-safety.md` § The account age band, ratified
//! 2026-08-22; the registration-mode interplay is
//! `docs/goal/architecture/nest/public-mode.md` § Registration Modes → *Age at
//! registration*).
//!
//! The band is per-account nest state on the guardianship plane, set **at
//! admission** (the invite mint / request-approve carry, beside
//! `guardian_actor`), and it is a **defaults dial, never a fourth enforcement
//! pillar**: it selects the guardian-policy defaults offered at admission
//! ([`crate::family::ReachPolicy::age_band_defaults`]); enforcement stays where
//! the three pillars put it. An account with **no guardianship link is
//! [`AgeBand::Adult`] by construction** — the at-rest representation of that
//! rule is *the absence of a band row*, so the default case is unrepresentable
//! rather than stored.
//!
//! Wire convention: the band and its provenance ride as their wire tokens
//! (`String` fields on the carrying structs, like the reach-policy knobs);
//! [`AgeBand::from_wire`] / [`AgeBandProvenance::from_wire`] are the single
//! typed parses. The nest refuses an admission carrying a token it cannot name
//! (the established knob rule — validate at write); there is deliberately no
//! `FAIL_CLOSED` value because **enforcement never keys on the band** — only
//! admission defaults and audits do (`family-safety.md` § The account age band,
//! provenance bullet).

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

/// The four bands — the same bands the store age APIs use
/// (`family-safety.md` § The account age band, D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeBand {
    /// Under 13 (`"u13"`).
    U13,
    /// 13–15 (`"13-15"`).
    Teen13To15,
    /// 16–17 (`"16-17"`).
    Teen16To17,
    /// 18+ (`"18+"`) — the by-construction value of every account with no
    /// guardianship link.
    Adult,
}

impl AgeBand {
    /// Every band, oldest last — the order an admission picker offers them.
    pub const ORDER: [Self; 4] = [Self::U13, Self::Teen13To15, Self::Teen16To17, Self::Adult];

    /// The single typed parse of the wire token. `None` for anything else —
    /// the nest refuses an admission carrying an unknown band (validate at
    /// write); a *reader* that meets one renders nothing (the band drives no
    /// enforcement, so there is no fail-closed arm to pick).
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "u13" => Some(Self::U13),
            "13-15" => Some(Self::Teen13To15),
            "16-17" => Some(Self::Teen16To17),
            "18+" => Some(Self::Adult),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::U13 => "u13",
            Self::Teen13To15 => "13-15",
            Self::Teen16To17 => "16-17",
            Self::Adult => "18+",
        }
    }

    /// `true` for every band below 18+ — the arm the store-says-minor refusal
    /// keys on (`public-mode.md` § Age at registration: a self-service path
    /// that would create an *unsupervised* account refuses a minor claim with
    /// a typed error pointing at guardian-mediated admission).
    pub fn is_minor(self) -> bool {
        !matches!(self, Self::Adult)
    }

    /// The band a **store age range** lands in — the ONE fold both mobile
    /// store signals feed (Play Age Signals' `ageLower`/`ageUpper`, iOS
    /// Declared Age Range's bounds), so no app hand-rolls it
    /// (`family-safety.md` § The account age band, D3). The store bands are
    /// D2's bands, so one bound names the band: the lower bound when the
    /// store shares it, else the upper (Apple's youngest band is open at the
    /// bottom, Google's oldest is open at the top). A range that straddles
    /// bands folds to the band of its **lower** bound — the youngest the user
    /// could be — because the claim's whole job is corroborating a possible
    /// minor. `None` when the store shared no bound at all (declined, not
    /// eligible, or a verified adult sharing no range).
    pub fn from_age_range(lower: Option<u32>, upper: Option<u32>) -> Option<Self> {
        Some(match lower.or(upper)? {
            0..=12 => Self::U13,
            13..=15 => Self::Teen13To15,
            16..=17 => Self::Teen16To17,
            _ => Self::Adult,
        })
    }
}

/// How a band was established (`family-safety.md` § The account age band, D5).
/// Admission policy and audits key on provenance; **enforcement never does** —
/// the band's value, not its pedigree, drives the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeBandProvenance {
    /// Verified Apple App Attest claim (`"attested-ios"`).
    AttestedIos,
    /// Verified Google Play Integrity claim (`"attested-android"`) — opened
    /// and checked nest-locally against the app's pinned Play Console
    /// response keys (plumbing ruled 2026-08-24, `family-safety.md` § The
    /// account age band). Never recorded while those keys are unset: a nest
    /// that cannot check an android attestation ignores it and handles the
    /// claim as declared-only (provenance [`Self::None`]).
    AttestedAndroid,
    /// The admitting guardian chose the band (`"guardian-asserted"`).
    GuardianAsserted,
    /// No claim was made (`"none"`) — the provenance of the by-construction
    /// `18+` on an open-mode self-registration.
    None,
}

impl AgeBandProvenance {
    /// The single typed parse of the wire token; `None` for anything else.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "attested-ios" => Some(Self::AttestedIos),
            "attested-android" => Some(Self::AttestedAndroid),
            "guardian-asserted" => Some(Self::GuardianAsserted),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AttestedIos => "attested-ios",
            Self::AttestedAndroid => "attested-android",
            Self::GuardianAsserted => "guardian-asserted",
            Self::None => "none",
        }
    }
}

// ── Display: the one place the band vocabulary meets the i18n keys ───────────
//
// Every app renders a band through these (`family-safety.md` § App surface →
// *Age-band surfaces*: "the labels are shared Rust, once — seven apps must not
// spell '13–15 · verified on Android' seven ways"). The keys live under
// `family.age_band.*` in `i18n/strings/en.yaml`. The composed lines carry the
// band/provenance labels as **nested keys** in their arguments, so a renderer
// resolves them with [`LocalizedText::resolve_nested`], never plain `resolve`.

impl AgeBand {
    /// The i18n key of the band's display label (`family.age_band.*`).
    pub fn label_key(self) -> &'static str {
        match self {
            Self::U13 => "family.age_band.u13",
            Self::Teen13To15 => "family.age_band.teen_13_15",
            Self::Teen16To17 => "family.age_band.teen_16_17",
            Self::Adult => "family.age_band.adult",
        }
    }
}

impl AgeBandProvenance {
    /// The i18n key of the provenance's display label (`family.age_band.provenance_*`).
    pub fn label_key(self) -> &'static str {
        match self {
            Self::AttestedIos => "family.age_band.provenance_attested_ios",
            Self::AttestedAndroid => "family.age_band.provenance_attested_android",
            Self::GuardianAsserted => "family.age_band.provenance_guardian_asserted",
            Self::None => "family.age_band.provenance_none",
        }
    }
}

/// The band's display label for a wire token — `None` for a token this
/// client cannot name (the readers' rule above: render nothing, never a
/// placeholder, because the band drives no enforcement).
pub fn age_band_label(band: &str) -> Option<LocalizedText> {
    AgeBand::from_wire(band).map(|b| LocalizedText::key(b.label_key()))
}

/// The provenance's display label for a wire token; `None` when unnamed.
pub fn age_band_provenance_label(provenance: &str) -> Option<LocalizedText> {
    AgeBandProvenance::from_wire(provenance).map(|p| LocalizedText::key(p.label_key()))
}

/// One "band + how it was established" line for the two family-page
/// readouts, keyed by `own`: the guardian's per-ward `family-ward-age-band`
/// row (`false`, "Age band: 13–15 · set by guardian") or the ward's own
/// `family-age-band-summary` (`true`, "Your age band: 13–15 · set by
/// guardian, set at admission"). `None` when the band is unnamed — the
/// surfaces are **absent, never placeholdered**, for a band-less admission; a
/// provenance this client cannot name (a newer nest) falls back to the
/// band-only form rather than mislabelling it. Resolve with
/// [`LocalizedText::resolve_nested`].
pub fn age_band_line(band: &str, provenance: &str, own: bool) -> Option<LocalizedText> {
    let band = AgeBand::from_wire(band)?;
    let key = |with_provenance: bool| match (own, with_provenance) {
        (false, true) => "family.age_band.ward_line",
        (false, false) => "family.age_band.ward_line_band_only",
        (true, true) => "family.age_band.own_summary",
        (true, false) => "family.age_band.own_summary_band_only",
    };
    Some(match AgeBandProvenance::from_wire(provenance) {
        Some(p) => LocalizedText::key_args(
            key(true),
            [("band", band.label_key()), ("provenance", p.label_key())],
        ),
        None => LocalizedText::key_arg(key(false), "band", band.label_key()),
    })
}

/// The admin's `invite-request-row-age-claim` text — **total**, because on
/// this row absence IS the signal (`family-safety.md` § The account age band,
/// D6): a request carrying no nameable claim reads "No app age verification";
/// one carrying a claim reads "Age 13–15 · verified on Android" / "… declared,
/// not verified". An unnamed provenance is rendered as declared-only — the
/// conservative reading for an admitting adult. Resolve with
/// [`LocalizedText::resolve_nested`].
pub fn age_claim_label(band: Option<&str>, provenance: Option<&str>) -> LocalizedText {
    match band.and_then(AgeBand::from_wire) {
        None => LocalizedText::key("family.age_band.claim_none"),
        Some(b) => {
            let p = provenance
                .and_then(AgeBandProvenance::from_wire)
                .unwrap_or(AgeBandProvenance::None);
            LocalizedText::key_args(
                "family.age_band.claim_line",
                [("band", b.label_key()), ("provenance", p.label_key())],
            )
        }
    }
}

/// The onboarding `invite-request-age-notice` text — what the nest will
/// record, shown before submit/redeem so the user knows what the application
/// carries (D3; `family-safety.md` § App surface → *Age-band surfaces*).
/// `attested_platform` is the platform of the attestation the machine **will
/// put on the wire** ([`AGE_ATTESTATION_PLATFORM_ANDROID`] /
/// [`AGE_ATTESTATION_PLATFORM_IOS`]) — never merely one the glue attached,
/// since the machine strips an attestation the addressed nest did not list.
/// `None` (or an unknown platform) is a declared-only claim, whose wording
/// blames nobody: the cause may be the device, the store or the nest. `None`
/// when the band is unnamed — the notice is absent when the store shared
/// nothing. Resolve with [`LocalizedText::resolve_nested`].
pub fn age_notice(band: &str, attested_platform: Option<&str>) -> Option<LocalizedText> {
    let band = AgeBand::from_wire(band)?;
    let store = match attested_platform {
        Some(AGE_ATTESTATION_PLATFORM_ANDROID) => Some((
            "family.age_band.store_android",
            "family.age_band.verifier_android",
        )),
        Some(AGE_ATTESTATION_PLATFORM_IOS) => {
            Some(("family.age_band.store_ios", "family.age_band.verifier_ios"))
        }
        _ => None,
    };
    Some(match store {
        Some((store, verifier)) => LocalizedText::key_args(
            "family.age_band.notice_attested",
            [
                ("band", band.label_key()),
                ("store", store),
                ("verifier", verifier),
            ],
        ),
        None => LocalizedText::key_arg("family.age_band.notice_declared", "band", band.label_key()),
    })
}

/// The registering app's age claim, riding `fauna.account.register` and
/// `fauna.account.invite_request.submit` as an additive optional field.
///
/// Without [`Self::attestation`] the claim is **declared** — the honest app
/// relaying what the platform told it (D3's corroboration posture). A declared
/// claim never mints a band row and never earns an `attested-*` provenance;
/// its two consumers are the store-says-minor refusal of unsupervised
/// self-service admission and the absence-as-signal column on the admin's
/// request row. With the attestation, the nest verifies the platform's own
/// signature over [`age_claim_signed_message`] before believing anything
/// (`family-safety.md` § The account age band — the verifying signature is
/// Apple's/Google's, never a key embedded in the Fauna app).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgeClaim {
    /// The claimed band's wire token ([`AgeBand::from_wire`]); an admission
    /// carrying a token the nest cannot name is refused
    /// (`fauna.account.invalid_request`).
    pub band: String,
    /// The platform attestation hardening the claim; absent = declared-only.
    #[serde(default)]
    pub attestation: Option<AgeAttestation>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Platform values for [`AgeAttestation::platform`].
pub const AGE_ATTESTATION_PLATFORM_IOS: &str = "ios";
/// See [`AGE_ATTESTATION_PLATFORM_IOS`].
pub const AGE_ATTESTATION_PLATFORM_ANDROID: &str = "android";

/// The genuine Fauna iOS app's App ID — `<Apple team id>.social.fauna.fauna`
/// ([`fauna_core::platform_ids::APPLE_IOS_APP_ID`], the owner of the team id
/// and bundle id). It is the `application_id` of [`age_claim_signed_message`]
/// on the iOS arm and the RP-ID the App Attest `authData` hashes.
///
/// **The arming point of the nest's iOS verifier.** While this is `None` the
/// nest holds no iOS verifier: it leaves `ios` out of
/// [`AgeNonceReply::attestation_platforms`] and ignores an iOS attestation
/// unread, handling the claim as declared-only (`family-safety.md` § The
/// account age band → *An attestation the nest cannot check*) — never an
/// App-ID check that silently checks nothing. The iOS app reads the same
/// constant (`fauna_ffi::ios_age_attestation_app_id`) only for the signed
/// message's `application_id`; whether to attach is the addressed nest's
/// list, never this app build's arming.
pub const FAUNA_IOS_APP_ID: Option<&str> = Some(fauna_core::platform_ids::APPLE_IOS_APP_ID);

/// A platform attestation over `{nest-issued nonce, band, application id,
/// actor}` — the hardening D5 adopted. The nest verifies it **nest-locally**
/// on both platforms — the App Attest chain against Apple's published root,
/// the Play Integrity verdict under the app's pinned Play Console response
/// keys (Google's signature is the one that matters) — and consumes the nonce
/// single-use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgeAttestation {
    /// `"ios"` | `"android"` — which platform seam verifies this
    /// ([`AGE_ATTESTATION_PLATFORM_IOS`] / [`AGE_ATTESTATION_PLATFORM_ANDROID`]).
    pub platform: String,
    /// 64-char hex of the 32-byte nonce `fauna.account.age_nonce` minted.
    pub nonce: String,
    /// iOS: 64-char hex of the App Attest key id (the SHA-256 of the attested
    /// public key, as `DCAppAttestService.generateKey` names it). Android:
    /// unused (Play Integrity has no per-key identity) — the app sends `""`.
    pub key_id: String,
    /// The raw platform artifact — iOS: the App Attest **attestation object**
    /// (CBOR); android: the Play Integrity **classic-request verdict token**
    /// (the compact-JWE string's ASCII bytes, as `IntegrityTokenResponse
    /// .token()` returns it), requested with the nonce
    /// [`age_claim_signed_message`] documents.
    pub attestation_object: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The exact bytes an attested age claim commits to: the length-prefixed
/// element list `lp(ACCOUNT_AGE_CLAIM_V1) ‖ lp(nonce) ‖ lp(band) ‖
/// lp(application_id) ‖ lp(actor_id)`
/// ([`crate::sig_domain::domain_separated_length_prefixed`]).
///
/// **Single source of the attested-payload contract** — the iOS app hashes
/// this (SHA-256) into the `clientDataHash` it hands
/// `DCAppAttestService.attestKey`; the android app passes the **same SHA-256,
/// base64url without padding**, as the Play Integrity *classic* request's
/// nonce (`IntegrityTokenRequest.setNonce` — Google echoes it verbatim in the
/// verdict's `requestDetails.nonce`; a digest on both platforms, so neither
/// Apple's nor Google's logs ever see the band or the actor id); and the nest
/// verifier recomputes it on both arms, so the three cannot drift (the
/// [`crate::account::register_signed_message`] pattern). The one digest both
/// apps take is `OnboardingMachine::age_claim_digest`; this crate stays
/// crypto-free.
/// `actor_id` is in the list on purpose: `register_signed_message` covers no
/// new field, so without it an intercepted attestation + live nonce could be
/// replayed onto a different registering key; binding the actor here closes
/// that seam at the payload the platform signs over. Crypto-free byte
/// assembly, as with every builder here.
pub fn age_claim_signed_message(
    nonce: &[u8; 32],
    band: &str,
    application_id: &str,
    actor_id: &[u8; 32],
) -> Vec<u8> {
    crate::sig_domain::domain_separated_length_prefixed(
        crate::sig_domain::ACCOUNT_AGE_CLAIM_V1,
        &[nonce, band.as_bytes(), application_id.as_bytes(), actor_id],
    )
}

// ── fauna.account.age_nonce (pre-identity) ───────────────────────────────────

/// `fauna.account.age_nonce` — mint a short-lived, single-use nonce the mobile
/// app feeds into its platform attestation ([`age_claim_signed_message`]).
/// Anonymous pre-identity kind (the registering actor has no account yet);
/// throttled like every anonymous kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AgeNonceRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply to `fauna.account.age_nonce`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AgeNonceReply {
    /// 64-char hex of the 32-byte nonce. Single-use; consumed by the
    /// registration or invite-request submit that presents it.
    pub nonce: String,
    /// Seconds the nonce stays redeemable (the app's retry budget).
    pub expires_in_secs: u64,
    /// The [`AgeAttestation::platform`] tokens ([`AGE_ATTESTATION_PLATFORM_IOS`]
    /// / [`AGE_ATTESTATION_PLATFORM_ANDROID`]) this nest holds a verifier for —
    /// the platforms whose attestation it can check (`family-safety.md` § The
    /// account age band → *An attestation the nest cannot check*). The app
    /// attaches an attestation only for a listed platform and otherwise sends
    /// the claim declared-only. `#[serde(default)]`: an unarmed nest
    /// sends an empty list, `vec![]` — **verifies nothing** (the
    /// `NestInfoReply.capabilities` skew precedent).
    #[serde(default)]
    pub attestation_platforms: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn a_store_age_range_folds_to_the_band_of_its_youngest_bound() {
        // Google's closed bands and open-topped adult band.
        assert_eq!(
            AgeBand::from_age_range(Some(13), Some(15)),
            Some(AgeBand::Teen13To15)
        );
        assert_eq!(
            AgeBand::from_age_range(Some(16), Some(17)),
            Some(AgeBand::Teen16To17)
        );
        assert_eq!(
            AgeBand::from_age_range(Some(18), None),
            Some(AgeBand::Adult)
        );
        assert_eq!(
            AgeBand::from_age_range(Some(0), Some(12)),
            Some(AgeBand::U13)
        );
        // Apple's open-bottomed youngest band.
        assert_eq!(AgeBand::from_age_range(None, Some(12)), Some(AgeBand::U13));
        // A straddling range folds young — the claim corroborates a possible minor.
        assert_eq!(
            AgeBand::from_age_range(Some(13), Some(17)),
            Some(AgeBand::Teen13To15)
        );
        // Nothing shared → no claim.
        assert_eq!(AgeBand::from_age_range(None, None), None);
    }

    #[test]
    fn band_wire_tokens_round_trip() {
        for band in AgeBand::ORDER {
            assert_eq!(AgeBand::from_wire(band.as_str()), Some(band));
        }
        assert_eq!(AgeBand::from_wire("adult"), None);
        assert_eq!(AgeBand::from_wire(""), None);
        for provenance in [
            AgeBandProvenance::AttestedIos,
            AgeBandProvenance::AttestedAndroid,
            AgeBandProvenance::GuardianAsserted,
            AgeBandProvenance::None,
        ] {
            assert_eq!(
                AgeBandProvenance::from_wire(provenance.as_str()),
                Some(provenance)
            );
        }
        assert_eq!(AgeBandProvenance::from_wire("attested"), None);
    }

    /// The display helpers never invent a band: an unnamed token renders
    /// nothing on the two family readouts (absent, never placeholdered), while
    /// the admin's claim row is total — absence is its signal.
    #[test]
    fn display_helpers_follow_the_readers_rule() {
        assert_eq!(
            age_band_label("13-15").map(|t| t.key),
            Some("family.age_band.teen_13_15".into())
        );
        assert_eq!(age_band_label("teen"), None);
        assert_eq!(age_band_provenance_label("bogus"), None);

        let ward = age_band_line("13-15", "guardian-asserted", false).expect("named band");
        assert_eq!(ward.key, "family.age_band.ward_line");
        assert_eq!(ward.args["band"], "family.age_band.teen_13_15");
        assert_eq!(
            ward.args["provenance"],
            "family.age_band.provenance_guardian_asserted"
        );
        let own = age_band_line("u13", "attested-android", true).expect("named band");
        assert_eq!(own.key, "family.age_band.own_summary");
        assert_eq!(
            own.args["provenance"],
            "family.age_band.provenance_attested_android"
        );
        // A newer nest's provenance token: the band still renders, band-only.
        let newer = age_band_line("16-17", "attested-quantum", false).expect("named band");
        assert_eq!(newer.key, "family.age_band.ward_line_band_only");
        assert!(!newer.args.contains_key("provenance"));
        assert_eq!(age_band_line("nope", "guardian-asserted", false), None);

        assert_eq!(
            age_claim_label(None, None).key,
            "family.age_band.claim_none"
        );
        assert_eq!(
            age_claim_label(Some("42"), Some("none")).key,
            "family.age_band.claim_none"
        );
        let claim = age_claim_label(Some("13-15"), Some("attested-android"));
        assert_eq!(claim.key, "family.age_band.claim_line");
        assert_eq!(
            claim.args["provenance"],
            "family.age_band.provenance_attested_android"
        );
        // No provenance at all on a claim = declared-only, the conservative reading.
        assert_eq!(
            age_claim_label(Some("13-15"), None).args["provenance"],
            "family.age_band.provenance_none"
        );
    }

    /// The onboarding notice names the store only for an attested claim; a
    /// declared-only claim cannot know which platform declined to verify.
    #[test]
    fn age_notice_names_the_store_only_when_attested() {
        let attested = age_notice("13-15", Some(AGE_ATTESTATION_PLATFORM_ANDROID)).expect("named");
        assert_eq!(attested.key, "family.age_band.notice_attested");
        assert_eq!(attested.args["store"], "family.age_band.store_android");
        assert_eq!(
            attested.args["verifier"],
            "family.age_band.verifier_android"
        );
        let ios = age_notice("u13", Some(AGE_ATTESTATION_PLATFORM_IOS)).expect("named");
        assert_eq!(ios.args["verifier"], "family.age_band.verifier_ios");
        let declared = age_notice("13-15", None).expect("named");
        assert_eq!(declared.key, "family.age_band.notice_declared");
        assert!(!declared.args.contains_key("store"));
        assert_eq!(
            age_notice("13-15", Some("sailfish")).map(|t| t.key),
            Some("family.age_band.notice_declared".into())
        );
        assert_eq!(age_notice("kid", Some(AGE_ATTESTATION_PLATFORM_IOS)), None);
    }

    #[test]
    fn minor_arm_covers_every_band_below_adult() {
        assert!(AgeBand::U13.is_minor());
        assert!(AgeBand::Teen13To15.is_minor());
        assert!(AgeBand::Teen16To17.is_minor());
        assert!(!AgeBand::Adult.is_minor());
    }

    /// Source-level pin on the attested-payload builder's tag structure
    /// (rule #8): signer (the iOS app's clientDataHash) and verifier (the
    /// nest) share this builder, so a builder that silently lost its tag or a
    /// prefix would stay green in every behavioural test.
    #[test]
    fn age_claim_signed_message_is_tagged_and_binds_every_element() {
        let nonce = [0x11_u8; 32];
        let actor = [0x22_u8; 32];
        let msg = age_claim_signed_message(&nonce, "u13", "TEAM.social.fauna.fauna", &actor);
        let tag = crate::sig_domain::ACCOUNT_AGE_CLAIM_V1;
        assert_eq!(&msg[..8], &(tag.len() as u64).to_be_bytes());
        assert_eq!(&msg[8..8 + tag.len()], tag);
        // Every element moves the message: band…
        assert_ne!(
            msg,
            age_claim_signed_message(&nonce, "18+", "TEAM.social.fauna.fauna", &actor)
        );
        // …application id…
        assert_ne!(
            msg,
            age_claim_signed_message(&nonce, "u13", "TEAM.social.fauna.other", &actor)
        );
        // …actor (the replay-binding element)…
        assert_ne!(
            msg,
            age_claim_signed_message(&nonce, "u13", "TEAM.social.fauna.fauna", &[0x23; 32])
        );
        // …and the nonce.
        assert_ne!(
            msg,
            age_claim_signed_message(&[0x12; 32], "u13", "TEAM.social.fauna.fauna", &actor)
        );
        // The length-prefixing makes adjacent-element resplits structurally
        // impossible (the register_signed_message item-3 lesson).
        assert_ne!(
            age_claim_signed_message(&nonce, "u13", "X.app", &actor),
            age_claim_signed_message(&nonce, "u13X", ".app", &actor),
        );
    }

    #[test]
    fn age_claim_round_trips_with_and_without_attestation() {
        let declared = AgeClaim {
            band: "13-15".into(),
            attestation: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&declared).expect("encode");
        assert_eq!(decode::<AgeClaim>(&bytes).expect("decode"), declared);

        let attested = AgeClaim {
            band: "16-17".into(),
            attestation: Some(AgeAttestation {
                platform: AGE_ATTESTATION_PLATFORM_IOS.into(),
                nonce: "aa".repeat(32),
                key_id: "bb".repeat(32),
                attestation_object: ByteBuf::from(vec![0xd9, 0x01, 0x02]),
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&attested).expect("encode");
        assert_eq!(decode::<AgeClaim>(&bytes).expect("decode"), attested);
    }

    #[test]
    fn age_nonce_reply_round_trips() {
        let reply = AgeNonceReply {
            nonce: "cc".repeat(32),
            expires_in_secs: 300,
            attestation_platforms: vec![AGE_ATTESTATION_PLATFORM_IOS.into()],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).expect("encode");
        assert_eq!(decode::<AgeNonceReply>(&bytes).expect("decode"), reply);
    }

    /// Once armed, the App ID must be the team-prefixed shape App Attest's
    /// RP-ID uses — a bare bundle id would make every genuine attestation fail
    /// the nest's RP-ID hash check.
    #[test]
    fn ios_app_id_when_armed_is_team_prefixed_bundle_id() {
        if let Some(app_id) = FAUNA_IOS_APP_ID {
            let (team, bundle) = app_id.split_once('.').expect("<team>.<bundle>");
            assert_eq!(bundle, "social.fauna.fauna");
            assert_eq!(team.len(), 10, "Apple team ids are 10 characters");
            assert!(
                team.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            );
        }
    }
}
