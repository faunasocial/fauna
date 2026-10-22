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
| **An always-on Linux machine on your network** | Any small 64-bit computer, running Ubuntu 26.04 LTS — the commands below use Ubuntu's own packages. It needs at least **1 GB of memory** and 20 GB of disk; give it more disk if it will hold a household's photos and mail, because everything you keep lives here. It does not need a public address, a domain, or anything opened on your router. |
| **Its address on your network** | Something like `192.168.1.50`. Give it a fixed one in your router's settings so it doesn't change. |
| **About 15 minutes** | |

You do **not** need a domain, a registry account, or an access token. The Fauna
server image is public and downloads anonymously.

Throughout, replace `192.168.1.50` with your machine's real address.

**What works:** the Fauna app, calendar and contacts, files, and messages
between your own devices — all served on your network.

**What needs a nest on the internet too:** receiving email from the outside
world, your public posts and web pages, the AT Protocol and ActivityPub
connections, and talking to people on other Fauna servers — anything the outside
world must reach. A machine behind your home router can't accept incoming
connections. The pairing at the end solves most of this: the internet nest receives your email and the messages people on other servers send you and keeps them until your home nest collects them, and it forwards your posts — web pages still need a nest on the internet.

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

There is no Stable release of the nest software yet. Until the first one is out, the **Stable** image this file names has nothing in it, so a machine set up this way has nothing to start; a numbered release is published on the **Test** channel, and the **Dev** channel's development builds are published. The file below is the shape the release will have.

Open a new, empty file:

```
nano /opt/fauna/docker-compose.yml
```

Paste in exactly this, then press **Ctrl-O**, **Enter** to save and **Ctrl-X**
to quit. Replace `192.168.1.50` with your machine's address:

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
      FAUNA_IMAGE_TAG: latest
      FAUNA_LAN_BIND_IP: 192.168.1.50
      FAUNA_PORT: "3000"
      FAUNA_BIND_ADDR: 127.0.0.1
    labels:
      - "social.fauna.update=true"
    logging: *default-logging
    stop_grace_period: 15s
    restart: unless-stopped
  fauna-updater:
    image: ghcr.io/faunasocial/nest:latest
    entrypoint: ["/usr/local/bin/fauna-updater"]
    network_mode: host
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
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

> **If this machine has a public address of its own** (some home servers do),
> don't use this setup on it: it would put your mail and calendar on the
> internet. Put the machine behind your home router first, or set it up as a
> [nest on the internet](nest-internet-setup.md) instead.

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

Planning to pair this with a nest on the internet later? Nothing has to match between the two — you are the same person on both, and linking them from one of the installed Fauna apps is what connects them and sets your mail up here. You can pick the same name
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

Then in Apple Calendar and Contacts, Thunderbird, DAVx⁵ or any similar app, add a CalDAV account for your calendars and a CardDAV account, at the same address, for your contacts:

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
  choice: already yours, already on, already paid for. What travels to it is encrypted — your mail, calendar, contacts and posts by this machine, with a backup key you grant it, and any folder you attach exactly as it is already stored, encrypted — so it holds your backup without being able to read it, apart from a folder you have made public, which is public by design. A friend's nest works exactly the same way. The whole picture, including how to restore after this machine's disk dies — which needs this machine rebuilt with its own identity, as [Rebuilding after a total loss](#rebuilding-after-a-total-loss) shows — is in [Backup with Fauna](cloud-backup.md).
  - Watch the destination's free space — this guide's internet nest asks for
    only 20 GB, which a few years of photos will outgrow.
  - A disk snapshot of the `fauna-data` storage area on a schedule is still
    worth having as a second line of defence, but it isn't the primary answer
    and you can't restore it from the app.
- **Consider encrypting the disk.** Your content is already stored encrypted on the nest, apart from what you publish to everyone, a community room's posts and messages if this nest is its home, and anything you've given the nest permission to read, such as the sign-in of an account you link on another network; and the text of one of your messages, or of a post only some people can open, that someone who can open it attaches to a report. Your nest also sees what any server that delivers your messages must: who you're connected to — your contacts, whom you follow, the groups you're in — and when and how much you send, as well as the sizes and dates of what you store. An encrypted disk keeps all of that from whoever takes the machine.
- **Keep your recovery kit safe** — the code the app showed you once when you created your identity, which the app also calls your recovery phrase. It gets your account back if you lose every device ([Your recovery kit](identity-and-devices.md#your-recovery-kit)); if you skipped it, make one under **Settings → Account → Recovery Kit**. The claim itself leaves no recovery secret for you to keep: the claim code is used up the moment you claim, and your Fauna app keeps a copy of the server's own key for you.

The Fauna software updates itself: on the default Stable builds the machine picks up a new version within the hour, and installs it. Keeping the operating system patched is up to you — the `unattended-upgrades` package from Step 3 installs Ubuntu's security updates, but some of them take effect only after a restart, so restart the machine now and then.

## Rebuilding after a total loss

If this machine's disk dies, or the machine itself is gone, you can build a
new one that is still the same nest. Your Fauna app keeps a copy of the
server's own key, and a new machine started with that key is the one your
installed Fauna apps already know: they reconnect to it as the same server, with no
warning that its identity changed.

1. Follow this guide again on the new machine, from Step 1, and in your router's
   settings give it the address the old machine had (`192.168.1.50`): your
   handle names that address, and it is where your apps look for your nest.
2. At Step 5, before you save the setup file, open an installed Fauna app (the web app
   is served by the machine you lost, so it can't open until the new one is
   running) and choose **Recover a lost box** (it appears on the "Couldn't reach your nest" screen,
   and on the screen of a newly installed app where you choose **Create New
   Identity**). Pick this nest, then
   **Install on my own server**, and copy the line it shows. It starts with
   `FAUNA_DEPLOYMENT_SEED=` and goes on with 64 letters and digits.
3. In the setup file, add one line at the end of the `environment:` block
   under `fauna-nest`, lined up with the lines above it and written like
   them — the name, a colon and a space, then the value in double quotes, as
   on the `FAUNA_PORT` line:

   ```yaml
         FAUNA_DEPLOYMENT_SEED: "PASTE-HERE"
   ```

   Replace `PASTE-HERE` with everything after the `=` in the line you copied.
   This line is the one exception to Step 5's "do not add anything else". It
   has to be there the first time the machine starts: a machine that starts
   without it makes a new key of its own and ignores the line from then on.
   The line is your server's key, so keep the file locked down as Step 5
   says.
4. Carry on through Step 8. Your browser, when you open the web app at
   Step 7, and your mail, calendar and contacts apps warn about the new
   machine's certificate once, as they did the first time; accept it.
5. Bring your data back from your backup destination, as
   [Backup with Fauna](cloud-backup.md) describes.
6. If this machine was linked to a nest on the internet, link them again,
   as Step 1 of [Link a home nest to an internet nest](nest-relay-setup.md)
   shows: the link was kept on the machine you lost, and until you redo it
   this machine doesn't collect your mail.

## If something doesn't work

**The page won't load.** Check the machine's address hasn't changed
(`hostname -I`), and that you're on the same network. There's no remote access
by design.

**Nothing starts.** Re-run the `up -d` command and read what it prints. Until the first Stable release is out, it is because there is no Stable image to download yet (see Step 5); otherwise it's usually a typo in the setup file or a full disk.

---

## Next: get your email delivered here

On its own, this nest can't receive email from the internet — nothing outside
can reach your home network. To fix that, you put a small server on the internet that accepts your mail and keeps it, encrypted, until this machine collects it a few seconds later — set up as the relay guide shows, it keeps no readable copy of the mail you receive:

1. Finish this guide.
2. [Set up a nest on the internet](nest-internet-setup.md).
3. [Link the two](nest-relay-setup.md).

Your mail then lives here, at home, and the internet server is only a doorway.

