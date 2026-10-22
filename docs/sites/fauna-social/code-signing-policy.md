---
title: Fauna Social — Code signing policy
description: How Fauna's downloads are signed, who may approve a signing, and how to check a signature — the code signing policy of the Fauna Social association.
---

# Code signing policy

*Illustration: badger*

Every Fauna download is built by a public workflow from the published source,
and the Windows installer is signed so that Windows can show who stands behind
it. This page says what is signed, who may approve a signing, and how you check
the result.

## Free code signing

Free code signing on Windows is offered to open-source projects by
[SignPath.io](https://signpath.io), with a certificate the
[SignPath Foundation](https://signpath.org) issues; the certificate belongs to
the foundation, so the publisher Windows shows for a Fauna installer reads
**SignPath Foundation**, not {legalName}.

Our application to the foundation is being prepared. Until the certificate is
issued, the Windows installers in our releases are unsigned, and Windows warns
about an unknown publisher when you run one. The verification recipe in every
release's notes works regardless of that warning.

## What is signed, and from where

Only builds of our own source are signed: the Windows installer
(`Fauna-Setup-x64.msi` and `Fauna-Setup-arm64.msi`) that the public release
workflow builds, on the hosting service's own runners, from a tagged commit of
the public repository {repo}. We maintain all of that source and every build
script ourselves. Every other released file carries a signature from the same
workflow's own identity; the release notes explain how to verify both.

The Microsoft Store package is signed by the Store itself and is not part of
this policy. The [official apps](/official-apps) page lists every identity a
Fauna app or release is published under.

## Who may sign

The team is small, and the roles the foundation asks us to name are held as
follows:

|  |  |
|---|---|
| Authors | {maintainer} |
| Reviewers | {maintainer} |
| Approvers | {maintainer} |

Every signing request is approved by hand by an approver; no release is signed
automatically. All team members use multi-factor authentication on both the
source repository and SignPath. Changes from people outside the team, such as
pull requests, are reviewed by a team member before they are merged.

## Privacy

Fauna's apps transfer no information to other networked systems beyond those
our [privacy policy](/privacy) names, and there is no telemetry.

## How to check a signature

On Windows, right-click the downloaded installer, open **Properties**, then
**Digital Signatures**: the signer reads **SignPath Foundation**. From a
terminal, `signtool verify /pa Fauna-Setup-x64.msi` prints the same. The
release notes carry the recipe for verifying every other file.

## Questions

Something on this page that is not true anymore? Tell us: {email}.
