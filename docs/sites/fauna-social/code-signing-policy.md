---
title: Fauna Social — Code signing policy
description: How Fauna's downloads are signed, who may approve a signing, and how to check a signature — the code signing policy of the Fauna Social association.
---

# Code signing policy

*Illustration: badger*

Every Fauna download is built by a public workflow from the published source.
The Mac download is signed and notarized so that macOS opens it, and every
download in our releases can be checked against the workflow that built it.
This page says what is signed, who may approve a signing, and how you check
the result.

## Windows code signing

The Windows installers in our releases do not carry a Windows code
signature, and Windows warns about an unknown publisher when you run one. The
check that a file is the exact output of the release workflow that built it,
under
[How to check a signature](#how-to-check-a-signature), below, works regardless
of that warning.

## What is signed, and from where

Only builds of our own source are signed. The downloads in our releases are
built and signed by one of two public release workflows, on the hosting
service's own runners, from a tagged commit of the public repository {repo}. We maintain
all of that source and every build script ourselves.

- The `Release` workflow builds the Windows installer (`Fauna-Setup-x64.msi`
  and `Fauna-Setup-arm64.msi`), the Linux app, the terminal app for Linux and
  Windows, and the server programs for Linux and Mac. Every file it releases,
  the installer included, carries a signature from the workflow's own identity,
  in a `.bundle` file beside it.
- The `Release (macOS)` workflow builds the Mac download (the `.dmg`) and the
  terminal app's Mac archives. It signs the `.dmg`, the app and the programs
  in each archive with our Apple Developer ID, has Apple notarize them, and
  records a build attestation for the `.dmg` and for each archive from the
  workflow's own identity.

The Microsoft Store package is signed by the Store itself, and the server's
container image, on its Test and Stable channels, by the public repository's
image workflow; neither is part of this policy. The [apps](/apps) page says where official
downloads come from and how to check one.

## Who may sign

The team is small, and the signing roles are held as follows:

|  |  |
|---|---|
| Authors | {maintainer} |
| Reviewers | {maintainer} |
| Approvers | {maintainer} |

Every signing request is approved by hand by an approver; no release is signed
automatically. All team members use multi-factor authentication on the
source repository. Changes from people outside the team, such as
pull requests, are reviewed by a team member before they are merged.

## Privacy

Fauna's apps, and the nest (the server program the Windows installer can
optionally set up), transfer no information to other networked systems beyond
those our [privacy policy](/privacy) names, and there is no telemetry.

## How to check a signature

On Windows, the installer carries no Windows code signature, so check it
against the release workflow that built it, as below, or compare its SHA-256
hash with the one the releases page lists: in PowerShell,
`Get-FileHash <file>` prints your file's hash.

On a Mac, macOS checks the Developer ID signature and Apple's notarization
itself when you first open the app.

To check that a file is the exact output of the release workflow that built
it, from a terminal: for
a file from the `Release` workflow, install
[cosign](https://docs.sigstore.dev/cosign/system_config/installation/), download
the file's `.bundle` beside it and run `cosign verify-blob
--certificate-identity-regexp="^https://github\.com/faunasocial/fauna/"
--certificate-oidc-issuer=https://token.actions.githubusercontent.com
--bundle <file>.bundle <file>`; for the Mac download or one of the terminal
app's Mac archives, the downloads from the `Release (macOS)` workflow, install the
GitHub CLI, sign in with `gh auth login` and run
`gh attestation verify <file> --repo faunasocial/fauna`.

## Questions

Something on this page that is not true anymore? Tell us: {email}.
