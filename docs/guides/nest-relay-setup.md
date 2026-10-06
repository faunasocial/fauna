# Link a home nest to an internet nest

This connects two nests you have already set up, so that **your email arrives at
home** instead of living on a rented server.

The internet nest becomes a doorway. Mail from the outside world arrives there,
is immediately handed to the machine in your home, and the doorway then deletes
its copy. It never holds anything it could read.

```
   the internet                 your home
 ┌───────────────┐          ┌───────────────┐
 │ internet nest │  hands   │   home nest   │
 │  the doorway  │ ───────▶ │   your mail   │
 │               │ it over  │   lives here  │
 │ keeps no      │          │               │
 │ readable copy │          │  your devices │
 └───────────────┘          └───────────────┘
```

You send mail out through the doorway, and read it from the machine at home.

---

## Before you start

Do both of these first, in this order. This guide does not repeat their steps.

1. **[Set up a nest on the internet](nest-internet-setup.md)** — including email,
   with the DNS finished and a test message working.
2. **[Set up a nest at home](nest-home-setup.md)** — claimed at its own address
   on your network, so its handle is `you@192.168.1.50`, not `you@example.com`.
   The part after the `@` is how the app finds a nest, so a home machine has to
   be claimed at the address you reach it on. The home guide covers this.

**You don't have to make anything match by hand.** Linking the two nests in
Step 1 below is what carries your mail setup from the internet nest to the home
one — including the key the home machine needs to open your mail. That is the
one thing that has to be the same on both, and the app copies it for you.

> **A note on which app to use.** Do the linking below from an **installed**
> Fauna app — Linux, Windows, macOS, iOS or Android. The web app can link nests
> but can't yet finish the mail part automatically, so you'd be left doing it by
> hand. Any installed app works; you only need it for this one guide.

---

## Step 1 — Link the two nests

You do this once, from the internet nest, while you are on your home network
(the app has to reach both nests).

1. Open your Fauna app and connect to the **internet** nest, at its usual
   internet address (`https://example.com`).
2. Go to **Settings → Nests**.
3. Add the home nest — type its address, `https://192.168.1.50`.

That one step links the two nests in both directions: it tells the doorway it
may hand your mail over, and it tells the home nest where the doorway is — the
address you connected to in step 1 — so the home nest starts collecting your
mail from there. Nothing else ever tells the home nest about the internet one;
there is no setting for it on the home machine. Afterwards each nest shows the
other in its **Settings → Nests** list.

The same step also sets up your mailbox on the home nest automatically, using
the same keys as the internet one. That's what makes your mail readable at
home, and it's why this is worth doing from an installed app. It sets the
mailbox up on the nest whose address you typed, so start from the internet
nest and type the home one — not the other way round.

## Step 2 — Stop the internet nest serving your mail

Your mail should only ever be read from home.

1. With the app connected to the **internet** nest, open **Settings → Mail**.
2. Turn **off** "serve my mail here".

This doesn't affect handing mail over to your home nest — that uses a different
path. It just closes the door on reading mail from the rented server.

## Step 3 — Point your mail app at both nests

This is the one part that looks unusual: **incoming and outgoing use different
servers**.

| | |
|---|---|
| **Incoming (IMAP)** | `192.168.1.50`, port 993, SSL/TLS — your **home** nest |
| **Outgoing (SMTP)** | `mail.example.com`, port 465, SSL/TLS — your **internet** nest |
| **Username** | the one the app shows you, on the **Mail** settings page of each nest |
| **Password** | the one the app showed you when it set that nest's mail up |

Your calendar and contacts also come from the home nest, at
`https://192.168.1.50:8443`.

Your mail app will warn about the home nest's certificate, because that machine
issues its own. Accept it once.

## Step 4 — Check it works

1. **Send yourself mail from outside** — from a Gmail account, say. It should
   appear in your mail app shortly, readable; the home nest collects new mail
   every few seconds.
2. **Send a message out** and confirm it arrives, and that a copy shows up in
   your Sent folder at home.

There is nothing to check on the doorway itself. It deletes its sealed copy as
soon as the home nest confirms it has the message, and with "serve my mail
here" off (Step 2) no mail app can read mail from it either way — so pointing a
mail app at the internet nest tells you nothing about what it holds.

---

## Before you rely on it

- **Keep the home machine on.** If it's off, mail waits at the doorway — sealed,
  unreadable, and not lost — and arrives when the machine comes back. But it
  waits indefinitely, so don't leave it off for weeks.
- **Back up the home machine.** It holds the only copy of your mail and the only
  keys that can open it. Relaying is not backing up — the internet nest
  deliberately keeps nothing of what passes through it. You can, however, add
  that same nest as a **backup destination**, which is a separate and deliberate
  choice: it then holds encrypted chunks it still cannot read. That is the
  recommended way to protect the home machine — see
  [Backup with Fauna](cloud-backup.md).
- **Check the reverse name** for your internet nest, set in
  [its guide](nest-internet-setup.md). Mail providers are strict about it and a
  mismatch quietly sends you to spam.
- **A brand-new server address has no reputation.** Expect a few weeks of
  occasional spam-filing while it warms up. Sending a normal, low volume is
  exactly the right way through that.

## If something doesn't work

**Mail arrives but you can't read it — it looks like gibberish, or the app
reports an error.** The home nest has no mailbox set up with your keys. This
happens when the linking was done from the web app, or from the home nest
instead of the internet one, or when the home nest was set up under a different
identity than the internet nest. Check that you are the same person on both
nests, then redo Step 1 from an installed app, connected to the internet nest.

**Nothing arrives at home, but mail reaches the internet nest.** The link in
Step 1 didn't take on one side. Open **Settings → Nests** on each nest and
confirm it lists the other; if either doesn't, redo Step 1.

**Mail stays readable on the internet nest.** Step 2 didn't take. Re-check that
"serve my mail here" is off there.

**Your posts don't show up on the internet nest.** Open **Settings → Nests** on
the home nest. If posts are waiting to reach your relay, it says so at the top
of the page, with the reason the last attempt failed. The usual cause is that
the internet nest hasn't been told it may forward your posts — redo Step 1,
which grants that, then press **Retry now** on the home nest's page rather
than waiting for the next automatic attempt. **Stop forwarding these** drops
the waiting posts from the relay queue; they stay on the home nest.

**Everything works but mail is slow to appear.** The home nest checks for new
mail on a short cycle, so a few seconds is normal. Minutes usually means the
home machine dropped off the network.
