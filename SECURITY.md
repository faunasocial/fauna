# Security Policy

Fauna is privacy- and security-focused software, and we take vulnerabilities
seriously.

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.**

Instead, use GitHub's **private vulnerability reporting**:

1. Go to the **Security** tab of this repository.
2. Click **Report a vulnerability**.
3. Provide a description, reproduction steps, affected components/versions, and
   any suggested remediation.

This routes your report privately to the maintainers. We aim to acknowledge
reports within a few days and will keep you informed as we investigate and fix.

## Scope

In scope: the Fauna server (Nest), the shared Rust core and protocol crates, the
clients, and the mail/bridge components in this repository. Out of scope: issues
that require a compromised device, and findings in third-party dependencies
(please report those upstream, and let us know if Fauna's use of them is
affected). A compromised nest or hosting provider, and a nest's own user acting
against others, are in scope: the design's promise is that a nest cannot read
what you did not share with it, beyond the mail and the messages it passes to
and from email and other networks.

## Disclosure

We follow coordinated disclosure: we ask that you give us a reasonable window to
release a fix before any public disclosure. Credit is given to reporters who wish
to be named.
