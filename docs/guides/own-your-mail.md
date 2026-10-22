# Own your email — self-hosted mail with Fauna

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
> exactly what is available now.

---

> **What this is.** Running your own email — `you@example.com`, received and sent by a server you control — with Fauna doing the parts that made self-hosted mail infamous. Includes connecting regular mail apps
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
stale record silently lands your mail in spam folders. Fauna's position is that none of this is a choice you need to make, so none of it needs configuring:

- **DKIM keys** are generated, stored, and rotated by the nest itself. You never see a key; you publish (or Fauna publishes) the records the DNS page lists, and when keys renew, the nest waits until the new records are live.
- **SPF, DMARC, MTA-STS, TLS reporting** are published with strict, correct defaults. There is nothing you must choose, though an admin can adjust a domain's DMARC policy.
- **TLS certificates** come from Let's Encrypt automatically, for the mail
  host too; DANE records are coupled to the live certificate.
- **New-IP warm-up** (sending gently from a fresh address for the first
  month) and **daily blocklist self-checks** run by themselves.

Beyond publishing the DNS records, what's left for you is at most two things, both at your hosting provider — covered [below](#two-things-at-your-hosting-provider).

## Turning mail on

If you claimed your nest with a real domain while it's reachable from the internet, and it was created with email — the normal case this guide assumes — **mail turns on by itself the moment you claim.** Your mailbox is `you@example.com`: your handle already *is* your email address, on the domain you claimed the nest with. Claiming
mints your mail keys and your first credential (a generated password) as part
of the same step. Find it any time in Settings → **Connected apps** — its row can show or copy it again whenever you set up a new mail app.

(Claimed without a domain, or on a private/home-network nest? Mail stays off
until you turn it on yourself, from the Settings → **Mail & Calendar**
page — or the deployment-wide switch on the admin area's Mail page, which an admin can
also use to turn mail off entirely.)

Either way, before mail actually flows:

1. **Publish the DNS records** — the admin **DNS** page lists every record mail needs (MX, SPF, DKIM, DMARC, MTA-STS, and friends) with a live red/green check per record, verified against public DNS. Two ways to get
   them green:
   - **Fauna-managed:** if you connected a supported DNS provider (such as Cloudflare, Hetzner, Porkbun, Gandi or Namecheap) during setup, the app publishes and maintains the records itself.
   - **Manual:** copy each listed name/type/value into your DNS provider's
     panel, then watch the checks turn green. This is the common path today.
2. **That's it for members.** Anyone who joins a mail-enabled nest gets a working `name@example.com` mailbox automatically when they first sign in, unless the admin has turned off **Auto-enable mail for new users** on the admin area's Mail page.

A server created without email — the social-only choice in the [setup guide](nest-app-setup.md), or a server set up by hand with its mail parts left out ([Set up a nest on the internet](nest-internet-setup.md)) — is the one case this page can't help with: mail can't be turned on there unless the server is set up again with email; both Settings → **Mail & Calendar** and the admin area's Mail page say so, and the **Enable mail** switch on each stays off.

## Two things at your hosting provider

Both live at your hosting provider, not in DNS, and both matter:

1. **Outbound port 25.** Fauna delivers mail directly to recipients' servers, which requires *outgoing* connections on port 25 — and most VPS providers, Hetzner among them, block that by default for new accounts. Ask your provider's support to unblock it. The failure mode without it is sneaky:
   receiving works, sending *appears* to work (the message sits in Sent), but
   nothing arrives and it eventually bounces.
2. **Reverse DNS (PTR).** Your server IP's reverse-DNS name must be your mail host. If the app created your server for you at a provider that allows it, and published your DNS records through a DNS provider you connected during setup, the setup has already set it; otherwise set it in your provider's control panel — only the IP's owner can. Fauna shows you the expected value and checks it.

## Mail in the Fauna app

There is no separate mail program: email lands in **Conversations**, threaded
with your other messages. Mail from `grandma@example.net` is a conversation like any other — reply from your phone the way you'd answer any message.

- **HTML mail is tamed, not trusted.** Incoming HTML is converted to clean
  text formatting; **remote images are blocked by default**, with a
  per-message "load remote content" button — so senders can't track you by
  embedding beacons.
- **Marking spam** is a message action; it trains *your* filter (see below).
- **A message you received shows its own scan.** Under a message you received, the app shows what your nest's checks made of it when it arrived: the virus scan's verdict, the spam score with the rules that fired, and what your nest did with the message. You only see this for mail you received yourself, and only for recent mail — your nest keeps a message's scan result for 30 days. A message no scanner checked shows no scan line. If you'd rather not see it, turn off **Show scan results on received mail** in Settings → **Mail & Calendar**; this only hides the line — your nest's checks still run.
- Every device with the app sees your mail, sent included.
- **A message the app cannot open never holds up the ones after it.** Mail your nest passes to and from email is readable to it on the way, as on every mail server — mail arriving from outside your nest is checked then, and your own rules run on it — and is stored encrypted as it lands. After that, your devices open it themselves; a mail app you connect reaches it through your nest, which opens it with that app's password and only for that signed-in session; and beyond those, your nest reads it for itself only if you give it permission to read your mail — by saying yes to **Trust this box?** when you claim or join a nest, or later in Settings → Nests (**Add trust…**), or by subscribing to a community labeler that reads your mail, which gets its own trust for that one labeler. Your nest also sees what any server that delivers your messages must: who you're connected to — your contacts, whom you follow, the groups you're in — and when and how much you send. Turning mail off and back on keeps your mail keys, and renewing them keeps every earlier key, so your earlier mail stays readable. If a message still cannot be opened, the app skips it, keeps delivering everything newer, and tells you at the top of Conversations how many it had to skip.
- **There is a sending limit, and it is per account.** Ordinary personal mail
  never comes near it — it exists so that one compromised account cannot burn
  the reputation of everyone else's address. It counts *your account*, not the
  address you put in the From line, and it is the same allowance whether you
  send from the Fauna app or from a regular mail app; sending from both does
  not give you two allowances. Mail to other people on your own nest doesn't
  count against it at all. If you do hit it, the app says so and the message
  isn't sent — wait, then send again. Newsletters have their own separate
  allowance (see below), so a mailing list never eats your personal one.
- **A new nest sends gently at first.** For about its first month, your nest lets only so much mail out each day while other providers learn to trust its new address. A message past the day's share is still accepted — it waits in the queue and goes out by itself, so you don't send it again — and in the conversation it says when it will be delivered and how many messages are ahead of it.
- **You can see how full your mailbox is, and switch receiving and sending off.** Settings → **Mail & Calendar** shows how much of your mailbox you use: its size and its number of messages, each against its limit. When the mailbox is full, the page says so, and new mail is refused until you delete some. The same page has a **Receive mail** switch and a **Send mail** switch, both on to start with. Turning receiving off refuses mail sent to your own addresses and keeps the mail you already have. Turning sending off stops you sending mail from your account; an automatic reply or a forward you have set up still goes out.
- **An automatic reply** answers each sender at most once a week while it is on. Turn **Automatic reply** on with the message you want to send, and off again to stop it.

## Connecting a regular mail app

Any standard mail client works over IMAP. Add a credential per app or device
in Settings → **Mail & Calendar** (each is listed under Settings → **Connected apps**, one row each, where it can be revealed, copied, or disconnected on its own — a lost laptop doesn't mean changing anything on your phone):

| Setting | Value |
|---|---|
| IMAP server | `mail.example.com`, port **993** (SSL/TLS) |
| SMTP server | `mail.example.com`, port **465** (SSL/TLS) |
| Username | `you@example.com` (or `you+laptop@example.com` for the credential named "laptop") |
| Password | the credential's generated password |

Settings → **Mail & Calendar** shows these values pre-filled for your domain. Junk-folder
moves in your mail app count as spam training too, so filing works the same
whether you're in Fauna or Thunderbird.

**You can send as any address that is yours** — your own name on any of the
nest's domains, or an alias you have set up — from a regular mail app just as
from Fauna. Put it in the From line, or pick it in the app's "send from" menu
where there is one. A From address that isn't yours (another person's on the
same nest, or one nobody owns) is refused when you send, so nobody on the nest
can send mail that looks like it came from you.

**If you think a password leaked,** don't just revoke that one credential —
use **Rotate mail keys** in Settings → **Mail & Calendar**. It replaces the encryption key behind every credential in one step (you can exclude a specific credential from being re-trusted, if that's the one you suspect), picks up cleanly if it's interrupted partway, and never touches mail you've already received — it
stays readable in the app and in your mail apps, however many times you
rotate.

## Addresses: aliases, plus-addresses, and role addresses

- **Plus-addressing works out of the box:** mail to `you+shopping@…` reaches
  you, tagged, with nothing to set up.
- **Aliases** — extra addresses that deliver to you — have a settings page in the app: add an exact address or a `you-*@…` wildcard pattern, label each one, disable it without losing it, or delete it outright. A message that arrives through one of your aliases says which one, with its label, so you can tell at a glance what each alias is being used for. Press **Show audit** on an alias to see what arrived on it in the past 30 days, up to the last hundred messages — the address each one was sent to, the sending domain and when — which is how you find out who has been handing an address out. Need a one-off address for a single shop or signup? Generate a disposable address on the spot; it expires on its own — after 30 days or its first message, or, if you add it as a disposable address yourself, after the number of days and messages you choose. Each disposable address shows when it expires, and **Extend** gives one that has not expired yet 30 more days. Migrating from another mail provider? Paste your existing list of allowed addresses in one batch and the app sorts out what's new, what already exists, and what's invalid.
- **Role addresses and catch-all** (`info@`, `postmaster@`, "everything else
  goes to me") are per-domain admin choices: the admin can keep them, or delegate one to another member of the nest (someone who handles `abuse@` reports, say). Sending a role address to a mailbox outside the deployment isn't
  available yet.
- **DMARC reports never reach anyone's inbox.** The big mail providers send
  your domain a daily report on who sent mail in its name. Your nest reads these reports itself and keeps them for 90 days — the admin can change that on the admin area's Mail page — instead of filling the admin's inbox with compressed attachments. Malformed reports, and reports about a domain your nest doesn't serve, are dropped. An admin reads them on the **Mail reports** page of the admin area. Your nest returns the favour: once a day it
  sends each domain that asks for reports in its DMARC record one report on
  the mail it received claiming to be from that domain, from
  `dmarc-report@` at your main domain.
- **TLS failure reports never reach anyone's inbox either.** Mail servers
  that had trouble making an encrypted connection to your nest send a daily
  report saying so. Your nest reads those itself too, keeps them for 90 days,
  and drops broken ones and ones about a domain it doesn't serve. A screen to
  read them in isn't in the app yet.
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

The same Email Filters form builds the other kinds of rule, too: move matching mail to a folder, add a label your mail apps show, or reply automatically (at most once per sender in the interval you choose); it can also keep a sender's mail in your inbox even when it would otherwise be filed as spam, discard a message, or refuse it at the door. A rule can match on the sender, the sender's domain, the subject, the body, whether a header is present, what a header says, or the spam score. Give a rule a lower order number to run it earlier, and tick **Keep checking later rules after this one matches** to let the rules after it apply too.

## Newsletters and mailing lists

Running a newsletter or a small announcement list from your own domain is a
settings page, not a separate service you sign up for.

- **Create a list** with its own sending address (`weekly@example.com`), a name,
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
  the send once, with one progress line for the whole send. An issue goes out
  from your own address or from the list's own; a send that names anyone else
  as its sender is refused before it reaches a single subscriber.
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

Filtering is two-layered: a deployment-wide pre-classifier plus a **personal Bayesian filter that learns from you alone** — your corrections train only your own filter unless you choose to share them (both ways of sharing are off by default), and the admin can't see or touch your model. The
default posture is deliberately permissive: suspicious mail goes to **Junk**,
and nothing is rejected on a content score unless the admin turns that on or a rule of your own refuses it.
Nothing is ever held back for someone else to review — except on a supervised child's account, where the guardian's settings can hold mail from an unknown sender for the guardian to review (see [Parental controls](cloud-sharing-family.md#parental-controls)). Your training history is visible, each entry
undoable, and the whole model can be reset.

If the deployment default is too strict or too lax for your own mail, set
your own spam-folder threshold on the Spam settings page — leave it blank to
just follow the admin's default.

(Separately from spam scoring, hard perimeter gates reject some mail outright
at the door: malware, known-bad sending hosts, and forged senders failing
SPF/DMARC.)

## Moving in from your old provider

Honest status, because this is the part everyone asks about:

- **Available now:** point your old address's forwarding at `you@example.com` and start living here; your old mail stays readable at
  the old provider. Run both in parallel as long as you like.
- **Available:** a proper **import wizard** (connect to Gmail/iCloud with an app password, or any IMAP server with a hostname/username/password, copy everything in, deduplicated and resumable) — pick the source, choose which mailboxes and how far back, review how many messages and roughly how much data are coming, and watch it run with pause/resume/cancel, a count of what each mailbox has brought so far, and a per-message skip/error log. "How far back" is a single date written as `2023-11-14`: everything that arrived on that day or after it comes across and nothing older does, so a "just the last year" move costs you no storage for the decade before it. Trash, Junk or Spam, and Gmail's All Mail, Bin, Important and Starred start unticked (they hold discards or copies of mail another mailbox already has) — tick any of them to bring it too. If the connection fails, the wizard says whether it could not sign in, could not reach the server or could not secure the connection, with a tip for Gmail, Outlook and iCloud. If your Fauna mailbox fills up, the import pauses and tells you so: ask your admin for more room and press **Resume** to carry on from the first message that did not fit — even if you closed the app in between. Until you close the app, you can also press **Skip the rest** to finish the import without the messages it had not brought in yet; you can cancel the import at any time. If the connection drops and does not come back, the import pauses with the reason and resumes from where it stopped. Once the wizard has connected to your old provider, it remembers the server, the port, the connection security and your username there — never the password — and fills them in the next time you open it, on any of your devices, until you press **Forget saved server details**. Close the app in the middle of an import and open it again, and the import wizard brings you back to the same import's progress: type your old provider's password again (the app never keeps it) and press **Resume**, and it carries on where it stopped, with the same mailboxes and the same "how far back" date. For an IMAP server other than Gmail or iCloud, check the port and connection security beside the password too — they start at 993 with implicit TLS, or at the ones the wizard remembered from your last connection. If your old provider stops accepting your password part-way through — you changed it there, say — the import stops and asks you to start again with the new one; if too many messages in a row cannot be read, it stops and says so. Either way the mail that already arrived stays. An import that stopped stays on its progress screen with the reason, after you close and reopen the app and on your other devices, until you press **Dismiss** or start another import, or for 30 days. When the import finishes, **Review skipped** lists every message that didn't come across and why — too large, already in your mailbox, unreadable — and any old mailbox that had disappeared by the time the import reached it. If your old provider rebuilds a mailbox after you paused or closed the import — it renumbers every message in it, which can happen after a repair or a move — the import warns you when you resume, still brings your other mailboxes, and waits with **Restart rebuilt mailbox**: press it and that mailbox is copied again from its first message, skipping everything that already arrived, so nothing comes across twice — unless you turned on **Import duplicates anyway (advanced)**, which brings that mailbox's messages again too. If you do want a second copy of mail you already have — a snapshot of an old account kept exactly as it was, say — turn on **Import duplicates anyway (advanced)** on the step where you choose the mailboxes: the messages you chose then come across even when they are already in your mailbox, each stored a second time and counted against your storage. It starts off, and a **Resume** keeps whichever you chose. For an Outlook, Hotmail or Office 365 mailbox, type the account's address and press **Sign in with Microsoft**: the app shows a short code and Microsoft's sign-in address, you enter the code in a browser on any device — your phone will do — and the import connects by itself once you have signed in. The app never sees your Microsoft password, and the sign-in lasts for this import only: close the app in the middle of one and it asks you to sign in again before **Resume**. The sign-in works once your nest's admin has entered a Microsoft app registration on the admin area's Mail page ([the tour of the admin area](admin-tour.md) says how); until then the button does nothing and a line beside it says the sign-in is not available yet.
- **Moving out** is the next section: export works in every app.

## Taking your mail with you

Moving in is only half of "your mail is yours". The other half is being able to
take the whole mailbox out again, whenever you like, in a format some other mail
program can read — with nobody in between able to see it.

Open **Export mailbox**, among the mail pages in Settings. It is a five-step wizard:

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
3. **Review.** A summary of what you just chose: how many messages that is,
   roughly how large the archive will be, and roughly how long it will take.
   Both are estimates. If you picked a date range, the real export can only
   be smaller and quicker. Nothing starts until you press **Start export**.
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

- **The server cannot read it.** Your app converts your mail into the archive and encrypts it before a single byte is uploaded. What rests on the server for the next 30 days is encrypted to a key only your devices hold, so whoever runs the machine — even if that is not you — sees nothing but noise. Your app decrypts it again when you download it.
- **A half-finished export is never handed to you.** If a download is cut short,
  or you try to grab one that is still running, your app refuses it rather than
  saving you a partial mailbox that looks complete. Run it again.
- **An export that fails says so, and says why.** If your app hits something it cannot read — a message it cannot open, say — it stops the whole export rather than quietly leaving the message out, and records what went wrong. Your other devices see the same thing: the export is marked
  failed, with the folder and message it stopped on, not merely gone — and it is
  still there after you close and reopen the app. **Dismiss** clears it and takes
  you back to the first step; starting a new export clears it too. Fix the cause
  and run it again.
- **It is yours to delete.** The finished archive sits on the server for 30 days
  so you can fetch it from another device, then goes on its own. **Discard** on
  the Done step removes it straight away.
- **Every download is announced.** Each time a finished export is downloaded, you
  get a security notice in Notifications saying so, with the address it came
  from. If it was not you, discard the export and take the steps in [Your identity and devices](identity-and-devices.md#if-a-device-is-lost-or-stolen): signing out your other sessions alone does not stop a device that holds your key. An account that is suspended or locked cannot download
  one at all.

## Where this stands today

| Feature | Status |
|---|---|
| Receiving & sending at your own domain; strict SPF/DKIM/DMARC enforced both ways | **Available** |
| Automatic DKIM provisioning & rotation, TLS via Let's Encrypt, MTA-STS, DANE, TLS reporting | **Available** |
| DNS page with per-record live verification (manual mode) | **Available** |
| Fauna-managed DNS publishing via a connected provider | **Available** |
| Mailbox auto-created for every new member | **Available** |
| Mail in Conversations: read, reply, HTML rendering, blocked remote images | **Available** on all seven apps |
| Instant new-mail push to apps | **Available** on all seven apps |
| IMAP (993) + submission (465) for standard mail apps, per-device revocable credentials | **Available** |
| Rotate mail keys after a suspected compromise (resumable, excludes chosen credentials) | **Available** |
| Plus-addressing; aliases (exact, wildcard, disposable), with a settings page, per-alias controls, bulk address import, and each alias's history of what arrived on it | **Available** |
| Per-domain catch-all, role addresses, external forwarders; multiple domains | **Available** |
| Forward-all per account (SRS, loop detection, bounces) and your own hourly forwarding limit | **Available** — set in Settings → Mail & Calendar → Forwarding |
| Per-rule forward action, with or without keeping your own copy | **Available** — Settings → Privacy → Email Filters, action **Forward** |
| Mail rules from the app: move to a folder, label, forward, automatic reply, keep in inbox, discard or refuse; match on the sender, its domain, the subject, the body, a header or the spam score; rule order and **Keep checking later rules after this one matches** | **Available** — Settings → Privacy → Email Filters |
| Newsletters/mailing lists: create a list, manage members (one at a time or pasted in bulk), send to a list from Conversations with its reach and daily allowance shown first, per-member unsubscribe & re-subscribe, standards-compliant one-click unsubscribe | **Available** |
| Personal spam training (in-app + Junk-folder moves), history, undo, reset | **Available** |
| Opt-in spam-report sharing: your flag joins an anonymized count (visible only once 3+ people on your nest flag the same message) your nest shares with peer nests, with a "what this nest publishes" transparency view | **Available** — off by default |
| New-IP warm-up & blocklist self-check | **Available** (automatic) |
| Import wizard from Gmail/iCloud (app password) or any IMAP server | **Available** — Gmail and iCloud with an app password, any other IMAP server with its password; Outlook, Hotmail and Office 365 with Sign in with Microsoft, once your nest's admin has entered a Microsoft app registration |
| Mailbox export (mbox/Maildir/EML zip), encrypted so the server cannot read it, with pause/resume/cancel | **Available** |
| Large attachments | **Available** up to roughly 50 MB per message, sending and receiving. A larger message is refused immediately with a permanent "too large" reply, so the sender is told at once rather than left retrying — and your mail app sees the real limit when it connects, instead of a wrong one. |
