# Sharing files and running a family nest

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
> exactly what is available now, what is landing, and what is planned.
>
> Part of the [Your own cloud](your-own-cloud.md) series.

---

Two related stories live here: **sharing files with another person**, and
**putting several people — typically a household — on one nest.**

## Sharing a folder with someone

Any folder you own can be shared with another Fauna user: pick the set,
press share, choose the person by their handle. What happens next depends on
who they are:

- **A confirmed contact**: the set simply appears in their folder list.
- **Anyone else** — including anyone on a *different* nest: they get a
  **knock**: a pending share they explicitly accept or decline. Nothing of
  yours is delivered to someone who didn't say yes, and nothing appears on
  your disk because a stranger decided to "share" it with you.

When you share, you also choose what the person can **do** with the set:

- **Reader** (the default): they can view and download — open, read and fetch
  the files, but not change them.
- **Writer**: they can also edit — bind the set to a location of their own and
  sync changes back, just like you do. Their uploads count against **your**
  storage, so you can give each writer a byte cap; leave the cap blank and
  the app reminds you that an uncapped writer can use your whole quota. If
  the folder is published — public, or sold to subscribers — the app also
  tells you that a writer can change what people outside the folder see.

You can change a member's access — or their cap — any time from the member
list, and demoting a writer back to reader takes effect immediately.
Recipients can leave a share whenever they want; a member cannot re-share
your set to third parties.

**What a demoted writer sees.** If you take someone's Writer access away while
they have the set bound to a location, their app stops syncing that location and
says so on the set — "the owner removed your permission to make changes, so this
location is no longer syncing." Nothing of theirs is deleted: every file already
there stays exactly where it is, including edits they hadn't uploaded yet.
The location simply stops being kept in step with yours. If you grant Writer again,
they just re-add it and it picks up where it left off. Fauna never lets a
location go quiet without telling the person who owns the screen.

**Revocation is real, not cosmetic.** Remove someone and the set's encryption
key is rotated: everything synced from that moment on is cryptographically
beyond them. This is a meaningfully stronger promise than a commercial cloud
"removing their access", which is a database row a bug or insider can bypass.
Your nest stops serving them the moment you remove them; the new key still has
to reach each of your devices that syncs the folder, and the app keeps trying
until it does. If a device can't fetch it for a minute or more, the sync-agent
line at the top of that device's sidebar reads **Keys pending** instead of
**Running**. Until it clears, files you add on that device wait there instead
of uploading, and they go up by themselves once the new key arrives. It goes
back to **Running** by itself as soon as the new key lands. One case it cannot
cover: a device that has had no contact with your nest since before the removal
does not know about it yet, so a file you add there can still be opened by the
person you removed if their device can reach yours directly, until yours is
back in contact with your nest.

### Sharing with someone sitting next to you

Everything above goes through your nest. When the two of you are in the same
room, there is a second way that needs no nest at all — useful on a plane, at a
cabin, on a conference network that blocks everything, or simply when the
internet is down.

Both of you need to be on the **same local network** — the same wifi, the same
hotspot, the same cable. There is no internet involved, but the two devices do
have to be able to see each other.

Both of you open **Folders** and pick "Share with someone next to you". Each
app shows **your code** — a long string that is your device's identity plus
where it can be reached on that local network. You copy yours across to their
**their code** box, and they copy theirs across to yours. That in-person
exchange *is* the security: nobody in the middle can substitute their own code,
because you are taking it from the person in front of you rather than from
anything the network said.

**Hand the code over in person — never send it.** Read it off their screen, or
read yours out to them; pasting it into a chat gives away the one thing the
exchange is protecting. If you are not close enough to do that, use the
ordinary sharing above instead.

The code is long, and deliberately so — it ends in a few short groups after a
dash, which are the addresses your device can be reached at. Those are what let
the other app find you with no nest in the middle, so a code with the groups
trimmed off will not connect.

Once the codes match, the person receiving presses **Ready to receive** first,
and then the person sharing presses **Begin sharing**. That order matters: the
receive step is what tells your device to expect this one exchange, from this
one person. Anyone else who tries to reach your device meanwhile is turned away
without getting a word in — and so is the same person, if you never pressed it.

The share then progresses on screen — invitation sent, waiting for them to
accept — and either side can cancel at any point.

On the receiving side the invitation arrives in **Shared with you**, the same
place a share sent through your nest would knock, with **Accept** and
**Decline** beside it. Accepting finishes the exchange, and the folder appears
in your folder list marked with who shared it. Declining is final: the
invitation stops asking on every one of your devices, and nothing is added.

Two things are worth knowing about a folder shared this way:

- **It has no name yet.** It is listed by a short code you can both see on your
  own screens — enough to tell two of them apart. Naming is being built.
- **Leaving one is not available yet.** The person who started the share is the
  one who can remove someone from it.

⏳ *This first version sets the folder up on both sides without copying its
files across — the two of you end up sharing the same folder, and it is listed
and readable in principle, but the contents do not transfer yet. That piece is
still being built. Sharing through your nest is the way to move files today.*

### When your devices fetch from each other

For a folder shared the ordinary way — through your nest — your app can also
fetch changes **directly from the other member's device** when both apps are
open. Your devices quietly learn each other's addresses while they sync
through the nest, and from then on they can hand files straight across —
useful when you are both on the same home network and the big file is on the
laptop next to you, or when the internet is out and the newest version of the
document exists only on your co-author's machine in the same room.

There is nothing to set up, and one thing to turn off if you want: each device
has a **Peer transfers** switch on the **Devices** page. Off means that device
opens no connection for other devices to reach — it neither hands files out nor
fetches them directly, and sharing simply goes through your nest. You can turn
any of your devices off from any other; only the device itself turns back on.
The **Folders** page shows a **Peer transfers**
section: one line saying whether this device is currently serving your shared
folders to other members, and below it a row for each folder and person the
app fetched from lately — what moved, and whether it is receiving, up to
date, still waiting to be admitted, or limited by a usage rule. Your nest
stays the always-on copy: device-to-device fetching is a shortcut in front of
it, never a replacement, and everything arrives with the same encryption and
the same membership checks as it would through the nest.

Serving happens only while your app is open, only for folders you have
actually shared, and only to people who are members of that folder — a
device that was removed from the folder is turned away at the door.

### Why there are no "anyone with the link" links

Fauna deliberately does not do public share links. A bearer link is a
password-in-a-URL: it gets forwarded, pasted into chats that get indexed,
scraped from logs — link leakage is one of the most common ways private files
go public. Instead:

- **Sharing with a person** means naming the person (above) — auditable and
  revocable.
- **Giving a non-Fauna tool or person managed access**: serve a set over
  [WebDAV](cloud-sync.md#mount-it-like-a-network-drive-webdav) with real
  credentials.
- **Actually-public files** — a photo page for the family reunion, your
  résumé: make an ordinary folder, set who can see it to **public**, and turn on
  **"Serve this folder as your website"** on its row. That publishes it as a real
  public website on your domain. Public on purpose looks like publishing, not
  like a "private" link you hope stays quiet — and your app confirms the step,
  because a public folder's *file names* are public too. See
  [Folders you make public](who-can-see-what.md#folders-you-make-public).

### Collaborative documents (coming)

Live co-edited documents — Fauna's answer to shared notes/Google-Docs-style
collaboration, with viewer/editor roles and end-to-end encryption — are a
designed, approved feature ("Spaces") that is **not built yet** and gated on
its cryptographic foundations maturing. Until it ships: share a folder for
files; use messaging for the discussion.

## A family on one nest

One nest happily serves a household: each person gets their **own full
account** — their own key, mail address on your domain, calendar, files,
photos, feeds. Not a "family member" sub-account: the real thing.

- **Inviting** is the admin's call: mint an **invite code** and hand it over
  (it's typed in during their sign-up), or approve their request from the
  admin page. Nobody joins your box without you.
- **Quotas are tiers.** You define tiers (storage cap, device count, mailbox
  size…) and assign each member one. The teenager's 4K-video habit gets a
  ceiling before it eats the disk; raising it later is one dropdown. Everyone
  sees their own usage; over-quota writes are refused cleanly (nothing is
  silently deleted).
- **Privacy holds inside the family too.** Every nest seals members' files
  with *their* keys — the admin administers capacity, not content. You can
  evict an account; you cannot read it. What a member's own box gets to
  actually process is up to that member: they mint the specific, revocable
  grants for it themselves, from their own Settings → Nests — never an
  admin-wide switch.
- **Leaving the admin chair aside**, members manage themselves: their own
  devices, their own mail settings, their own shares. Admin powers are about
  membership and resources — invite, set tier, evict (non-destructive) — not
  about reaching into accounts.

The same machinery gives you [friend-hosted backup guests](cloud-backup.md#friend-hosted-backup-you-hold-mine-ill-hold-yours):
a "member" whose tier allows storage only.

### Parental controls

The model: **supervised accounts**. A household admits a child's account with
a **guardianship link** to a parent's account — a relationship between two
accounts, deliberately *not* a power of the admin role. From the Family page in
any app, the guardian sets who can reach the child (enforced by the nest,
without reading anything: new contacts need guardian approval, cold email from
unknown senders can be held for review or rejected, and strangers on other
nests can be kept from initiating contact at all), reviews a queue of pending
approvals, and can pre-approve a contact directly. The child always sees a
permanent "this account is supervised" notice — supervision is never silent —
and "graduating" the account to a full private one keeps everything, same
identity, same data. Guardianship itself can be handed to another adult (a
consent handshake: the proposed guardian must accept, not just be assigned).
For young children, a parent can simply enroll one of their own devices into
the child's account — full visibility with no special back door. After
enrolling, the parent opens the child on their Family page and switches on
"Guardian device" beside that device in the list there. (The list shows each
device as a short device code rather than its name — the names a child gives
their devices are encrypted with the child's own keys, private even from the
server and the parent's own account. **Pick by the code, not the name:** open
the child's device list from the enrolled device itself and look for the row
marked **This device** — that's the one, nothing to compare by eye (reliable
today in the terminal app and on Linux; still landing on the rest). Don't go
by a device's name — a name is something the child types, so two devices can
share one, while the codes are always different from each other.) That switch is what
makes it the parent's: the device then shows up in the child's own device list
marked "Guardian device," so the child always sees which device is the
parent's, and the child cannot remove it. To un-enroll, the parent switches
the same control back off and then removes the device normally — and
graduating the account un-enrolls it automatically.

Where this stands (2026-07-15): the **guardian link, reach controls, and the
Family page are built and live** — guardianship set at admission,
nest-enforced reach controls (who can contact the child, whether strangers on
other nests can reach them, whether they can add new outside feeds), guardian
approval of the child's new contacts, guardianship transfer, and in-place
graduation. **Direct messages arriving over an outside account the child has
already connected** — a Nostr, Bluesky or Fediverse account, say — now have
their own control, "Unknown message senders", beside the feed-sources one. Set
it to *Hold for review* and a message from someone the child has never written
to is marked as held and listed in the guardian's approval queue; approving lets
that person's messages through from then on, and denying stops new ones
arriving. Nothing is deleted or hidden from the child either way: a held
conversation is still fully readable by them, marked so they can see what is
happening. Leave it at *Allow* and those messages arrive as before. This control
is available on every app. (Ordinary *mail*
from unknown senders is a separate control on the same page, and has been there
since the start.)

**Age bands.** When an admin admits a supervised account, they set the child's
age band beside the guardian — under 13, 13–15, 16–17 or 18+ — and it picks the
starting settings the guardian then adjusts. It never changes on its own:
nothing graduates an account when a birthday passes. The child sees their own
band on the Family page, with how it was set ("set by guardian", or "verified on
Android" / "verified on iOS" when the phone app confirmed the age range with its
app store); the guardian sees each child's band on the child's row. An account
admitted before bands existed simply shows none.

**Denying someone is not permanent.** The people you have denied for a child are
listed under "Denied message senders" on that child's section of the Family
page, and each one has an *Allow again* button. Changing your mind takes one
click, and it works however long ago you decided — you do not need the original
message or notification any more. Denying only stops *new* messages: anything
that already reached the child stays theirs to read, because refusing an arrival
is not the same as destroying something they were already given. This list is on
the terminal app first, and is rolling out to the rest.

**The child can ask, in the app.** When "require my approval for new contacts"
is on and the child tries to reach someone new — from the Contacts page or from
that person's profile — the app now offers "Ask your guardian" right where the
refusal happens, instead of stopping at a dead end.
Tapping it puts the request in the parent's approval queue — showing *who* the
child asked about, and nothing else: there is deliberately no message box, so a
request can never carry text the child would rather not have forwarded. The
child's screen then reads "asked — waiting for your guardian", and keeps saying
so until the parent answers, even after closing and reopening the app. Approving
is the same as adding the contact yourself. Declining simply drops the request —
it does *not* block the person, because the child asking about someone is not a
reason to punish a third party; the child is free to ask again. This is on the
terminal app first, and is rolling out to the rest.

**The same ask now covers outside sources.** If you have limited which feeds and
accounts your child can add through a connected outside account, then connecting
one, or following someone through it, stops with the same kind of dead end — and
the same "Ask your guardian" appears beside it. The request names the source, and
lands in the same approval queue. While you are deciding, the child's screen
reads "asked — waiting for your guardian"; once you approve, it changes to
"approved — try again", and the child adds the source themselves. That last step
is deliberate: approving grants the one addition, and the app never quietly
performs it in the background, so nothing is added to your child's feed at a
moment neither of you was looking. An approval that is never used simply lapses
after a week. This is on the terminal app first, and is rolling out to the rest.

**Content filtering is available:** from the same Family
page, a guardian can set a per-category floor — adult content, spam, phishing,
or ads — that either collapses a flagged post or message behind a "show anyway"
tap or hides it entirely behind a "hidden by your family policy" notice,
enforced on the child's own device after decryption (the nest still reads
nothing), in both the feed and their conversations. The
child's read-only policy view shows exactly which floors are set. A guardian can
also switch on **notifications**: their Family page then shows, for each child,
how many posts or messages the family policy filtered today in each category — a
running count only, never the content itself and never a way to read what was
hidden.

**Screen-time rules.** On the same Family page, a
guardian can set the hours a child may use their account — "usable from" and
"usable until", typed as plain times like `07:00` and `21:00`. The window may
run past midnight, so a late-shift household can say `10:00` to `01:00` just as
easily. Outside those hours the child's app shows a full-screen notice saying
when their screen time starts again and who set it, instead of the app. Leaving
a field empty removes that limit.

Two things are deliberate here. The child can **always still open the Family
page**, even while locked — so they can read who supervises them and exactly
what the rules are, rather than facing a wall with no explanation. And the
limit is kept by the apps on the child's own devices, not by the nest: the nest
never learns when a device is in use. That is the honest bound, and it is
stated on the guardian's own screen — a determined teenager with a modified app
is out of scope, the same as every other content-side rule here.

**Feature limits for one child.** A few features can be limited rather than
simply on or off — payments, zaps and file sharing. On the child's policy
screen, just below the Save button, a guardian sees one row for each of them
with what they have set, in words ("No limit set" until they set one). **Edit**
opens the limit for that child only: turn the feature off for them, or leave it
on and type how many uses, how many different people, or how much a day, a week
or a month — an empty box means no limit, and zero really means zero. The limit
has its own Save, separate from the rest of the policy, and **Remove limit**
takes it away again.

A guardian's limit can only tighten what already applies to the child — Fauna's
built-in limits, the region's rules and anything the nest admin set come first.
Nothing stops you typing a looser number, but the box then tells you it has no
effect and who already limits it. And the child is never left guessing: their
own Feature limits section shows the limit, how much they have left, and that
their guardian set it. Unlike the content and screen-time rules, these limits
are kept by the nest itself, so they hold on every app and device the child
uses. This is on the terminal app first, and is rolling out to the rest.

**Going offline does not lift the rules.** Each of the child's devices
remembers the rules it last saw, so a flight, a dead router, or a switched-off
connection leaves the hours, the daily budget and the content settings exactly
as they were — turning the network off is not a way around bedtime. The one
gap, and it is worth knowing when you set a device up: a device that has never
once been connected while the child is supervised has nothing to remember yet.
Finish the setup on a working connection and that first check takes care of it.

**A daily time budget works alongside the hours.** Set "minutes per day" and
the child gets that much use across *all* of their devices, not that much on
each — a phone and a laptop draw on the same allowance. Both the guardian's
ward list and the child's own summary show the same running figure ("45 of 120
minutes"), because a limit a child cannot see coming is a limit they can only
discover by hitting it. When the allowance runs out, the same full-screen
notice appears, naming the budget rather than a time of day, and the Family
page stays open as always. The count resets at midnight where the child is,
not on some server's clock. Time spent looking at the lock notice does not
count against the next day, and leaving a device switched on with the app open
does not quietly burn the allowance — only real use does. Leave the field empty
for no daily limit.

If your youngest kids need a fully supervised environment today, Fauna isn't
that yet; what a family nest *does* give every member, at any age, is an
ad-free, tracker-free, algorithm-free home for their data.

## Where this stands today

| Feature | Status |
|---|---|
| Share a set by handle; auto-appear for contacts; knock for others; cross-nest knock | **Available** |
| Read-only recipients; voluntary leave | **Available** |
| Revocation with real key rotation | **Available** |
| Read-write shared sets (writer members with per-member caps) | **Rolling out** — grant Writer at share time or per member row; a writer's read-write folder sync is **live on Linux, macOS, and the terminal app** and rolling out to the remaining apps (the web app has no local folder to sync, by design) |
| WebDAV credentialed access for non-Fauna tools | **Available** (server + the per-set toggle on every app) — see [sync guide](cloud-sync.md#where-this-stands-today) |
| Public website hosting of a folder | **Available** — turn on **"Serve this folder as your website"** on any folder you own |
| Share a folder with someone sitting next to you, with no nest involved | **Rolling out** — the whole exchange works on the terminal app (2026-08-19), on your local network, and the folder is set up on both sides; **the files themselves do not copy across yet**, and the other apps get the screens in turn |
| Collaborative documents (Spaces, viewer/editor) | **Planned** — designed, unbuilt |
| Multi-user nest: invite codes/requests, tiers-as-quotas (enforced), eviction | **Available** on all seven apps |
| Per-member privacy from the admin (content sealed at rest, no admin read access) | **Available** |
| Parental controls (supervised accounts: guardian link, reach policy, guardian device, graduation) | **Available** (2026-07-15) — guardian link at admission, who-can-reach-the-child enforcement, guardianship transfer, graduation |
| Parental controls: content filters (per-category collapse/hide of flagged posts and messages) | **Available** — guardian floors set from the Family page on every app, and enforced on the child's own feed and conversations on every app |
| Parental controls: direct messages from unknown senders on connected outside accounts | **Available** — the "Unknown message senders" control on the Family page holds a stranger's first message for guardian review, on every app |
| Parental controls: undoing a denied message sender | **Rolling out** — "Denied message senders" lists everyone you denied for a child, each with a one-click *Allow again*; on the terminal app, expanding to the rest |
| Parental controls: the child asking in-app for a new contact | **Rolling out** — "Ask your guardian" appears where the refusal happens and lands in the parent's approval queue; on the terminal app, expanding to the rest |
| Parental controls: the child asking in-app for an outside feed or account | **Rolling out** — "Ask your guardian" appears beside a blocked connect or follow; approving grants that one addition, which the child then makes themselves; on the terminal app, expanding to the rest |
| Parental controls: guardian notifications (daily per-category counts of filtered content, no content shown) | **Available** — the guardian's Family page shows per-child daily counts, on every app |
| Parental controls: screen-time rules | **Available** — the guardian sets usable hours and a daily time budget from the Family page, enforced on the child's own device on every app |
