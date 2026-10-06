# Your own cloud — replacing Dropbox, iCloud and Google Drive with Fauna

> **Status — this guide describes Fauna as it is taking shape.** Most of what
> you read here is built and tested today; some pieces are still landing. The
> body reads as the finished product so you can judge the destination, and each
> guide ends with a **"Where this stands"** table that says plainly what is
> available now, what is landing, and what is planned. Nothing here is
> speculation beyond Fauna's ratified design documents.

---

> **What this is.** The case for moving your files, photos, and backups off
> commercial cloud storage and onto a server you own, running Fauna. It links to
> four companion guides that each cover one job in depth:
>
> 1. **[Your files on every device](cloud-sync.md)** — sync, network drives, offline.
> 2. **[Backup — your own, and with friends](cloud-backup.md)** — snapshots, restore, and letting a friend hold your (unreadable) backup.
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
| iCloud+ 2 TB | Photo backup, device sync, family sharing | ≈ $11 / month |
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
  your devices                          your nest (a server YOU own)
 ┌──────────────────────┐              ┌────────────────────────────┐
 │ laptop, phone, NAS…  │              │  a small rented VPS, or a  │
 │                      │◄──encrypted──►  box at home — your files, │
 │ hold your ONE key;   │     sync     │  photos, backups, mail,    │
 │ files are encrypted  │              │  calendar. Stores what YOU │
 │ before they leave    │              │  can't read? Impossible —  │
 └──────────────────────┘              │  it stores ciphertext.     │
                                       └────────────────────────────┘
```

One Fauna server (a **nest**) does the jobs you're paying separate
subscriptions for: file sync across devices, automatic photo backup from your
phones, versioned backups, file sharing, media browsing — plus email, calendar,
contacts, messaging, and social feeds, which no storage subscription gives you
at all. The same apps (Windows, macOS, Linux, iPhone, Android, and web) drive
all of it.

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
    See [Set up a nest at home](nest-home-setup.md); a companion
    home-relay guide (mail flowing through the public box) publishes when its
    fully in-app flow lands.

So the steady state is roughly **€70–130 a year** for the VPS-only shape —
replacing subscriptions that cost twice that or more — and closer to just the
domain fee if your storage lives at home. You are trading a little setup work
(the setup guides above walk through it) for ownership.

## The trust model, in plain words

This is the part no mainstream cloud offers, and it's worth understanding
because it is *the* reason Fauna works the way it does:

- **Your identity is one key**, created on your device when you first sign up.
  It never leaves your devices. There is no password, so there is no password
  database to breach and no "reset my password" support desk that can be
  social-engineered. You copy the key to each new device yourself (shown as a
  QR code / recovery phrase at sign-in).
- **Files are encrypted on your device before they are uploaded.** The nest
  stores content-addressed encrypted chunks. Identical data is stored once
  (deduplication), only changed pieces are re-uploaded, and the server can
  verify integrity — all without ever being able to read the contents.
- **You choose how much the box gets to do with your data — any time, not
  just at setup.** Every nest holds ciphertext by default; there's no
  claim-time trust question to answer, right down to a rented VPS. When you
  want a box to do more — serve mail to any mail app on your home LAN, index
  your content for search, and so on — you mint the specific, revocable grant
  for it from Settings → Nests. Nothing is granted until you ask for it, and
  you can take it back the same way.
- **The flip side, stated honestly: your key is your account.** If you lose
  the key from *all* your devices and copies, no one — not you, not the nest,
  not the Fauna project — can recover your encrypted data. Keep the key on at
  least two devices and store the recovery phrase somewhere safe. The
  [backup guide](cloud-backup.md) § "The key that matters more than the backup"
  covers this.

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

| | **Fauna (your nest)** | iCloud+ | Google One | Dropbox | OneDrive | Nextcloud (self-hosted) |
|---|---|---|---|---|---|---|
| Who can read your files | **Only you** (E2EE by default) | Apple, unless you enable ADP | Google | Dropbox | Microsoft | Your server can (E2EE add-on is partial) |
| Runs on hardware you own | **Yes** (VPS or home box) | No | No | No | No | Yes |
| Multi-device file sync | **Yes** — desktop apps + network-drive access | Mac/iOS-centric | Yes | Yes | Yes | Yes |
| Automatic phone photo backup | **Yes** (iPhone and Android) | iPhone | Android + iPhone | Yes | Yes | Yes (app) |
| Versioned backups you control | **Yes** — snapshot history with retention you set | Time Machine is separate | Limited | 30–180 days | 30 days | Apps/add-ons |
| Backup to a friend's server, unreadable by them | **Yes** | No | No | No | No | No |
| Share to another person | **Yes** — to their Fauna identity; revocation actually re-encrypts | Yes | Yes | Yes | Yes | Yes |
| "Anyone with the link" public links | **No — deliberately.** Bearer links are how private files leak. Public content uses real web hosting instead | Yes | Yes | Yes | Yes | Yes |
| Also included: mail, calendar, contacts, messaging, social | **Yes** | No (mail separate) | Gmail separate | No | No | Via many add-on apps |
| Monthly fee scales with storage | **No** — your disk | Yes | Yes | Yes | Yes | No |
| Someone else runs it for you | No — you do (guided) | Yes | Yes | Yes | Yes | No |

That last row is the honest trade: Fauna's setup guides make it approachable,
but you are the admin of your own service in the way a Dropbox customer is
not. The rest of this guide series exists to show that the day-to-day
experience — once the nest is running — is the same "it just works" you're used
to, without the subscription or the surveillance.

## Where this stands

Fauna is in closed alpha (since June 2026). The per-feature status tables live
at the end of each companion guide — [sync](cloud-sync.md#where-this-stands-today),
[backup](cloud-backup.md#where-this-stands-today),
[photos](cloud-photos.md#where-this-stands-today),
[sharing & family](cloud-sharing-family.md#where-this-stands-today). The short
version: sync, photo backup, snapshots, friend-hosted backup storage, sharing,
and the per-file version history browser are built and tested; the most
visible pieces still landing are one-click disaster-restore from a backup
destination and read-write shared folders.
