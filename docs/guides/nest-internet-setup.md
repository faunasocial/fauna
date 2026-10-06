# Set up a Fauna nest on the internet

An **internet nest** is a Fauna server on a machine with a public address — a small
rented server, usually. It can receive email at your own domain, talk to other
Fauna servers, and be reached by your phone and laptop from anywhere.

Everything below is typed on the server over SSH. It takes about 20 minutes,
plus waiting for DNS.

> **There's an easier way.** The Fauna app can do all of this for you — create
> the server in your own cloud account, install everything, publish the DNS, and
> sign you in as the owner, with no terminal at all. You still pay your cloud
> provider directly, at their normal prices; the app just does the work. See
> **[set up a nest from the app](nest-app-setup.md)**.
>
> Follow this guide instead if you want to run the commands yourself, or your
> provider isn't one the app supports.

> **Want your mail stored at home instead?** Do this guide, then
> [set up a home nest](nest-home-setup.md) on a machine at home, then
> [link the two](nest-relay-setup.md). The internet nest becomes a front door that
> keeps no readable copy of your mail.

---

## What you need

| | |
|---|---|
| **A server with a public address** | Any provider. **1 GB of memory** is enough without email; **4 GB if you want email** — the virus scanner alone holds about 1.5 GB. 20 GB of disk. Nothing else may be using ports 80 and 443. |
| **A domain you own** | From any registrar. You add two records by hand below; the app tells you the rest later. |
| **About 20 minutes** | Plus DNS propagation, which can take an hour. |

You do **not** need a registry account or an access token. The Fauna server
image is public and downloads anonymously.

Throughout, replace `example.com` with your domain and `203.0.113.7` with your
server's address.

---

## Step 1 — Point your domain at the server

At your registrar, add two records:

| Type | Name | Value |
|---|---|---|
| A | `example.com` | `203.0.113.7` |
| A | `mail.example.com` | `203.0.113.7` |

If your provider gave you an IPv6 address, add the same two as `AAAA` records
too.

Then, in your provider's control panel, set the **reverse name** (sometimes
called reverse DNS or PTR) for the server's address to `mail.example.com`. Mail
providers reject or spam-file servers that skip this.

Check it took effect before moving on:

```
dig +short example.com
```

## Step 2 — Become root

Every command from here runs as root:

```
sudo -i
```

## Step 3 — Install Docker

```
apt update
apt install -y docker.io docker-compose-v2 ufw unattended-upgrades needrestart
```

> Use exactly this. Docker's own one-line web installer puts Docker in a
> separate software source that your server's automatic security updates will
> **never** patch. The packages above come from Ubuntu's own archive, so they
> get patched automatically.

## Step 4 — Create the nest's folders

```
mkdir -p /opt/fauna/maintenance /opt/fauna/maintenance-host
```

## Step 5 — Write the nest's setup file

Open a new, empty file:

```
nano /opt/fauna/docker-compose.yml
```

Paste in exactly this, then press **Ctrl-O**, **Enter** to save and **Ctrl-X**
to quit:

```yaml
x-logging: &default-logging
  driver: json-file
  options:
    max-size: "50m"
    max-file: "5"
services:
  fauna-nest:
    image: ghcr.io/faunasocial/nest:latest
    ports:
      - "80:8080"
      - "443:443"
      - "7842:7842/udp"
      - "25:25"
      - "465:465"
      - "587:587"
      - "993:993"
    volumes:
      - fauna-data:/data
      - /opt/fauna/claim-code:/run/fauna/claim-code-seed:ro
      - /opt/fauna/maintenance:/data/maintenance
      - /opt/fauna/maintenance-host:/data/maintenance-host:ro
    environment:
      FAUNA_MODE: public
      FAUNA_CLAMD_ADDR: clamd:3310
      FAUNA_RSPAMD_URL: http://rspamd:11333
    labels:
      - "com.centurylinklabs.watchtower.enable=true"
    logging: *default-logging
    stop_grace_period: 15s
    restart: unless-stopped
  watchtower:
    image: nickfedor/watchtower:latest@sha256:3b8d2e3f0f6ff9295a5d634e4fdb7062e5e6602f71d89baa15cd4d39d22b5743
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
    environment:
      WATCHTOWER_CLEANUP: "true"
      WATCHTOWER_POLL_INTERVAL: "300"
      WATCHTOWER_LABEL_ENABLE: "true"
      WATCHTOWER_ROLLING_RESTART: "true"
    logging: *default-logging
    restart: unless-stopped
  clamd:
    image: clamav/clamav:latest-debian@sha256:967334b92d1782e4d1314ddf903ae537d26792d21c9a39adecb8ac9757980514
    logging: *default-logging
    restart: unless-stopped
  rspamd:
    image: rspamd/rspamd:latest@sha256:86bc544548bc881276e19dcff4cf36bc4fb5c8a3050717f3c2315cb823938a80
    configs:
      - source: rspamd-worker-normal
        target: /etc/rspamd/local.d/worker-normal.inc
    logging: *default-logging
    restart: unless-stopped
configs:
  rspamd-worker-normal:
    content: |
      bind_socket = "*:11333";
volumes:
  fauna-data:
```

Lock it down — it can hold a recovery secret:

```
chmod 600 /opt/fauna/docker-compose.yml
```

**Don't want email?** Delete the four mail ports (`25`, `465`, `587`, `993`),
the `FAUNA_CLAMD_ADDR` / `FAUNA_RSPAMD_URL` lines, and the whole
`clamd` and `rspamd` sections plus the `configs:` block. A nest without email runs
comfortably in 1 GB.

**Do not add anything else to this file.** In particular never add `privileged`,
`cap_add`, `user:`, `entrypoint:`, `command:`, or `security_opt` — each one
switches off a protection built into the server image.

Notice there is no domain anywhere in the file. The nest learns its name from
you when you claim it in Step 8.

## Step 6 — Write your claim code

This is the one-time secret that proves the box is yours. It's built from
letters and digits chosen so nothing looks like anything else — no `O` vs `0`,
no `I` vs `1` — and grouped in fours so it's easy to read off a screen:

```
LC_ALL=C tr -dc 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789' < /dev/urandom \
  | head -c 8 | sed 's/.\{4\}/&-/g; s/-$//' > /opt/fauna/claim-code
chmod 600 /opt/fauna/claim-code
cat /opt/fauna/claim-code
```

You'll get something like `K7Q2-M9XJ`. Typing it back in Step 8 is
case-insensitive and the dash is optional.

> **Is eight characters enough?** Yes, because the nest strictly limits how fast
> anyone can guess: 10 tries a minute from any one address, and 60 a minute
> across the whole internet combined. At that rate, working through every
> possible code takes about **35,000 years** — 17,000 on average to stumble on
> yours. And the code stops existing the moment you claim, so the window is the
> few minutes between starting the box and Step 8.
>
> **If you'd rather not rely on that**, close the box to everyone but yourself
> while you claim it. Before Step 7, allow only your own address:
>
> ```
> ufw allow from YOUR.IP.ADDRESS.HERE
> ```
>
> Claim in Step 8, then remove that rule (`ufw status numbered`, then
> `ufw delete <number>`) and open the normal ports. Nobody else can reach the
> claim page at all during the one window where it matters. This also sidesteps
> the one nuisance the limits allow: someone flooding guesses can't take your
> box, but they can keep the shared limit busy and make *your* attempt bounce
> until a quiet moment.

## Step 7 — Open the firewall and start it

```
systemctl enable docker
systemctl start docker
ufw allow 22/tcp
ufw allow 80/tcp
ufw allow 443/tcp
ufw allow 7842/udp
ufw allow 25/tcp
ufw allow 465/tcp
ufw allow 587/tcp
ufw allow 993/tcp
ufw --force enable
docker compose -f /opt/fauna/docker-compose.yml up -d
```

(Skip the four mail lines if you skipped email above.)

Port 7842/udp lets your nest tell your devices their own public address, so
two of them on different networks can often connect straight to each other
instead of passing everything through your nest. It reveals nothing about
anyone else.

Watch it come up:

```
docker compose -f /opt/fauna/docker-compose.yml ps
docker compose -f /opt/fauna/docker-compose.yml logs -f fauna-nest
```

Press Ctrl-C to stop watching once it's running.

At this point the nest **does not know its own name yet** — it learns that from
you in the next step. So it is serving a temporary certificate it made itself.
That's expected, and it fixes itself once you claim.

## Step 8 — Claim it from the app

Read your claim code back:

```
docker compose -f /opt/fauna/docker-compose.yml exec fauna-nest cat /data/claim-code
```

Better, read the **claim URI** — the server prints it in its logs beside the
code:

```
docker compose -f /opt/fauna/docker-compose.yml logs fauna-nest | grep "CLAIM URI"
```

Paste that whole `fauna://claim?...` string into the app wherever it asks for
the claim code. It carries the code *plus* your server's identity, so the app
verifies it is really talking to your box before it sends the code anywhere —
nobody sitting between you and the server can intercept the claim. Typing the
bare code still works; it just skips that check.

Open any Fauna app and follow the setup screens. Enter the claim code (or
paste the claim URI) when asked, and pick your handle as **`you@example.com`**
— that domain becomes the server's identity and its email domain, so use the
real one.

Email switches itself on as part of this, and your mailbox is created with a
password the app shows you once. Write it down.

**Now** the nest knows its name, and fetches a real certificate for it within a
minute or two. Watch it happen if you like:

```
docker compose -f /opt/fauna/docker-compose.yml logs fauna-nest | grep -i acme
```

This needs **port 80** open and your domain resolving to this server. If you
must keep port 80 closed, see the last entry under "If something doesn't work".

## Step 9 — Finish the DNS

Open **Admin → DNS** in the app. It lists every remaining record — mail
routing, sender authentication, and the rest — with the exact values. Add them
at your registrar, then press **Verify** on that page until everything is green.

One exception: the **reverse DNS (PTR)** row is set with your *server
provider* — the control panel where you rent the machine and its IP address —
not at your registrar. It's the one record on the list your registrar cannot
set.

Some records take up to an hour to spread. That's normal.

## Step 10 — Check your email works

In your mail app, add the account:

- **Incoming:** IMAP, server `mail.example.com`, port 993, SSL/TLS
- **Outgoing:** SMTP, server `mail.example.com`, port 465, SSL/TLS
- **Username:** `you@example.com` — **Password:** the one from Step 8

Send a message to a Gmail address. It should land in the inbox, not spam. Open
it there, choose **Show original**, and check that SPF, DKIM, and DMARC all say
PASS. Then reply from Gmail and confirm it arrives.

If the first message from Gmail is slow, that's expected — new senders get held
back briefly and retried automatically.

---

## Keeping it running

The server updates itself and installs its own security patches. Beyond that:

- **Back up your data.** Everything lives in a storage area called
  `fauna-data`. Snapshot it on whatever schedule you're comfortable losing.
- **Save the recovery key** the app showed you when you claimed the nest. The
  claim code itself is used up at that moment and deleted — it's the recovery
  key that matters from then on.
- **Watch your disk** if you enabled email.

To pin a specific version instead of updating automatically, change the
`image:` line to a specific tag and run
`docker compose -f /opt/fauna/docker-compose.yml up -d fauna-nest`.

## If something doesn't work

**Nothing starts.** Re-run the `up -d` command and read what it prints. It's
almost always a typo in the setup file, a full disk, or no network.

**Mail bounces with a temporary error.** The virus scanner isn't ready yet. Its
first startup downloads signatures and takes a minute or two. Senders retry.

**Mail lands in spam.** Re-check every record on the DNS page, and confirm the
reverse name from Step 1 really resolves to `mail.example.com`. A brand-new
server address also has no reputation yet and warms up over the first weeks.

**Mail from outside never arrives.** Check that port 25 is open and that your
provider hasn't blocked it — most block outbound port 25 by default and need
you to ask. Inbound mail still works while it's blocked; only sending fails.

**The app says the nest is already claimed, but you never claimed it.** Run:

```
docker compose -f /opt/fauna/docker-compose.yml exec fauna-nest chown fauna:fauna /data/claim-code
docker compose -f /opt/fauna/docker-compose.yml exec -u fauna fauna-nest cat /data/claim-code
```

Then claim with the code it prints.

**You need to keep port 80 closed.** The server normally proves it owns your
domain over port 80. With that closed it can't, so it keeps its own temporary
certificate.

**Read this part before you decide.** The installed Fauna apps — Linux, macOS,
Windows, Android, iOS, and the terminal app — still work, because they check the
server's identity key rather than its certificate. **Fauna in a web browser does
not.** A browser will let you click past the certificate warning to load the
page, but it will *not* do the same for the live connection the app needs
underneath, and it gives no way to approve it. So on a nest with port 80 closed
and no certificate yet, the web app gets as far as looking normal and then sits
on **"Cannot connect"**, with no Admin section — including for the person who
just claimed the nest. Nothing is broken and nothing is lost; the nest simply
cannot be administered from a browser until it has a real certificate. Use one of
the installed apps to set it up, or get the certificate first.

Everything else that talks to your nest minds too: mail apps and web browsers
warn, and other mail servers trust your mail less.

The way round it is to prove ownership through your DNS instead, which the **app**
does rather than the server: give the app a token for your DNS provider on the
**Admin → DNS** page, and it gets the certificate and hands it to your nest. Do
that from an installed app — for the reason just above, the browser cannot reach
the Admin → DNS page on a nest that has no certificate yet.

Two things to know before you choose this:

- **The app has to do every renewal too**, about every two months. The server
  cannot renew on its own — it never holds your DNS token. Fauna will notify you
  when it's due, but if you ignore it, the certificate lapses back to the
  temporary one and mail delivery starts to suffer.
- **The server will make a few failed attempts** over port 80 in the meantime.
  That is safe and deliberately budgeted — it tries at most 4 times an hour,
  one under the limit certificate authorities allow, and it remembers the count
  across restarts so a reboot loop can't burn through it. Once the app has
  installed a certificate, the attempts stop completely.

---

## Next: keep your mail at home

This nest stores your mail. If you'd rather it lived on a machine in your own
home, with this server acting only as a front door that keeps no readable copy:

1. Finish this guide.
2. [Set up a home nest](nest-home-setup.md) on a machine at home.
3. [Link the two](nest-relay-setup.md).

<!--
  ⚠ MAINTAINERS — the compose file in Step 5 is the SHIPPING ARTIFACT.

  It must stay functionally identical to what the app hands a cloud provider
  when a user has Fauna set the server up for them, rendered by
  `build_cloud_init` in libs/fauna-provisioning/src/cloud_init.rs.

  If you change that function, change Step 5 in the same commit — and the
  matching step in nest-home-setup.md if the change is shared.

  This is enforced: `test_internet_nest_guide_matches_rendered_compose`
  in cloud_init.rs parses the YAML block out of this file and compares it to
  the rendered output, ignoring comments and blank lines. It will fail the
  merge if the two drift.
-->
