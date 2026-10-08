# Own your email — self-hosted mail with Fauna

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
> exactly what is available now, what is landing, and what is planned.

---

> **What this is.** Running your own email — `you@yourdomain.com`, received
> and sent by a server you own — with Fauna doing the parts that made
> self-hosted mail infamous. Includes connecting regular mail apps
> (Thunderbird, Apple Mail, K-9…) over IMAP.
>
> **Who it's for.** Someone with a nest on a public domain (see
> [Set up a nest on the internet](nest-internet-setup.md)) who wants off Gmail. Calendar
> lives in its own [companion guide](calendar-and-contacts.md).

---

## Why this is normally hard, and what Fauna does about it

Self-hosted email has a deserved reputation: it's not the mail server that's
hard, it's the *deliverability liturgy* — DKIM signing keys, SPF and DMARC
policies, TLS certificates, MTA-STS, DANE, staying off blocklists — where one
stale record silently lands your mail in spam folders. Fauna's position is
that none of this is a choice you need to make, so none of it is
configuration:

- **DKIM keys** are generated, stored, and rotated by the nest itself. You
  never see a key; you publish (or Fauna publishes) one DNS record.
- **SPF, DMARC, MTA-STS, TLS reporting** are published as fixed, strict,
  correct policies.
- **TLS certificates** come from Let's Encrypt automatically, for the mail
  host too; DANE records are coupled to the live certificate.
- **New-IP warm-up** (sending gently from a fresh address for the first
  month) and **daily blocklist self-checks** run by themselves.

What's left for you is exactly two things no software can do on your behalf —
covered [below](#the-two-things-fauna-cannot-do-for-you).

## Turning mail on

If you claimed your nest with a real domain while it's reachable from the
internet — the normal case this guide assumes — **mail turns on by itself the
moment you claim.** Your mailbox is `you@yourdomain.com`: your handle already
*is* your email address, on the domain you claimed the nest with. Claiming
mints your mail keys and your first credential (a generated password) as part
of the same step. Find it any time in *Settings → Mail & Calendar* — the
credential's row can show or copy it again whenever you set up a new mail app.

(Claimed without a domain, or on a private/home-network nest? Mail stays off
until you turn it on yourself, from the same *Settings → Mail & Calendar*
page — or the deployment-wide switch in *Admin → Mail*, which an admin can
also use to turn mail off entirely.)

Either way, before mail actually flows:

1. **Publish the DNS records** — the admin *Domains & DNS* page lists every
   record mail needs (MX, SPF, DKIM, DMARC, MTA-STS, and friends) with a live
   red/green check per record, verified against public DNS. Two ways to get
   them green:
   - **Fauna-managed:** if you connected a supported DNS provider
     (Cloudflare, Porkbun, Gandi, Namecheap) during setup, the app publishes
     and maintains the records itself.
   - **Manual:** copy each listed name/type/value into your DNS provider's
     panel, then watch the checks turn green. This is the common path today.
2. **That's it for members.** Anyone who joins a mail-enabled nest gets a
   working `name@yourdomain.com` mailbox automatically at sign-up.

## The two things Fauna cannot do for you

Both live at your hosting provider, not in DNS, and both matter:

1. **Outbound port 25.** Fauna delivers mail directly to recipients' servers,
   which requires *outgoing* connections on port 25 — and most VPS providers
   (including Hetzner) block that by default for new customers. Ask your
   provider's support to unblock it. The failure mode without it is sneaky:
   receiving works, sending *appears* to work (the message sits in Sent), but
   nothing arrives and it eventually bounces.
2. **Reverse DNS (PTR).** Set your server IP's reverse-DNS name to your mail
   host in the provider's control panel. Fauna shows you the expected value
   and verifies it, but only the IP's owner can set it.

## Mail in the Fauna app

There is no separate mail program: email lands in **Conversations**, threaded
with your other messages. Mail from `grandma@gmail.com` is a conversation
like any other — reply from your phone the way you'd answer any message.

- **HTML mail is tamed, not trusted.** Incoming HTML is converted to clean
  text formatting; **remote images are blocked by default**, with a
  per-message "load remote content" button — so senders can't track you by
  embedding beacons.
- **Marking spam** is a message action; it trains *your* filter (see below).
- Every device with the app sees your mail, sent included.
- **A message the app cannot unlock never holds up the ones after it.** Your
  mail is sealed to keys only your devices hold. If you turn mail off and back
  on, the old keys are gone for good, and anything sealed to them before the
  switch stays locked — the app skips those messages, keeps delivering
  everything newer, and tells you at the top of Conversations how many it had
  to skip. Renewing your mail keys is different: the app keeps the previous
  two generations, so your earlier mail stays readable.
- **There is a sending limit, and it is per account.** Ordinary personal mail
  never comes near it — it exists so that one compromised account cannot burn
  the reputation of everyone else's address. It counts *your account*, not the
  address you put in the From line, and it is the same allowance whether you
  send from the Fauna app or from a regular mail app; sending from both does
  not give you two allowances. Mail to other people on your own nest doesn't
  count against it at all. If you do hit it, the app says so and the message
  isn't sent — wait, then send again. Newsletters have their own separate
  allowance (see below), so a mailing list never eats your personal one.

## Connecting a regular mail app

Any standard mail client works over IMAP. Add a credential per app or device
in *Settings → Mail & Calendar* (each can be revealed, copied, or revoked on
its own — a lost laptop doesn't mean changing anything on your phone; in apps
that have the *Connected apps* page, the passwords you added are listed there,
one row each):

| Setting | Value |
|---|---|
| IMAP server | `mail.yourdomain.com`, port **993** (SSL/TLS) |
| SMTP server | `mail.yourdomain.com`, port **465** (SSL/TLS) |
| Username | `you@yourdomain.com` (or `you+laptop@yourdomain.com` for the credential named "laptop") |
| Password | the credential's generated password |

The same page shows these values pre-filled for your domain. Junk-folder
moves in your mail app count as spam training too, so filing works the same
whether you're in Fauna or Thunderbird.

**You can send as any address that is yours** — your own name on any of the
nest's domains, or an alias you have set up — from a regular mail app just as
from Fauna. Put it in the From line, or pick it in the app's "send from" menu
where there is one. A From address that isn't yours (another person's on the
same nest, or one nobody owns) is refused when you send, so nobody on the nest
can send mail that looks like it came from you.

**If you think a password leaked,** don't just revoke that one credential —
use **Rotate mail keys** on the same page. It replaces the encryption key
behind every credential in one step (you can exclude a specific device from
being re-trusted, if that's the one you suspect), picks up cleanly if it's
interrupted partway, and never touches mail you've already received — it
stays readable in the app and in your mail apps, however many times you
rotate.

## Addresses: aliases, plus-addresses, and role addresses

- **Plus-addressing works out of the box:** mail to `you+shopping@…` reaches
  you, tagged, with nothing to set up.
- **Aliases** — extra addresses that deliver to you — have a settings page in
  the app: add an exact address or a `you-*@…` wildcard pattern, label each
  one, disable it without losing it, or delete it outright. Need a one-off
  address for a single shop or signup? Generate a disposable address on the
  spot; it expires on its own — after 30 days or its first message, or, if
  you add it as a disposable address yourself, after the number of days and
  messages you choose. Migrating from another mail provider? Paste
  your existing list of allowed addresses in one batch and the app sorts out
  what's new, what already exists, and what's invalid.
- **Role addresses and catch-all** (`info@`, `postmaster@`, "everything else
  goes to me") are per-domain admin choices: the admin can keep them, or
  delegate one to another member of the nest (a moderator handling `abuse@`,
  say). Sending a role address to a mailbox outside the deployment isn't
  available yet.
- **More than one domain** on the same nest works: register additional
  domains on the admin page, and each gets its own DKIM keys, policies, and
  addresses.

## Forwarding

**Forward everything** to another address from the **Forwarding** section of
Settings → Mail & Calendar (shown once mail is on): type the address into
**Forward all incoming mail to** and press Enter. You keep your own copy of
every message; clear the field and press Enter to stop. It is done properly
under the hood — rewritten envelopes so SPF doesn't break, loop detection,
and bounces routed back to you. An address on a domain your own server hosts
is refused: add it as an alias instead. Beside it, **Hourly forwarding
limit** sets how many messages may be forwarded for you in one hour
(100 unless you change it, up to the most your server allows, which the
field names); past the limit, forwards wait for the next hour. Each
forwarded message also counts as one recipient against your daily sending
limit; once that is used up, forwards wait for the next day.

**Forward only some mail** ("if from X, forward to Y") with a rule: in
Settings → Privacy, under Email Filters, choose **Add Filter**, set the rule
that should match, pick **Forward** as the action, and type the address into
**Forward to**. **Keep a local copy** is ticked to start with, so the mail is
forwarded and also stays in your own mailbox; untick it to send the mail on
without keeping it. Editing the rule later shows the same choice, and saving
keeps it.

## Newsletters and mailing lists

Running a newsletter or a small announcement list from your own domain is a
settings page, not a separate service you sign up for.

- **Create a list** with its own sending address (`weekly@yourdomain`), a name,
  an optional description, help and archive links, and — if you want one — a
  cap on how many recipients a single send may reach. The archive link goes out
  with every message, so if it points somewhere other than your own server, the
  app asks once before saving it: press the button again to confirm.
- **Manage members** on the list's own page: add an address at a time, or paste
  a whole block of them and let the app sort out which are new, which were
  already subscribed, and which aren't valid addresses. Each member shows
  whether they're subscribed, and you can unsubscribe or re-subscribe anyone by
  hand.
- **Send an issue from Conversations.** Start a new conversation and type the
  list's address as the recipient. Before you send, the composer tells you how
  many subscribers it will reach and how much of today's sending allowance
  you've used — and warns you when you're close to the limit, or when this send
  would go past it. Each subscriber gets their own copy; your sent mail shows
  the send once, with one progress line for the whole send.
- **One-click unsubscribe is handled for you.** Every message the list sends
  carries the standard headers modern mail apps read, so a recipient's
  "Unsubscribe" button just works — over the web or by mail, whichever their
  app prefers. Nobody has to reply and ask you to take them off.
- **Unsubscribing sticks.** Once someone has opted out, they stay out unless you
  explicitly add them back. That's deliberate: it's what keeps a list a list
  rather than a nuisance, and it's a large part of why mail from your domain
  keeps being delivered.
- **Your own quota is visible on the list.** Each row shows how many sends and
  recipients you've used today, so there's no guessing about limits.

Each list has its own member count and last-sent date on the Lists page, and
deleting a list removes its members with it.

## Spam, your way

Filtering is two-layered: a deployment-wide pre-classifier plus a **personal
Bayesian filter that learns from you alone** — your corrections never train
your housemate's filter, and the admin can't see or touch your model. The
default posture is deliberately permissive: suspicious mail goes to **Junk**,
and nothing is rejected on a content score unless the admin turns that on.
Nothing is ever held back for someone else to review. Your training history is visible, each entry
undoable, and the whole model can be reset.

If the deployment default is too strict or too lax for your own mail, set
your own spam-folder threshold on the Spam settings page — leave it blank to
just follow the admin's default.

(Only hard perimeter gates reject outright at the door: malware, known-bad
sending hosts, and forged senders failing SPF/DMARC.)

## Moving in from your old provider

Honest status, because this is the part everyone asks about:

- **Available now:** point your old address's forwarding at
  `you@yourdomain.com` and start living here; your old mail stays readable at
  the old provider. Run both in parallel as long as you like.
- **Available:** a proper **import wizard** (connect to Gmail/iCloud with an
  app password, or any IMAP server with a hostname/username/password, copy
  everything in, deduplicated and resumable) — pick the source, choose which
  mailboxes and how far back, review the total, and watch it run with
  pause/resume/cancel and a per-message skip/error log. "How far back" is a
  single date written as `2023-11-14`: everything that arrived on that day or
  after it comes across and nothing older does, so a "just the last year"
  move costs you no storage for the decade before it. Outlook's one-click
  sign-in isn't wired yet — use its IMAP fallback (same as generic IMAP) in
  the meantime.
- **Moving out** is the next section: export is now built, starting with the
  terminal app, the Linux and Windows desktop apps, and the Mac, iPhone, iPad
  and Android apps.

## Taking your mail with you

Moving in is only half of "your mail is yours". The other half is being able to
take the whole mailbox out again, whenever you like, in a format some other mail
program can read — with nobody in between able to see it.

Open **Settings → Mail → Export**. It is a five-step wizard:

1. **Format.** Pick **mbox** (one file per mailbox — what Thunderbird and most
   Unix tools read), **Maildir++** (one file per message, in folders — what
   Dovecot and mutt read), or **EML zip** (one plain `.eml` per message plus a
   small index, which almost anything can open). If you have no preference, mbox
   is the default: one file per mailbox is the easiest shape to hand to another
   mail program. The download is about the same size whichever you pick.
2. **What to include.** Every mailbox is ticked except Trash and Junk; untick
   anything you do not want. You can narrow it to a date range, and you can
   choose to strip the transit headers — the routing trail a message picks up on
   its way to you — if you would rather not carry that along.
3. **Review.** A summary of what you just chose, and roughly how many messages
   that is.
4. **While it runs.** A progress screen with a bar and a per-mailbox count. You
   can pause and resume it, or cancel it outright. A big mailbox takes a while;
   you can leave the screen and come back.
5. **Done.** Press **Download** and the archive is saved to your downloads
   folder; the screen tells you exactly where. On an iPhone, iPad or Android
   phone the share sheet opens instead, so you can save it to Files or send it
   wherever you like. In the web app your browser saves it the way it saves
   any other download, once the whole archive has been checked.

What you get is a single file ending in `.zip.zst`. Run `zstd -d` on it once (or
open it with any tool that knows zstd) and you are left with an ordinary `.zip`
that Windows Explorer, Finder and every archive tool open by double-clicking.

A few things worth knowing:

- **The server cannot read it.** Your app converts your mail into the archive
  and seals it before a single byte is uploaded. What rests on the server for the
  next 30 days is sealed to a key only your app holds, so whoever runs the
  machine — even if that is not you — sees nothing but noise. Your app unseals it
  again when you download it.
- **A half-finished export is never handed to you.** If a download is cut short,
  or you try to grab one that is still running, your app refuses it rather than
  saving you a partial mailbox that looks complete. Run it again.
- **An export that fails says so, and says why.** If your app hits something it
  cannot read — a message sealed to a key this account no longer holds, say — it
  stops the whole export rather than quietly leaving the message out, and records
  what went wrong. Your other devices see the same thing: the export is marked
  failed, with the folder and message it stopped on, not merely gone. Fix the
  cause and run it again.
- **It is yours to delete.** The finished archive sits on the server for 30 days
  so you can fetch it from another device, then goes on its own. **Discard** on
  the Done step removes it straight away.
- **Every download is announced.** Each time a finished export is downloaded, you
  get a security notice in Notifications saying so, with the address it came
  from. If it was not you, revoke your sessions from Settings → Security and
  discard the export. An account that is suspended or locked cannot download
  one at all.

In the apps where the export cannot be run yet, the wizard is there but pressing
Start tells you so — that part is arriving app by app. In the meantime, IMAP works: point
any mail app at your mailbox and copy the folders out.

## Where this stands today

| Feature | Status |
|---|---|
| Receiving & sending at your own domain; strict SPF/DKIM/DMARC enforced both ways | **Available** |
| Automatic DKIM provisioning & rotation, TLS via Let's Encrypt, MTA-STS, DANE, TLS reporting | **Available** (DKIM is Ed25519-only today — a few very old receivers may not verify it) |
| DNS page with per-record live verification (manual mode) | **Available** |
| Fauna-managed DNS publishing via a connected provider | **Available** (one cleanup pass still landing) |
| Mailbox auto-created for every new member | **Available** |
| Mail in Conversations: read, reply, HTML rendering, blocked remote images | **Available** on all seven apps |
| Instant new-mail push to apps | **Available** on all seven apps |
| IMAP (993) + submission (465) for standard mail apps, per-device revocable credentials | **Available** |
| Rotate mail keys after a suspected compromise (resumable, excludes chosen devices) | **Available** |
| Plus-addressing; aliases (exact, wildcard, disposable), with a settings page, per-alias controls, and bulk address import | **Available** |
| Per-domain catch-all, role addresses, external forwarders; multiple domains | **Available** |
| Forward-all per account (SRS, loop detection, bounces) and your own hourly forwarding limit | **Available** — set in Settings → Mail & Calendar → Forwarding |
| Per-rule forward action, with or without keeping your own copy | **Available** — Settings → Privacy → Email Filters, action **Forward** |
| Newsletters/mailing lists: create a list, manage members (one at a time or pasted in bulk), send to a list from Conversations with its reach and daily allowance shown first, per-member unsubscribe & re-subscribe, standards-compliant one-click unsubscribe | **Available** |
| Personal spam training (in-app + Junk-folder moves), history, undo, reset | **Available** |
| Opt-in spam-report sharing: your flag joins an anonymized count (visible only once 3+ people on your nest flag the same message) your nest shares with peer nests, with a "what this nest publishes" transparency view | **Available** — off by default |
| New-IP warm-up & blocklist self-check | **Available** (automatic; admin tuning knobs planned) |
| Import wizard from Gmail/iCloud (app password) or any IMAP server | **Available** — Outlook's one-click sign-in isn't wired yet; use its IMAP fallback meanwhile |
| Mailbox export (mbox/Maildir/EML zip), sealed so the server cannot read it, with pause/resume/cancel | **Available** in every app — the terminal app, the web app, the Linux and Windows desktop apps, and the Mac, iPhone, iPad and Android apps |
| Large attachments | **Available** up to roughly 50 MB per message, sending and receiving. A larger message is refused immediately with a permanent "too large" reply, so the sender is told at once rather than left retrying — and your mail app sees the real limit when it connects, instead of a wrong one. |
