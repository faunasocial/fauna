# Running a nest for friends and family — the admin's guide

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
> exactly what is available now, what is landing, and what is planned.

---

> **What this is.** You have a nest running and people you'd like on it — a
> household, friends, a small community. This guide covers the *people* side of
> being the admin: letting members in, capacity, what powers you do and don't
> have, moderation, kids' accounts, and keeping everyone's data safe.
>
> **Who it's for.** The person who ran [Set up a nest on the internet](nest-internet-setup.md)
> or [Set up a nest at home](nest-home-setup.md) and is about to stop being the
> only account on it. The *file-sharing* side of a shared nest — shared folders, quotas as disk space, friend-hosted backups — is covered in
> [Sharing files and running a family nest](cloud-sharing-family.md); this
> guide is its companion, about accounts and administration.

---

## What "admin" means in Fauna

The admin is not a special account. It's an ordinary user — your posts, mail,
files, and calendar work exactly like everyone else's — that additionally
holds the **admin role**, which unlocks the admin area in the app: dashboard,
users, tiers, domains and DNS, mail, and nest settings. Everything below
happens in that area, in the same app everyone uses. Fauna has no server-side
admin console, no config files, no command line to administer people from.

Some structural facts worth knowing on day one:

- **There can be more than one admin.** You can grant the role to another
  member and share the job. The nest refuses to remove the *last* admin — a
  box with nobody at the wheel is a state Fauna doesn't allow you to reach.
- **An admin account can't be evicted, suspended, or deleted while it holds
  the role.** To retire an admin, remove their role first; then they're a
  regular member like any other.
- **You manage space and membership, not members' private content.** Apart from what a member publishes to everyone, which is public by design, members' files, messages, mail and calendars are stored encrypted on your nest, and apart from the short list below, their content is unreadable to you. You can evict an account; you cannot read it. That holds on every nest — there's no setup choice that changes it. What your nest — and so you, who run it — can read is the same short list everywhere: what a member publishes to everyone, which is public by design; the mail and the messages your nest passes to and from email and other networks, which it handles readable on the way through, as any mail server must; a community room whose members chose to let its home nest read it; and whatever a member gives this nest permission to read — by saying yes to **Trust this box?** when they join, later from their own **Settings → Nests**, or by turning on a feature that needs it, such as **Serve over WebDAV** on a folder or connecting a standard mail, calendar or contacts app (your nest opens what that app reads while it is signed in) — until they revoke it. You cannot widen that list yourself: a member widens it for their own data, and someone who can open a member's message or restricted post — the person it was sent to, a subscriber, a member of the room — can attach its text to a report, which you can then read. Your nest also sees what any server that delivers messages must: who each member is connected to — their contacts, whom they follow, the groups they're in — and when and how much they send. And when a member's app shows a preview of a link that stands on a line of its own, in a post or a message, it asks your nest to fetch that link, so your nest learns the link's address (see [Who can see what](who-can-see-what.md#link-previews-and-shared-media)).

## Letting people in

Who can join is your choice, on the admin **Users** page. Under **Who may create an account**, pick **Nobody — only I can admit people** (where a new nest starts), **Only people with an invite code**, or **Anyone** — who can then sign up on their own, at the free tier. **Limit free accounts** stops people joining on their own at the free tier — with no code, or with a free-tier invite code — once the nest holds that many free accounts; people you admit yourself are never refused by it. Leave **Accept only signups carrying app age verification** off: while it is on, nobody can join on their own, with or without a code, because no app can verify an age yet. Apart from **Anyone**, nobody joins your nest without you, and whichever door they come through, your real decision is the same: which **tier** (capacity level) the new account gets. There are three doors, all on the same page:

- **Invite codes.** Mint a code, choose the tier it grants and how many times
  it may be used, and hand it over however you like — chat, e-mail, paper.
  The person types it during sign-up ([Getting started](getting-started.md) § Step 3a) and is in immediately — as long as **Who may create an account** is not set to **Nobody — only I can admit people**, for a free-tier code **Limit free accounts** has room, and **Accept only signups carrying app age verification** is off. Someone whose phone reports them as a minor can't join on their own — with an ordinary code, or under **Anyone** with none: give them a code you made with their **Guardian** picked, or approve their join request with the **Guardian** picked.
- **Join requests.** Someone who tries to sign up at your domain without a
  code can submit a request instead. It appears in your pending list with
  their requested handle and message; you pick a tier and approve, or decline
  with an optional reason. Their app shows the decision as soon as you make
  it — requests survive them closing the app.
- **Admit someone directly.** If someone already has a Fauna identity, the **Admit someone directly** section takes their account key, the handle they'll have and a tier, and admits them on the spot.

(How you became the admin in the first place — claiming a fresh nest with its
one-time claim code — is in [Getting started](getting-started.md) § Step 3b.)

## Tiers: how you share the disk

Every account has a tier; the tier is the quota. A tier caps storage, inbox size, device count, the largest single message delivered to the account, and feed count. A new nest seeds a
sensible ladder — a small **free** tier (the default for new members), a
**personal** tier, and a roomier **community** tier — and you can edit the
caps or add tiers on the admin **Tiers** page. Changing someone's allowance
is a one-dropdown tier change on their row.

Enforcement is real and clean: writes past the cap are refused with a clear
error; nothing of theirs is ever silently deleted. There is also a
storage-only **backup** tier — an account that can hold encrypted backup
chunks but has no mailbox or feeds — which is how
["you hold mine, I'll hold yours" friend backup](cloud-backup.md#friend-hosted-backup-you-hold-mine-ill-hold-yours)
works. Promoting a backup guest to a full member later is, again, one tier
change.

## Members run their own accounts

Admin powers are about membership and resources. Members manage everything
else themselves: their own devices, their own mail settings and aliases,
their own shares and backups. You get a read-only indicator of things like
whether a member's mailbox is served from your box — visibility, not control.
The flip side: **you can't recover what you can't read.** If a member loses their identity secret from all their devices, you cannot reset it or restore their encrypted data. Their own recovery kit, if they saved one, is what gets their account back — see [Your identity, devices, and account recovery](identity-and-devices.md). Make "keep your secret in two places, and save your recovery kit" the house rule when you invite people.

## When someone has to go

Removing an account is deliberately a *timed* process, not a trapdoor:

1. **Evict** starts a ladder: first a **warning** phase, during which the
   member keeps full access — this is their window to export everything —
   then suspension, then deletion.
2. **Cancel Eviction** stops the process at any point until the deletion starts: same account, same keys, same data, no re-onboarding.

The ladder also won't start on a child's guardian while the guardianship stands. On their Family page, the guardian can hand it to someone else with **Propose new guardian** (once that person accepts), or end it with **Graduate to full account**; after that the eviction can go ahead.

The member is told. For the whole warning phase, their app shows a warning banner
on every page that can't be dismissed. It says when their access ends, when their account and data will be deleted, the reason you gave, and that they can export everything from **Settings → Account → Export My Data** until access ends. To take their mailbox to another mail service in a standard format, they also use **Export mailbox**, among the mail pages in Settings. If you cancel the eviction, the banner goes away on its own the next time their app checks.

An immediate **suspend** button — instant cut-off with no deletion timeline,
for "stop this right now" situations — sits right beside evict on every
member's row, in every app.

What the cut-off member sees: their app stops signing in and says so plainly
("This nest no longer signs you in…"), with a **Retry** button. If they were
using the app at the moment you cut them off, it switches to that message
right away — no restart needed. So once you lift the cut-off with **Cancel Eviction**, one tap puts them back where they were, same account, same data. The app does not say *why*, and neither does the nest: a suspended and
a removed account read the same from the outside, on purpose.

## Moderation, honestly

Fauna's moderation model will surprise admins coming from forum software: **there is no admin console for policing the feed item by item, on purpose.** What members see is governed first by their *own* filters — content labels produce badges, and each member chooses their own thresholds for hiding things in their own view — plus any rules you publish for the whole nest, which add to those filters and never loosen them.
You, as admin, don't decide what the household reads item by item: beyond those rules (see **Nest rules**, below), the most you can do by hand to a single item is withhold it — one someone reported, or one the law obliges you to remove — and never silently.

The real admin-side levers:

- **Mail perimeter policy.** Spam thresholds — what gets delivered, junked,
  or rejected at the door — are admin policy, set on the admin
  Mail page. A message filed to Junk stays visible to its recipient in their Junk folder. A filing has no appeal, as in any mail system: the member's lever is their own spam filter, which learns from each message they mark as spam and, in a mail app they sign in with, from each message they move out of Junk.
- **Reports and the block.** Members, and users of other nests, can report a post, message or account; reports land on the admin Nest page and remove nothing by themselves. For a reported post or message you can **Block…** that one item: everyone else your nest serves sees a notice naming the report's reason in its place, the author keeps seeing it — apart from its photos and files, which are withheld from everyone, and, for an author on another nest, a message their devices did not already have — and, for a post, can appeal, and a permanent record is kept. Nothing is deleted, **Unblock…** serves it again exactly as it was, and you can block only what someone reported — to block anything else, report it yourself first. For a reported account, suspension and eviction are on the Users page.
- **Nest rules.** Rules you publish on the admin Nest page — a category of content, keywords or a labeler, each set to block or to hide behind a warning, with a reason in your own words — which each member's own app applies to what reaches them through the nest. You never see what a rule blocks, a rule never singles out one item, and wherever a rule applies the member sees a notice naming the nest and your reason. See [A tour of the admin area](admin-tour.md).
- **Legal takedown.** The strongest of these, and the one for content you are legally compelled to remove. It requires a legal reference,
  is visible to the author and leaves an audited tombstone — it is a
  compliance tool, not an opinion lever. If the post had already been published onward to an outside network your nest posts to, the takedown withdraws it there too, where that network allows: a post your nest published to Bluesky — under an identity it hosts, or through a member's linked Bluesky account — is deleted; on Nostr your nest publishes a signed deletion request, which each relay decides for itself whether to honor; and a post already delivered to fediverse servers cannot be recalled. Copies others already fetched may persist beyond your nest's reach. Overturning
  a takedown restores the post on your nest, but deliberately does *not*
  re-publish it to those outside networks — putting it back out there is the
  author's call to make, not something an overturn does silently. (The author of a post can appeal from their **Moderation** page (in the web app, the **Content Moderation** section of Settings → **Privacy**); an appeal is recorded for an admin to review, never granted on the spot.)

## Kids on the nest

Fauna's design for children is **supervised accounts**: a real, full account
carrying a guardianship link to a parent's account — guardianship is a
relationship between two accounts, deliberately *not* an admin power (you
pick the guardian when admitting the child, and that's where your role ends).
The guardian controls who can reach the child, can set content rules — by category, keywords or a labeler — that block flagged posts and messages or hide them behind a warning on the child's own device, and can set usable hours and a daily time budget that lock Fauna on the child's devices outside those hours or once the budget is spent. The model,
its transparency rules, and its current honest status are covered in
[Sharing files and running a family nest § Parental controls](cloud-sharing-family.md#parental-controls).
The short version: the guardian link, reach controls, content rules and screen-time rules are built and live on the Family page.

## Keeping everyone's data safe

Three separate layers, from "automatic" to "yours to set up":

- **The nest backs up its own brain, automatically.** The nest keeps rolling
  restorable copies of its database — hourly exact copies and daily portable
  dumps — with no setup. This protects the box's own state (accounts, tiers,
  settings, the social graph) against corruption.
- **Members' snapshots are theirs.** Point-in-time backup of files is a per-member feature ([the backup guide](cloud-backup.md)), encrypted with their own keys — apart from a folder a member has made public, which is public by design. You provide the disk; you can't delete their snapshots, or read them beyond what a member has made public or let this nest read (see above) — and members' own "delete immediately" levers aren't
  yours to pull.
- **Off-box protection is the part to take seriously.** A member (you
  included) can add a **backup destination** — another nest, a friend's box —
  and replicate their encrypted data off your machine. Encourage it: the box's automatic self-backup stays on the same disk, while each member's backup destination copies their mail, calendar, contacts and posts, and any folder they attach to it, off your machine automatically. Anything a destination does not carry — and all the data of a member without one — depends on your disk, so **a nest that loses its disk can still lose members' data** unless the underlying disk has its own redundancy or backup. Treat host-level disk backup (RAID, VPS snapshots, disk images) as part of your job as the host.

## Where this stands today

| Feature | Status |
|---|---|
| Admin area in the app: dashboard, users, tiers, invites | **Available** on all seven apps |
| Who may create an account (anyone, invite code only, or nobody but you); invite codes (per-tier, limited-use), join-request approval and direct admission | **Available** |
| Tiers as quotas, enforced cleanly (storage, inbox, devices, message size) | **Available** |
| Storage-only backup tier for friend-hosting | **Available** |
| Multiple admins; last-admin and admin-deletion protection | **Available** |
| Eviction ladder (warning/export window → suspension → deletion) + cancel | **Available** |
| Immediate suspend button | **Available** |
| Member privacy from the admin (content stored encrypted on your nest, apart from what a member publishes to everyone; of their content you read only what a member publishes to everyone or lets this nest read, a community room whose members chose to let its home nest read it, the mail and messages on their way to and from email and other networks, and the text of a message or a restricted post that someone who can open it attaches to a report; and, as any server that delivers messages must, who each member is connected to and when and how much they send, and the address of a link a member's app asks it to preview) | **Available** |
| User-side moderation (labels, badges, per-user thresholds, training) | **Available** |
| Admin mail perimeter policy (spam thresholds) | **Available** |
| Legal takedown (audited tombstone) | **Available** — the author of a post can file an appeal from their **Moderation** page (in the web app, the **Content Moderation** section of Settings → **Privacy**); it is recorded for an admin to review |
| Reports, and blocking a reported item | **Available** — the author of a blocked post can appeal from their **Moderation** page (in the web app, the **Content Moderation** section of Settings → **Privacy**) |
| Nest rules (filters you publish, applied on members' devices) | **Available** |
| Supervised accounts for kids | **Available** — guardian link, reach controls, content rules, and screen-time hours and daily budgets are built and live on the Family page |
| Automatic nest database self-backup (hourly + daily) | **Available** |
| Per-member snapshots & backup destinations | **Available** — your nest does the uploading itself, so it works the same from every app, including the web app, and keeps going with all of them closed |
| Off-box replication of mail, calendar, contacts, posts and attached folders | **Available** through each member's backup destination — keep host-level disk backups for everything else |
