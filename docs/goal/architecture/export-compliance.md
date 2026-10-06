# Export compliance — the encryption posture, across every store

Owns: export-compliance
Status: ratified 2026-08-23 — posture chosen by the organization's representative; the effective-date stamp lands when the source publication it rides on goes live (§ Implementation status today)
Authority: owns the export-control posture for Fauna's encryption — the US EAR analysis, the single classification story every store declaration answers from (Google Play's export attestation, Apple's `ITSAppUsesNonExemptEncryption` key and App Store Connect questionnaire, the Microsoft Store developer-agreement clause), and any resulting filing duties. Neighbours that do **not** own this: [`release-integrity.md`](release-integrity.md) owns shipped-code trust; [`installers/android.md`](installers/android.md) and [`installers/windows.md`](installers/windows.md) own store-account and packaging posture; [`security.md`](security.md) owns the runtime crypto discipline the posture describes but never constrains.

> **Audience:** anyone answering a store's encryption/export question, touching
> a store listing's compliance declarations, or setting `ITSAppUsesNonExemptEncryption`.
> **This is a research record and an organizational decision, not legal advice.**
> Every regulatory claim below was verified against the cited primary text on
> 2026-08-23; re-verify against the current CFR before relying on it in a new way.

## Why Fauna is in scope at all

Fauna is developed in Norway by a Norwegian non-profit, but distributing through the
major app stores places copies of the app on US infrastructure, and worldwide
distribution from there is an export *from the United States* (15 CFR § 734.3(a):
items in the US are subject to the EAR wherever they came from). That is why every
store makes the developer attest to US export compliance regardless of nationality
or location. Fauna's own end-to-end encryption (MLS, sealed-at-rest content) is
squarely encryption software in EAR terms — ECCN 5D002 territory, above every
de-control threshold (256-bit symmetric) — so the question cannot be waved off as
"encryption incidental to the product".

## The ratified posture: publicly available source (decision 2026-08-23)

Fauna's posture is the **publicly-available route**:

- **Source code:** publicly available encryption source code classified under
  ECCN 5D002 is **not subject to the EAR** — 15 CFR § 742.15(b)(1)
  ([current text](https://www.law.cornell.edu/cfr/text/15/742.15)). The email
  notification to BIS/NSA survives only in § 742.15(b)(2) and **only for
  "non-standard cryptography"**. Fauna's cryptography is entirely standard,
  published algorithms — MLS (RFC 9420), ChaCha20-Poly1305, AES-GCM,
  Ed25519/X25519, HPKE, Argon2 — so **no notification is owed**. (Pre-2021 the
  notification was general; the 2021 BIS rule narrowed it. Some BIS guidance pages
  still print the old sentence — the CFR text governs.)
- **Binaries:** publicly available encryption *object code* whose corresponding
  source code meets § 742.15(b) is likewise **not subject to the EAR** —
  15 CFR § 734.7(b) ([current text](https://www.law.cornell.edu/cfr/text/15/734.7)).
  Fauna's apps are distributed free of charge, i.e. publicly available, so once the
  source is public the store binaries drop out of EAR scope with them.

Under this posture there are **no BIS filings, notifications, or annual reports**.
The store attestations are all answered by the same sentence: *the app's encryption
source code is publicly available and implements only standard cryptography, so the
software is not subject to the EAR (15 CFR §§ 742.15(b)(1), 734.7(b)).*

### The two conditions that perfect it

1. **The source must actually be public.** The posture takes effect the moment the
   public repository is live, and the effective date is recorded in
   § Implementation status today. Until that stamp exists the posture is chosen but
   not yet perfected. (Publication was imminent — days, not months — when the
   posture was ratified; that closeness is why no interim mass-market
   self-classification was filed.)
2. **The shipped binaries' encryption source must correspond to what is published.**
   § 734.7(b) releases object code only when its *corresponding* source meets
   § 742.15(b). Release discipline therefore includes: every crypto-relevant
   component of a shipped app build is present in the public tree. Verifying that
   correspondence — in particular for any vendored or patched dependency in the
   crypto path — is part of the publication and release process (tracked
   internally), and any gap found is a posture gap, not a cosmetic one.

## What each store form answers

- **Google Play** (export attestation at app creation): answered by the posture
  sentence above. The organization's representative checked the attestation on
  2026-08-22, ahead of perfection, on the strength of the imminent publication.
- **Apple** (`ITSAppUsesNonExemptEncryption` + App Store Connect questionnaire):
  Fauna uses its own encryption, not merely the OS's TLS/auth facilities, so the
  expected key value is `YES` with the compliance answer given from this posture
  (publicly available, standard cryptography, not subject to the EAR). ⚠
  Apple's `ITSAppUsesNonExemptEncryption` = "exempt" test is a **different legal
  question** than the EAR public-source-code exemption above — the key tracks
  whether the app uses its own crypto beyond OS-provided facilities, not the
  BIS/EAR classification. **The key is declared on the two app-target
  Info.plists only** (`Fauna-macOS/Resources/`, `Fauna-iOS/Resources/`), not
  the three appex targets (NSE, FileProvider, FileProviderUI) — App Store
  Connect reads the containing app's Info.plist, not an embedded extension's;
  pinned by `tests/e2e-unified/tests/test_apple_identifier_pins.py`. **The
  actual App Store Connect questionnaire answer remains a `NEEDS FROM USER:`
  item** — like the Play checkbox, it is a legal attestation only the org's
  representative may give, and it requires an authenticated App Store Connect
  session no dev-machine session has (verified 2026-08-23:
  [Apple's export-regulations page](https://developer.apple.com/documentation/security/complying-with-encryption-export-regulations)
  and its sibling `/documentation/appstoreconnectapi/*` pages render
  client-side and return no usable content to automated fetching — three URLs
  tried, all title-only; the static App Store Connect **Help** pages under
  `developer.apple.com/help/app-store-connect/...` do render and were used
  instead for the documentation-requirements table below).
  **New finding (2026-08-23), NOT covered by the EAR posture above:** Apple's
  own export-compliance documentation table is independent of the EAR
  public-source exemption — it asks what documentation the *store submission*
  needs, not what the *export itself* needs:
  | App's crypto | ASC documentation required |
  |---|---|
  | Limited to Apple OS facilities only | None |
  | Industry-standard algorithm not in Apple OS (Fauna's case — MLS, ChaCha20-Poly1305, Ed25519, HPKE, Argon2, all IETF/ISO-published) | **French encryption declaration**, but only if distributing on the French App Store |
  | Proprietary/non-standard algorithm | CCATS + French declaration |

  So Fauna's ratified "no BIS filing" posture stands, but a French ANSSI
  encryption-import declaration would be owed if Fauna distributed on the
  French App Store — a separate French-law obligation, orthogonal to the US
  EAR analysis this doc otherwise owns. **Decided 2026-08-23 by the org's
  representative: France is excluded from App Store distribution for now.**
  The questionnaire's French-store question is answered **No**, the app's
  Pricing and Availability territory list must exclude France so that answer
  stays true, and no ANSSI filing is owed today. Adding France later is a
  deliberate, ordered act — file the ANSSI declaration first, upload the
  acknowledgment, update the ASC declaration, then add the territory
.
  **The decided questionnaire answers (2026-08-23):** cryptography — **Yes**;
  algorithm type — **standard algorithms instead of, or in addition to, the
  OS's** (never "proprietary" or "both": MLS/RFC 9420, ChaCha20-Poly1305,
  AES-GCM, Ed25519/X25519, HPKE, Argon2 are all IETF/IRTF/ISO-published);
  French App Store — **No**. If an older-style form asks whether the app
  qualifies for a Category 5 Part 2 exemption, the posture's answer is
  **Yes** (publicly available source, standard cryptography, not subject to
  the EAR). Giving these answers in an authenticated ASC session remains the
  representative's own act — the guided walkthrough is tracked
  internally.
- **Microsoft Store:** Partner Center asks no encryption questionnaire; the App
  Developer Agreement makes export compliance the developer's responsibility
  generally. Same posture sentence, kept on file.

## Alternatives considered (held, not chosen)

- **Mass-market self-classification — kept executable as the fallback if
  publication were ever reversed.** A free app meets the "retail" criterion of
  Note 3 to Category 5 Part 2 (BIS FAQ: free-of-charge distribution counts). Under
  15 CFR § 740.17(b)(1) the app would be self-classified **ECCN 5D992.c** — an
  internal dated determination, no BIS pre-approval — shipping worldwide NLR except
  embargoed destinations (which the stores geo-fence anyway). Recurring duty: the
  annual self-classification report of § 740.17(e)(3) — a CSV per Supplement No. 8
  to Part 742 emailed to `crypt-supp8@bis.doc.gov` and `enc@nsa.gov` by
  **February 1** for the prior calendar year, a "nothing changed" email sufficing in
  unchanged years. Note: ordinary mass-market *applications* do owe this report;
  the 2021 rule dropped it only for stand-alone dev kits.
- **Stripping the encryption from a store build — rejected as a category error.**
  Standard-crypto mass-market E2EE apps are precisely what the de-controls above
  exist for, so there is nothing that needs removing; and encryption is Fauna's
  substrate, not a feature — a stripped client would break the sealed-at-rest
  invariant ([`../principles.md`](../principles.md) § The user always controls
  their data) and client↔nest wire compatibility
  ([`version-compatibility.md`](version-compatibility.md)).

**Norway:** Norwegian export control mirrors the same Wassenaar dual-use lists with
parallel public-domain and mass-market de-controls; no Norwegian licence is expected
for a free public app with published source. Flagged for counsel review rather than
relied on — nothing in the ratified posture depends on it.

## Decision record

- **2026-08-22** — Google Play's export attestation checked by the organization's
  representative to unblock app creation, with the posture question opened for
  ratification.
- **2026-08-23** — posture ratified by the organization's representative:
  publicly-available route, no interim mass-market filing, on the basis of imminent
  source publication. Research verified against the CFR text the same day.
- **2026-08-23** — French-territory decision by the organization's representative:
  France **excluded** from App Store distribution for now, so the ASC French-store
  question is answered No and no ANSSI declaration is owed until France is
  deliberately added (declaration first, then territory). The same sitting decided
  the ASC questionnaire answers (§ What each store form answers) — to be given by
  the representative in an authenticated ASC session, guided walkthrough
  tracked.

## Implementation status today

- **Effective-date stamp: PENDING.** The public repository was not yet live when
  this doc was ratified; the session that observes the publication go live stamps
  the date here, turning the posture from chosen to perfected.
- **Binary-correspondence check: PENDING** — to be run at the first release after
  publication (condition 2 above; the vendored-dependency review is tracked
  internally). For the macOS `.dmg` the check is satisfied by construction: it is
  built from the public tree by the public repository's own workflow and attested to
  it (decided 2026-08-25 — `installers/macos.md` § Build Pipeline → *The `.dmg`
  release pipeline*), so its crypto source is exactly the published source.
- **Apple leg — Info.plist half wired 2026-08-23:** `ITSAppUsesNonExemptEncryption`
  = `true` is set on both app-target Info.plists (`Fauna-macOS/Resources/`,
  `Fauna-iOS/Resources/`), citing this doc, and pinned by
  `tests/e2e-unified/tests/test_apple_identifier_pins.py`. **Decided
  2026-08-23:** France is excluded from distribution for now, so no ANSSI
  declaration is owed (adding France later files the declaration first). **Still open:** giving the
  decided questionnaire answers in an authenticated ASC session
  (`NEEDS FROM USER:` — legal attestation only the org's representative may
  give; the answers themselves are recorded in § What each store form
  answers, and the guided walkthrough is tracked), plus verifying the territory list excludes France in the same
  sitting.
- Play's attestation is checked (2026-08-22, record above); Microsoft requires no
  affirmative act beyond the agreement already accepted.
