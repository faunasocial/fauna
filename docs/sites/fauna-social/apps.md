---
title: Fauna Social — Apps
description: How to get the official Fauna Social apps.
---

# Apps

*Illustration: owl*

Fauna {version}, the current release, has these downloads:

{downloads}

## Downloads

Every file is also listed at {releases}, and our [code signing policy](/code-signing-policy) says how to check that it is ours.

### Check a download

The releases page lists each file's SHA-256 hash, and the Mac download (the
`.dmg`) and the terminal app's Mac archives each have a `.sha256` file beside
them there, named after the file, that holds its hash. Compare the hash with
yours:

- Windows (PowerShell): `Get-FileHash <file>`
- macOS: `shasum -a 256 <file>`, or `shasum -a 256 -c <file>.sha256` run in
  the folder that holds both the download and its `.sha256` file
- Linux: `sha256sum <file>`

The Mac download (the `.dmg`) and the programs in the terminal app's Mac
archives are signed with our Apple Developer ID and notarized by Apple.

The Windows installers in our releases do not carry a Windows code
signature; our [code signing policy](/code-signing-policy) says how to check
that an installer is ours.

Found a download claiming to be ours somewhere else? Write to {email}.
