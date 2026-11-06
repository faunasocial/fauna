# Export compliance — the encryption posture, across every store

Owns: export-compliance
Status: ratified 2026-08-23 — posture chosen by the organization's representative; the effective-date stamp lands when the source publication it rides on goes live (§ Implementation status today)
Authority: owns the export-control posture for Fauna's encryption — the US EAR analysis, the single classification story every store declaration answers from (Google Play's export attestation, Apple's `ITSAppUsesNonExemptEncryption` key and App Store Connect questionnaire, the Microsoft Store developer-agreement clause), and any resulting filing duties. Neighbours that do **not** own this: [`release-integrity.md`](release-integrity.md) owns shipped-code trust; [`installers/android.md`](installers/android.md), [`installers/ios.md`](installers/ios.md) and [`installers/windows.md`](installers/windows.md) own store-account and packaging posture; [`security.md`](security.md) owns the runtime crypto discipline the posture describes but never constrains.

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
  **the key is `NO`** (corrected 2026-11-06; it read `YES` from 2026-08-23).
  Apple's own rule, in *Complying with Encryption Export Regulations* (read
  2026-11-06 from the representative's saved copy): "Set the value to NO if
  your app … doesn't use encryption, or if it only uses forms of encryption that
  are exempt from export compliance documentation requirements. Otherwise, set
  it to YES." The test is therefore whether App Store Connect requires
  **documents**, not whether the app carries crypto of its own beyond the OS's:
  Fauna does carry its own (standard algorithms only), and App Store Connect's
  encryption questionnaire, answered with the decided answers below, replied
  "Based on your answers, you don't need to upload any documents." With `YES`,
  upload validation demands an `ITSEncryptionExportComplianceCode`, which Apple
  issues only after reviewing uploaded documents — so a `YES` with nothing to
  upload can never pass (measured 2026-11-01, [`installers/ios.md`](installers/ios.md)
  § Upload mechanics). ⚠ The key remains a **different legal question** than
  the EAR public-source-code exemption above: it answers Apple's documentation
  requirement, not the BIS/EAR classification. Apple's page also notes that an
  app using exempt encryption "might alternatively be required to submit a
  year-end self-classification report"; that report belongs to the mass-market
  route (§ Alternatives considered), which is held, not chosen — under the
  ratified publicly-available posture the software is not subject to the EAR
  and no report is filed. **The key is declared on the app-target
  Info.plists only** (`Fauna-macOS/Resources/`, `Fauna-iOS/Resources/`), not
  the three appex targets (NSE, FileProvider, FileProviderUI) — App Store
  Connect reads the containing app's Info.plist, not an embedded extension's;
  pinned by `tests/e2e-unified/tests/test_apple_identifier_pins.py`. **The
  App Store Connect questionnaire answer** — like the Play checkbox — is a
  legal attestation only the org's representative may give, in an
  authenticated App Store Connect session no dev-machine session has; Apple's
  developer-documentation pages cannot be read from a dev machine either (a
  saved copy the representative hands over can; verified 2026-08-23:
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
  the EAR). **Given 2026-11-06** by the representative in an authenticated ASC
  session (App Information → App Encryption Documentation, three steps: the
  app's purpose, the algorithm type, France), App Store Connect answering that
  no documents are needed — the record is § Decision record.
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
for a free public app with published source. Recorded as the organisation's own
reading, not relied on — nothing in the ratified posture depends on it, and review
by a lawyer is never a gate (`public-texts.md` § Rulings, 20).

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
- **2026-11-06** — the representative answered App Store Connect's encryption
  questionnaire on the iOS app record (App Information → App Encryption
  Documentation), with the decided answers: algorithm type — standard
  algorithms instead of, or in addition to, the OS's; France — No. The form's
  first step asks for the app's purpose (300 characters at most); the text
  entered, verbatim: "Fauna Social is a free social networking and messaging
  app. People post, message and share content through a server that they, or
  someone they trust, maintain through the app. Its complete source code is
  publicly available at https://github.com/faunasocial/fauna." The
  representative waived the public-texts round for this one filing, which only
  Apple's reviewers read, so the iOS app could make the 0.1.6 train. App Store
  Connect replied "Based on your answers, you don't need to upload any
  documents", so no compliance code exists; Apple's own key rule (quoted in
  § What each store form answers) then makes `ITSAppUsesNonExemptEncryption`
  `NO`, and the representative ruled the flip the same day, correcting the
  2026-08-23 reading that the key asked whether the app carries its own crypto.

## Implementation status today

- **Effective-date stamp: 2026-10-06.** The public repository went live that day,
  so the posture is perfected (condition 1) from 2026-10-06.
- **Binary-correspondence check: HOLDING by construction (recorded 2026-10-14)** —
  condition 2: every distributed binary is built from a recorded commit of the public
  repository (`release-integrity.md` § A store upload is built from a recorded public
  commit, ruled 2026-10-07; first confirmed by the Play 0.1.3 internal-testing release
  of 2026-10-09), so its crypto source is exactly the published source. The macOS
  `.dmg` and the nest image are public-built by their own workflows (decided
  2026-08-25 — `installers/macos.md` § Build Pipeline → *The `.dmg` release
  pipeline*). No crypto-relevant source in any shipped binary is unpublished; the
  vendored-dependency review is tracked internally.
- **Apple leg — done 2026-11-06.** `ITSAppUsesNonExemptEncryption` = `false` on
  the app-target Info.plists (`Fauna-macOS/Resources/Info.plist`,
  `Fauna-iOS/Resources/Info.plist`, and the kids target's
  `Fauna-iOS/Resources/Info-Kids.plist`), citing this doc, and pinned by
  `tests/e2e-unified/tests/test_apple_identifier_pins.py`; it was `true` from
  2026-08-23 to 2026-11-06, which is why the 0.1.5 iOS upload failed
  validation. The questionnaire is answered (§ Decision record, 2026-11-06), and
  no `ITSEncryptionExportComplianceCode` is owed. **Decided
  2026-08-23:** France is excluded from distribution for now, so no ANSSI
  declaration is owed (adding France later files the declaration first); the App Store Connect territory list
  excludes France — **read back 2026-11-06** by the representative (Pricing and
  Availability → App Availability: every country or region but France, 174, plus
  future App Store countries or regions; a France added later by Apple's list
  would not be auto-selected, since France already exists and is deselected).
  **App Store Connect's own record is a read, not an ask:** its API answers the
  app's encryption declarations, each with its state, answers and `codeValue`,
  to the Developer-role App Store Connect API key the macOS development VM holds
  — `just ios-export-compliance` prints them (the per-app relationship path
  Apple once documented is gone; the top-level `appEncryptionDeclarations`
  collection filtered by app is the read). Read 2026-11-06 after the
  questionnaire was answered: **zero** declarations — the answers needed no
  documents, so App Store Connect created no declaration and no code, which is
  what the `false` key states. The live witness `installers/ios.md` § Upload
  mechanics names holds the Info.plist to that record in both directions.
- Play's attestation is checked (2026-08-22, record above); Microsoft requires no
  affirmative act beyond the agreement already accepted.
