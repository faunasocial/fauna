# Set up a nest from the app — no terminal

> **This is the point-and-click path to a nest on the internet.** You enter a
> handle, pick a server size, and click through — the app creates the server,
> installs Fauna on it, publishes the DNS, fetches a real certificate, and signs
> you in as the owner. **No terminal, no Docker, no SSH, and no claim code to
> type.**
>
> **The server is yours, in your own account, and you pay for it directly.** You
> open an account with a cloud provider, and the app uses a token you give it to
> create the server there on your behalf. Your provider bills you, at their
> normal prices. Fauna does not host anything, does not resell anything, and
> never sees your card — it only drives the provider's controls for you.
>
> It sits alongside two other ways to get a nest:
>
> - *[Join or claim a nest from the app](getting-started.md)* — your first run,
>   when someone else (or a box you already installed) is waiting for you.
> - *[Set up a nest on the internet by hand](nest-internet-setup.md)* — the same
>   server, but you create the box and run the Docker commands yourself. That is
>   the path the Fauna project runs for its own alpha, so it is the most
>   battle-tested; this in-app path is the easy one.
>
> You need **your own domain** (bring one, or buy it inside the app) and a
> **cloud-server account** to build in. Everything else is automatic.

---

## What you're building

```
                the internet
 ┌─────────────────────────────────────────┐
 │  YOUR SERVER, IN YOUR OWN CLOUD ACCOUNT  │
 │  — created for you by the app            │
 │                                          │
 │   • the Fauna app, at                    │
 │     https://yourdomain.com/app/          │
 │   • posts, messages, files, calendar     │
 │   • email @yourdomain.com (optional)     │
 │   • a real TLS certificate, automatic    │
 └─────────────────────────────────────────┘
        ▲ phones, laptops — anywhere
```

The result is identical to the [hand-built internet nest](nest-internet-setup.md)
— a full internet-facing Fauna server that is yours, sealed to keys only your
devices hold. The difference is only in *how you get there*: the app does the
creating, installing, claiming, and DNS instead of you.

## What you need

| You need | Notes |
|---|---|
| **A domain** | Bring one you already own, **or buy one inside the app** while you set up. |
| **A cloud-server account** | Your account with a cloud provider, where the box gets built and which **bills you directly** for it. You'll paste a **Read & Write API token** so the app can create the server on your behalf — the token stays sealed to *your* account, and the finished nest never sees it. |
| **A DNS-provider token** *(optional)* | Hand the app a token for wherever your domain's DNS lives and it publishes every record for you. Don't want to? Choose **Set up later** and paste a couple of records by hand at the end instead. |
| **The Fauna app** | Web, desktop, or mobile — the wizard is the same on all of them. |
| **About 10–15 minutes** | Most of it is waiting for the new server to boot and DNS to catch up. |

> There is **nothing to set up by hand, no server to log into, and no claim code
> to copy.** The app generates the box's one-time claim secret itself, plants it
> on the server as it builds, and uses it to make you the owner the moment the
> box comes online.

---

## How it works, in one breath

Enter your handle → connect your DNS (or defer it) → pick a server → watch it
build → (paste a couple of DNS records, only if you deferred) → you're signed in
as the admin. Six pages, and the app carries the whole thing.

## Step 1 — Enter your handle

Open the Fauna app and start setup as usual: create or import your identity,
then enter your **handle** as **`you@yourdomain.com`** — the name you want, at
the domain you'll use.

When the app checks the handle and finds the domain has **no nest yet**, it
offers to build one for you and switches into the provisioning flow. **The
handle's domain becomes your server's identity** — its web address, its
certificate, and (if you turn it on) its email domain.

## Step 2 — Set up your domain's DNS

The DNS step is where you tell Fauna how your domain's records get published.

- **Let the app do it.** Pick where your domain's DNS lives — **Cloudflare,
  Hetzner, Porkbun, Gandi, or Namecheap** — paste that provider's API token,
  and verify. From here on the app publishes and maintains every record
  itself.
- **Buy the domain here.** Don't have one yet? Choose **Porkbun**, **Gandi**,
  **Namecheap**, or **Cloudflare**, and the app registers the domain for you
  as part of setup (you'll confirm the price first — and the price you'll pay
  each year after that is shown before anything is charged).
- **A name under a domain you already have.** A handle like
  `you@box.example.com` puts the server on a subdomain of `example.com`. The
  handle check tells you the name sits inside `example.com`, and this page
  opens with **Buy domain** unticked: pick the provider that hosts
  `example.com`'s DNS, verify, and the app publishes the server's records
  inside that zone. (Buying a name like `you@example.co.uk`? The check can't
  tell `co.uk` apart from a domain someone holds, so you get the same message.
  Tick **Buy domain** yourself after you verify the registrar.)
- **One company for everything.** A company that implements the open *Fauna
  Bundled Provider API* can register your domain, host its DNS, **and** rent
  you the server — you sign up and pay once, with them. Pick **Bundled
  provider (open API)**, paste the address the company gave you, press **Sign
  in at the provider…** (your browser opens their page; you create the
  account and enter payment details *there* — the app never sees your card),
  then verify as usual. Fauna doesn't endorse or profit from any such company;
  the domain is registered in *your* name and you can move it, and the server,
  away at any time.
- **Set up later.** Prefer to paste records by hand? Press **Set up later** and
  skip this page — the app shows you the exact records to add at the very end,
  once the server is built ("Almost ready", Step 5).

**Why some providers are greyed out.** The two tick-boxes at the top of the page
narrow the list, because not every provider does every job. Ticking **Buy domain
on Continue (this page)** leaves only the ones that can register a domain;
ticking **Buy VPS with same provider (next page)** leaves only the ones that can
also rent you the server. Each greyed-out provider tells you which box is ruling
it out — untick that box and it becomes selectable again. Tick both and only a
bundled provider stays selectable — none of the named providers both registers
domains and rents servers — so unless you have a bundled provider's address,
pick one box or the other.

## Step 3 — Choose your server

Pick where and how big:

- **Provider and location** — choose your cloud account and a region near you.
- **Size** — a short list of sensible server sizes, each showing its CPU,
  memory, disk, and monthly price.
- **Email — on or off.** One toggle decides whether this box is set up for
  **full email** at your domain:
  - **On** (the default for a real domain) — the box is built with everything
    email needs, including a virus scanner and spam filter. Those need memory,
    so the size list only offers plans with **2 GB RAM or more**.
  - **Off** — a leaner, cheaper **social-only** box (posts, messages, files,
    calendar, federation — everything *except* email), which can run on the
    smallest **1 GB** size. **Note:** a social-only box can't add email later
    without moving to a bigger server, so leave email **on** if you might ever
    want it.
- **Updates.** Your server keeps itself up to date automatically; this choice
  decides which versions it follows:
  - **Stable** (the default) — released versions. Recommended.
  - **Test** — release candidates that are still being checked.
  - **Dev** — the newest development builds, before any checking. Expect
    breakage; pick it only for a server you are happy to rebuild.

  The choice is made here, when the server is created.

Turning email on here only *sizes* the box for mail (the memory the virus
scanner and spam filter need) — it doesn't switch mail on by itself. Once you
claim the finished box with your real domain and confirm it's publicly
reachable (the default), the app turns email on for you automatically; see
"After it's up" below.

## Step 4 — Watch it build

Before the build button, a short recap shows what you're about to be charged —
the domain's one-time registration price (if you're buying one) and the
server's monthly price. Both prices are ones you already saw and agreed to
earlier; this is just a reminder, not a new charge.

Press the build button and the app provisions everything, showing four steps in
order:

1. **Domain** — registers or verifies your domain and its DNS.
2. **Server** — creates the cloud server and installs Fauna on it.
3. **DNS** — publishes the records that point your domain at the new box.
4. **Online** — waits for the server to answer as a healthy Fauna nest, then
   signs you in to it as its owner.

Each step shows its progress, and transient hiccups (a slow provider, DNS still
settling) **retry on their own** — you don't have to babysit it. If a step ever
stops on a hard error, a **Retry** picks up where it left off; nothing already
built is redone or lost. This usually takes a few minutes.

*(If you chose **Set up later** in Step 2, the Domain, DNS, and Online steps are
skipped here — there's no DNS for the app to write or check yet. The box is
still built; you finish the DNS in the next step.)*

## Step 5 — "Almost ready"

You reach the **"Almost ready"** screen in either of two situations, and it says
which one you are in.

**If you chose *Set up later*,** it lists the exact DNS records to add at your
registrar — the apex and `mail` address records, plus `MX`, `SPF`, and `DMARC`.
Copy them in, and the app **keeps checking on its own**: as soon as your records
resolve and the fresh box answers, it finishes setup and signs you in
automatically.

**If setup was interrupted** — you closed the app, or it stopped, while your box
was being built — you come back to this same screen with no records to add and a
line telling you your server is starting. Nothing is lost and there is nothing
to type: the app remembers the box it was building, reaches it at its own
address rather than waiting for your domain to spread across the internet, and
signs you in the moment it answers.

Either way you can **close the app and come back later** — it picks up right
where it left off. (There's no email-signing DKIM record here on purpose: your
box generates that key itself after it boots, and you publish it from the app's
**DNS** page once it exists.)

**If your box is never going to answer** — the order failed before it was built,
or you deleted the box at your provider — the screen has a way out: **Use a
different nest**. It forgets the box the app was waiting for and takes you to the
handle page with the same identity you already made, so you can set up a new nest
or sign in to an existing one. If a box was built, the app does not delete it —
it stays at your provider until you remove it there. (The button is switched off
only for the moment the app is actually finishing setup on a box that has just
answered.)

## Step 6 — You're the owner

However you got here — the app finishing on its own or you pasting the last
records — the result is the same: **you land in the app as your nest's admin, no
claim code to type.** Within a minute or two of coming online, the box fetches a
real TLS certificate for your domain from Let's Encrypt automatically — no
certbot, no configuration. Reload and you're on your own Fauna nest, reachable
from anywhere.

Your content is sealed to your devices' keys from that first moment. Nothing on
the box can read it, and nothing is granted by default; if you ever want the box
to do more with your data, you mint the specific grant for it later, from
**Settings → Nests**, and every grant is revocable.

---

## After it's up

Everything from here is the same as any Fauna nest — all in the app, no config
files:

- **Finish and check DNS.** **Admin → DNS** lists every record your deployment
  wants, each with a live check that turns green when it's published. If you
  gave the app a DNS token it keeps them green for you; otherwise this is where
  you confirm your hand-added records.
- **Email.** If you built a mail-ready box, email is already on —
  claiming with a real domain while publicly reachable (the default) turns it
  on automatically, no switch to flip. Open **Settings → Mail** for the
  one-time mailbox password the app generated for you, then publish the records
  the **Admin → DNS** page now asks for (including the DKIM key your box has
  generated). `you@yourdomain.com` is a real, working email address — in your
  Fauna conversations and in any standard mail app. (Chose **Private**
  reachability instead? Email stays off; turn it on yourself from
  **Settings → Mail**.)
- **Back it up.** All your nest's data lives on that one server; snapshot it on a
  schedule with your cloud provider's backup tooling.

For a screen-by-screen map of everything you just unlocked, take
[the tour of the app](app-tour.md) and, for the owner's controls,
[the tour of the admin area](admin-tour.md).

## If something doesn't work

- **The build stalls on "Online" for a long while** — DNS can take from minutes
  to a few hours to propagate. The app keeps retrying; give it time, or check
  the record at your registrar.
- **"Verify" fails on your cloud or DNS token** — make sure the token has
  **write** permission (a read-only token can list your account but can't create
  the server or publish records), and that you pasted it whole.
- **Email doesn't arrive after you turn it on** — most providers block outbound
  port **25** on new accounts until you ask them to unblock it, and inbound mail
  needs your `MX` record green on the **Admin → DNS** page.
- **A DNS provider won't verify in the web app** — see the note in the table
  below; the desktop and mobile apps talk to every provider directly.

## Where this stands today

| Piece | Status |
|---|---|
| The whole in-app flow — handle, DNS, server pick, live build progress, automatic claim, automatic TLS | **Works today** — built into every Fauna app. The hand-built [by-hand path](nest-internet-setup.md) is the one the project runs for its own alpha, so it's the most road-tested; this path is the easy one. |
| Buying a **server** inside the app | **Works today** — Hetzner, DigitalOcean, Vultr, OVH, and Linode. Hetzner is the most road-tested and the recommended default. |
| Buying a **domain** inside the app | **Porkbun, Gandi, Namecheap, and Cloudflare today.** |
| Managed DNS (hand the app a token, it maintains every record) | **Works today** — Cloudflare, Hetzner, Porkbun, Gandi, Namecheap. |
| A free **Fauna-hosted address** (no domain of your own at all) | **Planned** — for now you bring or buy a domain. |
| DNS/server providers in the **web app** | Buying through **Porkbun** and building on **Hetzner** needs nothing extra in the browser. Cloudflare, Namecheap, Gandi (DNS and domain purchase) and Vultr (server) currently verify best from the **desktop or mobile** app, which reaches them directly, until a small helper service for the web app is deployed. |
| One-command installers / packaged app releases | **Planned** — building from source (native apps) or the web app is the way in during alpha. |
