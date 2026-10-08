# A tour of the app

> **What this covers:** every screen in the Fauna app — what it is for and what
> you can do there. It applies to every Fauna app: web, Linux, macOS, iOS,
> Android, Windows, and the terminal app. Fauna is built as **one app**: the
> same pages, the same names, the same behavior everywhere, arranged to fit
> each screen.
>
> **How to read it.** This is a map, not a manual — each section says what a
> page does in a paragraph or two and points to the guide that goes deeper.
> New to Fauna? Do [Getting started](getting-started.md) first; this tour
> assumes you are signed in.

> **A note on completeness.** Fauna is in alpha and the apps advance in
> parallel: everything below is real, implemented UI, but a few screens reach
> some platforms before others. This tour deliberately describes the app at its
> fullest rather than tracking who has what this week — if a screen is missing
> on your platform, it is parity work in flight, not a design difference.

## One app, three shapes

Navigation is the same everywhere; only its container changes with the form
factor:

- **Phones** (iOS, Android) show **Feeds**, **Conversations**, and **Contacts**
  up front, with the remaining pages behind a *More* menu or drawer. A few
  house-keeping pages (and the Admin area, if you are an admin) live inside
  *Settings*, where phone apps traditionally keep them.
- **Desktops** (Linux, macOS, Windows) and the terminal app show all pages in
  a **sidebar**, with Settings at the bottom. In the terminal app a dim line
  along the bottom of the screen names the keys that work where you are — left
  and right move between the sidebar and the page, up and down move within one,
  and Enter opens what is highlighted. The line changes as you go, so it only
  ever offers keys that do something right then. Below every sidebar entry
  sits **Exit Fauna** — the terminal app's only quit affordance besides `q`
  and Ctrl+C, since a terminal window has no titlebar close button.
- **The web app** puts the same list in a navigation bar; every page has its
  own address under `https://<your-nest>/app/`.

Every page shows errors in place — there is no hidden log to consult when
something fails (though there is a log page, see [Settings](#settings)).

## When the app cannot reach your nest

Most of the app keeps working without a connection. Writing a post, liking
one, teaching a feed what you like, editing your profile, answering a
subscription request, accepting a contact — all of these are yours to do
offline; they are saved on the device and sent on as soon as your nest is back.

A few things genuinely cannot happen without your nest, because it is the one
that has to decide them: paying for something, following or subscribing to
someone, adding a payment provider, minting a claim code, linking a bridge
account, changing your handle, pairing another nest or handing one a share of
your trust, restoring a backup, handing guardianship of a child account to
someone else, and disconnecting an app you had connected. Those controls turn
grey while the connection is away, each one saying *Needs a connection to your
nest* rather than letting you press it and fail.

Grey means *this one needs the nest*, never *the app is offline*. Opening a
form, typing into it, choosing an option and closing it again all keep working
with no connection — only the button that would commit the change waits. So you
can fill in a form while disconnected and press it the moment you are back.
There is no "offline mode" to switch on or off, and no banner across the
screen — each control answers for itself, and comes back on its own when the
connection does. The current state is always on screen: see **Status** under
[Settings](#settings).

## Feeds

The reading and writing surface. A *feed* is a filtered, ranked view of posts;
you can keep several and switch between them in the feed list.

- **Compose** — write a post with tags, attach images or files. A compact
  compose bar sits on the page; a button expands it into a full compose dialog.
- **An unfinished post keeps itself** — walk away mid-sentence, quit the app, or
  pick up a different device: the text, the tags and the audience you picked are
  all still waiting in the composer. Only posting it, or clearing the box
  yourself, lets it go. Like everything else you write, an unfinished post is
  encrypted before it leaves the device — your nest stores it, but cannot read
  it. A file you attached is remembered by name, but the file itself stays on
  the device you picked it on: after quitting the app, or on another device,
  the composer still shows the file's name and size, and posting asks you to
  attach it again rather than posting without it — or remove it to post just
  the text.
- **View a picture** — click or tap an image on a post to open it full-screen,
  and dismiss it to go back to the feed. In the terminal app the picture is
  redrawn larger rather than just repeated at card size.
- **Play a video** — tap a video on a post and it plays right there in the
  feed, with the usual player controls. Nothing downloads until you tap, so a
  video never starts on its own.
- **Content provenance** — a small badge appears on images carrying embedded
  content-credential (C2PA) information — the kind cameras and editing tools
  can attach to certify how a picture was captured or edited. Tap it to see
  who signed it and when.
- **Post to a tier** — creators can limit a post to one of their subscription
  tiers with the composer's audience picker. You write a public teaser everyone
  can see and a full body only subscribers can open; the full text is encrypted
  on your device before it leaves, so no server along the way can read it.
  Gated posts wear a small tier badge in the feed; subscribers just open the
  post and the full body appears — photos included. A gated post's pictures are
  locked with the same key as its words, so a subscriber sees them the moment
  the post opens and nobody else can fetch them at all.
- **Post to a room** — the same audience picker lists the group conversations
  you are in, as "Room: …". Pick one and write a public teaser as you would for
  a tier; the full body is sealed with that room's own key on your device, so
  only the room's members can open it. Its card wears a "room" badge, and a
  member just opens the post to read it — photos included.
- **Sell a single post** — the same audience picker has a "Sell this post…"
  option for charging for one piece on its own, with no subscription involved.
  Pick it, set a price, and write the public teaser exactly as you would for a
  tier-gated post. "Subscribers get it free" is on by default, so people already
  paying you aren't asked to pay twice; turn it off to make the post
  pay-per-view for everyone. You don't create or manage anything beforehand —
  the access is set up for you as part of publishing, and the post behaves like
  any other gated post from then on.
  There are two separate price fields, and they do different things: the
  regular price is just text shown to readers (write it however you like, like
  "$3" or "pay what you want"), while the optional asking price is a number in
  sats that lets a payment actually unlock the post automatically once it meets
  that amount — leave it blank if you only want the display price. The same
  optional asking price is also available when creating or editing a
  subscription tier, for the same reason.
  A sold post can carry a picture, and the picture is protected the same way
  the writing is: only people who have bought the post can see it. Attach the
  file the usual way — choose "Sell this post…" first, then attach, so the app
  knows to lock the picture rather than publishing a readable copy of it.
- **Buy a sold post** — a sold post you have not bought yet can show its price
  right on the card with a Buy button, which sends the same access request as
  subscribing to it from the seller's profile — no need to go find the right
  tier there yourself. The price and Buy button drop off the card once your
  request is on its way, so you can see it went through and won't ask twice.
  It is approved the same way any subscription request is
  (see [Profile](#profile) above): automatically once the seller's payment
  verification confirms it, otherwise once they review it, or via a claim code
  if you paid another way.
- **Posts** show author, text, tags, and media; open one for the full thread
  with replies and quoted posts. You can delete your own posts at any time
  (a two-step confirm in the post's menu) — it removes the post everywhere it
  was shown, including any copy it was forwarded or bridged to. Other people's
  replies and quotes of it stay, because those are theirs; where a quote showed
  your post, it now says "Post not found".
- **React to a post** — like, reply, repost, or quote it from the row under
  every post; each button shows a plain icon until the post has any activity,
  then an icon plus a count. Quoting reposts it into your feed today; adding
  your own words to a quote is on the way. **Like and repost are both
  toggles**: tap once, tap again to take it back — no confirmation dance either
  way. A liked post keeps its heart lit so you can see at a glance what you
  have already reacted to, and the count moves in both directions. A repost
  shows up in the timeline as a compact card of the original with a "reposted"
  note; open it to read and react to the original itself.
- **See who tipped a post** — a post that has received tips shows the total and
  how many people sent one, right on the card. Tap the total to see who they
  were: each tip names the sender when it can, or the account it came from on
  whatever network carried it. A tip is a straightforward thank-you — it buys
  nothing and unlocks nothing, so nobody gets extra access by paying. Sometimes
  a payment arrives without a readable amount; when that happens the post still
  says how many tips it got, and simply shows no figure, rather than pretending
  the amount was nothing.
- **Trending** — a built-in feed at the top of the list shows what is drawing
  unusually high *recent* engagement, on your nest and across the nests it talks
  to. It is computed only from public engagement counts and timestamps (never
  post content), the signal fades over hours, and it composes with your own
  factors — so a muted word still sinks a trending post. Trending-among-your-
  corner-of-the-network, not a global chart. Trending is also one of the
  factors you can weigh when you build a feed: into that one feed, or into all
  of your feeds at once.
- **Build your own feed** — create a feed from rules (include/require terms,
  tags, sources) combined with ranking factors and weights. Your feeds are
  yours: delete and remake them freely.
- **Follow the wider world** — subscribe a feed to an external source through
  a [bridge](bridges-bluesky-nostr.md) (for example a Bluesky feed), and posts
  from there appear alongside native ones. Pictures on posts from Bluesky and
  the Fediverse show in the feed too, loaded through your own nest — your
  device never contacts the site they came from.
- **Search within the feed** filters what is on screen; for searching your
  posts and profile info more broadly, see [Search](#search).

What lands in your feeds is further shaped by your
[Personalization](#personalization) settings — muted words, community
labelers, and trained topics.

## Conversations

All person-to-person communication in one place. Fauna does not separate
"chat" from "email": a conversation is a thread, and each thread rides a
*rail* — end-to-end-encrypted Fauna messaging, email (if your nest has mail
enabled), or a bridged network (Bluesky, Nostr, the Fediverse). An icon on
each thread shows which; the reading and writing experience is the same.

- **The list** — threads with sender, snippet, subject and unread counts;
  filter by typing, sort as you like. A thread is marked unread when a message
  from someone else arrives while you don't have it open; opening the thread
  clears the mark, and messages that arrive while you are reading it never set
  it. Your own messages never count. On Fauna messaging threads the mark
  belongs to your account, not to one device: a thread you read on one device
  is read on the others, and a message that came in while the app was closed
  is flagged the next time you start it. Email follows your mailbox's own read
  flag: a mail that came in while the app was closed is marked unread at
  launch, one you already read in another mail program is not, and opening an
  email thread marks it read on your other devices and mail programs too. The
  first launch may mark older mail you only ever read in Fauna as unread once;
  opening the thread clears it.
- **New conversation** — type a handle or an email address into the recipient
  picker; it resolves and confirms who you will reach, and the chip shows which
  way the message will travel. If someone you already talk to over Fauna can't
  be reached right now, the picker says *Lookup failed — try again* instead of
  quietly falling back to plain email. Add several recipients
  and it becomes a group. Subjects are optional — toggle one on for
  email-style threads; changing the subject mid-thread shows as a divider.
- **In a thread** — messages with timestamps and attachments (images inline),
  a reply box with formatting toolbar and file attach. A file you attach shows
  up in the reply box as a small tag with its name and size, so you can see
  what is going out — and take it back off with the × before you send. Badges
  mark what the cryptography actually verified: *encrypted*, *signed*,
  *verified sender*. A received image carrying embedded content-credential
  (C2PA) information — the same provenance data the Feeds badge shows — wears
  its own small badge too.
- **Conversations over a bridge** — when an app you connected carries another
  network's conversations into Fauna, each one appears in this same list, with
  that bridge's own icon and name on the row so you can tell which network it
  came from. Open one and the header reads *Transport-only*: the bridge sits
  between you and the other person and passes each message on, so it — and the
  network behind it — can read what is said. Your nest cannot: it only stores
  messages sealed for you, and your reply is sealed so that only that bridge
  can open it. What you can do in such a thread (attach files, reply to one
  message, and so on) is whatever that bridge says it supports. To write to
  someone new, type their address the way that network writes it; the
  new-conversation box lists the networks you can reach this way and tells you
  which one recognised the address. Reading and replying need your mailbox to
  be turned on, because bridged messages are sealed with the same key as your
  mail.
- **A conversation your guardian is reviewing** — on a supervised account, a
  bridged conversation with someone your guardian has not approved yet is
  marked *Waiting for your guardian*, in the list and at the top of the
  thread. You can still open and read it. If your guardian decides against
  that person, the mark changes to *Blocked by your guardian*: their new
  messages no longer arrive, and you cannot write to them.
- **Being told a message arrived** — while the app is running, a new message
  raises your system's own notification, showing who wrote it and the first
  line. The one exception is the conversation you already have open: you are
  looking at it, so it stays quiet. Threads that were already waiting when you
  signed in are not announced either — only what arrives while you are there.
- **Formatting you can read while you write** — the reply box styles Markdown as
  you type: `**bold**` turns bold, `` `code` `` turns monospace, headings and
  quotes take their shape. The asterisks and backticks themselves stay out of
  your way — they are hidden until your cursor moves into the phrase they wrap,
  which is when you might want to edit them. What you typed is exactly what gets
  sent; nothing is rewritten behind your back. If you would rather see every
  marker all the time, the toolbar's show-markers button brings them back for
  that box, dimmed so the words still stand out.
- **Groups** — rename the thread, see members in the header, add participants.
  Adding or removing someone can fail — their device may not have published the
  keys yet, or your connection may drop mid-change. When that happens the member
  list stays as it actually is, and the page tells you what went wrong so you can
  try again; nobody is ever shown as part of a group they are not in. Adding
  someone who is already there is harmless, and repeating an add that was cut
  short is exactly how you finish it.
- **Members whose name you have not met yet** — join a group someone else set
  up and your app may not know everyone's name at first. Those people show as
  a short code rather than as an empty row, so you can always tell one member
  from another and count who is in the room; anyone your app has met elsewhere
  shows by name, and the rest are filled in as the group's home nest learns
  them — including members who live on another nest, whose own nest introduces
  them by name the first time they read the group from there. This works from
  either seat: when the group itself lives on someone else's nest, your app
  reads its member list through your own nest, so you see the same names
  everyone else in the room sees.
- **Who runs a group** — a group you start is yours: you are its *owner*, and
  the header marks you and any *admins* you appoint beside your names. Owners
  and admins invite people, remove them, and rename the group; everyone else
  reads and writes, and their add and remove controls show greyed rather than
  hidden, so the room's shape is visible to all. Nobody can remove the owner.
  These rules are enforced by the members' own devices, not by a server — a
  change someone's role does not allow is refused before it ever leaves their
  device, and every other member would refuse it if it did. A group that
  includes someone whose app predates these rules works exactly as groups
  always have, with no roles; the header shows which kind of room you are in.
- **Deleting someone else's message** — anyone can delete a message they sent
  themselves. In a group with roles, the owner and the admins can also delete a
  message another member sent: the same *Delete* in the message's **⋯** menu,
  with the same confirmation, and everyone in the group then sees "This message
  was deleted" in its place. A plain member's menu simply has no *Delete* on
  other people's messages. Each member's own device checks that the person
  deleting really was the owner or an admin when they did it, so someone who is
  later demoted cannot have their earlier deletions undone, and cannot make new
  ones. A member still running an older version of the app keeps seeing the
  message until they update.
- **Room settings** — the header's *Room settings* control (open to every
  member) shows the room's rules: *who can invite* (owner and admins, or any
  member), *history for new members* (nothing before they join, or the whole
  conversation), and — for the owner alone — an admin switch and a *make
  owner* control beside each member. The controls that change the rules and
  Save are live for owners and admins and greyed for everyone else, who read
  the settings but cannot edit them. Save applies the changes; a change your
  role does not allow is refused right there, with the reason. Handing the
  room over is the owner's act
  and completes on the new owner's device: once it has, the room is theirs
  to run, and you are an ordinary member — free to leave, or to be removed
  like anyone else.
  **Leave room** is in the same settings, below Save, and it asks you to
  confirm. It takes effect at once rather than waiting for Save, and nobody
  else has to act: the other members stop seeing you among them, and the room
  stops reaching you. The owner cannot use it — a room always has an owner, so
  hand the room over first and then leave. Leaving does not delete your copy:
  the conversation stays in your list and everything you already read is still
  there to re-read.
  In a **community** room — the kind the header describes as searched and
  labelled by the home nest — the same settings also list the community
  labelers published on that nest, each with an *Inspect* button that shows
  exactly what it looks for before you pick it. The owner and admins switch on
  up to four, and Save applies them; from then on each new message is
  labelled as it arrives, with the same badges you see everywhere else, and
  every member's own settings decide what a label does. Other members see
  which labelers the room uses, greyed.
  When history is shared, the person who adds someone sends them the
  conversation so far from their own device; under the other setting a
  newcomer sees only what is said after they arrive. The new-conversation box
  states which kind of room you are about to create before the first message
  goes out.
- **Community rooms** — a room can also include your home nest as a member.
  In the new-conversation box, switch *Home nest joins* to *yes* before you
  send the first message: the box then says the room is a community room,
  searched and labelled by the home nest, and your first message creates it.
  Messages are still locked on the members' devices before they leave, but
  your home nest holds a key too, which is what lets it search the room for
  its members — the one search nobody's own device can run over a long
  conversation it only holds part of. Everyone you added gets an invitation
  at the top of their conversation list naming who asked them, with
  *Accept* and *Decline* — people on other nests too, addressed by their
  full handle, exactly as anyone else. Accepting opens the room right away; its messages
  appear once the owner's or an admin's app lets you in, which happens by
  itself the next time one of them is online — until then the room's header
  says it is waiting for a room key. If the room's moderation rests on
  someone your device cannot check — an owner or admin who has since moved to
  a new identity on another nest — the header says so too: the messages that
  moderation acted on stay shown rather than silently disappearing, because
  your device could not confirm the action. Declining just removes the
  invitation — nobody is told. Invitations still waiting for an answer are
  listed in the room settings under *Pending invitations* — each row names
  who was invited, who invited them and the rank on offer, and says so when
  it can no longer be accepted (the person who sent it lost the rank that
  let them). The owner and admins see every pending invitation, and anyone
  else sees the ones they sent. *Withdraw* takes one back at once, without
  saving: the invitation simply stops standing, the invitee is not told,
  and inviting them again is the undo. The owner and admins can take the home
  nest's key back at any time: *Home nest reads this room* in the room
  settings, switched to *no* and saved. The nest then deletes everything it
  had built from the room, and the room keeps working exactly as before,
  only without the nest's search.
- **Unfinished messages keep themselves** — a half-written message is not
  something you have to remember to save. Leave a thread mid-sentence, start a
  new conversation and come back, quit the app entirely, or pick up a different
  device: the text, the subject and the files you had attached are all still
  waiting where you left them. A file comes back as its name and size, though,
  not the file itself: send from a device that no longer has it — a different
  device, or this one after the app has restarted — and the app asks you to
  attach it again rather than sending the message without it. Each thread keeps its own unfinished reply, and
  the new-conversation box keeps one of its own, so switching between them never
  costs you a word. Only sending the message, or explicitly cancelling it,
  clears one. Like everything else you write, these drafts are encrypted before
  they leave the device — your nest stores them, but cannot read them.
- **Junk control** — mark a message as spam from its menu; that trains your
  personal filter (see the Spam section under
  [Mail, calendar & contacts](#mail-calendar--contacts)).

For how email specifically flows through here, read
[Own your email](own-your-mail.md).

## Contacts

Two address books in one page, switched by a segment control:

- **Contacts** — your social graph on the network. Find a person by handle,
  send a contact request (a *knock*), accept or dismiss requests aimed at you,
  and block senders. Your inbox-mode setting (under Settings → privacy)
  decides who may knock at all. The person does not have to be on your own
  server: type their full address — `someone@their-server` — and the knock
  travels to wherever they are, landing in their requests list there. You need
  nothing set up in advance for this, and neither do they.
- **Address Book** — classic contact *cards* (names, emails, phone numbers,
  addresses), the kind your phone's contacts app holds. These sync over
  CardDAV — see [Calendar and contacts](calendar-and-contacts.md) — and open
  into a card detail view.

## Profile

Your public face — and everyone else's. Opening your own profile shows
editing controls — display name, bio, links, and a profile picture and banner
image — plus a Tiers tab; opening someone else's adds the relationship
actions: **follow**, **message**, **block**, **report**, and **request
contact** — the same contact request (knock) [Contacts](#contacts) sends,
without having to look the person up first.

**Reporting** tells your nest's administrators about something you think they
should see — an account from its profile, a post from its **⋯** menu, or a
message someone sent you from that message's **⋯** menu. You pick a reason,
can add a note, and can also block the person in the same step. A message is
private, so your administrators cannot read it unless you tick the box that
attaches its text — the box says so. When the person you are reporting lives
on another nest, your report is passed on to that nest's administrators too,
without your name. A report informs; it removes nothing and is never shown to
the person you reported. What it does change is your own view: what you
reported shows *You reported this* in its place from then on. Your reports,
and what became of each, are listed on the Moderation page, where you can
withdraw one while it is still open.

Someone else's profile also has a section marked **Only you can see this**,
for what *you* want to remember about them:

- a **nickname** — your own name for them. Once set, it is what the app calls
  them: at the top of their profile, in your contacts list, above their
  messages, among a group's members, on their posts and on a subscription to
  them. Where there is room, their public name stays
  underneath, so you never lose track of who someone really is. Among a
  conversation's members the nickname appears once the encrypted conversation
  with that person is set up — until then the member shows the address you
  typed, so a name you gave someone can never be borrowed by an address that
  has not yet proven who is behind it. The nickname
  never leaves your devices in anything they or anyone else can read — a
  message you write still names them by their public handle;
- **notes** — anything you like, for your eyes only;
- **labels** — short tags such as "family" or "book club". Your contacts list
  shows each person's labels, and typing a label in its search box narrows the
  list to the people carrying it. When you add a label, the ones you have
  already used on other people are offered.

Edit any of these and press **Save**; nothing changes until you do, and only
what you changed is saved, so an edit you made on another of your devices in
the meantime is kept. You can do this for anyone whose profile you can open,
whether or not they are in your contacts, and it stays even if you later
remove or block them. It is shared with all your own devices and nobody else.
If the person later replaces their identity (for example after recovering a
lost account), what you wrote about them moves over to their new identity by
itself once the app has confirmed the change.

Profiles also carry Fauna's subscription machinery, for creators who want
supporters:

- **On your own profile** you define *tiers* (free or paid levels of access),
  review pending subscription requests, and see your subscribers per tier.
- **On another profile** you see the tiers on offer and can subscribe.
  Following someone is simply subscribing to their free tier. A paid tier
  usually needs the creator's approval, so the row says so until they get to
  it; open the Tiers tab again whenever you want to check — it re-reads every
  time, and the row changes to show your subscription once you are in.

Creators charging for a tier can connect their own payment provider in the
**Payment providers** section of the same tab: pick the provider, paste the
webhook verification secret from the provider's dashboard, and choose which
tier a payment unlocks. As soon as you pick a provider, the form shows the
exact webhook URL to register at that provider's dashboard, with a button to
copy it — no more hand-assembling the address yourself. From then on a
verified payment notification grants the buyer's subscription automatically —
money never touches Fauna, and the secret you store can only *verify*
notifications, never move funds. Each connected provider shows a status —
"Configured" until its first notification arrives, "Verified" once one has
been confirmed, or "Error" if the most recent notification failed
verification (worth re-checking the secret) — based purely on notifications
actually received, never an active check. A payment-verified request in
**Pending requests** carries a "Paid" badge so you can see at a glance which
subscribers paid rather than requested access directly.

Payments made without a linked Fauna account (bank transfer, cash, or any
other out-of-band method) produce a *claim code* the buyer can redeem later.
You can also mint a code yourself in the **Manual claim codes** section for
any tier — hand the code to the buyer however you like. Every code, whether
you minted it or a provider's webhook did, is listed there with its
redemption status (unredeemed, redeemed, or voided) so you always have an
audit trail of who's paid.

Your own active subscriptions are listed under Settings →
[Subscriptions](#subscriptions).

## Events

A full calendar. Create any number of calendars, control each one's
visibility, and view them as an agenda, month, week, or day. Events carry
description, location, start/end times, and reminders; invitees RSVP with
*going / interested / declined* right on the event — including invitees on
other nests, and (via email) people on Gmail or Outlook.

The same calendars are served over CalDAV, so Apple Calendar, Thunderbird,
and DAVx⁵ can edit them too — the whole story is in
[Calendar and contacts](calendar-and-contacts.md).

## Media

A file explorer over everything you store in Fauna. Media shows the contents
of all your [folders](#folders) in one view — switch between list and
thumbnail layouts, sort by name, size, or date, filter down to a single set,
and upload new files. The set filter lists every folder you have, including
one you created a moment ago and have not put anything in yet — so a brand-new
set is ready to receive its first upload, and a set published as a website
shows up here too, browsable like any other. (A backup-only set can be
browsed but not uploaded into — its files arrive from the device being
backed up.)
Each item shows whether the files in its folder can be
reached right now, alongside the file's own state — synced, uploading, downloading, or
waiting to be fetched. Opening an item gives you a detail view with the file's
**version history**, from which any older version can be restored, and a
**Download** button that saves the file itself: Fauna decrypts it — with the
folder's shared key when the folder was shared with you, with your own key
otherwise — and hands it to your browser's or system's usual download path.
A file in a public folder you follow downloads the same way, with no key
involved.

Media keeps up while you are looking at it. A file that arrives — from another
of your devices, from someone you share a folder with, or from a folder Fauna
is watching on this device — turns up in the list on its own, within moments.
You do not have to leave the page and come back.

The detail view is also where you **delete a file**. Deleting asks you to
confirm, then removes the file everywhere it syncs. It is not a per-device
action and it is not reversible from the app, so the confirmation is worth
reading. Two things it deliberately does *not* touch: snapshots you have
already taken keep their copy of the file (that is what
[Backups](#backups) are for), and destinations you have set up as
backup-only are never asked to drop it.

A file in a folder you have made public can be **shared as a link**. Its
detail view offers **Share a link**: choose how long the link lasts — a day,
a week (the default), a month or a year; there is no "forever" — and
**Create link**. The link appears once your nest has recorded it, ready to
copy and send; whoever receives it opens the file in an ordinary browser,
with no account. **Shared links** on the Media page lists every link you have
made — active, expired or revoked — and lets you copy an active one again or
**Revoke** it, after which it stops working for everyone.

A file in a private folder — one only you can open — can be shared as a link
too. That link carries the key that unlocks the file, and Fauna tells you so
when you make it: anyone holding the link can open the file, and wherever you
paste it, anyone who can read that place can open it too. Whoever opens it
sees the file's name and size and a **Download** button, with a picture,
sound, video or plain-text file shown in place; the file is unlocked in their
own browser, and your nest never sees it, its name or the key. Because the
key exists only in the link you copied when you made it, **Shared links**
cannot copy a private link again — keep the one you copied. Files in folders
you share with other people do not offer a link.
On Windows you can start from File Explorer too:
right-click a file in a public synced folder → **Fauna → Share**, and Fauna
opens on that file with **Share a link** ready; on a synced folder itself,
**Share** opens that folder's sharing in Fauna.

In the terminal app — which plays no audio or video inline — an audio or
video file's detail view offers **Open externally** instead: Fauna decrypts
the file to a private temporary location and hands it to your system's own
media player. By default it asks before opening anything; a setting under
Settings lets you choose *always* or *never* instead, and *never* removes the
option entirely.

How files get here — sync locations, photo backup, sharing — is the
[Your own cloud](your-own-cloud.md) series.

## Backups

Point-in-time snapshots of your folders, and the controls for keeping a
copy of your nest's data somewhere else entirely.

- **Snapshots** — pick a folder, see when it last backed up, create a
  snapshot now, browse a snapshot's files, apply the set's retention policy
  (shown as a preview first, so you see what would go before anything does),
  and verify integrity. Snapshots you have deleted stay listed with the
  deadline they can still be recovered by. Deleting a snapshot immediately
  requires re-typing its name *and* an acknowledgement — destructive actions in
  Fauna never ride on a single click.
- **Backup destinations** — register another nest as an off-site destination;
  each destination row shows its last successful upload and any backlog.
- **Restore** — pick a destination, pick a snapshot, choose what to restore
  (mail, calendar), confirm by typing, and watch progress. If the snapshot did
  not include your account's configuration, the page says so when the restore
  finishes: the bridge cannot sign in after it restarts until that is restored
  too. A restore history
  records every run, and if other devices reconnected with newer state after
  a restore, the divergence is flagged instead of silently merged.

Details and strategy: [Backup](cloud-backup.md).

## Bridges

Where your identity connects to other networks. Each available bridge —
Bluesky, the Fediverse (ActivityPub), [Nostr](#nostr) — gets a card: link or
unlink your account there, adjust its settings, and manage who you follow on
that network. Once linked, bridged posts and DMs flow through your normal
[Feeds](#feeds) and [Conversations](#conversations).

Why bridges exist and what each one does:
[Bluesky & Nostr from your nest](bridges-bluesky-nostr.md). Before you link one,
it is worth knowing what each network can then see — and what stays private
either way: [Who can see what](who-can-see-what.md).

## Nostr

Nostr's key-based model gets its own page: generate or import your Nostr key
(browser-extension signing works in the web app), choose what gets
cross-published (posts, replies, reactions), whether inbound Nostr posts show
in your feed, and manage your relay list and follows. Nostr DMs land in
Conversations like everything else.

## Notifications

Mentions, replies, contact requests, invitations — one list with an unread
badge and a *mark all read* button. Security notices from your own nest
land here too: a sign-in from a new location, a queued account action and
its cancellation window (cancel it under Settings → Pending actions if it was
not you), a recovery-key change, a download of your account archive or of a
mailbox export. They arrive even if your
nest has no mail address set up, so nothing security-relevant depends on
having email configured.

**Opening one takes you to what it is about.** A notification that someone
liked your post opens that post; a knock takes you to the message request on
your contacts page; a note about someone you look after — or, if someone looks
after you, that they approved a source you asked for — takes you to your
family page. A notification from Bluesky about one of your posts opens that
post on Bluesky's own website in your browser, since Fauna has no page of its
own for it. Some notifications have nowhere to go — a security notice already
says everything it has to say, and a new follower on Bluesky has no post to
show. Those rows are plain text rather than a button, so you can see at a
glance which ones lead somewhere.

**Nothing here expires on its own.** Your notifications stay until you remove
them; your nest never trims the list by age. The one exception is a contact
request you turn away: dismissing or blocking a knock removes its
notification along with it, and a request left unanswered for ninety days
goes when the request itself does. Accepting a request keeps its notification
— and the message that came with it — as part of your own record.

**A count you can see without opening Fauna.** On a phone, put Fauna's widget
on your home screen — on a Mac, on your desktop or in Notification Center — and
it shows how many unread messages are waiting: the same number your
conversations list shows, kept up to date in the background; tap it to open
Fauna. On a Mac the widget updates while Fauna is running, even with its window
closed; after you quit Fauna it keeps showing the last count until you open the
app again. When you sign out or switch accounts, the widget clears rather than
showing the previous account's count. On Linux the same count sits on Fauna's
icon in your dock or task bar as a badge — on Ubuntu's dock, KDE Plasma and
elementary out of the box — for as long as Fauna is running, which is from
sign-in onwards once it has started at login for you. A desktop whose dock
shows no badges (GNOME without a dock extension) shows none for Fauna either.
On Windows the count is the badge on Fauna's taskbar icon, which Windows keeps
showing even after you close the window; it needs the Explorer integration
that a default install includes, and an install without it shows no badge.

**Alerts on a device that isn't open.** In the browser and on Mac,
**Settings → General** (on iPhone, **Settings → Notifications**) turns on alerts
your nest can send while the app is closed — one switch, **Notify me on this
device**. Switching it on asks your device for permission once, then tells your
nest where to send them; switching it off tells your nest to stop and forget
the address. Off
stays off — the app will not quietly turn alerts back on the next time you open
it, and the switch reads off again so you can see that the change took. If
switching on does not work, the switch settles back off and a line under it
says why. Your device's own notification permission is
separate and stays as you left it; to take that back, use your operating
system's notification settings, and Fauna links you straight there if you have
refused it. What the alerts *say* is encrypted on the way — Apple, Google and
Mozilla only ever relay it, and only your device can read it. On a device
holding several identities, alerts follow the one you're using — the identity
you switch away from stops alerting here until you return.

**On a computer, Fauna's background helper shows the alert.** In the terminal
app, **Settings → Account → Push Notifications** has one switch, *Notify me on
this device*. Turn it on and your nest sends alerts for new messages, contact
requests and invitations to this computer even after you quit Fauna; the small
Fauna helper that keeps your files in sync while the app is closed shows them
as ordinary desktop notifications. While Fauna itself is open it shows its own
alerts instead, so you never get the same one twice. Nothing leaves your nest's
own connection to this computer, and the alert says only what kind of thing
arrived ("New message"), not who sent it or what it says. The switch stays as
you set it when you switch between identities or sign out and back in. If the
helper isn't running, or the computer has no desktop to show notifications on —
say you reached it over SSH — the page tells you so under the switch.

## Search

Search your public posts and profile info, with a type filter to narrow
results. Every nest answers the same way — your content stays sealed at rest
by default, and search only ever reaches the public part of it, so there is
no per-nest setting that changes what search can see.

Public posts that reach you over a [bridge](bridges-bluesky-nostr.md) are
searchable too, so something you saw from someone you follow on Bluesky or
Nostr turns up alongside your own posts. Only the public side is ever
included — direct messages that arrive over a bridge are never searchable
this way. Your nest keeps a recent window of bridged posts rather than all of
them, so very old ones eventually drop out of search while staying untouched
on the network they came from.

Much of what's yours is searched **privately, on your device** — and that half
works differently, because that content is sealed and your nest cannot read
it. Your device builds a private search index as content arrives — and as
you send it, so a message you just wrote is findable straight away — seals it
with a key only you hold, and stores it alongside the rest of your content.
Searching runs that index on your device, so the results appear mixed in with
everything else while your nest never learns what you searched for or what
matched. A device that hasn't caught up yet simply shows fewer results rather
than an error, and it catches up on its own. Today this on-device search
covers your mail, your conversations, your unsent drafts, your contacts, your
own posts, and your synced or backed-up files.

Every result says what kind of thing it is — a post, a profile, an email, a
message, a draft, a contact, a file — and shows the words around what matched,
so you can tell results apart before opening any of them. If part of a search
fails, the page says so and still shows everything the rest of it found,
rather than going blank.

Selecting a result opens it where it lives: a post opens in its full view, a
message or draft opens its conversation, a contact opens that person's card in
your address book, and a file opens its details on the Media page. When the
result is a message, the conversation doesn't just open — it opens showing the
message that matched, marked so you can tell it from the ones around it. In a
long back-and-forth that is the difference between finding what you searched
for and having to look for it a second time. (Marking the matched message is
new and is rolling out — live on the terminal app, Linux, and Android; the
other apps open the right conversation today and will mark the message as
they catch up.) This works
even when the result is something you haven't scrolled to in a while — an old
post that is nowhere on your current feed is fetched when you open it, a contact
is found whichever address book holds them, even one you have never opened, and
a file opens even if you were browsing a different folder at the time — so you
never land on an empty page. Anything that has gone since you last searched says
so rather than opening blank: a post removed under a legal obligation, a deleted
contact, a file you have since deleted or renamed.

One honest caveat: search doesn't yet reach everything — events use the same
on-device mechanism and are still being added to it.

## Family

Appears only when a family relationship involves you (set up when a guardian
[invites a supervised member](nest-for-friends-and-family.md)):

- **Guardians** see each supervised account, its *reach policy* — whether new
  contacts need approval, what happens to mail from strangers (allow, hold
  for review, or reject), whether people on other servers can initiate
  contact, whether new feed sources are allowed — plus a queue of pending
  approvals, a way to pre-approve contacts, and a **graduate** button that
  converts the account to a normal one when the time comes. The same panel
  sets **content filters** — a per-category floor (adult content, spam,
  phishing, ads) that either collapses a flagged post or message behind a
  "show anyway" tap or hides it entirely behind a "hidden by your family
  policy" notice, enforced on the member's own device after decryption.
  Switching on **notifications** shows, for each supervised member, how many
  posts or messages the policy filtered today in each category — a running
  count only, never the content itself. The same panel
  lists that account's **devices** with a **Guardian device** switch on each:
  switch it on for a device you enrolled yourself, and the supervised member
  can see it is yours but cannot remove it. Switching it back off un-enrolls.
  Devices in this list are identified by a short device code, not by name —
  the names a member gives their devices are encrypted with the member's own
  keys, so not even the server (or a guardian) can read them. **Pick by the
  code, not the name:** open the member's own device list from the device you
  enrolled and look for the row marked **This device** — that's the one,
  nothing to compare by eye (reliable today in the terminal app and on
  Linux; still landing on the rest). Don't go by a device's name — a name is
  something the member types, so two devices can share one, while the codes
  are always different from each other. You can always switch a wrong guess
  back off.
  The same panel sets **screen time**: the hours the account may be used
  ("usable from" / "usable until", typed as times like `07:00` and `21:00`,
  and allowed to run past midnight) and a **daily budget** in minutes that
  counts across all of that member's devices together. Their usage so far
  today shows beside their name. Leave a field empty for no limit.
- **Supervised members** see exactly who supervises them and what the policy
  is, including the same screen-time figure their guardian sees. Supervision is
  visible to the supervised — Fauna does not do covert monitoring. Outside the
  screen-time hours, or once the daily budget is spent, their app shows a
  full-screen notice naming what ran out and who set it — but the Family page
  stays open to them throughout, so the rules are always readable.

## Settings

The house-keeping hub. On desktops and web it has its own sidebar of
sub-pages; on phones the same entries stack the way platform settings usually
do. The sub-pages:

- **Status** — connection state, your actor ID, the nest's address, service
  and sync/encryption information at a glance.
- **Account** — change your handle, copy your actor ID, and manage
  **multiple accounts**: add another identity, switch between them, remove
  one. Each account row also has a **Require confirmation to switch** toggle:
  turn it on (say, for your admin identity) and switching to that account
  first asks you to confirm. Where your device has a screen unlock, that is the
  prompt you already know — Face ID, Touch ID, or your passcode/password;
  elsewhere Fauna asks you to confirm in the app. Cancelling simply leaves you
  on the account you were using. Admin accounts get this protection turned on
  automatically the first time the app sees they are an admin; your own
  choice always wins — turn the toggle off and it stays off. On desktop you
  don't have to switch at all: every account row, including the one you're
  currently using, has an **Open in new window** button that opens that
  account in its own window, side by side with the one you're using — each
  window stays signed in as its own account, and the confirmation protection
  above applies to these new windows too. If you start Fauna again while the
  account it would open is already running, it asks what you meant instead of
  silently doing nothing: pick another of your accounts and this new window
  opens as that one, **switch to the open window** — which brings that
  account's window forward whichever way it was opened — or **log in as a new
  user**, which normally hands you over to the window already running, since
  that's where adding an account happens, and otherwise starts you off right
  here. If the window you asked to switch to closed in the meantime, Fauna
  just carries on opening the account for you. **Export Identity** shows the
  QR a new device scans to import you —
  hidden behind a *Show QR Code* button, with a warning beside it while it is
  on screen, because anyone who scans it gets your identity
  ([Your identity, devices & recovery](identity-and-devices.md)). Signing out
  (with confirmation) clears this device — every account you had added here,
  and the local copies their conversations were kept in, not just the keys; your
  content itself stays on your nest, and signing back in re-downloads it.
  Removing a single account from the switcher does the same for just that one
  — unless another Fauna window is using that account, in which case nothing
  is removed and Fauna asks you to close that window first.
  **Export My Data** downloads what your nest holds for you as a single zip —
  your profile, contacts, messages, posts, groups, folders, feeds and devices
  as readable files. If you use your nest for mail, calendars or address books,
  the archive carries those too: your mailbox layout and which messages you had
  read, your calendars and their events, your address books and their cards,
  your addresses and aliases, the mail rules you wrote, your mail settings, and
  any mailing lists you run with their send history. For each conversation you
  are part of, the archive carries your nest's record of its messages — their
  order and when each arrived — listed under the conversation they belong to.
  You get the records of every conversation you are in and of no conversation
  you are not: being a member is what puts one in your archive. The contents
  themselves come too — the files you uploaded, the message bodies behind your
  mail, posts, calendar entries and contact cards, and the messages in your
  conversations — so the archive can be large if you keep a lot on your nest.
  There is nothing to tick: the export is always the whole thing. Content your
  nest only ever held sealed stays sealed in the zip, exactly as it rests; your
  keys are what open it, and they are on your devices, not in the archive. Every
  archive carries a manifest saying precisely what it does and does not include
  — worth a look, because the few things it leaves out are named there rather
  than hidden. Deleted messages are not carried. Nor is anything your nest is
  under a legal obligation to withhold, though the archive still shows that
  something stood where it was, so nothing disappears silently.
  Anything your nest keeps encrypted stays encrypted in the archive — it is
  copied out exactly as it rests, because your nest cannot read it either; your
  apps hold the keys that open it. Inside it, a *manifest* lists what the archive contains
  and, just as importantly, what it does not. Some things are deliberately left
  out and named there with the reason. Recovery and sign-in secrets are never
  written into a downloadable file: the zip outlives the moment you made it,
  and an export can be fetched with a weaker key than the one that unlocks your
  account — so a copy of your secrets inside it would be a second place for
  someone to steal them from. Things the archive can already rebuild from what
  it carries are left out as duplication, and so is your nest's own
  housekeeping about you, which means nothing away from that nest. Anything not
  yet reviewed for export is listed by name too, rather than quietly missing,
  so the archive never claims to be more complete than it is. **Delete account**
  lives here too. If you own a community room that has other members on your
  nest, hand the room to one of them first: until you do, deleting your account
  is refused, so the room is never left without an owner. The big asks on this page — changing your handle, deleting
  your account, deleting a backup snapshot — don't happen straight away: they
  sit for a waiting period first, so you can change your mind. The **Pending
  actions** list on this page is where you see them waiting. It is always
  there — empty most of the time — and each entry says what will happen and
  when, with a **Cancel** beside it. Cancelling takes one click and asks no
  questions: taking something back is the safe direction. Until the waiting
  period runs out, nothing has changed — a handle change you cancel never
  takes effect, and your old name keeps working exactly as before. One kind
  of entry here is not yours: if a nest admin schedules the deletion of your
  account, you are told the moment it is scheduled (a security notice in
  Notifications, and by mail where your nest serves it), the deletion shows
  in this same list with the admin's name, and the same **Cancel** is yours
  to use — the nest waits a week before an admin's deletion runs.
- **Encryption** — how many one-use starter keys are waiting on your nest for
  people who want to start an encrypted conversation with you while your
  devices are off. Your app tops the supply up each time you sign in; the page
  tells you when it is running low, and **Refresh** tops it up on the spot.
- **Quotas** — how much of your tier's inbox, storage, and device allowance
  you are using.
- **Feature limits** — a few features can be limited rather than simply on or
  off: payments, zaps, and file sharing. This section lists each one with the
  limits in force, how much you have left in each period, and — the part that
  matters most — **who set each limit**: Fauna's own built-in ceilings, your
  nest admin, your guardian, your region's rules, or a limit you set yourself.
  Nothing here is hidden from you. If a button elsewhere in the app is greyed
  out because of one of these limits, it says so on the spot rather than
  failing when you press it, and this section is where you can see the whole
  picture. Limits differ per feature, and a feature you are not limited on
  still appears — "no limit is currently binding you" is an answer worth
  seeing.

  You can also **set a limit on yourself**. Each feature's row shows "Your own
  limit" — "No limit set" until you set one — and **Set your own limit** opens
  it right there: turn the feature **Off** for yourself, or leave it **On** and
  type how many uses (or how much) you want to allow per day, week or month. An
  empty box means no limit; zero is a limit. After **Save**, the row above shows
  your limit in force straight away, marked as your own setting. Your own limit
  can only make things tighter than what already applies; if you type a looser
  number, the box tells you it has no effect and who already limits it.
  **Remove limit** takes your limit away whenever you like — there is no waiting
  period. Saving needs a connection to your nest.
- **Region** — the region your device is in, as your device itself declares
  it: your system region or locale setting (or your app store's region, for an
  app installed from a store). The app never guesses from your network, and
  there is no switch for it in the app; to change it, change your system
  setting. Where a region has an official authority that publishes rules about
  content, the app applies those rules on your device, and this section lists
  each one in force — the authority's name, the version of its rules, and when
  the app last checked for updates. Where there is no such authority — which is
  everywhere today — the section says so and nothing is filtered. A post or
  message those rules block is never simply missing: a notice takes its place
  saying it was not shown in your region, naming the authority, and giving that
  authority's own reason in its own words. Rules that only ask for something to
  be hidden put it behind a **Show** button instead. If the app has not been able
  to check for updated rules lately, a warning says so; the rules it last saw
  stay in force until newer ones arrive. If your device is set to a format of
  rules the app does not yet understand, the section says that too, and nothing
  is filtered under them.
- **Privacy** — who may reach you (*inbox mode*: open, knock-first,
  contacts-only, or closed), spam thresholds, and mail filter rules — create,
  edit, or delete a rule any time; editing changes it in place, no need to
  delete and recreate.
- **Devices** — every device signed into your identity, with online status
  and a remove button. See
  [Your identity, devices & recovery](identity-and-devices.md).
- **Sessions** — every place your account is signed in right now, this app
  first (marked *This app*), then this device's other sign-ins (*This
  device*), then the rest, newest activity first. Each shows what kind of
  sign-in it is — an app sign-in, one of your devices by name, or a device key
  that is not in your device list — and when it signed in, was last active and
  expires. You can **Revoke** any one of them except this app's own (to leave
  this app, sign out from Account), or **Sign Out Everywhere Else** with a
  second press to confirm. Revoking ends a sign-in, but it does not sign a
  device out: a device that holds your secret key or a device grant signs
  itself straight back in. To end a device for good, remove it under
  **Devices**; if someone else holds your secret key, only your recovery kit
  ends them (**Account → Recovery Kit → My Identity Was Stolen**). The last
  control, **Lock for 24 Hours**, needs you to type `LOCK` first and says
  plainly what it does before you do: every device is signed out, this one
  too; nobody — you included — can sign in for 24 hours and there is no
  unlock; it does not remove someone who holds your secret key, and they can
  lock you out the same way; your recovery kit still works while the account
  is locked, and a lock you did not set is the sign to use it. While the
  account is locked, the app opens on an **Account locked** screen instead of
  signing in: it says when the lock ends and that a lock you did not set means
  somebody else holds your secret key. If you set the lock yourself, there is
  nothing to do but wait — the app signs back in by itself once the lock ends.
  If you did not, press **My Identity Was Stolen** on that screen, paste your
  recovery kit and type `SUCCEED`: the account moves to a new key the other
  person does not have, the lock does not apply to it, and you are signed in
  straight away.
- **Folders** — the control plane for file sync; big enough to get its
  [own section below](#folders).
- **Mail, calendar & contacts** — likewise,
  [its own section below](#mail-calendar--contacts).
- **Web** — your personal website. Every user can flip on
  `https://<you>.<your-domain>/` and the page shows the live URL; its content
  comes from a folder you sync with its **website** switch on, plus posts you
  publish to the web. The order does not matter: turn the switch on for a folder
  whose files are already synced and the whole folder goes up, not just what you
  add afterwards — including a folder kept sealed behind a paid tier, whose
  back-catalogue your device re-publishes for you within a scan or two.
  You put a post on the web from the post itself: open your own post's **⋯**
  menu in the feed and tap **Publish to web**, and it gets its own page. The
  same menu then offers **Unpublish**, **Copy web link**, and — for a post
  behind a paid tier — **Copy paywall link**, so you can share a post the
  moment you publish it without coming here at all. The menu only ever shows
  these on your own posts.
  Below the switch, **Published posts** lists every post you have put on the
  web, with the address each one serves under. Each row gives you **Copy web
  link** — the public page, the link you share to sell a paid post, since
  visitors without access see the free preview — and **Unpublish**, which takes
  the page down in one tap and can be undone by publishing again. A post you
  have put behind a paid tier also offers **Copy paywall link**: that one opens
  the *full* post for anyone who has it, but only for about ten minutes, so it
  is for showing someone a preview, not for giving lasting free access — for
  that, send a claim code instead. If your website switch is off you will see
  the links greyed out with the reason: a published post with nowhere to serve
  from has no address to share yet. The same is true — switch on or off — when
  the nest itself has not been given a domain yet: the page says so instead of
  showing an address, because until an admin sets one up there is no web
  address for your site to live at.
  Very occasionally the page shows a notice that your published pages are
  temporarily unavailable. That means your nest hit a storage problem while
  rebuilding your pages and took them offline rather than risk showing
  something you had just removed. It brings them back by itself and the notice
  goes away — there is nothing you need to do, your links stay the same, and
  files you synced to your site are not affected.
- **Subscriptions** — the tiers you subscribe to, across all creators
  (the consumer side of [Profile](#profile)'s tiers). Paid for a
  subscription outside Fauna and got a claim code? Redeem it here — the
  subscription binds to your account and activates as soon as the creator's
  app syncs. Unsubscribing works the same way: your leave is recorded at
  once, needs no approval from the creator, and completes the next time
  their app syncs — the row stays listed until then.
- **Nests** — link your *other* nests, so one identity can live on several
  boxes. A linked nest also keeps a sealed copy of your account's settings,
  which only your own devices can open, so if one of your boxes is lost you
  can recover it from any nest that is still running. Each nest's row shows
  what it is trusted to read, renewable and
  revocable per item, with a history view of every change. A folder you
  have put behind a paywall shows on its row by name, and so does each
  change to that trust in the history. To trust a nest
  with something new, pick what from the list on its row — read and filter
  your mail, read your calendar, or serve one of your subscription tiers'
  paywalled posts on the web — choose how long it lasts (a few hours, or 90
  days) and confirm; the trust appears on the row immediately, ready to
  revoke whenever you change your mind. Tick **Keep this box's trust
  renewed** on a box you rely on, such as your home server: its 90-day trust
  then renews itself while you use Fauna, and new trust you give it defaults
  to 90 days. On a box you haven't ticked, new trust defaults to a few hours
  and simply runs out — trust that is about to run out, or has, says so on
  its row with a renew button. You only see
  choices that make sense right now (mail options appear once mail is
  enabled; a paywall option per tier you've created). If you've set up a
  [backup destination](#backups), your home nest's row also shows what
  it's trusted to do for your backups — seal and upload your messages, and
  write to each destination you've configured — each revocable on its own;
  revoking one freezes new backup writes without touching what's already
  stored there. Revoking doesn't erase what a nest already wrote, either:
  each backup destination keeps every version a revoked nest's writes
  superseded for a recovery window before it reclaims the space, and the
  same row lists them so you can review each one and roll back to it before
  that window closes. If you have used your recovery kit to take an account
  back, this list asks you to look at it once, for the same reason the
  [backups list](#backups) does: the trust you had given was recorded under
  the *old* identity, so anyone who held it could have added some, and your
  app can't tell those apart from your own. It restores all of it — nothing
  goes dark while you sort things out — and marks each one for you to check,
  with **Keep** beside the usual **Revoke**. Keep it if you recognise it;
  revoke it if you don't. Most rows will be yours: the mark means "not yet
  checked", never "something is wrong". Whichever nest holds a sealed copy of
  your recovery escrow is labeled "Holds your recovery escrow" right on its
  row, so you always know where that copy lives.
  A nest that's keeping a sealed backup copy of your account for you — a
  custodian — gets its own row here too, once you've set that up: what it's
  trusted to hold (never to read), and how recently it last checked in —
  current, gone stale, or not checked in yet, so a custodian that's quietly
  stopped keeping up shows up as exactly that, never as if nothing were
  wrong. Revoking a custodian stops it receiving anything new; what it
  already holds stays there, sealed, for good.
- **Muted words** — an exact, inspectable list of terms; a post or message
  that matches collapses behind a reveal control instead of confronting you,
  and in ranked feeds matching posts also sink toward the bottom. A word you
  add on another of your devices appears on this page while it is open — you
  do not have to leave the page and come back. A word you are still typing
  stays put when that happens.
- **Personalization** — [below](#personalization).
- **Task delegation** — heavy background jobs (backup uploads, content
  re-scoring, search indexing) have to run *somewhere*, and this page shows
  what is running each one. Where a choice exists, you can pin a job to a
  particular device or leave it automatic; jobs nothing is currently able to
  run simply wait. *This device* is offered per job, not per device: the same
  computer may be offerable for one job and not another, because whether it
  can do a job depends on that job. If you do not see it for a job you
  expected, that app cannot run that particular job — pinning it there would
  only make the job wait forever, so it is not offered.
  Your nest is preferred when it can do the job, because it is always on —
  message backups, for instance, are uploaded by your nest itself, so they
  keep happening while every device you own is asleep. A job only runs on
  your nest while you have granted it what that job needs: permission to read
  the content for re-scoring, or a backup destination plus permission to seal
  your backups (both on the Nests page). Withdraw either and the job simply
  waits until you restore it — re-scoring and backups only ever run on your
  nest, never on a device. Phones and tablets are never asked to run heavy
  jobs like these, even when nothing else can.
  What you pin is remembered for the account, not for the app you pinned it
  in, so every device you own shows the same assignments — a job you pin on
  your laptop shows as pinned there when you open this page on your desktop,
  and you can clear it from either. The device you pinned it to picks the job
  up, and the others let it alone; if that device is switched off, the job
  waits for it to come back rather than quietly moving somewhere else, which
  is the point of pinning. Set a job back to automatic and it goes wherever
  the usual order sends it again. A device that is already open when you
  change an assignment picks the change up on its own within a minute or so;
  you never have to restart anything.
- **Connected apps** — everything that acts for you from outside Fauna, in
  one list: websites you signed in to with Fauna, apps on your other devices,
  apps signed in with one of your app passwords, and the Nostr apps your nest
  signs for. Each row says what the app is, where it comes from, what it may
  reach in plain words, when it was last used and how long it lasts, and has
  a **Disconnect** button that asks you to confirm before it acts. Above the
  list, **Connect an app** is where you type the code an app on another device
  (a TV, a watch, a command-line tool) shows you; a valid code opens the same
  approval card a browser sign-in shows. An app on the same device as Fauna
  can skip the code: it hands you a link that opens Fauna straight onto its
  approval card, and once you approve, the app carries on by itself. The link
  works once and only for a minute or so; if it has run out, Fauna says so
  and you start again from the app. And when an app you already use asks
  for more — without a browser in between — its request waits at the top of
  the page under **Requests**. An app you have never approved never interrupts
  you with a notification: its request just waits here. If you did not expect
  it, decline it, or choose **Never show requests from this app**. An app you
  blocked that way is listed at the bottom of the page under **Blocked apps**,
  with **Allow requests again** to undo it. A mail app password's row also
  shows the exact address a mail app signs in with, and lets you copy it and
  reveal or copy the password itself.
- **Content moderation** — the training queue: correct the classifier when it
  mislabels something, and it learns. If your nest's administrator has ever
  been legally compelled to withhold something you posted, that decision shows
  here too, named as what it was — and you can **appeal** it. An appeal asks
  for a reason, and goes on your nest's permanent record for an administrator
  to review; it does not undo the decision by itself, and the entry stays in
  your queue either way, so nothing about the action is quietly erased. Flags
  your own device made carry no appeal — there is no one else's decision
  behind them; correcting them is the lever. Below the queue, **Your reports**
  lists everything you have reported: the reason, where it went, and whether
  it is still open, was acted on, or was dismissed. Withdrawing an open report
  deletes your note and any text you attached, everywhere it was sent.
- **Logs** — the app's own recent log, filterable by severity, copyable in
  one click for a bug report. (Admins get the equivalent page for the nest in
  the [admin area](admin-tour.md).)
- **General** — small platform comforts for the desktop apps. Fauna starts
  automatically when you sign in to your computer and keeps running in the
  system tray when you close its window, so folder badges and message
  notifications stay live without the app being open on screen — turn either
  behavior off here ("Start Fauna when you sign in", "Close to tray"). When
  Fauna starts at sign-in it starts quietly, with no window — open it from the
  tray or menu-bar icon, or the Dock — unless it needs you to sign in, in which
  case its window appears. On a Mac, turning Fauna off under System Settings →
  General → Login Items counts too: the switch here then shows off, and turning
  it back on takes you to that System Settings page. To
  stop Fauna entirely, use **Quit** in the tray icon's menu. File sync
  itself runs as its own small background helper, so your synced folders
  keep uploading and downloading even when Fauna is fully quit — it signs
  itself in with a limited per-device credential you can revoke any time
  from the Devices page.

### Folders

A **folder** is the unit of file storage in Fauna: a named collection that
syncs, backs up, or serves as a website — the substrate under
[Media](#media), [Backups](#backups), and [Web](#settings).

**A folder has no type.** You do not pick "a sync folder" or "a backup folder"
when you create one; you create a folder, and then say what each device does
with it and who can see it. The wizard asks for:

- **Name** — entered once, here.
- **Devices** — which of your devices keep a copy, and what each one does with
  it: whether files added *there* upload to the rest, whether changes from
  elsewhere land there, and whether someone else's delete deletes there too.
  Leaving that last one off is how you make an archive device — it keeps what
  the others removed.

That is the whole wizard. Everything else is a setting on the folder's own row
afterwards: who can see it (private, shared, or public), whether it is served as
your website, whether it is available over WebDAV, and how long the nest keeps
snapshots and file versions. (There is no scan frequency to pick: saves sync
within seconds, and the periodic catch-up behind them runs on the same schedule
on every device.)

On each set you can then edit selective-sync include/exclude paths, bind a
location on desktop machines (with on-demand placeholders where the OS
supports them), toggle **serve over WebDAV**, and resolve any sync
**conflicts** — each conflict shows its type and the file involved, with a
resolve action. Binding a location on a computer you left unchecked in the
wizard adds that computer to the folder's devices, with all three switches on;
change them on the folder's row if it should do less.

Folders are also how you **share files with people**: share a set with a
contact and they see it read-only or read-write under their own Media page;
members show as pending until they accept; removing a member re-keys the set.
The full story: [file sync](cloud-sync.md) and
[family sharing](cloud-sharing-family.md).

### Mail, calendar & contacts

Everything about your mailbox, in one family of pages (your nest's admin must
have [mail enabled](own-your-mail.md) first):

- **The main page** — enable mail for your account, see sync status, and
  manage **app credentials** for standard mail apps: create a credential
  (generated or chosen password, or a token shown once), see connection
  instructions (host, ports, username format) for IMAP/CalDAV/CardDAV
  clients, and **rotate keys** if a credential may have leaked — rotation
  walks you through it with a progress indicator, and you choose which
  credentials survive.
- **Aliases** — extra addresses that deliver to you: exact aliases, wildcard
  prefixes, and one-click **disposable addresses** with optional expiry and
  use limits. Any alias can carry its own **rate limit** — a cap on how much
  mail it accepts per hour or per day; once an address goes over, further mail
  is turned away with a "try again later" that real senders retry, so a leaked
  address can be throttled without losing it (set the cap back up, or to
  blank for unlimited, at any time). Bulk-import your existing aliases by
  pasting a list.
- **Lists** — run a newsletter or small mailing list: create a list with its
  own sending address, manage members (add, bulk-import, per-member
  unsubscribe state), with standards-compliant one-click unsubscribe handled
  for you.
- **Export** — a wizard for taking your mailbox out in standard formats
  (mbox, Maildir++, or a zip of EML files), scoped by folder and date range,
  with a pause/resume progress screen. **Landing:** the wizard is built end
  to end but starting an export doesn't complete yet — see [Moving in from
  your old provider](own-your-mail.md) for what works today.
- **Spam** — your personal spam filter: reset its learned model, opt in (or
  not) to contributing to the nest's shared baseline, and review the
  training history — every "marked as spam" event is listed and undoable.
  Opting back out, or resetting your filter, takes your training out of
  the shared baseline straight away, not the next time it is rebuilt.
  A separate switch, **Share spam reports**, is off by default too: turn it
  on and the fact that you flagged a message as spam joins an anonymized
  count your nest shares with other nests — but only once you and at least
  two other people here have flagged the identical message, and never your
  identity or the message itself. Turning it off withdraws every report you
  contributed. **What this nest publishes**, just below the switch, shows
  exactly which anonymized counts your nest shares — the same view a peer
  nest sees.

### Personalization

One home for everything that shapes *what you see*: your feeds, your muted
words, the community labelers you subscribe to, and your trained topics.

- **Feeds** and **Muted words** link to their own pages, described above.
- **Community labelers** — content classifiers published by people you can
  inspect before trusting. The catalog shows each one's publisher, its kind,
  and exactly what it gets to read — down to whether a program reads the
  pictures themselves or only what kind they are. Subscribing to one that
  reads your mail also trusts your nest's mail service to run that one
  labeler, and only it, over your mail: the trust appears on the Nests page
  beside any other, and unsubscribing withdraws it. If your nest has no mail
  service yet, or mail isn't set up for you, the labeler is listed but won't
  run until it is — the page tells you so. Subscribe and its labels become ranking
  factors you can weight in your feeds — unsubscribe any time. A labeler comes
  in three kinds: a **program** that scores content as it arrives, a **list** —
  someone's published set of posts with scores — or a **word-pattern model**,
  which matches posts nobody has seen yet. Inspecting a list shows you its name
  and every single post and score it contains; inspecting a word-pattern model
  shows you its name and every pattern it matches on, in full — so either way
  you know precisely what you're subscribing to before you do. If a
  word-pattern model was built by a newer version of the app than yours, its
  kind reads **needs a newer app**: it stays in your list and keeps whatever
  you've set, but it won't affect your feeds until you update — rather than
  quietly scoring your posts by rules this version doesn't understand.
- **Trained topics** — teach the app what a topic means to *you*. Create a
  topic (say, "Cats"), then on any post open the **⋯** menu and tap **More
  like this** or **Less like this**; each tap teaches the topic on the spot,
  and both are toggles you can un-tap. Build a feed around the topic from the
  create-feed dialog — pick your topic as a ranking factor and give it a
  weight — and the feed reorders as you teach. Rename or delete a topic here
  any time, and see how many examples each one has learned from.
- **Learn from my activity** — each trained topic also has a switch that lets
  it learn gently from how you actually read: linger on a post and the topic
  nudges toward it, scroll straight past and it nudges away. It's off until
  you turn it on, per topic, and turning it off stops the learning without
  undoing what a topic already knows. Explicit taps always count far more
  than a lingering read, and re-reading something you skipped undoes the
  skip. The **Clear activity data** button below the list erases everything
  the app has noted about your reading rhythm, from every device, in one tap.
- **Publish…** — share what a trained topic knows, so other people can subscribe
  to it. Tap it on any topic and you get a review sheet before anything leaves,
  with a **Share as** choice at the top: a **list of posts** or a **word-pattern
  model**. Either way you review everything that would go, untick anything you'd
  rather keep back, give it a public name of its own, and publish. Switching
  between the two starts the review over — they share completely different
  things — but your public name is kept.

  A **list of posts** is the simpler share: the posts your topic scored, best
  matches first, each one ticked to be included. Only the **posts** go — never
  the topic itself or anything it learned about you — and only posts your device
  has already loaded and seen can be included, so the list won't cover posts you
  never saw.

  A **word-pattern model** shares the words and phrases your topic learned to
  recognise, which is the one thing a list cannot do: it also matches posts
  nobody here has seen yet, so people who subscribe get your topic working on
  new content instead of a fixed set of old posts. It's rebuilt from scratch at
  the moment you publish, out of the public posts you marked by hand — nothing
  you merely read or watched goes into it, and the topic's own learning never
  leaves your device even in a scrubbed form. A pattern is only included if it
  appears in at least **three** of your marked public posts, so nothing that
  could quote a single post survives, and the review lists every surviving
  pattern with the number of posts it came from and whether you marked those
  posts as *more like this* or *less like this* — what you disliked is part of
  what you'd be sharing, so it's shown. If nothing clears the three-post bar the
  sheet says so and there is nothing to publish; mark a few more public posts
  and try again.

  Both kinds are published **anonymously**: nothing links them back to you or
  your account, so the public name you choose is the only thing anyone will know
  it by — pick one you'd be happy to see in the Community labelers catalog.
  Publish the same topic again later and it replaces your earlier one with an
  updated version, and you can switch a topic from a list to a word-pattern
  model (or back) the same way — anyone subscribed keeps their subscription.
- **Share anonymous engagement signals** — a single switch, off by default,
  that lets your reading help *everyone's* feeds, never just your own. Turn it
  on and your device contributes a coarse verdict — *watched to the end* or
  *skipped* — about **public** posts only, and only ever as part of an
  anonymized count: nothing about a post is shared until at least three people
  on your nest reach the same verdict about it, and your identity and your
  actual activity never leave your device. Turning it off withdraws every
  signal you contributed. Just below the switch, **What this nest publishes**
  shows exactly the anonymized counts your nest shares with the wider network —
  the same view a peer nest sees, nothing hidden — so you can check for
  yourself what leaves and what doesn't.

Trained topics — and the reading activity they may learn from — are private
by construction: the learning happens on your device, and what your nest
stores is sealed with your key. The nest never learns the topic's name, what
it means, which posts you marked, or what you lingered on — yet your topics
follow you to your other devices automatically. Only two things ever leave as
more than a sealed blob, and you choose both: the anonymized group counts you
opt into sharing, which reveal no one; and what you deliberately publish from a
topic, which carries no link back to you — either the posts you ticked, or a
word-pattern model rebuilt at that moment from the public posts you marked by
hand. The topic's own learning — the thing on your device that knows how you
read — is never what gets sent, in any case.

If you run the nest, one more entry appears — beside Settings on desktops,
inside Settings on phones. It gets [its own tour](admin-tour.md).
