# Set up a Fauna nest at home

A **home nest** is a Fauna server on a machine on your own network — a
mini-PC, a NAS, a Raspberry Pi, a spare laptop. It is never reachable from the
internet. Your calendar, contacts, files and messages live on hardware you can
touch.

You can run one on its own, or pair it with a nest on the internet so your email
arrives at home instead of sitting on a rented server. Both are covered here;
the pairing is a separate guide at the end.

Everything below is typed on the home machine, directly or over SSH. It takes
about 15 minutes.

---

## What you need

| | |
|---|---|
| **An always-on Linux machine on your network** | Any small computer. **1 GB of memory** and 20 GB of disk is plenty. It does not need a public address, a domain, or anything opened on your router. |
| **Its address on your network** | Something like `192.168.1.50`. Give it a fixed one in your router's settings so it doesn't change. |
| **About 15 minutes** | |

You do **not** need a domain, a registry account, or an access token. The Fauna
server image is public and downloads anonymously.

Throughout, replace `192.168.1.50` with your machine's real address.

**What works:** the Fauna app, calendar and contacts, files, and messages
between your own devices — all served on your network.

**What needs a nest on the internet too:** receiving email from the outside
world, and talking to people on other Fauna servers. A machine behind your home
router can't accept incoming connections. That's what the pairing at the end
solves.

---

## Step 1 — Find the machine's address

```
hostname -I
```

Take the first address — that's your `192.168.1.50`.

## Step 2 — Become root

```
sudo -i
```

## Step 3 — Install Docker

```
apt update
apt install -y docker.io docker-compose-v2 unattended-upgrades needrestart
```

> Use exactly this. Docker's own one-line web installer puts Docker in a
> separate software source that your machine's automatic security updates will
> **never** patch. The packages above come from Ubuntu's own archive, so they
> get patched automatically.

No firewall step here — this machine stays on your network, and nothing is
opened on your router.

## Step 4 — Create the nest's folder

```
mkdir -p /opt/fauna
```

## Step 5 — Write the nest's setup file

Open a new, empty file:

```
nano /opt/fauna/docker-compose.yml
```

Paste in exactly this, then press **Ctrl-O**, **Enter** to save and **Ctrl-X**
to quit. Replace **both** copies of `192.168.1.50` with your machine's address:

```yaml
x-logging: &default-logging
  driver: json-file
  options:
    max-size: "50m"
    max-file: "5"
services:
  fauna-nest:
    image: ghcr.io/faunasocial/nest:latest
    network_mode: host
    volumes:
      - fauna-data:/data
    environment:
      FAUNA_MODE: private
      FAUNA_LAN_BIND_IP: 192.168.1.50
      FAUNA_PORT: "3000"
      FAUNA_BIND_ADDR: 127.0.0.1
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
      WATCHTOWER_POLL_INTERVAL: "86400"
      WATCHTOWER_LABEL_ENABLE: "true"
      WATCHTOWER_ROLLING_RESTART: "true"
    logging: *default-logging
    restart: unless-stopped
volumes:
  fauna-data:
```

Lock it down:

```
chmod 600 /opt/fauna/docker-compose.yml
```

**Do not add anything else to this file.** In particular never add `privileged`,
`cap_add`, `user:`, `entrypoint:`, `command:`, or `security_opt` — each one
switches off a protection built into the server image.

There is no virus scanner and no mail ports here. This machine never accepts
mail from the internet directly.

> **If this machine happens to have a public address of its own** (some home
> servers do), remove the `network_mode: host` line and instead publish only to
> your network address, so your mail and calendar are never exposed:
>
> ```yaml
>     ports:
>       - "192.168.1.50:443:443"
>       - "192.168.1.50:993:993"
>       - "192.168.1.50:8443:8443"
> ```

## Step 6 — Start it

```
systemctl enable docker
systemctl start docker
docker compose -f /opt/fauna/docker-compose.yml up -d
docker compose -f /opt/fauna/docker-compose.yml logs -f fauna-nest
```

Press Ctrl-C once it's running.

## Step 7 — Claim it from the app

The nest made its own one-time claim code on first start. Read it:

```
docker compose -f /opt/fauna/docker-compose.yml exec fauna-nest cat /data/claim-code
```

On any device **on the same network**, open `https://192.168.1.50/app/` and
follow the setup screens. Enter the claim code when asked.

Your browser will warn that the connection isn't private. That's expected — this
machine issues its own certificate because it has no public name. Accept the
warning and continue.

Pick any name you like for the part before the `@`. The part **after** the `@`
has to be the address you just opened — `192.168.1.50`. So your handle here is
`you@192.168.1.50`.

That part is not cosmetic: it is how the app knows where to look for your nest.
Give it a domain name instead, like `you@example.com`, and the app goes looking
on the public internet rather than at the machine in front of you, and setup
stops there.

Planning to pair this with a nest on the internet later? Nothing has to match
between the two — you are the same person on both, and the pairing step in the
app is what links them and sets your mail up here. You can pick the same name
before the `@` on both if you find it tidier.

A nest claimed this way has no domain name of its own, and for a machine at
home that is the right resting state, not something to fix later. Its mail
arrives from your nest on the internet, which is where your domain lives.

> **If you are tempted to give this machine your domain anyway, don't.** Your
> domain points at your nest on the internet, so putting it on this machine too
> would leave two machines answering to one name. It is also a one-way door:
> the first domain you add becomes the machine's identity, and no screen in the
> app will take it back off again.

## Step 8 — Connect your calendar and contacts

In the app, turn on **Calendar** and **Contacts** under Admin.

Then in Apple Calendar, Thunderbird, DAVx⁵ or any similar app, add a CalDAV
account:

- **Server:** `https://192.168.1.50:8443`
- **Username and password:** the ones the app shows you

Accept the certificate warning the first time, same as above.

---

## Keeping it running

- **Back it up — with Fauna itself.** This machine holds the only copy of your
  mail and files, so this is the bullet that matters most. You don't set it up
  here; you set it up in the app, on the **Backups** page, by adding a **backup
  destination** — another nest you own. If you followed the
  [internet nest guide](nest-internet-setup.md), that nest is the obvious
  choice: already yours, already on, already paid for. What travels to it is
  encrypted on your own device, so it holds your backup without ever being able
  to read it. A friend's nest works exactly the same way. The whole picture,
  including how to restore after this machine's disk dies, is in
  [Backup with Fauna](cloud-backup.md).
  - Watch the destination's free space — this guide's internet nest asks for
    only 20 GB, which a few years of photos will outgrow.
  - A disk snapshot of the `fauna-data` storage area on a schedule is still
    worth having as a second line of defence, but it isn't the primary answer
    and you can't restore it from the app.
- **Consider encrypting the disk** — anyone who walks off with the machine has
  what's on it.
- **Save the recovery key** the app showed you when you claimed the nest. The
  claim code itself is used up at that moment and deleted — it's the recovery
  key that matters from then on.

The server updates itself once a day.

## If something doesn't work

**The page won't load.** Check the machine's address hasn't changed
(`hostname -I`), and that you're on the same network. There's no remote access
by design.

**Nothing starts.** Re-run the `up -d` command and read what it prints. Usually
a typo in the setup file or a full disk.

**The app says the nest is already claimed, but you never claimed it.** Run:

```
docker compose -f /opt/fauna/docker-compose.yml exec fauna-nest chown fauna:fauna /data/claim-code
docker compose -f /opt/fauna/docker-compose.yml exec -u fauna fauna-nest cat /data/claim-code
```

Then claim with the code it prints.

---

## Next: get your email delivered here

On its own, this nest can't receive email from the internet — nothing outside
can reach your home network. To fix that, you put a small server on the internet
that accepts your mail and immediately hands it to this machine, keeping no
readable copy:

1. Finish this guide.
2. [Set up a nest on the internet](nest-internet-setup.md).
3. [Link the two](nest-relay-setup.md).

Your mail then lives here, at home, and the internet server is only a doorway.

<!--
  ⚠ MAINTAINERS — the compose file in Step 5 is a SHIPPING ARTIFACT.

  It is the private half of the home-with-public-relay deployment and must stay
  functionally in step with docker-compose.home.yml at the repo root (same
  image, same FAUNA_MODE/FAUNA_LAN_BIND_IP/FAUNA_PORT/FAUNA_BIND_ADDR contract,
  same host networking). The differences are deliberate and beginner-facing:
  literal values instead of ${VAR} + .env, and no docker-config mount (the
  image is public). Nothing in the compose names the internet nest — the link
  in nest-relay-setup.md records it on the user's pairing row.

  If you change that compose or the env contract it depends on, change Step 5
  in the same commit. The public sibling, nest-internet-setup.md, is pinned to
  build_cloud_init by an automated test; this one is not, so it needs your
  attention by hand.
-->
