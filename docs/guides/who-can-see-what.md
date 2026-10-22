# Who can see what — the ways your data reaches the outside world

Fauna stores your content encrypted on your nest, apart from what you publish to everyone, which is public by design. Nobody who connects to your nest — not another Fauna user, not a stranger on the internet, not the person who runs the server — can read your private posts, messages, files, or contact details unless you deliberately shared them. That is the default, and it does not change.

Some things sit beside that. Your nest sees what any server that delivers your messages must: who you're connected to — your contacts, whom you follow, the groups you're in — and when and how much you send. And beyond what you publish to everyone and the mail and messages your nest passes to and from email and other networks, what any machine may read is a permission you granted and can revoke in the app. A group may also choose to be a community room, which its home nest can read. And a link standing on a line of its own in a post or a message is fetched for its preview by the nest of each person who reads it, so that nest learns the link's address. And someone who can open one of your messages, or a post only some people can open, can attach its text to a report, which the admins it goes to can then read. The sections below say where each of these applies.

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

- **Your private content stays private.** Posts only some people can open, the content of your direct messages on Fauna and Nostr, your files, your contacts, your follower and following lists on your nest, and how many users your nest has — none of these are visible to outsiders. See
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
- **Nothing is on until someone turns it on in the app.** Every bridge is off until you turn it on yourself, from your own app. The mail server is off on a new nest until the nest's admin turns it on — which the first claim of an internet-facing nest at its own internet domain does by default, unless the server was created without email; a nest at home runs no mail receiver of its own, so it gets email from outside only through an internet nest it is linked to. Turning one off stops it from then on; DNS records and certificates already published stay (see [Turning things off](#turning-things-off--what-it-does-and-doesnt-undo)).

---

## Public posts and your profile

When you make a post public, it is public. Anyone can read it. People on your nest can also find it through search, it appears on your web page if you publish it there (**Publish to web**, on the post's menu), and it travels to any network you have bridged it to. Your profile
(display name, bio, picture, banner, and your account's public identifier) is
public in the same way once you have a public presence.

- **Who sees it:** anyone.
- **Reaches other networks?** Only if you turn on a bridge (below). A plain public post on a nest with no bridges is readable by people using Fauna, and on your web page if you published it there.
- **Turning it off:** you can delete a post, but if it already travelled to
  another network, copies there may remain.

Your **paid (monetized) posts** are different: the short **free preview** you
write is public — it shows on the paywall, and it is searchable — but the full body is stored encrypted and opens only for people with a valid subscription (and, for a paid post on your website, for your nest, which opens it for each visitor with a valid code). See
[Paid posts](#paid-posts-what-is-public-and-what-is-not).

---

## Folders you make public

Every folder is private by default. Your files are encrypted on your nest, and nobody — not another user, not an outsider, not whoever runs the nest — can read them, unless you turn on something that needs your nest to read the folder for you: **Serve over WebDAV**, so other file apps can open it, or selling it to your subscribers on your website. A single file in a folder only you can open, and not served over WebDAV, can also be shared as a link (**Share a link**, on the file in **Media**): the link carries the key that unlocks that file, so anyone holding it can open the file in a browser, and your nest never sees the key — see [A tour of the app](app-tour.md#media).

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
on.** Your site is served at your own web address, and that address is opt-in: switch on **Publish my website** in **Settings → Web**. Until it is on, visitors to your address get
the nest's own placeholder page rather than your site — so if you have published
a folder and see a "Fauna Nest" page instead of your own, that switch is why.

You can turn the folder switch on first and make the folder public later; the
app tells you the site is not visible yet rather than hiding the switch. Two combinations
are refused, and your app says so: a public folder cannot also be served over
WebDAV (that needs the folder encrypted), and a public folder cannot be
paywalled (there would be nothing to pay for).

---

## Bridges — Bluesky, Nostr, and the Fediverse

Bridges connect your Fauna identity to other social networks. They are **per person and off by default** — you turn each one on yourself: Bluesky on its own **AT Protocol** page, Nostr on its own **Nostr** page, and the Fediverse on the **Bridges** page. The trade-off is always the same: reach the wider world, and the wider
world can see what you send it. Background on each:
[Bluesky & Nostr from your nest](bridges-bluesky-nostr.md).

### Bluesky

When you link your Bluesky account and turn on cross-posting, your public posts are copied to the Bluesky network. Bluesky's feed is world-readable, and its history is copied by many independent servers and apps: deleting your Fauna post also deletes the Bluesky copy, but anyone who already copied it may keep it.

- **What's published:** the post text (up to a length limit), its links and tags,
  the time you posted, and your own Bluesky handle. **Photos and video are not
  cross-posted.**
- **Mentioning someone does not publish their account key.** A mention of another Fauna user goes out only as the text you wrote. If that text is their handle, Bluesky sees the handle, but nothing else from their Fauna account is attached.
- **Who sees it:** anyone on Bluesky.
- **Turning it off:** on the AT Protocol page, set **Cross-post to Bluesky** to **Disabled** (or to **Manual**, which sends only the public posts you tag `crosspost`). That stops future cross-posts; it does not recall past ones. A reply to, or quote of, a Bluesky post still goes to Bluesky whatever this setting says, since that is where it answers.

**Hosting your Bluesky identity on your nest.** Instead of linking an account you already have, you can choose **Hosted here — visible** on the AT Protocol page, once your nest has its own domain name (until then the option is greyed out, with the reason). Your nest then holds your Bluesky identity itself, with a handle at your nest's domain, and publishes your public posts to the Bluesky network — photos, videos, your profile picture and banner included. Your earlier public posts go too only if you tick **Also publish my *N* existing public posts** (the box shows how many there are). Paid posts, posts you imported from another service, posts for a room and messages are never published. The app shows what a change will do before anything happens. Stepping back down stops publishing and takes your account offline on Bluesky, keeping the identity for if you return; **Delete my Bluesky presence** removes what your nest published. Neither can recall copies the network already made.

### Nostr

Nostr is a public relay network. When you turn on **Expose content** on the Nostr page, your public posts become readable by anyone who connects to your nest's relay — no account needed. Because Nostr is built for open reach, a few things follow:

- **Anyone can read** all your exposed public posts, and can **search** them by
  keyword and **count** them (how many posts you've made, how many replies
  reference a given post). This is normal for a public relay.
- **Your direct messages stay private.** Nostr direct messages travel in encrypted envelopes that hide what is said, and who sent it, from the relays that carry them. Your nest opens the ones that reach you as they arrive, wraps the ones you send, and stores them encrypted for you. The relays can still see who a message is addressed to and roughly how large it is.
- **Who sees it:** anyone, for your exposed public posts.
- **Turning it off:** turn off **Expose content**. Your relay stops serving the posts it already serves, and serves no new ones except those your nest still publishes for you: a post **Auto-publish posts** sends, or a quote of or reply to a Nostr note (a reply only while **Publish replies** is on). Copies other people already fetched remain, and so do posts already sent to other relays.

### The Fediverse (ActivityPub / Mastodon-compatible)

Enabling Fediverse federation publishes your handle to the wider Fediverse and
lets Mastodon-style servers follow you and receive your public posts.

- **Enabling it publishes your handle permanently.** Your Fauna handle becomes
  your Fediverse address, and it is remembered by every server that sees it.
  There is no rename afterwards, so choose before you enable.
- **What's shared:** your profile and your public, non-paid posts. Your follower and following **counts** are visible, but your nest does not publish **the lists themselves**. When you follow someone on the Fediverse, or someone there follows you, their server sees it. Paid-post full text is never federated.
- **Who sees it:** any Fediverse server and its users, once you enable it for your
  account.
- **Turning it off:** stops future federation; servers that already cached your
  handle or posts may keep them.

---

## Mail

Running your own mail at your domain is a powerful feature, and mail is an old,
public protocol. When mail is on, a nest on the internet becomes a mail server that the rest of the internet must be able to reach — so it announces itself. (A nest at home runs no mail receiver of its own: once it is linked to an internet nest, that nest receives its mail and is the one that announces itself.) See
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
  addresses exist. **This can be closed:** your nest's admin can choose a **catch-all** address for a domain (on the admin **DNS** page, on the domain's row) so every address is accepted and none can be singled out.
- **Your stored mail stays private.** Your nest encrypts each message as soon as it arrives. After that, your Fauna apps open it on your own devices; a mail app you connect reaches it through your nest, which opens it with that app's password and only for that signed-in session; and beyond those, only a machine you have given permission to read your mail can open it. Email itself is an open protocol, though: your nest, and the sender's and recipient's mail servers, handle a message in readable form while delivering it. Outgoing mail is scrubbed of your device's address and internal details before it leaves.
- **Who sees the footprint:** anyone (the greeting, the DNS records). **Who reads your mail:** you; your nest, while it delivers a message and while it serves a mail app you connected; any machine you have given permission to read it; and — as with all email — the other mail servers that carry it between you and the other person.
- **Turning it off:** turning mail off closes the mail ports completely — a
  scanner finds nothing. The public DNS records stay until your nest's admin removes them.

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
- **The full body is stored encrypted** and opens only for people who hold a valid subscription — and, for a paid post on your website, for your nest, which opens it for each visitor with a valid code. It never travels to Bluesky, Nostr, or the Fediverse.
- **Your tier names and prices are public.** So the paywall and other networks can
  show them, your subscription tiers, their prices, and payment links are readable
  by anyone. This is the storefront, meant to be seen.

**What a visitor without a Fauna account sees.** A paid post on your website
shows the free preview, your price and payment link, and a box to enter an
access code. A visitor who pays through your payment link comes back to a page on your site that shows their access code, once you have set your payment provider's "after payment" redirect to your site's return address (the provider form shows it, with a button to copy it, once your web address is switched on). One you gave a code to by hand enters it directly. Entering the code opens the full post for a few minutes.
The code itself keeps working: when the link expires, the visitor enters the
code again on the paywall page (most browsers remember it), so they should keep
it somewhere safe. The same code can still be added to a Fauna account later.
A code stops working when the payment is refunded or the subscription it paid
for runs out. Paid files in a paywalled website folder work the same way, one
file at a time.

## Posts for one room — what is public and what is not

A post you address to a room also has a **public teaser** and a **full body encrypted for the room**.

- **The teaser is public**, exactly like a paid post's, and so is the fact that
  the post is for a room: its card carries a badge for anyone who can see your
  posts. A member of the room sees the room's name on it, as it appears in
  their own conversation list, and opens the post as usual; everyone else
  sees "room".
- **The full body is encrypted with the room's own key**, the same protection the room's messages have. Only the room's members can open it, each on their own device — and, if the room is a community room, its home nest, which labels and indexes it for the members.
- **Membership decides, not following.** Someone who follows you but is not in
  the room sees only the teaser. Someone who leaves the room keeps what they
  could already open, but not what you post after they left.

## Replying to a post that is not public

A reply is a post of your own. Ordinarily it goes to **your** followers, not to
the readers of the post you answer — so under a paid post or a post for a room,
the app takes care that your words do not end up somewhere wider than the post
they answer:

- **If you are in the room, your reply goes to the room.** Reply to a post
  addressed to a room you are a member of, and your reply is encrypted for that same room, exactly like the post it answers. Everyone else sees that you replied — a card with the room's badge and no text — but only the room's members can open it and read what you said (and, in a community room, its home nest, as for the post).
- **If it is your own paid post, your reply goes to the same subscribers.**
  Replying under a post you restricted to one of your tiers encrypts the reply for that tier: your subscribers can open it, and nobody else can.
- **Otherwise, your reply can only be public — and the app asks first.** A
  subscriber replying under someone else's paid post, or a reader who is not
  in the room, has no way to write for that audience: a reply would go to your
  own followers, like any post of yours. So the reply box says so, and offers
  a checkbox, *Post my reply publicly*. Leave it unchecked and the reply is not
  sent — the app tells you why instead of posting your words to everyone.
  Check it and your reply is posted publicly, in the clear. The box is unchecked every time you open the reply box; nothing is remembered.
- **A repost, or a quote without commentary, is still public** — it carries no
  words of yours, and shows your followers only what they could already see:
  the teaser and the badge.

What stays visible on a reply only its room or tier can open is *that* you replied, and to which post.
What you wrote does not.

---

## Search

Search on a nest covers public content — public posts and profiles. Anyone with
an account on the nest can search it, and the **free preview** of a paid post is
included (because that preview is public anyway). The full body of a post only some people can open, direct messages, and your files never reach the nest's search. (A community room's own search lives in the room, for its members — see [A tour of the app](app-tour.md#conversations).)

---

## Your handle, your domains, and security certificates

Some public footprints come from the security certificates and web addresses a
nest uses:

- **A public web page for your posts, or your own domain,** means a security
  certificate is issued for that address. Certificates are recorded in public
  **Certificate Transparency** logs — a permanent, world-searchable record. So
  enabling a public page at `alice.example.com` or connecting your own domain
  makes that name permanently public, even after you turn the page off.
- **This is normal for the whole web** — every HTTPS site appears in these logs.
  It is worth knowing only because it is *permanent*: the record cannot be
  removed later.
- **Who sees it:** anyone (the logs are public).

If you would rather not publish a name this way, do not enable the public web page
or custom domain for it.

---

## Link previews and shared media

When a post or a message shows a link on a line of its own, the app of each person reading it asks that reader's own nest for a preview, and the nest fetches the linked page. So the linked site sees the reader's *nest's* address, not their personal device's — a privacy benefit. The site does learn that *someone on that nest* is looking at its link, and roughly when, and the reader's nest learns which link it fetched.

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
other DNG files), which are sent with their metadata still attached. See
[Photos in the cloud](cloud-photos.md#what-the-metadata-strip-covers) for the
full picture.

---

## What always stays private

None of the following is visible to an outsider — anonymous, another user, or a bridged network — except where a bullet below names the exception:

- **Posts only some people can open, and the bodies of your messages on Fauna** — stored encrypted, readable only by the intended people (and, in a community room, by its home nest; for a paid post on your website, by your nest, which opens it for each visitor with a valid code; someone who can open a message or a post only some people can open can also attach its text to a report, which the admins it goes to can then read; email is the exception while it travels; see [Mail](#mail)).
- **Direct messages** — their content, and on Nostr who sent them. A conversation with someone on Bluesky or the Fediverse is the exception: the network that carries it can read what is said.
- **Your files and photos** — synced and backed up encrypted. The exceptions are a folder *you* deliberately make public, which you confirm first (see [Folders you make public](#folders-you-make-public)); a file you share as a link, which anyone holding the link can open; and a file in a folder you sell on your website, which your nest opens for each visitor with a valid code.
- **Your follower and following lists on your nest** — bridges publish *counts* at most, never the lists. When you follow someone on another nest or another network, or someone there follows you, their nest or that network sees it.
- **How many users your nest has** — user and post totals are visible only to the
  admin, never to the public.
- **Your contacts and calendar** — served only to you over your authenticated
  apps.

---

## Turning things off — what it does and doesn't undo

| If you turn off… | It removes going forward… | It cannot undo… |
|---|---|---|
| **A bridge** (Bluesky / Nostr / Fediverse) | that network's whole view of your posts | anything already sent to that network |
| **Mail** | the mail server and all its ports (a scanner finds nothing) | public DNS records (until your nest's admin removes them) |
| **Expose content** (Nostr) | your relay serving your posts, apart from new ones your nest still publishes for you (see [Nostr](#nostr)) | copies already fetched, and posts already sent to other relays |
| **A public web page or custom domain** | the page and future visits | the certificate already in the public logs |
| **A public folder** (back to private) | outside access to it, and files added afterwards are encrypted again | anything already downloaded, indexed, or archived while it was public |

The rule of thumb: **the switches let you reduce what goes out from now on, but
what has already reached a public network or a public log is out.** When in
doubt, decide *before* you enable — that is the moment you have full control.

---

*Related: [A tour of the app](app-tour.md) · [A tour of the admin area](admin-tour.md) · [Your identity, devices & recovery](identity-and-devices.md) · [Bluesky & Nostr from your nest](bridges-bluesky-nostr.md) · [Own your email](own-your-mail.md)*
