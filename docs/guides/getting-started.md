# Getting started with Fauna

> **What this covers:** your first run of the Fauna app — creating an identity, joining a server, and signing in on a second device. It applies to every Fauna app — web, Linux, macOS, Windows, iOS, Android and the terminal app. The steps are the same everywhere, apart from the few noted below.

## Three words

- **Identity** — who you are. A cryptographic key pair that only you hold.
  There is no account password and no e-mail signup; your key proves who you are.
- **Nest** — a Fauna server. It stores and forwards your content, but it is
  not an authority: everything you publish is signed by your identity, and
  every app checks the signatures itself. Anyone can run a nest.
- **Handle** — your name on the network, written like an e-mail address:
  `you@somedomain.com`. The domain part is the nest you live on.

## What you need

1. **The Fauna app** — see [Installing the app](install.md). The quickest path is the web app, which a nest serves itself by default at `https://<the-nest's-domain>/app/`.
2. **A nest to join** — one of:
   - an **invite** from someone who runs a nest,
   - **your own nest** — see [Set up a nest on the internet](nest-internet-setup.md)
     or [Set up a nest at home](nest-home-setup.md),
   - or just a machine in front of you, for trying things out.

## Step 1 — Create your identity

In the web app, and in the terminal app on a server or over SSH, the very first screen is **Protect your credentials**: choose a passphrase, typed twice. The app asks for it again at each launch (in a browser, once per session, as long as one Fauna tab stays open), and nobody can reset it if you forget it, so pick one you will remember.

On first launch the app asks you to choose: **Create New Identity** or **Import from Another Device**.

Choose **Create New Identity**. The app generates your key pair locally and shows you the **secret** — a long hex string.

> **Save the secret now, somewhere safe** — a password manager is ideal. It is how you sign in on another device. The app never sends it anywhere by itself, and nobody — not the nest, not the Fauna project — can reset or recover it for you.

Press **Continue**, and the app stores the secret in your device's secure storage (the system keychain — or, in the web app and in the terminal app without one, encrypted with the passphrase you chose).

Next the app offers your **recovery kit**: a second key, kept only by you and only offline, that can get your account back if you lose your secret — or take it back if someone steals it. It is shown once. You can choose **Skip for now**, but we strongly suggest saving it; it finishes activating when your account comes online at the end of setup, once you have joined or claimed a nest in the steps below. See [Your recovery kit](identity-and-devices.md#your-recovery-kit).

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

If the nest already has an owner, its admin decides who can join, usually by invitation. The invite page gives you both ways in:

- **Ask the admin:** press **Request invite**. The nest's admin sees the
  request in their app and approves or declines it; the page tells you where
  your request stands, and **Check again** asks right now if you are impatient.
  Your request survives closing the app — reopen it and you are back on this
  page.
- **Have an invite code?** If someone sent you a code out-of-band (chat, e-mail, paper), enter it in the code field and check it — a valid code lets you in immediately, no waiting for review. Press **Continue** to redeem it: the app registers your handle on the nest and signs you in. A code works only while the nest lets people sign up with one: a new nest starts set to admit nobody on their own, and until its admin changes that, a code is refused — ask them to, or press **Request invite** instead. A code for the nest's free tier is also refused once the nest has as many free accounts as its admin allows, and any code is refused while the nest accepts only sign-ups carrying an age range a store has verified, which no app can send yet.

**On an Android phone, the app may ask Google Play for your age range.** It asks during this step, before your request or code reaches the nest. Google Play may show its own prompt, and you can decline. If a range is shared, it goes to the nest with your request or code, marked as declared — the store's verification of it is not switched on yet — and a line on the invite page tells you so. With a request, the nest's admin sees it, and it only helps them decide. With an invite code it is not kept, but it can decide for you. If the range says you are a minor, an ordinary code does not let you in; a code the nest's admin made with your guardian named on it does, and so does a join request the admin approves with your guardian picked. A nest can also be set to accept codes only together with a range the store has verified. If nothing is shared, nothing is sent and the line does not appear.

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

If instead your handle's domain has **no nest at all yet**, the wizard offers to **build one for you** — it creates a small server in your own cloud account, installs Fauna on it, and claims it as yours automatically, no terminal required. A browser cannot trust a brand-new server the way the other apps can, so from the web app the build may not finish yet; build it from a desktop, phone or terminal app. Your cloud provider bills you directly for the server. See *[Set up a nest from the app](nest-app-setup.md)*.

Claiming makes you the nest's **admin** and registers your handle in one
stroke. The wizard then asks one setup question — how it's connected to the
internet — and drops you into the app as its signed-in owner. Your content is stored encrypted on the nest from that moment on, apart from what you publish to everyone, which is public by design, and a group that chooses to be a community room, which its home nest can read; there's no storage question to answer, and beyond the mail and messages your nest passes to and from email and other networks, no default that lets the box read it. Your nest sees what any server that delivers your messages must: who you're connected to — your contacts, whom you follow, the groups you're in — and when and how much you send. And a link standing on a line of its own in a post or a message is fetched for its preview by the nest of each person who reads it, so that nest learns the link's address. And someone who can open one of your messages, or a post only some people can open, can attach its text to a report, which the admins it goes to can then read.

Right at the end — of claiming a nest, and equally of joining one in Step 3a —
you may be offered one optional extra: **trust this box?**
Saying yes lets the nest read your mail after it has arrived, to run its virus and spam checks on it again when those checks are updated (mail from outside your nest is checked once as it arrives, whatever you answer), and read your calendar for itself — a community labeler that reads your mail gets its own trust when you subscribe to it, and a calendar or mail app you connect works with its own password either way — and that trust renews itself while you use Fauna, so it doesn't quietly run out. Saying no changes nothing you have now: mail from outside your nest is still checked as it arrives, and only those later re-checks wait until you say yes. Either
way the answer is not final. Everything you have trusted a nest with is listed
under **Settings → Nests**, where you can withdraw it at any time or grant it
there later, one purpose at a time.

## Step 4 — Your second device

On the new device, choose **Import from Another Device** instead, then either:

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
- **Your browser warns about the certificate of a nest on your own network** — a nest at `@<ip>` or `@*.local` serves a certificate it made itself, which the browser cannot vouch for, so that warning is expected. The installed apps don't rely on the certificate: the first time they connect they remember the nest's identity, and they warn you if it ever changes.
- **You lost your secret** — if you are still signed in somewhere, that device can show it again (**Settings → Account → Export Identity**). If you are signed in nowhere, use your recovery kit: on the screen where you choose **Create New Identity**, choose **Restore my account from a recovery phrase** — see [Your recovery kit](identity-and-devices.md#your-recovery-kit). Without a kit or a saved copy of the secret, the identity cannot be recovered — create a new one. This is the flip side of nobody-can-reset-your-account.
- **The Android app shows a full screen saying a change to it needs your parent's approval** — if Google Play reports that a parent must approve a change to the app, the app covers its whole screen until they do. The screen says whether it is still waiting for your parent to say yes or your parent said no. Google Play asks your parent for you; the app checks again each time you come back to it and opens as usual once your parent has said yes. This is Google Play's own parental approval, separate from a Fauna guardian's controls, and it is about the person Google Play knows on this device rather than your Fauna account, so the screen offers no sign-out. Fauna on a computer or in a browser never shows this screen.
