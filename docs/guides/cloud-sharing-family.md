# Sharing files and running a family nest

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
exactly what is available now, what is being built, and what is planned.
>
> Part of the [Your own cloud](your-own-cloud.md) series.

---

Two related stories live here: **sharing files with another person**, and
**putting several people — typically a household — on one nest.**

## Sharing a folder with someone

Any folder you own can be shared with another Fauna user: pick the folder, press share, choose the person by their handle. What happens next depends on
who they are:

- **A confirmed contact** on your own nest: the folder simply appears in their folder list.
- **Anyone else** — including anyone on a *different* nest: they get a
  **knock**: a pending share they explicitly accept or decline. Nothing of
  yours is delivered to someone who didn't say yes, and nothing appears on
  your disk because a stranger decided to "share" it with you.

When you share, you also choose what the person can **do** with the folder:

- **Reader** (the default): they can view and download — open, read and fetch
  the files, but not change them.
- **Writer**: they can also edit — bind the folder to a location of their own and sync changes back, just like you do. Their uploads count against **your**
  storage, so you can give each writer a byte cap; leave the cap blank and
  the app reminds you that an uncapped writer can use your whole quota. If
  the folder is published — public, or sold to subscribers — the app also
  tells you that a writer can change what people outside the folder see.

You can change a member's access — or their cap — any time from the member
list, and demoting a writer back to reader takes effect immediately.
Recipients can leave a share whenever they want; a member cannot re-share your folder to third parties.

**What a demoted writer sees.** If you take someone's Writer access away while they have the folder bound to a location, their app stops syncing that location and says so on the folder — "the owner removed your permission to make changes, so this location is no longer syncing." Nothing of theirs is deleted: every file already
there stays exactly where it is, including edits they hadn't uploaded yet.
The location simply stops being kept in step with yours. If you grant Writer again,
they just re-add it and it picks up where it left off. Fauna never lets a
location go quiet without telling the person who owns the screen.

**Revocation is real, not cosmetic.** Remove someone and the folder's encryption key is rotated: everything synced from that moment on is cryptographically
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

### Links for people who don't use Fauna

A file in a folder you have made public, or in a private folder — one only you can open, and not served over WebDAV — can be **shared as a link** that anyone opens in an ordinary browser, with no account: open it on the Media page and choose **Share a link**. Every link expires — after a day, a week, a month or a year, never "forever" — and **Shared links** on the Media page lists every link you have made and lets you **Revoke** an active one, after which it stops working for everyone. A link to a private file carries the key that unlocks it, so treat it like a password: anyone holding the link can open the file, and wherever you paste it, anyone who can read that place can open it too. Files in a folder shared with other people offer no link unless the folder is public. More in the [app tour](app-tour.md#media). For everything else:

- **Sharing with a person** means naming the person (above) — auditable and
  revocable.
- **Giving a non-Fauna tool or person managed access**: serve a folder over [WebDAV](cloud-sync.md#mount-it-like-a-network-drive-webdav) with real credentials.
- **Actually-public files** — a photo page for the family reunion, your résumé: make an ordinary folder, set who can see it to **Public**, turn on **"Serve this folder as your website"** on its row, and switch on **Publish my website** in Settings → Web. That publishes it as a real public website on your domain. Public on purpose looks like publishing — and your app confirms the step, because a public folder's *file names* are public too. See [Folders you make public](who-can-see-what.md#folders-you-make-public).

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

- **Inviting** is the admin's call: mint an **invite code** and hand it over (it's typed in during their sign-up), approve their request from the admin page, or admit someone who already has a Fauna identity with **Admit someone directly**. **Who may create an account** decides whether a code works — a new nest starts at **Nobody — only I can admit people**, which refuses codes — and whether people can sign up on their own, at the free tier (**Anyone**); two more settings on the same page can refuse a sign-up too. [Letting people in](nest-for-friends-and-family.md#letting-people-in) explains each setting. Apart from **Anyone**, nobody joins your nest without you.
- **Quotas are tiers.** You define tiers (storage cap, device count, inbox size…) and assign each member one. The teenager's 4K-video habit gets a
  ceiling before it eats the disk; raising it later is one dropdown. Everyone
  sees their own usage; over-quota writes are refused cleanly (nothing is
  silently deleted).
- **Privacy holds inside the family too.** Every nest stores members' files encrypted with their own keys — the admin manages space and membership, not what is in those files. You can evict an account, but you cannot read what is in it. A member can loosen that for their own data, and you cannot do it for them: by publishing to everyone — a post, or a folder they make public — by serving a folder over WebDAV or putting one behind a paywall, by linking an account on another network, by connecting a standard mail, calendar or contacts app (the nest opens what that app reads while it is signed in), or by giving the nest permission to read some of their data — saying yes to **Trust this box?** when they join, later from their own Settings → Nests, or subscribing to a community labeler that reads their mail — revocable whenever they like, and never an admin-wide switch. Mail and messages the nest passes to and from email and other networks arrive readable and are stored encrypted as they land, and a group may choose to be a community room, which its home nest can read. And someone who can open a member's message or restricted post can attach its text to a report, which the admins can then read. The nest also sees what any server that delivers messages must: who each member is connected to — their contacts, whom they follow, the groups they're in — and when and how much they send. And when a member's app shows a preview of a link that stands on a line of its own, in a post or a message, it asks the nest to fetch that link, so the nest learns the link's address (see [Who can see what](who-can-see-what.md#link-previews-and-shared-media)).
- **Leaving the admin chair aside**, members manage themselves: their own devices, their own mail settings, their own shares. Admin powers are about membership and resources — invite, set tier, suspend, evict — plus the rules you publish for your nest and withholding a single item someone reported or the law obliges you to remove, always with a notice in its place (see [Moderation, honestly](nest-for-friends-and-family.md#moderation-honestly)); none of them opens what a member keeps encrypted. An eviction is never sudden: the member is warned and can export their data until their access ends, two weeks later; about four weeks after the warning, their account and everything in it are deleted, unless you cancel it first.

The same machinery gives you [friend-hosted backup guests](cloud-backup.md#friend-hosted-backup-you-hold-mine-ill-hold-yours):
a "member" whose tier allows storage only.

### Parental controls

The model: **supervised accounts**. A household admits a child's account with
a **guardianship link** to a parent's account — a relationship between two
accounts, deliberately *not* a power of the admin role. From the Family page in
any app, the guardian sets who can reach the child (enforced by the nest, which goes by who is writing, never by what they wrote: new contacts need guardian approval, cold email from unknown senders can be held for review or rejected, and strangers on other nests can be kept from initiating contact at all), reviews a queue of pending
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
server and the parent's own account. **Pick by the code, not the name:** open the child's device list from the enrolled device itself and look for the row marked **This device** — that's the one, nothing to compare by eye. Don't go
by a device's name — a name is something the child types, so two devices can
share one, while the codes are always different from each other.) That switch is what
makes it the parent's: the device then shows up in the child's own device list
marked "Guardian device," so the child always sees which device is the
parent's, and the child cannot remove it. To un-enroll, the parent switches
the same control back off and then removes the device normally — and
graduating the account un-enrolls it automatically.

Where this stands: the **guardian link, reach controls, and the Family page are built and live** — guardianship set at admission,
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
since the start. A held email shows up in the approval queue as who sent it and
when it arrived — never its subject or its text; approving moves it to the
child's inbox and lets that sender's later mail straight through, and denying
discards it.)

**Age bands.** When an admin admits a supervised account, they set the child's
age band beside the guardian — under 13, 13–15, 16–17 or 18+ — and it picks the
starting settings the guardian then adjusts. It never changes on its own:
nothing graduates an account when a birthday passes. The child sees their own band on the Family page, with how it was set — "set by guardian"; a band confirmed by the phone's app store is not available yet. The guardian sees each child's band on the child's row. An account
admitted before bands existed simply shows none.

**Denying someone is not permanent.** The people you have denied for a child are
listed under "Denied message senders" on that child's section of the Family
page, and each one has an *Allow again* button. Changing your mind takes one
click, and it works however long ago you decided — you do not need the original
message or notification any more. Denying only stops *new* messages: anything
that already reached the child stays theirs to read, because refusing an arrival
is not the same as destroying something they were already given. 

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
reason to punish a third party; the child is free to ask again. 

**The same ask now covers outside sources.** If you have limited which feeds and
accounts your child can add through a connected outside account, then connecting
one, or following someone through it, stops with the same kind of dead end — and
the same "Ask your guardian" appears beside it. The request lands in the same
approval queue, and it shows exactly what approving would allow: the service,
whether it is a connection, a follow or a feed, and the account or feed address
itself. Any name your child gave it follows in quotes, as their own description,
so check the address rather than the name. While you are deciding, the child's screen
reads "asked — waiting for your guardian"; once you approve, it changes to
"approved — try again", and the child adds the source themselves. That last step
is deliberate: approving grants the one addition, and the app never quietly
performs it in the background, so nothing is added to your child's feed at a
moment neither of you was looking. An approval that is never used simply lapses
after a week. 

**Content rules are available:** from the same Family page, a guardian sets rules for the posts and messages the child sees — by category (NSFW, spam, phishing or commercial), by keywords, or by a labeler — each of which either hides a matching post or message behind a warning, with a **Show anyway** button, or blocks it, leaving a "Hidden by your family policy" notice in its place, enforced on the child's own device after decryption (the filtering needs nothing from the nest), in both the feed and their conversations. The apps themselves detect spam and phishing; a rule on NSFW or commercial content is saved and applies as soon as a post or message is labelled that way. The child's read-only policy view shows, for each category a rule covers, whether it blocks or collapses, and lists each keyword or labeler rule the same way, each with the guardian's reason where the rule carries one. A guardian can also switch on **Notify me about flagged content**: their Family page then shows, for each child, how many posts or messages the guardian's category rules filtered today in each category — a running count only, never the content itself and never a way to read what was hidden.

**Screen-time rules.** On the same Family page, a
guardian can set the hours a child may use their account — "usable from" and
"usable until", typed as plain times like `07:00` and `21:00`. The window may
run past midnight, so a late-shift household can say `10:00` to `01:00` just as
easily. Outside those hours the child's app shows a full-screen notice saying
when their screen time starts again and who set it, instead of the app. Leaving
a field empty removes that limit.

Two things are deliberate here. The child can **always still open the Family
page**, even while locked — so they can read who supervises them and exactly
what the rules are, rather than facing a wall with no explanation. And the limit is kept by the apps on the child's own devices, not by the nest. When you set a daily time budget, the devices tell the nest roughly how many minutes they have been in use, so one allowance covers them all; those reports carry the minutes and the device's time-zone offset, never what the device was used for. But the nest never decides whether a device may be used: the apps on the child's devices do. That is the honest bound, and it is
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
uses. 

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

A separate app for children under 13, limited to contacts their guardian approves, is planned. What a family nest gives every member, at any age, is an ad-free, tracker-free, algorithm-free home for their data.

## Where this stands today

| Feature | Status |
|---|---|
| Share a folder by handle; auto-appear for contacts; knock for others; cross-nest knock | **Available** |
| Read-only recipients; voluntary leave | **Available** |
| Revocation with real key rotation | **Available** |
| Read-write shared folders (writer members with per-member caps) | **Available** — grant Writer at share time or per member row; a writer's folder syncs both ways (the web app keeps no local folder to sync) |
| WebDAV credentialed access for non-Fauna tools | **Available** (server + the per-folder toggle on every app) — see [sync guide](cloud-sync.md#where-this-stands-today) |
| Public website hosting of a folder | **Available** — set a folder you own to **Public**, turn on **"Serve this folder as your website"** on its row, and switch on **Publish my website** in Settings → Web |
| Share links for people without Fauna (expiring, revocable; private files carry their key in the link) | **Available** — **Share a link** on a file in Media |
| Share a folder with someone sitting next to you, with no nest involved | **Being built** — the whole exchange works on your local network, and the folder is set up on both sides; **the files themselves do not copy across yet** |
| Collaborative documents (Spaces, viewer/editor) | **Planned** — designed, unbuilt |
| Multi-user nest: invite codes/requests, tiers-as-quotas (enforced), eviction | **Available** on all seven apps |
| Per-member privacy from the admin (files stored encrypted with each member's own keys, apart from what a member makes public) | **Available** |
| Parental controls (supervised accounts: guardian link, reach policy, guardian device, graduation) | **Available** — guardian link at admission, who-can-reach-the-child enforcement, guardianship transfer, graduation |
| Parental controls: content rules (by category, keywords or labeler; block flagged posts and messages or hide them behind a warning) | **Available** — set from the Family page and enforced on the child's own feed and conversations |
| Parental controls: direct messages from unknown senders on connected outside accounts | **Available** — the "Unknown message senders" control on the Family page holds a stranger's first message for guardian review, on every app |
| Parental controls: undoing a denied message sender | **Available** — "Denied message senders" lists everyone you denied for a child, each with a one-click **Allow again** |
| Parental controls: the child asking in-app for a new contact | **Available** — "Ask your guardian" appears where the refusal happens and lands in the parent's approval queue |
| Parental controls: the child asking in-app for an outside feed or account | **Available** — "Ask your guardian" appears beside a blocked connect or follow; approving grants that one addition, which the child then makes themselves |
| Parental controls: guardian notifications (daily per-category counts of what the category rules filtered, no content shown) | **Available** — the guardian's Family page shows per-child daily counts, on every app |
| Parental controls: screen-time rules | **Available** — the guardian sets usable hours and a daily time budget from the Family page, enforced on the child's own device on every app |
