# Front-door deploy artifacts

Staged by Track I for the VPS migration (Track H) to consume at the cutover.
**The contract's owner is `docs/goal/architecture/front-door.md`** — this
directory is the operational form of § The box:

| File | Purpose |
|---|---|
| `fauna-front-door.service` | The door unit — full hardening set, `StateDirectory=fauna-front-door` (ACME account + certs), content read-only. |
| `fauna-cors-proxy.service` | The loopback CORS-proxy unit (`BIND_ADDR=127.0.0.1`, `PORT=8402` — matches the door's `PROXY_UPSTREAM` constant). |
| `door-deploy-receive.sh` | Forced command for the restricted deploy key: rsync into `releases/<stamp>` + atomic `flip` of the `current` symlink, prune keep-3. Nothing else — it rebuilds the rsync command from one allowlisted shape rather than running the client's, so there is no read direction and no path outside the releases dirs. Pinned by `tests/scripts/test_door_deploy_receive.py`. |
| `workflow-drafts/` | DRAFT push-triggered workflows for the site and the SPA. **Not active** — Track H swaps them into `.github/workflows/` at the cutover. |

Content layout on the box:

```
/srv/fauna/site/releases/<stamp>/   # rsync lands here (complete)
/srv/fauna/site/current -> releases/<stamp>   # atomic flip
/srv/fauna/app/…                    # same shape for the SPA
```

Deploy-user requirements the wrapper relies on (provisioning must establish
them): `/usr/bin/rsync` present (no python3 needed — `rrsync` is not used);
the deploy user owns **only** `/srv/fauna/{site,app}/releases` and the two
parent dirs it flips `current` in; its `authorized_keys` is root-owned and not
writable by it (e.g. `AuthorizedKeysFile /etc/ssh/authorized_keys/%u`). The
upload flags are part of the contract: the wrapper accepts exactly what
`rsync -az --delete` sends, so change the workflows' flags and the wrapper's
option word together (the test's real-rsync round trip reds otherwise).

Box provisioning order (Track H's runbook owns the full sequence): minimal
Debian + unattended-upgrades → deploy user + forced-command key → binaries
(x86_64/aarch64 from release builds) → units enabled → content deployed →
DNS staged → nameserver switch → HTTP-01 certs issue on-box.
