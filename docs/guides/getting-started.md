# Getting started with Fauna

> **What this covers:** your first run of the Fauna app — creating an identity,
> joining a server, and signing in on a second device. It applies to every
> Fauna app: web, Linux, macOS, iOS, Android, and Windows — the screens and
> steps are the same on all of them.

## Three words

- **Identity** — who you are. A cryptographic key pair that only you hold.
  There is no password and no e-mail signup; the key *is* the account.
- **Nest** — a Fauna server. It stores and forwards your content, but it is
  not an authority: everything you publish is signed by your identity, and
  every app checks the signatures itself. Anyone can run a nest.
- **Handle** — your name on the network, written like an e-mail address:
  `you@somedomain.com`. The domain part is the nest you live on.

## What you need

1. **The Fauna app** — see [Installing the app](install.md). The quickest
   path is the web app, which every nest serves itself at
   `https://<the-nest's-domain>/app/`.
2. **A nest to join** — one of:
   - an **invite** from someone who runs a nest,
   - **your own nest** — see [Set up a nest on the internet](nest-internet-setup.md)
     or [Set up a nest at home](nest-home-setup.md),
   - or just a machine in front of you, for trying things out.

## Step 1 — Create your identity

On first launch the app asks: **Create** a new identity or **Import** an
existing one.

Choose **Create**. The app generates your key pair locally and shows you the
**secret** — a long hex string.

> **Save the secret now, somewhere safe** — a password manager is ideal. It is
> the only way to sign in on another device or to recover access. It never
> leaves your device unencrypted, and nobody — not the nest, not the Fauna
> project — can reset or recover it for you.

Confirm, and the app stores the secret in your device's secure storage (the
platform keychain; on web, the browser's local storage).

## Step 2 — Enter your handle

Type the handle you want, e.g. `alice@yourdomain.com`, and press **Check**.
The app probes the domain and tells you exactly what it found and what happens
next. The common cases:

| The app finds | What happens when you continue |
|---|---|
| A nest, with your name free | You request an invite (Step 3a). |
| A brand-new nest with no owner yet | You claim it and become its admin (Step 3b). |
| Your name already registered there | You sign in (it verifies your key). |
| No nest at that domain | It tells you — check the spelling, or set the nest up first. |

**Trying it out locally?** A handle can also point at a machine instead of a
domain: `alice@192.168.1.50`, `alice@pi.local`, or `alice@localhost` all work,
and skip the domain checks. This is how you reach a nest on your own network
— see [the home-nest guide](nest-home-setup.md).

## Step 3a — Join with an invite

If the nest already has an owner, membership is by invitation. The invite page
gives you both ways in:

- **Ask the admin:** press **Request invite**. The nest's admin sees the
  request in their app and approves or declines it; the page tells you where
  your request stands, and **Check again** asks right now if you are impatient.
  Your request survives closing the app — reopen it and you are back on this
  page.
- **Have an invite code?** If someone sent you a code out-of-band (chat,
  e-mail, paper), enter it in the code field and check it — a valid code lets
  you in immediately, no waiting for review. Press **Continue** to redeem it:
  the app registers your handle on the nest and signs you in.

**On a phone, the app may ask your app store for your age range.** The iPhone
and Android apps ask the store (Declared Age Range on iPhone, Play on Android)
during this step, before your request or code reaches the nest. The store may
show its own prompt, and you can decline. If a range is shared, it goes to the
nest's admin with your request, marked as verified by the store or only as
declared, and on iPhone a line on the invite page tells you so before you press
anything. It only helps the admin decide; it never lets you in
or keeps you out by itself. If nothing is shared, nothing is sent and the line
does not appear.

**When the admin approves, the app signs you in by itself.** There is nothing to
press and nothing to keep an eye on — leave the page open and it moves you into
the app on its own. If the admin declines, the page says so and gives their
reason, and you can send a fresh request from the same page.

Either way in — a code you redeemed or a request that was approved — the last
thing you see before the app is the optional **trust this box?** offer
described at the end of the next step. It is the same offer, it means the same
thing, and declining it is just as free. You see it once, when you join; simply
signing in again later does not ask again.

## Step 3b — Claim a new nest

If the handle points at a freshly-installed nest that nobody owns yet, the
wizard switches to the **claim** step: enter the one-time **claim code** that
the nest printed when it was set up (the person who installed it has it; if
that's you, see the self-host guide's claim step). Hyphens or not, upper- or
lowercase — the code is accepted either way.

If instead your handle's domain has **no nest at all yet**, the wizard offers to
**build one for you** — it creates a small server in your own cloud account,
installs Fauna on it, and claims it as yours automatically, no terminal
required. Your cloud provider bills you directly for the server. See
*[Set up a nest from the app](nest-app-setup.md)*.

Claiming makes you the nest's **admin** and registers your handle in one
stroke. The wizard then asks one setup question — how it's connected to the
internet — and drops you into the app as its signed-in owner. Your content is
sealed on the nest from that moment on; there's no storage question to
answer, and no default that lets the box read it.

Right at the end — of claiming a nest, and equally of joining one in Step 3a —
you may be offered one optional extra: **trust this box?**
Saying yes lets the nest read the things it looks after for you — filtering
your mail as it arrives, serving your calendar to your other devices — and
that trust renews itself while you use Fauna, so it doesn't quietly run out.
Saying no changes nothing at all, and nothing is lost by
declining: those jobs simply keep happening on your own devices instead. Either
way the answer is not final. Everything you have trusted a nest with is listed
under **Settings → Nests**, where you can withdraw it at any time or grant it
there later, one purpose at a time.

## Step 4 — Your second device

On the new device, choose **Import** instead of Create, then either:

- **Paste the secret** you saved in Step 1, and type your handle, or
- **Scan a QR code** — on a device you are already signed in on, open
  **Settings → Account → Export Identity** and press *Show QR Code*. The code
  carries both your secret and your handle, so the new device pre-fills the
  handle step for you.

The QR stays hidden until you press *Show QR Code*, and a warning appears
beside it while it is on screen. That is deliberate: **anyone who scans that
code gets your identity.** Only reveal it when nobody is looking over your
shoulder — and never while sharing your screen.

Same identity, same handle, both devices — end-to-end-encrypted conversations
sync across your devices.

## You're in

What you'll find inside: the **feed** (posts from people you follow — on your
nest and, via bridges, on Bluesky and the Fediverse), **conversations**
(end-to-end-encrypted messaging, which is also where e-mail lands if your nest
has it enabled), **files**, **calendar**, and **settings**. If you claimed the
nest, you also get the **admin** area — invites, domains and DNS, mail, and
moderation all live there, in the app. Fauna has no server-side config files
to edit: if you can choose it, you choose it in the app.

For a screen-by-screen map of everything you just walked into, take
[the tour of the app](app-tour.md) (and, for admins,
[the tour of the admin area](admin-tour.md)).

## If something doesn't work

- **The handle check fails on a domain you know is right** — the nest may not
  be reachable yet (DNS still propagating, or the server is down). The message
  under the check says which probe failed.
- **The check warns about the connection on a local address** — a local nest
  serves a self-signed certificate; the app authenticates it cryptographically
  on first contact. That warning is expected for `@<ip>` / `@*.local` targets.
- **You lost your secret** — if you are still signed in somewhere, that device
  can show it again (as the QR/secret under identity settings). If you are
  signed in nowhere, the identity is unrecoverable — create a new one. This is
  the flip side of nobody-can-reset-your-account.
