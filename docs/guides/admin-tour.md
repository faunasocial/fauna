# A tour of the admin area

> **What this covers:** every screen in the Admin area — the pages that appear
> in the app when your account is the nest's admin. Like the rest of Fauna, it
> is the same on every app: web, Linux, macOS, iOS, Android, Windows, and the
> terminal app.
>
> **Who it's for.** Nest admins. This is the map of the controls; the
> *judgment* — admission policy, moderation, capacity, kids — is
> [Running a nest for friends & family](nest-for-friends-and-family.md).
>
> **The companion rule worth restating:** in Fauna, *the app is the only
> configuration surface*. There are no server config files to edit and no
> shell commands to run — if an admin can decide it, it is a control on one of
> these pages, and everything else configures itself. (This tour, like the
> [app tour](app-tour.md), describes the admin area at its fullest; a screen
> missing on your platform is parity work in flight.)

## Getting in — and back out

Admin is not a separate account or a separate app: an admin is a normal user
with one extra role, and the Admin area is one extra entry in the navigation —
beside Settings on desktop and web, inside Settings on phones. It only renders
for admins; other members never see it. Every admin page has a back control
that returns you to the regular app.

## Dashboard

The at-a-glance page: stat cards for the nest's vitals — members, storage,
activity, plus a **Mail** card carrying the one-line mail verdict from the
[Mail](#mail) page. Start here when you just want to know "is everything fine?"

## Users

The people page — who is on, who wants on, how to let someone on, and what is
about to change. Six sections:

- **Pending requests** — people who asked to join from the app's signup flow.
  Each row is approved (choosing a tier as part of the approval) or denied.
- **Registration** — whether people can sign themselves up at all. Choose
  **Closed** (nobody signs up on their own — you let people in yourself; this is
  how a new nest starts), **Invite required** (they can sign up, but only with a
  code you minted), or **Open** (anyone can sign up). You can also cap how many
  free-tier accounts exist, whichever setting you pick — leave it blank for no
  limit. The same section has a switch to **accept only sign-ups whose app
  verified the person's age range with its app store** — off unless you turn it
  on, and saved with the rest of the section. Only the phone apps can carry such
  a verification (web and desktop sign-ups never do), and only from a store
  your nest can check — for now an Android sign-up arrives as declared, the
  same as one with no store check at all — so leave it off unless
  yours is a strict family nest. Save applies immediately; nobody has to
  restart anything.

  Closing registration never affects the people already on your nest, and it
  never blocks *you* — approving a request or minting a code works in every
  setting. And "Open" only means people may *sign up*; it never lets someone use
  your nest without an account.
- **Admit someone directly** — for when you already have the person's account
  key (the 64-character code their app shows them): paste it, type the handle
  they'll go by, pick their tier, and they're in — no code to hand out, no
  request to wait for. Name the handle carefully: you can clear a member's
  handle later, but not change it for them, and without a handle they can't
  send email from your nest's domains. Leaving the handle blank admits them
  without one, for exactly that restricted state.
- **Invite** — mint invite codes to hand out through any channel you like.
  A code carries the tier the invitee will get, an optional use limit, and —
  for a child or supervised member — the guardian the new account will be
  attached to, plus the child's **age band** (under 13, 13–15, 16–17 or 18+).
  The band becomes pickable once a guardian is chosen; it sets the starting
  settings the guardian then adjusts on the Family page, and it changes
  nothing on its own. The same two pickers sit on each pending request, where
  the band starts at whatever age range the person's app reported — and the
  row says whether that range was verified by the app store, merely declared,
  or not reported at all, so you decide with that in view. Copy the code with
  one tap; delete codes that should stop working.
- **Users** — the member list. Per member you can change their tier, see
  whether their mailbox is being served, and take the removal actions:
  **suspend** (immediate, reversible), **evict** (a timed warn → suspend →
  delete pipeline), and **cancel** an eviction in progress. You can cancel
  until the deletion starts. After that the account is being removed and
  can no longer be restored. Each row also
  offers **Make admin** or **Remove admin**, the way you give a trusted
  member the keys to the nest alongside you, or take them back.
- **Pending admin actions** — the admin-side actions that don't happen
  straight away: making or removing an admin, deleting a member's account.
  Each waits a day or more first, and this list is where every admin sees
  what is waiting, who scheduled it, when it runs, and — for the actions
  that need another admin's say-so — how many approvals it still has to
  collect. **Approve** adds yours (you can't approve your own); **Cancel**
  takes one click and calls it off, whoever scheduled it. When another
  admin schedules one of these, you get a security notice pointing here, and
  the member an action names gets one too, with a Cancel of their own on
  their Settings page. An action that still needs approvals when its time
  comes simply expires, and everyone involved is told. If you are the only
  admin, there is nobody else to approve, so your actions need no approvals
  and run after their waiting period unless you cancel them.

The thinking behind admission and removal:
[the admin's guide](nest-for-friends-and-family.md).

## Tiers

Tier *definitions* — what "family", "friend", or "guest" actually grants.
Each tier row edits its caps in place: inbox size, storage, device count,
maximum single-file size, and how many feeds a member can define. Assigning
tiers to people happens on [Users](#users); this page only defines what the
labels mean.

Below the rows, **Define a new tier** adds one of your own: give it a name,
fill in the same five limits as whole numbers (bytes for the size limits,
plain counts for devices and feeds), and choose **Add tier**. It then appears
in the list like any other. A missing name, a limit that isn't a whole number,
or a name that's already taken is refused with a message and nothing is
added — what you typed stays so you can fix the one field. A tier can't be
renamed or removed once it exists, so pick the name with care.

### Membership (paid nest access)

If you've set up any subscription tiers of your own — the same paid tiers a
creator offers on their profile — this section lets you link one of them to
paid access to *your nest*. Each row is one of your subscription tiers; the
two pickers say which of the tiers above (the quota tiers) a paying member
is admitted at, and which one they degrade to if their payment lapses —
lapsing is a reversible, automatic downgrade, never a suspension, and it
self-heals the moment they renew. Designating a tier here doesn't create or
change either tier — it only links two things that already exist, and
clearing the link leaves your subscription tier untouched. If you have no
subscription tiers yet, create one first from your own profile's Tiers tab.

Once a tier is linked, someone can redeem one of the tier's claim codes
(minted the same way as any other subscription's — see Payment providers,
above) either from their own Settings → Subscriptions page if they already
have an account, or by entering it in the invite-code field during sign-up if
they don't — either way their quota tier is set right away. What's still
missing: there's no page yet that shows a prospective member your membership
offering before they sign up — hand them the code directly for now.

## Nest

Nest-wide settings that belong to no single feature:

- **Network mode** — whether this nest is reachable from the internet
  (*Public*) or sits on a home network behind a router (*Private*). You answer
  this once during setup; this is where you change your mind later — for
  example after moving a box from a hosted server to your home. Mail serving
  follows the change immediately; certificates and connectivity re-check the
  next time the nest restarts. Reaching the internet means a few things become
  publicly visible — for exactly what, and what stays private, see
  [Who can see what](who-can-see-what.md).
- **Web app** — what people get when they open the app at this nest's own
  address: the app this nest ships (the default), or a redirect to the Fauna
  project's address, `https://app.fauna.social`, with this nest already filled
  in on the sign-in page. The status line shows exactly where people are sent.
  It changes only what your nest's address answers: anyone who opens
  `https://app.fauna.social` directly loads the app from there either way, and
  an address someone types or bookmarks always wins. A nest without a domain
  yet has nothing to fill in, so it keeps serving its own app whatever you
  choose. Leave it on the default until the project's address is live (see
  [Installing](install.md)).
- **Pairing** — the master switch for whether members may link this nest from
  their other nests.
- **Serving port** — the port the nest serves apps on, for networks where
  the default is awkward.
- **Host maintenance** — when the nest manages its own machine, pending
  security updates and reboots show here, with a *restart now* button to
  expedite an update instead of waiting for the idle window.
- **Region** — the region whose laws this nest operates under. You type it in;
  it is never guessed from your address or your network, and there is no
  "detect" button anywhere, by design. A fresh nest declares nothing, and that
  is a perfectly normal state to leave it in — nothing stops working.

  Declaring a region matters only where a region has an *authority*: a body that
  publishes rules about which features may be used, openly and on the record. If
  the region you declare has one, this section names it and the version of the
  rules currently in force, and those rules apply to accounts hosted here. If it
  doesn't — which is the case everywhere today — the section says so plainly and
  nothing changes. Declaring a region with no authority is allowed and does
  nothing; it is a fact about where the nest lives.

  An authority may also publish rules about content. Those are normally applied
  by each person's own app, for the region that person's device is in — with one
  exception: the public web pages this nest publishes from people's posts, which
  anyone on the internet can open with no app in between, follow the content
  rules of the region you declare here. A post those rules block shows a notice
  in its place on the public page, naming the authority whose rules withheld it
  and the reason that authority gives; a post they only ask to hide sits behind
  a "select to show". The posts themselves are untouched in everyone's apps.
  Declaring, changing or withdrawing the region updates the public pages
  straight away.

  You can change the region or withdraw it entirely at any time. Both retire the
  previous region's rules, since a region's rules are only ever about nests in
  that region. If the nest hasn't been able to check for updated rules lately it
  will say so — a note, not a fault: the rules it already has stay in force, so
  nothing stops working while it catches up.
- **Feature limits for everyone** — limits you set on the few features that can
  be limited rather than simply switched on or off: payments, zaps, and file
  sharing. Each row names the feature and what you have set for it, in words —
  "No limit set" until you set one. Press **Edit** to open the limit: turn the
  feature **Off** for everyone on this nest, or leave it **On** and type a number
  into any of the boxes — uses per day, per week or per month, and for payments
  and file sharing also an amount (sizes like "50 GB", or whole sats) and the
  largest single one. An empty box means no limit; zero is a limit. **Save**
  applies it to every account on the nest, and each person sees it in their own
  Feature limits section, marked as set by their nest admin.
  **Remove limit** takes it away again.

  Your limits can only tighten what already applies — Fauna's own built-in
  limits and your region's rules come first. Nothing stops you typing a looser
  number, but the box then tells you it has no effect and what already limits it.
  Saving needs a connection to the nest; the buttons wait while you are offline.
- **Deployment identity** — gives the nest a brand-new identity, behind an
  explicit confirmation.

  Every app that has connected to this nest remembers *which* nest it is, and
  refuses to be quietly swapped for another one. Rotating the identity is the one
  sanctioned way to change it: apps that already trust this nest re-trust it
  automatically, without anyone re-approving anything, while anyone still holding
  a copy of the *old* identity stops being able to use it.

  That last part is the point. Recovering a lost nest needs a copy of its
  identity, and every administrator's app keeps one — which means removing
  someone from the admin list doesn't take back the copy they already have. No
  system can un-tell a secret. What rotation does instead is make the old copy
  worthless. So the order matters: **remove the person first, then rotate.** The
  confirmation lists everyone who currently administers this nest, because that
  is exactly the set of people who will receive the new identity — if the person
  you are removing is still on that list, wait for the removal to take effect
  before you go ahead.

  What rotation does *not* do is undo anything. Whatever a former administrator
  saw while they had access, they still saw. Rotation closes the door going
  forward; it does not rewrite the past.

  Afterwards the nest carries on serving normally — it never restarts and nobody
  is signed out. Your own recovery list quietly drops the old identity. If it
  still shows the old one, the app will tell you so and clear it the next time it
  reconnects.

  Backups to other nests carry on as well. A backup destination only accepts
  copies from the nest it was set up with, so right after the rotation it turns
  the renamed nest away. The next time one of your apps checks your backups —
  opening the Backups page is enough — it confirms that the new identity really
  is this nest's successor and tells each destination so. From then on the
  backups flow again, into the same copy as before; nothing is uploaded twice. A
  destination whose backups you froze stays frozen.
- **Outside-app sign-in keys** — the keys your nest signs outside apps'
  sign-in passes with, and the secret behind the longer-lived saved sign-ins
  those apps renew their passes with. The section lists the keys in use: the
  one signing now and, right after a replacement, the previous one with how
  many more minutes it will be accepted.

  Three buttons, and which one to reach for depends on what happened:

  - **Replace sign-in key** is the precaution, for when a key *might* have
    been exposed — an administrator who left, a backup stored somewhere it
    shouldn't have been. A new key starts signing at once, but the previous
    one stays accepted for about twenty minutes, so sign-ins already under way
    still work. Nobody is signed out.
  - **Replace sign-in key at once** is for a key you *know* has leaked. Every
    key in use stops being accepted immediately — including a previous one
    still inside its twenty minutes — so a stolen copy is worthless from that
    moment. The price is that every outside app signed in with those keys has
    to sign in again, and the confirmation tells you how many keys that covers
    before anything happens.
  - **End all saved sign-ins** ends every saved sign-in at once, and each
    connected outside app then has to be approved again. After a known leak,
    do this *as well as* replacing the key at once: a stolen saved sign-in
    could otherwise be traded for a fresh pass signed by the brand-new key.

  Each button reports what it did: which key signs now, which keys stopped
  working, and since when the saved sign-ins it ended had been issued.

  Your nest is taking these sign-ins over from the Bluesky bridge. Until it
  has, Bluesky apps still check the key on that bridge's own card (see
  [Bridges](#bridges)) — after an exposure, replace that one too.
- **Legal takedown** — the one way any content is ever removed for everyone,
  and it exists solely for legal compulsion: a court order, a statutory demand,
  genuinely illegal material. Fauna has no "remove this post for everyone
  because I disagree with it" lever, for admins or anyone else — moderation of
  what people see is each person's own filter. This console is the narrow
  exception the law can force, built so it can never quietly become anything
  more.

  You enter the identifier of the item named by the legal obligation and the
  legal reference you are acting under — the reference is mandatory, and the
  console will not proceed without one. A confirmation then shows exactly what
  will be withheld and which citation will be recorded, before anything
  happens.

  Nothing about a takedown is silent. Readers see a notice, with the reference,
  in place of the removed content — never a blank space. The author sees the
  action, labeled as removed under legal obligation, in their own moderation
  queue. And a permanent audit record keeps who did it, what, and under what
  reference.

  A takedown also reaches the places a post can be read without opening it: if
  the author had published the post on their website, its page comes down
  before the console reports success, and search stops finding it — for
  everyone, the author included.

  The same console overturns a takedown — tick *restore*, and the content is
  served again, including its page on the author's website if they had
  published it there (this is why takedowns never delete anything). The record of the
  original takedown remains; overturning closes the episode, it does not erase
  it.
- **Reports** — what your users, and users of other nests, have reported about
  content or accounts on this nest. A new report rings a notification (one per
  reported item per day, however many people report it). Each entry shows the
  reason, the reporter's note, any text they chose to attach — the only way
  you ever read a private message — and who sent it: one of your users by
  handle, or "a user of" another nest, whose reporter's identity never reaches
  you. A report is information, not a lever. **Open in takedown console** fills
  the legal-takedown console above with the item, and every rule of that
  console still applies; **Mark as acted on** and **Dismiss** only record your
  decision. The reporter is told which of the two you chose, and nothing
  more; the person reported is never told they were reported.
- **The danger zone** — factory reset, behind an explicit confirmation. A reset
  returns the nest to its just-installed state and hands your app the one-time
  code needed to set it up again. If the app closes or crashes before you finish
  setting it up — even in the moment right after you confirm — just reopen it:
  it remembers the code and picks the setup back up where it left off. You never
  need to reach the machine itself to recover.
- **Retire this server** — sits right beside the reset, and is its opposite: a
  reset wipes a server you keep, retiring destroys the server itself at your
  cloud provider, after removing the DNS records that pointed at it. You'll need
  your cloud provider's token. See *[Retire a nest](retire-a-nest.md)*.

## Mail

The mail *policy* page — the admin half of [Own your email](own-your-mail.md).
The headline control is the **mail on/off toggle** (plus whether new members
get mail enabled automatically). Everything else is grouped policy, each group
with its own save button, and the defaults are sensible enough that most
admins never scroll past the toggle.

Right under the toggle, **Mail health** tells you whether mail is actually
working. Its first line is the verdict — delivering, warming up, outgoing mail
delayed, DNS records needing attention, the server address blocklisted, the
mail service not connected, or simply off — followed by when mail was last
delivered and last received. A quiet box is not a broken one: those two times
are there for you to read, never a reason for the verdict to turn red. Seven
rows underneath show what the verdict rests on: the mail service connection,
the blocklist check, the outgoing queue, your DNS and authentication records,
the sending warm-up, and the two last-delivered/last-received times. Two
buttons sit below them:

- **Check again** runs a fresh blocklist check and records check now, instead
  of waiting for the daily one, and updates the rows.
- **Restart warm-up** starts the gentle sending ramp over at day 1 — do this
  only after your server's outgoing address changed, since a new address has no
  reputation yet. The first press only asks you to confirm; press again to
  restart.

While your server's address is on a blocklist, a **Request removal from the
blocklist** link appears too, pointing at that list's removal page (in the
terminal app, it shows the address and copies it for you).

The policy groups follow:

- **Spam & inbound perimeter** — the two score thresholds (junk-folder and
  reject), DNS blocklists, greylisting, connection rate caps,
  reverse-DNS strictness, and message size limits. The size limit has a hard
  upper bound of 250 MB: raise it beyond that and the save is refused with an
  explanation. The bound is not arbitrary — every message your nest accepts is
  scanned for malware before it is delivered, and 250 MB is the largest message
  the scanner is set up to handle. A ceiling the scanner could not cover would
  mean quietly letting the biggest messages through unscanned, which your nest
  will not do. (It is also well clear of what other mail providers accept;
  anything larger is better shared as a file than mailed as an attachment.)
- **Authentication enforcement** — how hard to enforce SPF, DKIM, and DMARC
  verdicts on inbound mail, with a log-only mode for observing before
  enforcing.
- **Submission quotas** — how much mail one member can send per day and per
  message.
- **Mailbox serving (IMAP)** — session timeouts, per-member mailbox size and
  message-count defaults, deletion semantics.
- **Outbound delivery** — retry schedule, failure timeouts, delay warnings,
  bounce-handling behavior, IPv6.
- **Alias policy** — per-member alias caps, reserved local-parts
  (`postmaster` and friends), and whether plus-addressing and wildcard
  aliases are available.
- **Shared spam baseline** — publish an aggregated starting model from
  members who opted in (it refuses to publish unless enough members
  contribute — no one member's mail is inferable). When a contributing
  member opts out, resets their filter, or deletes their account, the
  shared baseline is withdrawn straight away so their training stops
  being handed out; publish again to rebuild it from the members still
  contributing. Once a baseline has been published, a new one goes out
  only when at least three contributing members' training has changed
  since the last one (someone joined, left, or trained more) — otherwise
  comparing two baselines could reveal one member's recent training — so
  a publish can come back "Waiting for more contributor activity" until
  enough has changed. Switch on **Keep a shared spam baseline published**
  and the nest republishes by itself every 24 hours, under the same rules;
  switching it off stops that and withdraws the published baseline. The
  **Publish deployment baseline** button still publishes right away,
  either way. Beside them, a line tells you what is published now —
  "Published over N contributors on <date>" or "No baseline published" —
  without ever saying when or why a baseline was withdrawn, since that
  could point at who had been contributing.

What is deliberately *not* here: DKIM keys, TLS certificates, MTA-STS, DMARC
publishing — the deliverability liturgy is automated, and its DNS records
render read-only on the [DNS](#dns) page.

## Calendar, Contacts, and Files

Three small sibling pages, one switch each: serve members' calendars over
**CalDAV**, their address books over **CardDAV**, and their folders over
**WebDAV** — the open protocols that let standard apps (Apple Calendar,
Thunderbird, DAVx⁵, OS file managers) talk to the nest. Calendar also carries
the port these listeners share. Each protocol is independent: run mail without
calendars, calendars without mail, or all of it.

## Web

One control: which member's website is the nest's front page. Members each
get `https://<them>.<domain>/` from their own [Web settings](app-tour.md#settings);
this page decides whose content answers at `https://<domain>/` itself — or
leaves the built-in info page.

## Aliases

Nest-level **external forwarders**: addresses at your domains that forward to
an outside mailbox (`info@` to a co-op's Gmail, say). Pick the domain, type
the local part, type the destination. Members' own aliases are self-serve in
their mail settings and are none of the admin's business — only forwarding to
*external* addresses lives here.

## DNS

The domain manager, and the page that saves you from copy-pasting DNS records:

- **Domains** — add the domains the nest answers for. Each domain row shows
  every DNS record Fauna needs (address, mail, DKIM, your nest's identity, and
  the rest of the liturgy) with a live red/green check against what is actually
  published. Publishing the identity record is what lets a brand-new app verify
  it is talking to *your* nest the very first time it connects, instead of
  trusting whatever answers. **The very first domain you add is a one-way
  door** — the app tells you so before you confirm. It becomes your nest's
  primary domain and can never be removed; changing your mind later means
  registering a different domain and running the rename wizard onto it, not
  undoing the add.
- **"Fauna controls DNS"** — the master switch. On, with a DNS-provider
  credential stored, Fauna publishes and maintains the records itself.
  Off, the rows become your checklist: copy each value to your DNS host and
  watch the checks go green.
- **Provider credentials** — API credentials for your DNS provider(s), held
  by your app. They are write-only: enterable, verifiable, clearable —
  never displayed back.
- **Per-domain extras** — the catch-all address designation (who receives
  mail to unknown names at a domain), renaming a domain through a guided
  wizard with a grace window, and a 30-day undo for removed domains. If the
  person your catch-all pointed at ever recovers their account onto a fresh
  identity, the designation is cleared automatically rather than silently
  following them or being left in place — the row tells you this happened
  and mail to unknown names bounces instead of vanishing until you pick a
  new address. The rename wizard is also the way out if you *lose* a domain — a registration
  that lapsed, or a name taken from you: register the replacement, add it,
  and start the rename. The nest stops asking the dead name to prove itself
  so the move can complete, and everyone's handles are held for them at the
  new domain.
- **The expiry watch** — you should not find out about a lapse from the
  failures. The nest checks its main domain's registration with the registry
  itself, and if that registration is within a week of expiring, has expired,
  or has been put on hold or into redemption, a red banner appears on every
  page — for everyone on the nest, not just you, since their addresses and
  their phrase-only account recovery depend on the name too. Yours says to
  renew at your registrar; theirs says to contact you. There is nothing to
  switch on and no threshold to set. Two things worth knowing: the warning
  window is deliberately short, because the registry cannot tell us whether
  you have auto-renew on and a longer window would put a false banner on a
  perfectly healthy domain every year; and the hold/redemption warning does
  not depend on the date at all, because a domain can show a comfortable
  future expiry while it is already being taken back.

## Bridges

Approval cards for bridge processes asking to join the deployment. Bridges
enroll themselves when they start up on the box; the admin's job is one
glance and one approve/deny per card. The box's own mail bridge is the
exception: once you've enabled mail, it approves itself with no card to
click — cards here are for anything else that enrolls (a Bluesky or Nostr
bridge, or a mail bridge you enroll before turning mail on). (Which bridges
*exist* and what they do:
[Bluesky & Nostr from your nest](bridges-bluesky-nostr.md).)

Below the pending cards is a roster of already-approved bridges, each with a
**rotate key** action — useful if a bridge's identity may have been
compromised. Confirming disconnects that bridge and forces it to re-enroll
with a fresh key; for the box's own bridges the fresh key re-approves itself
automatically, nothing else to click. Rotating the mail bridge leaves your
DKIM signing key alone — the nest holds that key itself — so the published
DKIM record stays valid and there is nothing to republish.

The keys outside Bluesky apps check when someone signs in with them are your
nest's own, not the bridge's: they are the **Outside-app sign-in keys** on the
Nest page, described above. **Rotate key** on the Bluesky bridge's card
disconnects only the bridge, and leaves those sign-in keys alone.

## Held Custody

People on your nest can ask it to hold a sealed copy of a friend's data for
them — a friend's backup, kept safe on your box. That is a feature, not a
problem: the copy is sealed, your nest cannot read it, and the friend gets a
place their data survives losing every device.

What this page gives you is the other half: a list of every such
arrangement on the nest, whoever set it up. Each row shows who on your nest
asked for it, whose data is being held, the address your nest fetches from,
how much space it is allowed and how much it is actually using, whether it
has been paused, and when the arrangement last checked in. The list puts the
biggest holdings first, because "what is filling my disk" is usually the
question that brings you here.

Anyone with an account can set one of these up, and each one makes your nest
reach out to an address they chose, on a schedule. Your nest already refuses
the obviously bad cases on its own — there is a ceiling on how much any one
arrangement may hold, a limit on how many any one person may have, and the
space counts against that person's own storage allowance. This page is for
the cases a limit doesn't cover: something you don't recognise, or someone
using their whole allowance on it when you'd rather they didn't.

**Remove** is how you end one. It frees the space straight away, and once
the last arrangement for that particular friend is gone, the stored copy goes
with it. It cannot be undone from here — the person who set it up would have
to ask again — so the button asks you to confirm first.

Two things worth knowing before you use it. Removing is not the same as the
**pause** the person themselves can apply from their own device: pausing
stops the fetching but keeps everything already stored, so a paused row is
still using space. And the space is simply counted, never banked — removing
gives it back immediately, with nothing to credit anywhere.

## Logs

The nest's recent log — the server twin of the app log page under
Settings. Filter by severity, copy the visible lines for a bug report.
Message content and secrets are never logged, so nothing here compromises
members' privacy.

Alongside the nest's own entries you'll see a small number of events from
the other services running beside it — the mail server, the relay — tagged
with the service they came from, such as `mta:` or `relay:`. These are the
handful of things worth acting on: a service coming up, a port it couldn't
take, a TLS certificate it couldn't fetch, a connection that keeps
dropping. Everyday mail traffic doesn't appear here, and neither do
addresses or message details.
