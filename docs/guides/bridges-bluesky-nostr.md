# Not alone on your own server — Bluesky and Nostr from your nest

> **Status — this guide describes Fauna as it is taking shape.** Bluesky,
> Nostr, and the Fediverse are all live today, each at a different level of
> polish — the [table at the end](#where-this-stands-today) says exactly
> what is available now, what is landing, and what is planned.

---

> **What this is.** How a nest connects you to the wider social world —
> Bluesky, Nostr, and the Fediverse — so that self-hosting doesn't mean
> shouting into an empty room.
>
> **Who it's for.** Anyone whose friends aren't (yet) on Fauna.

---

## One feed, many networks

Fauna's answer to "but everyone I know is on X-or-other" is bridges: your
nest speaks other networks' protocols, and everything lands in **one feed**
and **one conversations list**. A bridged post looks and behaves like any
other post — same reply box, same like button — with a small badge showing
where it came from (a butterfly for Bluesky, a bolt for Nostr, a globe for
the Fediverse). A post carried in by an app you connected wears the icon that
app chose with its name beside it. You read in one place; Fauna routes your reactions back out
to the network they belong to.

**When a bridge can't be connected, it says so.** If a network is having
trouble — its service is unreachable, or your nest can't reach it right now —
that bridge's **Link** button is greyed out rather than left looking ready,
and the reason appears next to it, in the network's own words where it gave
one. A Link button you can press is one that will actually do something; if
pressing it can't work, you get an explanation instead of silence. Bridges in
this state stay in the list rather than disappearing, so a temporary outage
never looks like a removed feature.

### Keeping bridged posts out of — or in — your Search results

Every bridge you link (Bluesky, Nostr, the Fediverse) has two settings on its
card: **Show in search** — whether that network's public posts you receive
show up when you search Fauna — and **Limit posts in search**, a cap on how
many of a bridge's newest posts stay searchable at once (older ones drop off
as new ones arrive, so a busy bridge doesn't crowd out everything else).
Nothing private — DMs, gift-wrapped Nostr events — is ever searchable this
way, on any bridge; the toggle only ever governs a bridge's public content.
Both settings save the moment you change them.

## Bluesky, today: bring your account

What ships now is an **account link**: connect the Bluesky account you
already have (say `you.bsky.social`) from **Settings → AT Protocol**. It's a
standard OAuth flow — you approve Fauna in your browser, Bluesky hands your
nest a session, and from then on **your nest is your Bluesky client**:

- **Bluesky notifications** (likes, replies, follows, mentions…) arrive in
  your unified notification stream.
- **Bluesky DMs** appear in Conversations alongside everything else.
- **Cross-posting** is a setting with three positions: off (default), every
  post automatically, or manual per-post. Remember what "on" means: your
  Fauna posts are published to Bluesky — public, searchable, and effectively
  permanent there. A reply you write under one of your own cross-posted posts
  is threaded under it on Bluesky, so a thread you continue in Fauna stays a
  thread there.
- **Your Bluesky timeline in your Fauna feed.** Posts from the people you
  follow on Bluesky, and from any custom feed you subscribe to on the Feed
  page, arrive in your unified feed a few minutes after they are posted, with
  the Bluesky badge. Liking, reposting, replying to or quoting one from inside
  Fauna acts on Bluesky as your account; a reply or quote is posted to Bluesky
  whatever your cross-posting setting says — the post you replied to is the
  invitation. A post shows its author the way Bluesky knows them — their
  display name, or their handle when they have not set one — and the same
  goes for a post that reached you from the fediverse or from Nostr. Two
  things still to come: the author's picture is not shown on their posts
  yet, and the images a Bluesky post carries are not rendered in the feed
  yet.

Two honest limits: **following and blocking people stay on Bluesky's side**
— manage your follows in a Bluesky app; your nest reads the results — and
Bluesky images are fetched through Bluesky's CDN rather than re-hosted by
your nest.

One requirement: **your nest needs its own domain name** for this. Bluesky's
sign-in service has to reach your nest by name to complete the approval step,
so on a nest that has no domain yet the Bluesky connect button is greyed out
and says so. Give your nest a domain and it becomes available — nothing else
to set up.

Privacy note: for this to work your nest holds a session credential for your
Bluesky account (that's what "your nest is your client" means). Unlinking
removes it.

## Bluesky, the destination: your nest as your Bluesky home

The next step goes further: instead of linking an account hosted on
`bsky.social`, **your nest itself becomes your Bluesky server** (in AT
Protocol terms, your PDS). Your AT Protocol identity would live on your box,
your handle would be your own domain, and your public Fauna posts would
appear on Bluesky natively — no second account anywhere.

This is a real, working feature, not just a design: turn it on from
Settings → AT Protocol and your nest mints the identity, holds the repo, and
starts publishing. Only **public** posts ever project — encrypted
conversations and gated content stay on Fauna by design — and publishing
your existing post history is a separate, deliberate opt-in. Your **profile
travels too**: display name, bio, and your profile picture and banner appear
on Bluesky and follow your edits, so people who find you there see an account
that looks like yours rather than an empty shell. Clearing a picture on Fauna
clears it there as well.

**Photos and videos travel with your posts.** Pictures you attach are
re-published from your own box, so they load from your nest rather than
depending on anything staying online elsewhere. A video is published as a real
Bluesky video, playable in the timeline: your nest picks the best-quality
version that fits within a size limit and publishes that one. Two limits are
worth knowing. A very large attachment is left off — the post still goes out,
just without that picture. And if a video is too large at every quality, the
post is not published to Bluesky at all, because a video post has no text of
its own and an empty post there would say nothing. It stays on Fauna as
normal. One honest
question stays outside Fauna's control: whether Bluesky's infrastructure
reliably picks up and indexes a small self-hosted server. That hasn't been
confirmed on a live deployment yet, so treat this as an early capability
rather than a proven one — if the network doesn't cooperate, the
account-link approach above remains the fallback.

Settings → AT Protocol now organizes all of this around one question:
**how deep do you want your Bluesky integration to go?** A single depth
selector offers four ordered choices —

- **Off** — no Bluesky presence.
- **Linked account** — the account link described above: read, interact,
  and cross-post through an existing Bluesky account.
- **Hosted here — visible** — your nest holds your identity and publishes
  your public posts to the Bluesky network.
- **Hosted here — full access** — additionally, other Bluesky apps can sign
  in as you against your nest.

Each level brings its own controls with it, right below the selector. Choose
**Linked account** and the connect-your-account controls appear there — the
same Connect / Disconnect button, cross-posting settings and follows list that
used to live on the *Bridges* page, just moved to where Bluesky now lives.
(Bluesky has its own page for the same reason Nostr does: there is far more to
it than a single on/off row. The *Bridges* page keeps the lighter-weight
integrations.)

Picking a different level shows a plain-language card spelling out exactly
what the change will do *before* anything happens — a new public identity is
created, an account link is dropped, publishing begins, apps gain or lose
access — and nothing changes until you confirm it. Stepping back down is
always reversible: your identity and keys are kept, so re-enabling restores
the same identity rather than making a new one. The two "hosted" levels are
greyed out, with a reason, until your nest is reachable at a real public
domain (they can't work on a `localhost` or IP-only box).

The deepest level reveals the login-plane controls — **App credentials** and
**Connected apps**, for signing other AT Protocol apps in against your
nest, plus a single toggle that instantly shuts off every external app's
access without touching any individual credential. Logging another AT Protocol
app in to read, interact, post and edit your profile through your nest works
end to end — posting and profile edits after you grant the separate approval
described next.

### Deleting what you published to Bluesky

Stepping the selector back down to **Off** stops publishing and takes your
profile and posts out of circulation, but it keeps everything ready to switch
back on. If you want the posts themselves taken down from the Bluesky network,
there is a separate, stronger action below the selector: **Delete my Bluesky
presence**.

It asks first. A confirmation card spells out what will happen: every post you
published to Bluesky is deleted and the network is told to remove them, the
Bluesky apps you are signed in to are disconnected, other apps can no longer
post as you, and your Bluesky setting returns to Off.

**Your AT Protocol identity is kept.** This removes what you published, not who
you are — the handle stays yours, and turning AT Protocol hosting back on later
restores the same identity rather than creating a new one. That is the
difference between this and deleting an account elsewhere, and it is why the
card names your handle while it asks.

### Retiring the identity for good

The same card offers one more box, never ticked for you: **Also permanently
retire my AT Protocol identity — this cannot be undone.** Tick it only if you
want the identity itself gone, not just what you published. Once the deletion
has finished, your device publishes the retirement to the public record of
your identity, signed with the recovery key only your devices hold, and from
then on that identity no longer exists on the Bluesky network. Nobody can
bring it back — not you and not whoever runs your nest. If you turn AT
Protocol hosting on again later, you get a new, different identity.

While the box is ticked, the card says exactly this in place of "your identity
is kept", so you never confirm with both promises in front of you. The box is
greyed out, with the reason beside it, when there is nothing it could do: an
identity tied to your own domain ends when your domain stops serving it, and an
identity that has not been published yet has nothing to retire.

If the deletion goes through but the retirement cannot be recorded, the card
stays open and says so. Confirming again is safe: deleting what is already
deleted changes nothing, and the retirement is tried again.

One thing no service can promise, Fauna included: deletion cannot recall copies
that other servers on the public network already made. Your nest deletes what it
published and tells the network to do the same, and the well-behaved parts of the
network will — but caches and third-party mirrors are outside anyone's reach.
That is the nature of publishing publicly, and it is why turning any of this on
takes a deliberate choice in the first place.

The deletion also does not happen instantly on screen. Confirming records the
decision and starts the work; the posts are removed in the background, and the
page shows your identity as deleted once the decision is in. So the page tells
you the deletion has *started* rather than claiming your posts are already gone
everywhere — which would not be true yet.

### If something is wrong with your identity, you will be told

An AT Protocol identity you host is yours because of a recovery key that only your
own devices hold. Your apps check on that quietly, in the background — and they
check it by asking the public directory directly, not by asking your nest, so a
misbehaving server cannot vouch for itself.

Two things get checked. First, that the public record of your identity still
names *your* recovery key as the senior one — if it named someone else's, that
person could take the identity from you. Second, that your identity is still
published under a handle at your own domain — if it were published under some
other name, everyone searching for you would find a stranger instead.

Both checks run whenever the AT Protocol settings page refreshes, which covers the
moment the identity is created. They also run on their own in the background —
starting the moment you sign in, and again every few hours for as long as you
stay signed in — so you find out even if you never open Settings again, whether
that session lasts a minute or the app stays open for days.

If either check fails, you get a red banner that appears on **every** page and
cannot be dismissed. That is deliberate: these are the two things worth
interrupting you for, and a warning tucked away on a settings page you have no
reason to visit would never be seen. The banner disappears on its own once the
condition is fixed. If your apps simply cannot reach the public directory,
nothing is claimed — you will not be warned about something that was never
actually checked.

### Undoing a change you did not make

Being told is not much use on its own, so there is something you can do about
it. If the public record of your identity was changed using a key your devices
do not hold, Settings → AT Protocol shows that at the very top of the page, above
everything else, with a plain description of what was changed and roughly how
long you have left to undo it. Choose to undo, read the confirmation, and
confirm: your app signs the reversal with your own recovery key and sends it to
the public directory itself, without going through your nest. That matters,
because the nest is exactly what you might be defending against.

If it works, the change and anything built on top of it are reversed, only your
own keys can decide who controls the identity, and the red banner comes down by
itself once the public directory confirms it — never merely because your app
tried. If it fails, nothing has changed and you can simply try again.

Two honest limits. There is a **time limit** — Bluesky's directory only accepts
this kind of reversal within 72 hours of the change, and after that the change
stands for good, so do not sit on the banner. And undoing this does **not**
evict your nest entirely: it keeps the separate key it uses to publish your
posts, so it could still post as you. Replacing that key is a separate step.

Sometimes there is nothing to undo, and you will be told which of three reasons
it is, instead of being offered a button that cannot work: the time limit has
passed; the identity was created with a key you never held, so there is no
earlier state to return to; or the public record itself does not check out.

That last one is worth understanding, because it is the one case where doing
nothing is the safe answer. Your app checks that every change in the public
record was signed by a key that record's own history allows. If that check
fails, what you are looking at is not a misused key — it is a record that has
been tampered with, or a connection to it that something is intercepting.
Undoing is not offered, on purpose: signing a reversal of a false history with
your own recovery key is how you would lose your real one. Nothing is signed or
changed from your side. Try again from a different network, and talk to whoever
runs your nest.

### Connecting another app

There are two ways an app signs in, and which one you use is the app's choice,
not yours.

Older apps ask for a **handle and a password**. Mint one under *App
credentials*: your nest shows the generated secret once, you paste it into the
other app, and you can re-reveal or revoke it later from any Fauna app. Nothing
about it is a password you chose — there is no such thing anywhere in Fauna.

Newer apps use the **modern sign-in**, and it is worth knowing what to expect
because part of it happens in your Fauna app rather than in the app you started
from. You type your handle over there; it opens a page in your browser. That
page names the app asking, lists what it wants to do in plain language, and
shows a short code like `K7M-P2X`. It then waits.

Open Fauna on any device you're signed in on. A card is waiting under
**Requests** on Settings → Connected apps (in apps that do not have that page
yet, at the top of Settings → AT Protocol), showing the same app, the same list,
and a code. **Check
that the code matches the one in your browser**, then approve. The browser
finishes the sign-in by itself within a second or two, and the app you started
from is connected.

Sometimes an app asks for a **named bundle of permissions** rather than listing
them one by one — a *permission set*, published by whoever wrote it. When that
happens, the card and the browser page both show the bundle's name, who
published it, and, underneath, every individual permission it actually covers.
You are never asked to approve a name on trust: the bundle's own description is
there to explain the point of it, not to stand in for the list. What you
approve is frozen at that moment, too — if the publisher later widens the
bundle, your existing connection is unaffected, and an app that wants the new
permissions has to ask you again.

The same sign-in also works for a website that offers **Sign in with Fauna**
and wants nothing from your Bluesky account at all — only to know it is you.
The card then lists up to three plain rows, each one separate so you can see
exactly what leaves: signing you in with a stable account identifier (it stays
the same if you later change your handle), your handle, and your email address.
The site learns nothing it did not ask for, and nothing you do not have — if
your account has no mailbox on your nest, no email address is sent. This kind
of sign-in does not need Bluesky to be switched on for your account.

An app can also ask to **read your nest's public timelines** — the trending
and local feeds of public posts that anyone could see on your nest. The card
shows this as its own row. It reaches public posts only: nothing from your
private feeds, nothing about what you liked or reposted, and nothing posted to
a closed audience. An app holding this permission talks to your nest directly
for as long as you leave it connected; revoking the app on Settings →
**Connected apps** cuts that link at once.

An app can also ask to **keep its own records in your account** — notes, tasks,
or whatever kinds of data it was built around, each one published under the
app's own web address. The card names that publisher and how many kinds of
records the app wants, and says **read only** when the app has not offered a
way to sign what it writes. When you approve, your Fauna app hands the app keys
to exactly those records and nothing else: they are encrypted like the rest of
your account, your nest stores them without being able to read them, and the
app cannot see your mail, your posts, your files, or any other app's records.
An app run as a service on someone else's server can never ask for those at
all — only for records it creates itself. Revoking the app on Settings →
**Connected apps** cuts its access at once, and the records it wrote stay in
your account.

If an app you already connected asks again with a **new key** — after a
reinstall, say — the card says so under its list: approving ends the access the
old key was given (or, when only its signing key changed, the old key's
permission to write). Your Fauna app ends that access on your nest as you
approve, and your grant history records it, so a key the app no longer uses
never keeps a way into your records.

That code comparison is the whole point, so it is worth being pedantic about:
it is what makes a sign-in someone *else* started visibly wrong. If a card
appears when you weren't signing in to anything, or the code on it doesn't
match what your browser shows, decline it. Declining is a real answer, not
just dismissing the card — the waiting browser is told cleanly rather than
being left to hang.

A few things follow from how this works. The card can be answered from any
device you're signed in on, not only the one holding the browser. Only one
device needs to answer — the card disappears everywhere once it is. The
request expires after about ten minutes, so if you get distracted, start again
from the app. And your browser never holds a Fauna secret at any point: the
approval travels from your Fauna app to your nest over your own connection,
signed by the key that is your identity.

Once connected, the app appears under Settings → **Connected apps** with the
name it published, what you approved it for, when it connected, and when your nest last
saw it. If you approved a named bundle of permissions rather than a list, the
row says which bundle and who published it, so the permissions above it still
have an explanation months later. That last figure is a genuine "last seen", not a usage log: your nest
notices an app when it renews its access, so a recent time means it is
definitely still in use, while an older one only means it has not renewed
lately. Revoking it there
ends the connection: the app stops being able to refresh, and drops off within
about fifteen minutes. The kill-switch above it does the same to every app at
once — including apps that talk to your nest directly rather than through
Bluesky, which are cut off immediately rather than at their next renewal — and
is reversible: flip it back and the apps you kept are usable again, with nothing
to set up afresh.

Signing out inside the other app ends the connection the same way, from the
other direction — it disappears from **Connected apps** without you having to
do anything, and your Fauna app updates while you are looking at it. Either
end can end a connection, and neither needs the other's cooperation: if the app
is gone, uninstalled, or simply not listening, revoking from Fauna still works.

Signing in this way gets an app *read* access as you. Posting still needs the
separate, device-signed permission described next.

Posts you write in the other app arrive as real Fauna posts — text, hashtags,
links, replies, quotes and photos alike. Attach a photo the way you normally
would over there; it lands in your nest's own media store and shows up in
every Fauna app, exactly as if you had posted it here.

**Signing in is not permission to post.** Letting another Bluesky app sign in
lets it *read* as you; posting on your behalf is a separate decision you make
separately, under **Posting from other apps**. Until you grant it, other apps
can sign in and browse, but every attempt to post is refused. Choose *Let
other apps post as me* and the row fills in with what you allowed, when you
allowed it, and when the permission runs out — roughly three months, after
which it stops on its own rather than lasting forever. It warns you a couple
of weeks ahead, and renewing is the same single button; you never have to
revoke first. *Stop other apps posting as me* ends it immediately.

The same permission also lets those apps **edit your profile** — change the
display name, bio, profile picture and banner you show the world from whichever
app you happen to be in. Only those four things: an app over there has no way
to express the rest of what your Fauna profile holds, so your links, the list of
nests that make you reachable and everything else are left exactly as they were.

Clearing any of the four in the other app clears it here, which is the point —
otherwise you could never remove a name or a picture from the app you set it in.
That cuts both ways, so it is worth knowing: an app that saves your profile
**without** a picture is telling your nest you removed it. Ordinary apps send
your picture back untouched when you edit only your bio, so this is not
something you will normally trip over — and nothing is destroyed either way. The
image itself is still stored; you can set it again from any Fauna app.

A picture an app sends can only ever be one your nest already holds — either one
you uploaded from a Fauna app, or one the app itself uploaded to your nest first.
An app cannot point your profile at an image living somewhere else.

What makes this more than a server-side setting: **the permission is signed on
your device, by the key that is your identity.** Your nest can store it and
enforce it, but it cannot grant itself permission to write as you — and when
your app displays the row, it re-checks the stored permission against your own
key first. If what your server holds isn't something you actually signed, the
row is withheld and you're told, rather than being shown a permission you
never gave. Revoking stops future posts; anything already published has spread
to other servers and can't be recalled, which is true of every post on any
public network.

**You can always see what another app actually wrote.** A post another app made
as you carries a small **Via connected app** marker on its card, right in your
feed, next to any post you wrote yourself. It appears on quoted posts too, so a
quote written by an app is marked even when you wrote the post quoting it. This
isn't a note your server keeps — your app works it out from the post itself,
from the same signature that made the post valid in the first place. That
matters: a server can't quietly present someone else's post as yours, or yours
as an app's, because it would have to forge your identity key to do either.

The permission row also shows when an app last used it. Treat that one as a
hint rather than proof — it's a number your nest reports, not something checked
against your key the way the permission itself is, so it can't tell you an app
*hasn't* posted. If you want to know what was actually written, the markers in
your feed are the answer; scan for them, and if you find posts you don't
recognise, *Stop other apps posting as me* ends the permission immediately.

**Your identity stays verifiably yours.** When your nest creates a hosted
AT Protocol identity, the app on your device generates a recovery key that
never leaves your device — it, not your server, is what ultimately controls
the identity. The app then independently checks the public AT Protocol
identity directory to confirm the published record really does name your
device's key as the most senior one. If the record ever disagrees, a prominent red security
banner appears across the whole app — not just on the AT Protocol page — telling
you the identity may not be under your control. No news is good news: you'll
never see the banner unless something is genuinely wrong.

## Nostr: a personal Nostr home

The Nostr integration is built deep rather than bolted on: the nest acts as
your **personal relay** (`wss://yourdomain.com/nostr`), gives you a
`you@yourdomain.com` NIP-05 identity for free (you already own the domain),
syncs with the public relays you list, and bridges Nostr DMs into
Conversations. The relays you list — and the relay named in an external
signer's connect string — have to be reachable on the public internet: your
nest refuses to dial a relay on a private or local network address, the same
rule it applies to every other address someone hands it, so that no account
on a shared nest can point the box at the network it sits on.
You can generate a fresh Nostr key, import your existing
`nsec`, or — if you'd rather the nest never hold your key — connect an
external NIP-46 signer, or (in the web app) a browser signing extension. A
key you keep outside the nest has to be proven before it is linked: your
browser extension asks you to approve one signature, and a NIP-46 signer is
contacted and asked to sign while you link, so paste a fresh connection
string and keep the signer reachable. A key that isn't proven is never
linked, so nobody can claim your npub for their `@yourdomain.com` name. A
Nostr key belongs to one account per nest: if another account on the same
nest has already linked that key, linking it again is refused and their
account is left untouched.

Two things to know before planning around it:

- **It shipped to the production server image on 2026-07-16.** The full page
  is live in every Fauna app.
- **It requires granting your nest access to your content.** Relaying and
  signing on your behalf means the nest has to work with your post content —
  sealed by default like everything else on Fauna. Turning Nostr on mints the
  specific grant for it, from Settings → Nests, and you can revoke it the
  same way if you ever turn Nostr off.

**Follows catch up, not just keep up.** The people you follow on Nostr flow
into your unified feed, and your nest asks each relay for what it missed — so
posts written while your nest was off or unreachable arrive on the next sync
instead of being skipped, and following someone new brings in their recent
posts rather than only what they write from that moment on. Backfilled posts
keep their original dates, so they sort into your feed where they belong
instead of arriving at the top as if they were new.

**Sign in to other Nostr apps with your nest.** If you generated or imported
your key on the nest, your nest can also be the *signer* for the Nostr apps you
already like — Damus, Amethyst, noStrudel, and anything else that speaks
"Nostr Connect". Open the **Connected apps** section on the Nostr page and tap
**Connect an app**: your nest shows a one-time connect code as a QR you scan (or
text you paste) into the other app. From then on, when that app needs to post or
sign, it asks your nest — **your key never leaves the box**. Each connected app
gets its own row on Settings → **Connected apps**, beside every other app that
acts for you, showing when it was last used; **Disconnect** it the moment you
want it gone — that takes effect immediately.
(This is the mirror image of the "external signer" option above: there, some
other box holds your key; here, *your* box signs for everyone else.)

**Replying to and quoting Nostr posts.** Reply to or quote a Nostr post in
your feed the way you would any other post: your nest signs it with your key
and sends it to your relays, and Nostr apps show it in the right thread (a
quote embeds the post you quoted). Two switches on the Nostr page decide the
rest: *Publish replies* lets your replies go out to Nostr at all, and
*Automatically publish Fauna posts to Nostr relays* sends your ordinary posts
there too. A reply or quote needs a key your nest holds and at least one relay
in your list — if either is missing, the reply button says what to fix and
nothing is sent.

**Zaps: deciding whose word you take for a tip.** A zap is a Lightning tip, and
the receipt that says one arrived is signed by *the wallet service that received
the payment* — not by the person who sent it. That means anyone can write a
receipt claiming you were paid. So your nest starts out believing **nobody**, and
you tell it whose word to take: open **Zap signers** on the Nostr page, paste the
signer key your wallet provider publishes (they list it as a "Nostr pubkey"),
give it a name you will recognise, and tap **Designate signer**. From then on,
receipts signed by that provider count as real tips to you, and everything else
is ignored. Until you add one, the section says so plainly — an empty list is not
"nothing has happened yet", it means every zap you receive is being ignored.
**Stop trusting** removes a signer at any moment; you can always undo a
designation, and doing so never needs anyone's permission but yours.

**Running two boxes?** If your nest is split into a home box and a public box
(the same two-box shape as the home-relay mail setup), your Nostr home spans
both automatically: the public box is the face the Nostr network sees — it
serves your relay and your NIP-05 identity around the clock — while your Nostr
key stays on your home box, which does all the signing and message-unwrapping
behind the scenes. There is nothing to configure: linking the two boxes turns
it on, and if you ever unlink them the public box stops serving Nostr
immediately. On the public box's Nostr page the account shows as *proxied* —
served here, signed at home. (Linked your boxes before this existed? Run the
link step once more to switch it on — it's safe to repeat.)

## The Fediverse (Mastodon & friends)

ActivityPub support — your nest federating with Mastodon and the rest —
shipped to the production server image on 2026-07-16. Turn it on from the
app's *Bridges* page and your nest mints a Mastodon-compatible actor
(`@you@yourdomain.com`): remote fediverse accounts can find and follow you
and receive your public posts, and you can follow remote accounts back into
your unified feed. Delivery has been confirmed end-to-end against real
Mastodon and GoToSocial servers.

**Replying to and quoting fediverse posts.** Reply to or quote a fediverse
post in your feed exactly as you would any other post. What you write is your
own post — it stays in your nest, threads under the post you answered, and
deleting it deletes it on the fediverse too — and your nest sends it to the
person you answered as well as to your followers, so it shows up in their
thread. A quote carries a link back to the post you quoted, which every
fediverse app can open. If replying is not possible yet (for example, you have
not turned the Fediverse on), the reply shows an error and nothing is sent —
turn the Fediverse on from the Bridges page and try again.

**Direct messages.** When someone on the fediverse sends you a direct message
— a post addressed to you alone, not to the public or to their followers — it
arrives in Conversations as a private conversation with that person, not in
your feed. Reply there and your answer goes to them alone. Posts someone
shares only with their followers are not direct messages and are not shown.

## Where this stands today

| Feature | Status |
|---|---|
| Unified feed & conversations with per-network source badges | **Available** |
| Show in search / limit posts in search, per bridge | **Available** |
| Bluesky: link your existing account (OAuth) | **Available** |
| Bluesky: timeline into your feed; custom-feed subscriptions | **Available** — polled every few minutes; author names, pictures and post images are not shown yet |
| Bluesky: like/reply/repost/quote from Fauna | **Available** — on any Bluesky post in your feed; replies under your own cross-posted posts thread on Bluesky too |
| Bluesky: notifications & DMs in Fauna | **Available** |
| Bluesky: cross-posting (off / auto / per-post) | **Available** |
| Bluesky: follow/block from Fauna | **Not bridged** — manage follows in a Bluesky app |
| Bluesky: your nest as your own Bluesky server (PDS), domain handle | **Early** — the Settings → AT Protocol depth selector can mint your identity and publish your public posts today; not yet confirmed on a live deployment: whether Bluesky's network actually crawls and indexes it. App credentials + Connected apps let another AT Protocol app log in and read/interact; once you grant *Let other apps post as me*, it writes as you — posts with photos, replies, quotes and profile edits all arrive as the real thing |
| Nostr: personal relay, NIP-05 identity, relay sync, DMs, key import or external signer | **Available** — shipped to the production server image 2026-07-16; requires minting a content grant for your nest, from Settings → Nests |
| Nostr: sign in to other Nostr apps with your nest (Nostr Connect) | **Available** — the *Connected apps* section on the Nostr page, in every Fauna app |
| Nostr: reply to and quote Nostr posts from Fauna | **Available** — needs a key your nest holds; *Publish replies* on the Nostr page controls replies |
| Nostr: choose whose zap receipts you believe | **Available** — the *Zap signers* section on the Nostr page. Until you designate a signer, no zap counts as paid, which is the deliberate starting point |
| Fediverse / ActivityPub federation | **Available** — shipped to the production server image 2026-07-16; confirmed against real Mastodon and GoToSocial servers |
| Fediverse: reply to and quote fediverse posts from Fauna | **Available** — your reply reaches the person you answered and threads under their post |
| Fediverse: direct messages in Conversations | **Early** — one-to-one messages arrive and your replies are sent; not yet confirmed against a live Mastodon server, and starting a new conversation needs the person's profile address |
