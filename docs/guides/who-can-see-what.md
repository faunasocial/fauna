# Who can see what — the ways your data reaches the outside world

Fauna keeps your content sealed on your nest. Nobody who connects to your
nest — not another Fauna user, not a stranger on the internet, not the person
who runs the server — can read your posts, messages, files, or contacts unless
you deliberately shared them. That is the default, and it does not change.

But a nest is also a *server*. The moment it opens a port to the world — to send
your mail, to show your public posts, to reach the wider social web — it starts
telling the world a few things. Most of those are things you asked it to publish.
A handful are side effects of running a public service that other software has to
be able to find and talk to.

This guide walks through every feature that can reveal something, in plain
language, so you can answer one question before you flip any switch: **if I turn
this on, who can see what?** Each section says who the "who" is — anyone on the
internet, any Fauna/Nostr/email user, or a whole public network — and whether
turning the feature back off takes it away again.

**One thing that is true everywhere:** some disclosures are *permanent*. A public
post that spreads to other servers, a security certificate logged in a public
transparency log, an address published in a public record — once these are out,
turning the feature off stops *new* copies but cannot recall the ones already
made. Where that applies, this guide says so.

---

## The short version

- **Your private content stays private.** Sealed posts, direct messages, your
  files, your contacts, who follows you, who you follow, and how many users your
  nest has — none of these are visible to outsiders. See
  [What always stays private](#what-always-stays-private).
- **What you publish is public — everywhere it travels.** A public post, your
  profile, and the free preview of a paid post are meant to be seen. If you also
  connect to Bluesky, Nostr, or the Fediverse, "public" means public on those
  networks too, and often permanently.
- **Running a public service announces the service.** Mail, a public web page for
  your posts, and the bridges each leave a public footprint — a mail server's
  greeting, DNS records, a security certificate. These identify the service and
  its domains; they do not reveal your private content.
- **A folder is public only if you say so, twice.** Folders are private by
  default; making one public means the files *and their names* are readable by
  anyone, so your app asks you to confirm. See
  [Folders you make public](#folders-you-make-public).
- **The switches are yours.** Every bridge and the mail server are off until you
  turn them on, per person, from your app. Turning one off removes its
  footprint going forward.

---

## Public posts and your profile

When you make a post public, it is public. Anyone can read it — on your nest's
web page, through search, and on any network you have bridged it to. Your profile
(display name, bio, picture, banner, and your account's public identifier) is
public in the same way once you have a public presence.

- **Who sees it:** anyone.
- **Reaches other networks?** Only if you turn on a bridge (below). A plain public
  post on a nest with no bridges is readable by people using Fauna, and on your
  nest's public web page if you enabled one.
- **Turning it off:** you can delete a post, but if it already travelled to
  another network, copies there may remain.

Your **paid (monetized) posts** are different: the short **free preview** you
write is public — it shows on the paywall, and it is searchable — but the full
body stays sealed and is only unlocked for people with a valid subscription. See
[Paid posts](#paid-posts-what-is-public-and-what-is-not).

---

## Folders you make public

Every folder is private by default. Your files are encrypted on your nest, and
nobody — not another user, not an outsider, not whoever runs the nest — can read
them.

A folder can also be **shared** with specific people, which you do from the
sharing section on the folder. Shared folders are still encrypted; the people you
shared with simply hold a key.

The third setting is **public**, and it is the one to understand before you use
it. On the folder's row in **Settings → Folders**, open it and set who can see it
to *Public*. Your app asks you to confirm, because a public folder is different
in kind from the other two:

- **The files are no longer encrypted.** Anyone on the web can read them.
- **The names are public too.** File and folder names become part of each file's
  web address, so a folder called `tax-2025` announces itself even before anyone
  opens a file.
- **Going back to private protects only what comes after.** Files that were
  public can have been copied, indexed, or archived by anyone in the meantime.
  Treat anything you published as public for good.

That is why nothing becomes public by accident: it takes the setting *and* the
confirmation, and a folder shared with specific people has to stop being shared
before it can become private again.

Going back works from the same control. A private folder that went public
returns to *Private*. A folder shared with specific people returns to *Shared*
instead: pick it and the folder is encrypted again so only those people can
read it — with the same caveat as above about anything published in the
meantime.

Making a folder public also records that **you** made it public, signed with
your own key, so an up-to-date app never takes your nest's word for it alone.
A folder you made public from an older app, or one you took over from someone
else, carries no such record yet. Its row then says your app can't confirm you
made it public, and until you do, up-to-date apps may keep its files encrypted
and your website may not show them. Choose **Confirm public** on the row and
answer the same confirmation as before; the folder stays public and is fully
readable again.

### Following someone else's public folder

The other side of the same feature: if someone tells you their handle and the
name of a folder they made public, you can follow it from **Settings → Folders**
→ *Follow a public folder*. Their handle is enough — you never need their nest's
address. Someone on your own nest is just `alice`; someone on another nest is
`alice@their-nest.example`, exactly as you would write them in a message, and
your app finds their nest from that. It appears in a **Folders you follow** list, which
says whose folder each one is — by the handle you followed them by, or, if you
followed them by their long ID or their handle has since changed, by the start
of that ID. Its contents browse in **Media**: pick it in Media's folder filter to
see what is inside. It is read-only there — you can look and download, but
uploading, deleting, and version history belong to its owner.

Two things worth knowing:

- **Following is invisible to them.** Their nest is never told who follows a
  public folder — there is no follower list, and nothing to approve. Reading a
  public folder is like reading a web page.
- **It can stop working, and that is normal.** If the owner makes the folder
  private again or deletes it, your row changes to *No longer available* and
  stays there until you remove it — so you can see what happened rather than
  having the folder quietly vanish. If they publish it again, it starts working
  again on its own. A lost connection is not the same thing: while your app
  cannot reach your nest, the folders you follow stay listed as they were.

Removing a followed folder only removes your own copy of the record. There is
nothing to cancel on their side, because they never knew.

### Publishing a folder as your website

The same page has a **"Serve this folder as your website"** switch. Turn it on
and your nest publishes that folder as a site — an `index.html` in it becomes
your home page.

The switch and the audience are two separate choices, and both have to line up
for visitors to see anything:

| Folder is… | With the website switch on… |
|---|---|
| Private | nothing is served — the site has no readable content yet |
| Paywalled to a tier | your subscribers can read it |
| Public | anyone can read it |

**There is a third switch, in another place, and it is off until you turn it
on.** Your site is served at your own web address, and that address is opt-in:
**Settings → Web**, "web address". Until it is on, visitors to your address get
the nest's own placeholder page rather than your site — so if you have published
a folder and see a "Fauna Nest" page instead of your own, that switch is why.

You can turn the folder switch on first and make the folder public later; the
app tells you the site is not visible yet rather than hiding the switch. Two combinations
are refused, and your app says so: a public folder cannot also be served over
WebDAV (that needs the folder encrypted), and a public folder cannot be
paywalled (there would be nothing to pay for).

---

## Bridges — Bluesky, Nostr, and the Fediverse

Bridges connect your Fauna identity to other social networks. They are **per
person and off by default** — you link each one yourself from the **Bridges**
screen. The trade-off is always the same: reach the wider world, and the wider
world can see what you send it. Background on each:
[Bluesky & Nostr from your nest](bridges-bluesky-nostr.md).

### Bluesky

When you link Bluesky and turn on cross-posting, your public posts are copied to
the Bluesky network. Bluesky's feed is world-readable and **permanent** — its
history is kept by many independent servers, so deleting your Fauna post removes
it from your nest but cannot guarantee removal everywhere it spread.

- **What's published:** the post text (up to a length limit), its links and tags,
  the time you posted, and your own Bluesky handle. **Photos and video are not
  cross-posted.**
- **A subtlety worth knowing:** if you **mention another Fauna user** in a post
  you cross-post, that person's account identifier travels into the public
  Bluesky record too — even if they never joined Bluesky. If that matters for
  someone you mention, leave them out of posts you cross-post.
- **Who sees it:** anyone on Bluesky, permanently.
- **Turning it off:** set cross-posting to off (or to "only tagged posts") on the
  Bluesky card. That stops future cross-posts; it does not recall past ones.

### Nostr

Nostr is a public relay network. When you enable content exposure, your public
posts become readable by anyone who connects to your nest's relay — no account
needed. Because Nostr is built for open reach, a few things follow:

- **Anyone can read** all your exposed public posts, and can **search** them by
  keyword and **count** them (how many posts you've made, how many replies
  reference a given post). This is normal for a public relay.
- **Your direct messages stay private.** Nostr DMs are sealed and only delivered
  to the intended recipient; nobody else can see who messaged whom, when, or how
  much.
- **Who sees it:** anyone, for your exposed public posts.
- **Turning it off:** turn off content exposure on the Nostr page. New public
  posts stop being exposed.

### The Fediverse (ActivityPub / Mastodon-compatible)

Enabling Fediverse federation publishes your handle to the wider Fediverse and
lets Mastodon-style servers follow you and receive your public posts.

- **Enabling it publishes your handle permanently.** Your Fauna handle becomes
  your Fediverse address, and it is remembered by every server that sees it.
  There is no rename afterwards, so choose before you enable.
- **What's shared:** your profile and your public, non-paid posts. Your follower
  and following **counts** are visible, but **not the lists themselves** — nobody
  can see *who* follows you. Paid-post full text is never federated.
- **Who sees it:** any Fediverse server and its users, once you enable it for your
  account.
- **Turning it off:** stops future federation; servers that already cached your
  handle or posts may keep them.

---

## Mail

Running your own mail at your domain is a powerful feature, and mail is an old,
public protocol. When you turn mail on, your nest becomes a mail server that the
rest of the internet must be able to reach — so it announces itself. See
[Own your email](own-your-mail.md) for the full setup.

- **The mail server greets anyone who connects.** A mail server, by design,
  answers connections with a greeting that names your mail domain. This tells a
  curious observer that your domain runs mail and what its address is. It does
  **not** reveal any message.
- **Public DNS records describe your mail setup.** To send and receive mail that
  other providers trust, your domain publishes standard public records (where
  your mail is handled, your sending-signature keys, your policy). Anyone can look
  these up — that is how mail works everywhere — and they reveal which domains you
  host mail for and that you offer calendar/contacts, but no message content.
- **Someone can test whether an address exists.** Like most mail servers, yours
  will, by default, tell a sender whether a given address is a real mailbox
  (by accepting or refusing the message). Someone can use that to guess which
  addresses exist. **You can close this:** turn on a **catch-all** for a domain
  (in the admin **Mail** settings) so every address is accepted and none can be
  singled out.
- **Your messages themselves stay sealed.** Message bodies are encrypted at rest;
  the mail server hands them only to the authenticated mailbox owner. Outgoing
  mail is scrubbed of your device's address and internal details before it
  leaves.
- **Who sees the footprint:** anyone (the greeting, the DNS records). **Who sees
  your mail:** only you.
- **Turning it off:** turning mail off closes the mail ports completely — a
  scanner finds nothing. The public DNS records stay until you remove them.

**A note for guardians:** a mailbox you set up for a child with an approved-senders
list refuses mail from unapproved senders with the same generic "no such address"
message a nonexistent mailbox gets — a stranger cannot tell your child's mailbox
apart from one that doesn't exist, let alone that it is a restricted one.

---

## Paid posts — what is public and what is not

A monetized post has two parts: a short **free preview** you write, and the
**full body** behind the paywall.

- **The free preview is public** — it appears on the paywall page for anyone, and
  it shows up in search, exactly like any public text. Write it knowing it is
  visible to everyone.
- **The full body stays sealed** and is only unlocked for people who hold a valid
  subscription. It never travels to Bluesky, Nostr, or the Fediverse.
- **Your tier names and prices are public.** So the paywall and other networks can
  show them, your subscription tiers, their prices, and payment links are readable
  by anyone. This is the storefront, meant to be seen.

## Posts for one room — what is public and what is not

A post you address to a room also has a **public teaser** and a **sealed full
body**.

- **The teaser is public**, exactly like a paid post's, and so is the fact that
  the post is for a room: its card carries a badge for anyone who can see your
  posts. A member of the room sees the room's name on it, as it appears in
  their own conversation list, and opens the post as usual; everyone else
  sees "room".
- **The full body is sealed with the room's own key**, the same protection the
  room's messages have. Only the room's members can open it, each on their own
  device.
- **Membership decides, not following.** Someone who follows you but is not in
  the room sees only the teaser. Someone who leaves the room keeps what they
  could already open, but not what you post after they left.

## Replying to a post that is not public

A reply is a post of your own. Ordinarily it goes to **your** followers, not to
the readers of the post you answer — so under a paid post or a post for a room,
the app takes care that your words do not end up somewhere wider than the post
they answer:

- **If you are in the room, your reply goes to the room.** Reply to a post
  addressed to a room you are a member of, and your reply is sealed for that
  same room, exactly like the post it answers. Everyone else sees that you
  replied — a card with the room's badge and no text — but only the room's
  members can open it and read what you said.
- **If it is your own paid post, your reply goes to the same subscribers.**
  Replying under a post you restricted to one of your tiers seals the reply for
  that tier: your subscribers can open it, and nobody else can.
- **Otherwise, your reply can only be public — and the app asks first.** A
  subscriber replying under someone else's paid post, or a reader who is not
  in the room, has no way to write for that audience: a reply would go to your
  own followers, like any post of yours. So the reply box says so, and offers
  a checkbox, *Post my reply publicly*. Leave it unchecked and the reply is not
  sent — the app tells you why instead of posting your words to everyone.
  Check it and your reply is posted publicly, in the clear. The box is
  unchecked every time you open the reply box; nothing is remembered. A quote
  with commentary under such a post is still not sent.
- **A repost, or a quote without commentary, is still public** — it carries no
  words of yours, and shows your followers only what they could already see:
  the teaser and the badge.

What stays visible on a sealed reply is *that* you replied, and to which post.
What you wrote does not.

---

## Search

Search on a nest covers public content — public posts and profiles. Anyone with
an account on the nest can search it, and the **free preview** of a paid post is
included (because that preview is public anyway). Sealed content, direct messages,
and your files are never searchable by anyone but you.

---

## Your handle, your domains, and security certificates

Some public footprints come from the security certificates and web addresses a
nest uses:

- **A public web page for your posts, or your own domain,** means a security
  certificate is issued for that address. Certificates are recorded in public
  **Certificate Transparency** logs — a permanent, world-searchable record. So
  enabling a public page at `yourhandle.yournest` or connecting your own domain
  makes that name permanently public, even after you turn the page off.
- **This is normal for the whole web** — every HTTPS site appears in these logs.
  It is worth knowing only because it is *permanent*: the record cannot be
  removed later.
- **Who sees it:** anyone (the logs are public).

If you would rather not publish a name this way, do not enable the public web page
or custom domain for it.

---

## Link previews and shared media

When you post a link, your nest fetches the page in the background to build a
preview. It does this **on your behalf** so the linked site sees your *nest's*
address, not your personal device's — a privacy benefit. The linked site does
learn that *someone on your nest* opened the link, and roughly when.

Media you view from a bridged network (like a Bluesky image) is fetched by your
nest the same way, to keep your device's address private.

---

## Photos you send in a conversation

Fauna removes the location data a photo carries before sending it as a
conversation attachment — so a photo you share does not also reveal where it
was taken. This happens automatically; there is nothing to turn on.

**One limit worth knowing.** This works for **JPEG, PNG, WebP, GIF and
HEIC/HEIF** pictures — including HEIC, an iPhone's default camera format — and
for **MP4/MOV videos**, including the **Motion Photos** many Android phones
take by default — a photo with a short video inside, where the video half
carries its own location; both halves are cleaned. It does **not** work yet for
**PDFs**, **WebM** videos, or **RAW photos** (Apple ProRAW, Android RAW and
other DNG files), which are sent with their metadata still attached. (Earlier versions of
this guide suggested switching your iPhone camera to "Most Compatible" to work
around the HEIC gap; that is no longer necessary.) See
[Photos in the cloud](cloud-photos.md#what-the-metadata-strip-covers) for the
full picture.

---

## What always stays private

None of the following is ever visible to an outsider — anonymous, another user,
or a bridged network:

- **Your sealed posts and message bodies** — encrypted at rest, readable only by
  the intended people.
- **Direct messages** — including, on Nostr, even the fact that two people are
  messaging.
- **Your files and photos** — synced and backed up sealed. The one exception is a
  folder *you* deliberately make public, which you confirm first; see
  [Folders you make public](#folders-you-make-public).
- **Who follows you and who you follow** — bridges publish *counts* at most, never
  the lists.
- **How many users your nest has** — user and post totals are visible only to the
  admin, never to the public.
- **Your contacts and calendar** — served only to you over your authenticated
  apps.

---

## Turning things off — what it does and doesn't undo

| If you turn off… | It removes going forward… | It cannot undo… |
|---|---|---|
| **A bridge** (Bluesky / Nostr / Fediverse) | that network's whole view of your posts | anything already sent to that network |
| **Mail** | the mail server and all its ports (a scanner finds nothing) | public DNS records (remove them yourself) |
| **Content exposure** (Nostr) | new public posts being exposed | posts already read/copied |
| **A public web page or custom domain** | the page and future visits | the certificate already in the public logs |
| **A public folder** (back to private) | outside access to it, and files added afterwards are sealed again | anything already downloaded, indexed, or archived while it was public |

The rule of thumb: **the switches let you reduce what goes out from now on, but
what has already reached a public network or a public log is out.** When in
doubt, decide *before* you enable — that is the moment you have full control.

---

*Related: [A tour of the app](app-tour.md) · [A tour of the admin area](admin-tour.md) · [Your identity, devices & recovery](identity-and-devices.md) · [Bluesky & Nostr from your nest](bridges-bluesky-nostr.md) · [Own your email](own-your-mail.md)*
