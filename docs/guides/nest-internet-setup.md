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
> [link the two](nest-relay-setup.md). The internet nest becomes a front door that, linked as the relay guide shows, keeps no readable copy of the mail you receive.

---

## What you need

| | |
|---|---|
| **A server with a public address** | Any provider, running 64-bit Ubuntu 26.04 LTS — the commands below use Ubuntu's own packages. **1 GB of memory** is enough without email; **2 GB or more if you want email** — the virus scanner alone holds about 1.5 GB. 20 GB of disk. Nothing else may be using ports 80 and 443. |
| **A domain you own** | From any registrar. You add two records by hand below; the app tells you the rest later. |
| **About 20 minutes** | Plus DNS propagation, which can take from seconds to a few hours. |

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

There is no Stable release of the nest software yet. Until the first one is out, the **Stable** image this file names has nothing in it, so a server set up this way has nothing to start; a numbered release is published on the **Test** channel, and the **Dev** channel's development builds are published. The file below is the shape the release will have.

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
      FAUNA_IMAGE_TAG: latest
      FAUNA_CLAMD_ADDR: clamd:3310
      FAUNA_RSPAMD_URL: http://rspamd:11333
    labels:
      - "social.fauna.update=true"
    logging: *default-logging
    stop_grace_period: 15s
    restart: unless-stopped
  fauna-updater:
    image: ghcr.io/faunasocial/nest:latest
    entrypoint: ["/usr/local/bin/fauna-updater"]
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
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

Lock it down:

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
> yours. And the code is used up the moment you claim, so the window is the few minutes between starting the box and Step 8.
>
> **If you'd rather not rely on that**, close the box to everyone but yourself
> while you claim it. Before Step 7, use the firewall in your server provider's
> control panel to let in only your own address. Use the provider's firewall,
> not `ufw`: over IPv4, `ufw` doesn't filter the ports Docker opens for the
> nest.
>
> Claim in Step 8, then open the provider's firewall to everyone again straight
> away — until you do, the server can't fetch its real certificate. Nobody else
> can reach the claim page at all during the one window where it matters. This also sidesteps
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

(Docker opens the nest's own ports itself over IPv4, whatever `ufw` says, but connections over IPv6 still pass through `ufw`, so keep these lines. You can skip the four mail lines if you skipped email above.)

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
the claim code. It carries the code *plus* your server's identity, so an installed Fauna app verifies it is really talking to your box before it sends the code anywhere — nobody sitting between you and the server can intercept the claim. Typing the bare code still works; it just skips that check.

Open an installed Fauna app and follow the setup screens. (Fauna in a web browser works too if you open it at your server's own address, `https://example.com/app/`: the browser first warns that the connection isn't private, because the server is still using its temporary certificate, and you continue past the warning. Fauna's web app at any other address can't reach the server until it has a real certificate.) Enter the claim code (or paste the claim URI) when asked, and pick your handle as **`you@example.com`** — that domain becomes the server's identity and its email domain, so use the real one.

If you kept email in Step 5 and confirm the nest is reachable from the internet (the default), it switches itself on as part of this, and your mailbox is created with a password the app generates for you; you can show or copy it again any time from its row in **Settings → Connected apps**. If you removed email, the nest knows it has no virus scanner and spam filter: email stays off, and it can't be turned on unless the server is set up again with the email parts in its setup file.

**Now** the nest knows its name, and fetches a real certificate for it within a
minute or two. Watch it happen if you like:

```
docker compose -f /opt/fauna/docker-compose.yml logs fauna-nest | grep -i acme
```

This needs **port 80** open and your domain resolving to this server. If you
must keep port 80 closed, see the last entry under "If something doesn't work".

If the certificate can't be fetched, **Admin → DNS** tells you why. A line above your domains says the certificate request failed and when, and gives the certificate authority's own explanation — for example, that it couldn't reach your server on port 80. The line goes away once a request succeeds, and it shows when the page loads or you refresh it, not live.

## Step 9 — Finish the DNS

Open **Admin → DNS** in the app. It lists every remaining record — mail
routing, sender authentication, and the rest — with the exact values. Add them at your registrar, then press **Refresh** on that page until every record shows **OK**.

One exception: the **reverse DNS (PTR)** row is set with your *server
provider* — the control panel where you rent the machine and its IP address —
not at your registrar. It's the one record on the list your registrar cannot
set.

Some records can take a few hours to spread. That's normal.

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

## Step 11 — Close SSH

From here on, Fauna never needs a shell on this server: everything else is
done in the app. An SSH server left open to the internet is a door you would
have to keep guarding yourself, so close it now.

Before you do, give the server a password you can type at its console — a
server you reach with an SSH key usually has none:

```
passwd
```

Then either turn the SSH server off for good:

```
apt remove -y openssh-server
```

or at least close its port to the world:

```
ufw delete allow 22/tcp
```

If you ever need a shell on the server again, your provider's control panel has
a console that doesn't need SSH: sign in there as `root` with that password.

---

## Keeping it running

The Fauna software updates itself: the server picks up a new version within the hour on Stable, or within about a minute if you choose Test or Dev builds (below), and installs it. Keeping the operating system patched is up to you — the `unattended-upgrades` package from Step 3 installs Ubuntu's security updates, but some of them take effect only after a restart, so restart the server now and then from your provider's control panel. Beyond that:

- **Back up your data.** Add a backup destination on the app's **Backups** page ([Backup with Fauna](cloud-backup.md)). A snapshot of the whole server from your provider's control panel is yours to add as a second copy; Fauna neither takes nor restores it.
- **Keep your recovery kit safe** — the code the app showed you once when you created your identity, which the app also calls your recovery phrase. It gets your account back if you lose every device ([Your recovery kit](identity-and-devices.md#your-recovery-kit)); if you skipped it, make one under **Settings → Account → Recovery Kit**. The claim itself leaves no recovery secret for you to keep: the claim code is used up the moment you claim, and your Fauna app keeps a copy of the server's own key for you.
- **Leave room on the disk** if you enabled email: mail and its attachments fill it over time, so pick a plan you can resize from your provider's control panel.

To choose which builds the nest installs — **Stable**, the default, or **Test** or **Dev** — use **Updates** under **Admin → Nest** in the app. Don't change the `image:` lines in the setup file: the nest goes on following its update channel whatever they say.

## Rebuilding after a total loss

If the server's disk dies, or the server itself is gone, you can build a new
one that is still the same nest. Your Fauna app keeps a copy of the server's
own key, and a new server started with that key is the one your installed Fauna apps already
know: they reconnect to it as the same server, with no warning that its identity changed.

1. Get a new server and follow this guide again from Step 1, which points
   your domain at the new server's address. Do that first, before you claim
   it or restore anything.
2. At Step 5, before you save the setup file, open an installed Fauna app (the web app
   is served by the server you lost, so it can't open until the new one is
   running) and choose **Recover a lost box** (it appears on the "Couldn't reach your nest" screen,
   and on the screen of a newly installed app where you choose **Create New
   Identity**). Pick this nest, then
   **Install on my own server**, and copy the line it shows. It starts with
   `FAUNA_DEPLOYMENT_SEED=` and goes on with 64 letters and digits.
3. In the setup file, add one line at the end of the `environment:` block
   under `fauna-nest`, lined up with the lines above it and written like
   them, the name, a colon and a space, but with the value in double quotes:

   ```yaml
         FAUNA_DEPLOYMENT_SEED: "PASTE-HERE"
   ```

   Replace `PASTE-HERE` with everything after the `=` in the line you copied.
   This line is the one exception to Step 5's "do not add anything else". It
   has to be there the first time the server starts: a server that starts
   without it makes a new key of its own and ignores the line from then on.
   The line is your server's key, so keep the file locked down as Step 5
   says.
4. Carry on to the end of the guide, Step 9 included: on **Admin → DNS**,
   update any record that doesn't show **OK**.
5. Bring your data back from your backup, as
   [Backup with Fauna](cloud-backup.md) describes.
6. If this server was the doorway for a home nest, don't say yes to
   **Trust this box?** when you claim it at Step 8 (if you did, take that back
   for it under **Settings → Nests**), then redo Steps 1 and 2 of
   [Link a home nest to an internet nest](nest-relay-setup.md): the link and
   the setting that stops this server serving your mail were kept on the
   server you lost.

## If something doesn't work

**Nothing starts.** Re-run the `up -d` command and read what it prints. Until the first Stable release is out, it is because there is no Stable image to download yet (see Step 5); otherwise it's almost always a typo in the setup file, a full disk, or no network.

**Mail bounces with a temporary error.** The virus scanner isn't ready yet. Its
first startup downloads signatures and takes a minute or two. Senders retry.

**Mail lands in spam.** Re-check every record on the DNS page, and confirm the
reverse name from Step 1 really resolves to `mail.example.com`. A brand-new
server address also has no reputation yet and warms up over the first weeks.

**Mail you send never arrives.** Check that your provider hasn't blocked port 25 — most block outbound port 25 by default and need you to ask. Mail coming in still works while it's blocked; only sending fails.

**You need to keep port 80 closed.** The server normally proves it owns your
domain over port 80. With that closed it can't, so it keeps its own temporary
certificate.

**Read this part before you decide.** The installed Fauna apps still work, because they check the server's identity key rather than its certificate. **Fauna in a web browser mostly does not.** Opened at your server's own address, `https://example.com/app/`, it works once you click past the browser's certificate warning — and everyone who opens it there has to do the same. Opened from any other address, it cannot connect at all: a browser lets you click past the warning for the page in front of you, but not for the live connection the app needs to a different address, and it gives no way to approve that. There the web app gets as far as looking normal and then sits on **"Cannot connect"**. Nothing is broken and nothing is lost; until the server has a real certificate, use an installed app, or the web app at the server's own address.

Everything else that talks to your nest minds too: mail apps and web browsers
warn, and other mail servers trust your mail less.

The way round it is to prove ownership through your DNS instead, which the **app**
does rather than the server: give the app a token for your DNS provider on the
**Admin → DNS** page, and it gets the certificate and hands it to your nest. Do that from an installed app.

Two things to know before you choose this:

- **The app has to do every renewal too**, about every two months. The server cannot renew on its own — your DNS token is kept encrypted in your account, where the server cannot read it. An installed Fauna app signed in as the admin renews it by itself while it's open, so open one now and then; if none is opened for weeks, the certificate lapses back to the temporary one and mail delivery starts to suffer.
- **The server will make a few failed attempts** over port 80 in the meantime.
  That is safe and deliberately budgeted — it tries at most 4 times an hour,
  one under the limit certificate authorities allow, and it remembers the count
  across restarts so a reboot loop can't burn through it. Once the app has installed a certificate, the attempts stop until the next renewal is due.

---

## Next: keep your mail at home

This nest stores your mail. If you'd rather it lived on a machine in your own
home, with this server acting only as a front door that, linked as the relay guide shows, keeps no readable copy of the mail you receive:

1. Finish this guide.
2. [Set up a home nest](nest-home-setup.md) on a machine at home.
3. [Link the two](nest-relay-setup.md).

