# Installer: Linux Nest (Native) — target state

Owns: linux-nest-packaging
Status: ratified
Authority: the native (non-Docker) Linux nest deployment — binary layout, the systemd unit set (fauna-nest plus the two mail-bridge role instances), the supervisor-sidekick socket's systemd dispatch analog, `bins/fauna-nest/install.sh`, and the `/etc/fauna` + `/var/lib/fauna` layout deltas from Docker; defers the canonical `nest.toml` schema and the s6/container wiring to `installers/docker.md`, bridge lifecycle/enablement + DKIM provisioning to `../../behavior/mail-bridge-lifecycle.md`, TLS acquisition to `../nest/tls-certificates.md`, the claim flow to `../../behavior/onboarding.md`, DNS custody to `../../behavior/dns-management.md`.

## Implementation status today

- **Built:** `bins/fauna-nest/install.sh` is the primary install path — creates the `fauna` service user, installs the binary, renders `/etc/fauna/nest.toml` from `config/default.toml`, generates the claim code, and writes a hardened systemd unit (`--config`-only ExecStart, `Restart=on-failure`, `ProtectSystem=strict`, `CAP_NET_BIND_SERVICE`). The nest and mail-bridge binaries build and run natively.
- **Target-state (unbuilt): the two-instance bridge units + the systemd sidekick dispatcher.** Nest's `mail_enable` side is supervisor-agnostic (it writes the lifecycle flag files and speaks the sidekick socket regardless of supervisor), but the only shipped sidekick *listener* is the Docker image's s6 `fauna-supervisor` (`s6-svc` dispatch); no systemd-dispatching listener exists in the repo yet. Until it lands, a bare-metal deploy has no admin-toggle-driven bridge supervision — § Mail bridge below is the target contract, not shipped behavior.
- No package-manager integration (no `.deb`/`.rpm` for the nest). The Docker deployment (`installers/docker.md`) remains the primary and recommended method.

## Goal

Provide a native Linux deployment of the nest server as an alternative to the recommended Docker deploy, for self-hosting admins who prefer running the binaries directly under systemd. The `fauna-nest` and `fauna-mail-bridge` binaries install from source, are configured via the artifact-written `/etc/fauna/nest.toml`, and run under systemd as dedicated service users. Supports modern Linux distributions on x86_64 and aarch64. (`fauna-mail-bridge` is the Go mail bridge; the bare-metal systemd packaging below is the native-Linux complement to the Docker image's s6 services — lifecycle semantics: `../../behavior/mail-bridge-lifecycle.md`.)

## Platform Support

| Distribution | Supported |
|-------------|-----------|
| Ubuntu 22.04+ / Debian 12+ | Yes |
| Fedora / RHEL 9+ | Yes |
| Arch Linux | Yes |
| Any modern Linux | Yes (from source) |

| Architecture | Supported |
|-------------|-----------|
| x86_64 (amd64) | Yes |
| aarch64 (arm64) | Yes |

## What Gets Installed

| Binary | Purpose | Required |
|--------|---------|---------|
| `fauna-nest` | Main nest server | Yes |
| `fauna-mail-bridge` | Go mail bridge — runs as **two supervised instances** (MTA + MDA roles), each with its own keypair; SMTP/IMAP/CalDAV, talks WS-RPC to nest | If mail/DAV enabled |

DNS is **not** a server-side component: the nest never writes DNS and never holds DNS credentials. TLS acquisition is tiered — nest-side HTTP-01 plus client-driven DNS-01 (managed / manual / CNAME-delegated) — owner: `../nest/tls-certificates.md` § B. DNS records are published by the admin's client (`../../behavior/dns-management.md`).

## Installing (recommended path: `bins/fauna-nest/install.sh`)

```
sudo bash bins/fauna-nest/install.sh --non-interactive \
  --local-binary target/release/fauna-nest --mode public
```

The script (idempotent; `--upgrade` swaps the binary and restarts; `--uninstall [--remove-data]` removes):

1. Creates the `fauna` system user (nologin, home `/var/lib/fauna`).
2. Installs the binary to `/usr/local/bin/fauna-nest`.
3. Renders `/etc/fauna/nest.toml` from the repo's `config/default.toml` template (Docker `/data/` paths rewritten to `/var/lib/fauna/`; `--bind`, `--mode`, `--blob-dir` applied; preserved if it already exists). The file is **artifact-written IPC, not a hand-edited surface** — every human choice comes from the app UI.
4. Generates the first-run claim code at `/var/lib/fauna/claim-code` (see § First Run).
5. Writes and enables the hardened `fauna-nest.service` unit (§ Systemd Services).

`--mode public|private` seeds the NAT axis pre-claim (owner: `../nest/common.md` § NAT mode). **The installer takes no flag for a domain, an ACME contact or mail settings** (removed 2026-10-01: `--domain`, `--acme`, `--acme-email`, `--email`, `--email-domain`, `--smtp-bind`): the nest learns its domain at claim (`../nest/domains-and-tls-bootstrap.md` § Env contract), ACME-run is **derived** and its CA and contact are constants (`../nest/tls-certificates.md` § ACME settings — constants, not choices), and mail is enabled from the app. Pinned by tier_1 `tests/e2e-unified/tests/platform/linux/test_installer_structure.py`.

## Building from Source

Install prerequisites: the pinned Rust nightly toolchain, Go, and standard build tools (`gcc`, `pkg-config`).

```
cargo build --release -p fauna-nest
just mail-bridge-build   # cgo build linking libfauna_ffi.so; see justfile
```

For a manual (script-free) install, copy the binaries to `/usr/local/bin/`:

```
install -m 755 target/release/fauna-nest /usr/local/bin/
install -m 755 bins/fauna-bridges/fauna-mail-bridge /usr/local/bin/
```

## Configuration

`/etc/fauna/nest.toml` is rendered by the installer (step 3 above); the format is identical to the Docker deployment — `installers/docker.md` owns the canonical schema. Data lives under `/var/lib/fauna` (created by the script; manual installs mirror its `useradd`/`mkdir`/`chown` steps).

**DKIM:** provisioning is automatic and nest-side (the nest mints and holds the key), with the public record published by the admin's client — owner: `../../behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic). There is no manual key generation and no on-disk `dkim.pem` to create.

**TLS / ACME:** the nest obtains its certificate automatically. HTTP-01 runs nest-side (the enable is *derived* from the NAT axis + an orderable domain — no config key; port 80 must be reachable); DNS-01 is **client-driven** — the admin's client holds the DNS provider credential and completes the order; the nest never sees DNS keys (`../nest/tls-certificates.md` § B tier 2). The ACME cache is the fixed `/var/lib/fauna/acme/` directory (the `<data>/acme/` convention) — not a config key.

## Data Directory Layout

```
/var/lib/fauna/
├── nest.db            (SQLite database)
├── blobs/             (blob storage)
├── claim-code         (first-run admin claim code; deleted after use)
├── acme/              (TLS certificate cache + retry state)
└── keys/              (bridge service-user keypairs: keys/mta/, keys/mda/ — target-state, see § Mail bridge)
/etc/fauna/
└── nest.toml          (main configuration — artifact-written)
```

## Systemd Services

### fauna-nest (written by the installer)

The installer writes `/etc/systemd/system/fauna-nest.service`:

```ini
[Unit]
Description=fauna-nest
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=fauna
Group=fauna
ExecStart=/usr/local/bin/fauna-nest --config /etc/fauna/nest.toml
Restart=on-failure
RestartSec=5
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/fauna
PrivateTmp=true
NoNewPrivileges=true
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

Manual installs use the same unit shape — DB/blob/bind come from `nest.toml`, not ExecStart flags. Check status with `systemctl status fauna-nest`; logs via `journalctl -u fauna-nest -f`.

### Mail bridge — two role instances (target contract)

The bridge mirrors the Docker image's supervision model (`installers/docker.md` § s6-overlay Services): **two instances of the same binary**, one per role, each enrolled as its own service user with its own keypair:

```ini
# /etc/systemd/system/fauna-mail-bridge-mta.service (fauna-mail-bridge-mda.service is the twin)
[Unit]
Description=Fauna Mail Bridge (MTA role)
After=fauna-nest.service
BindsTo=fauna-nest.service

[Service]
Type=simple
User=fauna-bridge
ExecStart=/usr/local/bin/fauna-mail-bridge \
  --keypair-file /var/lib/fauna/keys/mta/bridge.key \
  --nest-endpoint https://127.0.0.1:3000
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

`--nest-endpoint` is the loopback **base https URL** (the bridge derives the WS path itself; the nest's internal listener defaults to `:3000`). The bridge discovers its role and all configuration from nest after presenting its enrolled keypair — there are no `--config`, `--mode`, or env-var knobs (`principles.md` § One configuration surface); the MDA instance differs only in its keypair file (`keys/mda/`).

**Enablement is default-off and admin-toggle-driven — never hand-started.** The admin toggles mail/CalDAV in their Fauna app; nest materializes the lifecycle flag files and writes a line-framed JSON command (`{"action": "up", "service": "fauna-mail-bridge-mta"}`) to the **supervisor sidekick socket** `/run/fauna-supervisor.sock` (Unix-domain, `fauna`-owned, 0600). On bare metal the sidekick listener's dispatch arm is `systemctl start/stop fauna-mail-bridge-{mta,mda}` — the systemd analog of the Docker listener's `s6-svc -u/-d`. Which commands exist is owned by `../../behavior/mail-bridge-lifecycle.md` § Wire shapes; this doc owns the systemd dispatch shape. **The systemd listener is unbuilt** (see § Implementation status today) — do not `systemctl enable --now` the bridge units as a workaround; that recreates the always-on pre-lifecycle model.

### Enabling and starting

```
systemctl daemon-reload
systemctl start fauna-nest     # the installer already ran `systemctl enable`
```

The bridge instances are brought up by the admin toggle via the sidekick (above), not by hand.

## Firewall / Ports

| Port | Protocol | Purpose | Required |
|------|----------|---------|---------|
| 443 | HTTPS | Main API + web UI (admin-set `serving_port`, default 443) | Yes |
| 80 | HTTP | ACME http-01 challenge | If using http-01 TLS |
| 25 | SMTP | Inbound mail (MTA) | If mail enabled |
| 587 | SMTP | Submission (MTA) | If mail enabled |
| 465 | SMTPS | Implicit-TLS submission (MTA) | If mail enabled |
| 993 | IMAPS | IMAP client access (MDA) | If mail enabled |
| 143 | IMAP | IMAP STARTTLS access (MDA) | If mail enabled |
| 8443 | HTTPS | MDA CalDAV listener (admin-set `caldav_port`, default 8443) | If CalDAV enabled |

The client-facing serving port is the admin-set `serving_port` singleton, and the internal loopback split is fixed — owner: `../nest/common.md` § Serving ports. The `CAP_NET_BIND_SERVICE` grant in the unit lets the nest bind 443 directly; no reverse proxy or port-redirect layer is part of this deployment shape.

## First Run (Admin Bootstrap)

The installer (or, absent a pre-seeded file, `fauna-nest` on first boot) writes a claim code to `/var/lib/fauna/claim-code`. Enter it in a Fauna app to claim the admin account. The file is deleted after a successful claim and cannot be reused.

See `../../behavior/onboarding.md` § 3a for the claim-code format and the claim transaction.

## Uninstall

Recommended: `sudo bash bins/fauna-nest/install.sh --uninstall [--remove-data]` — stops/disables the service, removes the unit, binary, and `/etc/fauna`; `--remove-data` additionally deletes `/var/lib/fauna` (irreversible) and the `fauna` user.

Manual removal mirrors the script: `systemctl disable --now fauna-nest fauna-mail-bridge-mta fauna-mail-bridge-mda`, remove the unit files + `systemctl daemon-reload`, remove `/usr/local/bin/fauna-{nest,mail-bridge}`, remove `/etc/fauna`, and optionally `/var/lib/fauna` + `userdel fauna`.
