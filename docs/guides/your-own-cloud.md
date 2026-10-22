# Your own cloud — files, photos and backups on a server you control

> **Status — this guide describes Fauna as it is taking shape.** Most of what you read here is built and tested today; some pieces are still being built or planned. Each companion guide ends with a **"Where this stands"** table that says plainly where each piece stands.

---

> **What this is.** The case for moving your files, photos, and backups off commercial cloud storage and onto a server you control, running Fauna. It links to five companion guides that each cover one job in depth:
>
> 1. **[Your files on every device](cloud-sync.md)** — sync, network drives, offline.
> 2. **[Backup — your own, and with friends](cloud-backup.md)** — snapshots, restore, and letting a friend hold your backup without being able to read it, apart from a folder you have made public.
> 3. **[Photos and media](cloud-photos.md)** — automatic camera-roll backup from iPhone and Android, and browsing your media anywhere.
> 4. **[Sharing files and running a family nest](cloud-sharing-family.md)** — sharing with other people, and putting your household on one server.
> 5. **[File sync FAQ](file-sync-faq.md)** — working together in shared folders: simultaneous edits, conflicts and merging, Office and VM disks, sync timing.
>
> **Who it's for.** Someone paying for one or more cloud-storage subscriptions
> who is willing to rent a small server (or run one at home) to stop.

---

## The bill you're paying today

A typical household pays for several of these at once (ballpark prices, mid-2026):

| Service | What it does | Typical price |
|---|---|---|
| Google One 2 TB | Drive, Photos, Gmail storage | ≈ $10 / month |
| Dropbox Plus 2 TB | File sync and sharing | ≈ $12 / month |
| Microsoft 365 Personal (1 TB) | OneDrive + Office | ≈ $10 / month |

Two of those together run **$240+ a year, every year** — and the price has only
ever moved in one direction. In exchange, your files sit on someone else's
computers, readable by that company (end-to-end encryption is absent or opt-in
almost everywhere), tied to accounts that can be locked by a mistaken automated
flag, with quotas designed to push you to the next tier.

## What Fauna replaces it with

```
  your devices                      your nest (a server YOU control)
 ┌──────────────────────┐              ┌────────────────────────────┐
 │ laptop, phone, NAS…  │              │  a small rented VPS, or a  │
 │                      │◄──encrypted──►  box at home — your files, │
 │ hold your key; files │     sync     │  photos, backups, mail,    │
 │ are encrypted before │              │  calendar, stored          │
 │ they leave by default│              │  encrypted on your nest    │
 └──────────────────────┘              │  by default.               │
                                       └────────────────────────────┘
```

One Fauna server (a **nest**) does the jobs you're paying separate subscriptions for: file sync across devices, automatic photo backup from your phones, versioned backups, file sharing, media browsing — plus email, calendar, contacts, messaging, and social feeds, which no storage subscription gives you at all. The same Fauna apps — on your computer, on your phone, in a browser or in a terminal — drive all of it.

### What it costs instead

Ballpark, mid-2026:

- **A small VPS** (2 vCPU / 4 GB RAM / 40–80 GB disk): **€5–10 / month.** Plenty
  for a personal nest: mail, calendar, sync, and a modest file/photo library.
- **A domain name:** **≈ €10–15 / year.** Your address on the internet
  (`you@yourname.example` for mail *and* your handle).
- **For multi-terabyte libraries**, two good options:
  - a storage-heavy server (≈ €10–25 / month gets hundreds of GB to a few TB,
    depending on provider), or
  - **the sweet spot: keep the bulk at home.** A nest on a mini-PC or NAS with
    cheap multi-terabyte disks, paired with a small VPS as its public front
    door. Disk you own outright costs ~€20 *once* per terabyte, not per month.
    See [Set up a nest at home](nest-home-setup.md), which serves your files on your own network, and [Link a home nest to an internet nest](nest-relay-setup.md), which has the small server receive your email and keep it until the nest at home collects it.

So the steady state is roughly **€70–130 a year** for the VPS-only shape —
replacing subscriptions that cost twice that or more — and closer to just the
domain fee if your storage lives at home. You are trading a little setup work
(the setup guides above walk through it) for ownership.

## The trust model, in plain words

This is the part no mainstream cloud offers, and it's worth understanding
because it is *the* reason Fauna works the way it does:

- **Your identity is one key**, created on your device when you first sign up — a key on your devices, not an account with anyone. There is no account password, so no server stores a password that would let someone sign in as you, and there is no "reset my password" support desk that can be talked into handing over your account. You move the key to each new device yourself: paste it, or scan it as a QR code from a device you are already signed in on.
- **The Fauna apps encrypt your files on your device before uploading them**, except in a folder you make public; a file you save into a folder over WebDAV is encrypted by your nest as it arrives. The nest stores content-addressed encrypted chunks. Identical data within a folder is stored once (deduplication), only changed pieces are re-uploaded, and the server can verify integrity — all without being able to read the contents, unless you choose otherwise: a folder you make public, serve over WebDAV or put behind a paywall is readable by the box that serves it.
- **You choose how much the box gets to do with your data — any time, not just at setup.** Every nest — right down to a rented VPS — stores your content encrypted, apart from what you publish to everyone, which is public by design. At the end of claiming or joining a nest you may be offered **Trust this box?**; it's optional, and **Not now** changes nothing. When you want a box to do more — read your mail again to re-run its spam and virus checks when they are updated, for example (mail from outside your nest is checked once as it arrives either way) — you give it permission for that one job from Settings → Nests (**Add trust…**) — or by subscribing to a community labeler that reads your mail, which gets its own trust for that one labeler. Nothing is granted until you ask for it, and you can take it back the same way. Linking an account on another network gives your nest that account's sign-in until you unlink it. Connecting a standard mail, calendar or contacts app lets your nest open what that app reads while it is signed in. Mail and messages your nest passes to and from email and other networks arrive readable and are stored encrypted as they land, and a group may choose to be a community room, which its home nest can read. When your app shows a preview of a link that stands on a line of its own, in a post or a message, it asks your nest to fetch that link, so your nest learns the link's address. And someone who can open one of your messages, or a post only some people can open, can attach its text to a report, which the admins it goes to can then read. And your nest sees what any server that delivers your messages must: who you're connected to — your contacts, whom you follow, the groups you're in — and when and how much you send.
- **The flip side, stated honestly: whoever holds your key can act as you, and losing it needs a way back.** Make a **recovery kit** — a second key, kept only by you and only offline, that gets your account back if you lose your key or someone steals it — and keep the key itself on at least two devices. If you lose the key from *all* your devices and copies and have no recovery kit, no one — not you, not the nest, not the Fauna project — can recover your encrypted data. The [backup guide](cloud-backup.md) § "The key that matters more than the backup" covers this.

## Quotas — your disk is the limit

Commercial clouds sell you a number: 200 GB, 2 TB. On your own nest there is no
number to buy. Storage accounting exists — every account on a nest has a
**tier** with a storage cap, and the cap is genuinely enforced — but *you* are
the admin who defines the tiers. For yourself, set it as high as your disk
allows; the real limit is the hardware you chose. Tiers matter when you host
*other people* — family members, or a friend storing backups on your box — so
one person's photo library can't eat the whole disk. See
[sharing & family](cloud-sharing-family.md).

There is no per-file size ceiling worth worrying about: files are chunked, so
even huge disk images sync fine; a single file is bounded only by the storage
cap.

## How it compares

| | **Fauna (your nest)** | Google One | Dropbox | OneDrive | Nextcloud (self-hosted) |
|---|---|---|---|---|---|
| Who can read your files | **Only you** and the people you share with (end-to-end encrypted by default) | Google | Dropbox | Microsoft | Your server can (E2EE add-on is partial) |
| Runs on a server you control | **Yes** (a rented VPS or a box at home) | No | No | No | Yes |
| Multi-device file sync | **Yes** — desktop apps, your phone's Files app, and network-drive access | Yes | Yes | Yes | Yes |
| Automatic phone photo backup | **Yes** (iPhone and Android) | Android + iPhone | Yes | Yes | Yes (app) |
| Versioned backups you control | **Yes** — snapshot history with retention you set | Limited | 30–180 days | 30 days | Apps/add-ons |
| Backup to a friend's server, unreadable by them | **Yes**, apart from a folder you have made public | No | No | No | No |
| Share to another person | **Yes** — to their Fauna identity; removing them rotates the folder's key, so nothing added afterwards reaches them | Yes | Yes | Yes | Yes |
| "Anyone with the link" links | **Yes** — links that expire when you choose and that you can revoke; a private file's link carries its own key | Yes | Yes | Yes | Yes |
| Also included: mail, calendar, contacts, messaging, social | **Yes** | Gmail separate | No | No | Via many add-on apps |
| Monthly fee scales with storage | **No** — your disk | Yes | Yes | Yes | No |
| Someone else runs it for you | No — you do (guided) | Yes | Yes | Yes | No |

That last row is the honest trade: Fauna's setup guides make it approachable,
but you are the admin of your own service in the way a Dropbox customer is
not. The rest of this guide series exists to show that the day-to-day
experience — once the nest is running — is the same "it just works" you're used
to, without the subscription or the surveillance.

## Where this stands

Fauna is in closed alpha. The per-feature status tables live at the end of each companion guide — [sync](cloud-sync.md#where-this-stands-today), [backup](cloud-backup.md#where-this-stands-today), [photos](cloud-photos.md#where-this-stands-today), [sharing & family](cloud-sharing-family.md#where-this-stands-today), [the file sync FAQ](file-sync-faq.md#where-this-stands-today). The short version: sync, photo backup, snapshots, backup copies on a nest you back up to — a friend's, say — or on one of your own devices, one-click restore from such a copy after losing your nest (from a nest's copy, once the lost nest is rebuilt with its own identity — **Recover a lost box** in the app does this, or gives a server you set up by hand the line to rebuild it), sharing — with other people, read-only or read-write, and by link — and the per-file version history browser are built and tested; the companion guides' tables name what is not available yet.
