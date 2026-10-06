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
> only account on it. The *file-sharing* side of a shared nest — shared file
> sets, quotas as disk space, friend-hosted backups — is covered in
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
- **You administer capacity, not content.** Members' files, messages, and
  mail are sealed with *their* keys; the nest — and therefore you — stores
  ciphertext. You can see how much space someone uses, not what's in it. You
  can evict an account; you cannot read it. That holds on every nest,
  unconditionally — there's no setup choice that changes it. What a member's
  own box gets to actually process is up to that member: they mint the
  specific, revocable grants for it themselves, from their own Settings →
  Nests.

## Letting people in

Nobody joins your nest without you. There are two doors, both in the admin
**Users** page, and in both cases your real decision is the same: which
**tier** (capacity level) the new account gets.

- **Invite codes.** Mint a code, choose the tier it grants and how many times
  it may be used, and hand it over however you like — chat, e-mail, paper.
  The person types it during sign-up ([Getting started](getting-started.md)
  § Step 3a) and is in immediately.
- **Join requests.** Someone who tries to sign up at your domain without a
  code can submit a request instead. It appears in your pending list with
  their requested handle and message; you pick a tier and approve, or decline
  with an optional reason. Their app shows the decision as soon as you make
  it — requests survive them closing the app.

(How you became the admin in the first place — claiming a fresh nest with its
one-time claim code — is in [Getting started](getting-started.md) § Step 3b.)

## Tiers: how you share the disk

Every account has a tier; the tier is the quota. A tier caps storage, mailbox
size, device count, maximum file size, and feed count. A new nest seeds a
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
The flip side: **you can't recover what you can't read.** If a member loses
their identity key from all their devices, you cannot reset or restore their
encrypted data — see
[Your identity, devices, and account recovery](identity-and-devices.md).
Make "keep your key in two places" the house rule when you invite people.

## When someone has to go

Removing an account is deliberately a *timed* process, not a trapdoor:

1. **Evict** starts a ladder: first a **warning** phase, during which the
   member keeps full access — this is their window to export everything —
   then suspension, then deletion.
2. **Restore** cancels the process at any point before the end: same account,
   same keys, same data, no re-onboarding.

An immediate **suspend** button — instant cut-off with no deletion timeline,
for "stop this right now" situations — sits right beside evict on every
member's row, in every app.

What the cut-off member sees: their app stops signing in and says so plainly
("This nest no longer signs you in…"), with a **Retry** button. If they were
using the app at the moment you cut them off, it switches to that message
right away — no restart needed. So once you
restore them, one tap puts them back where they were, same account, same
data. The app does not say *why*, and neither does the nest: a suspended and
a removed account read the same from the outside, on purpose.

## Moderation, honestly

Fauna's moderation model will surprise admins coming from forum software:
**there is no admin console for policing the feed, on purpose.** What members
see is governed by their *own* filters — content labels produce badges, and
each member chooses their own thresholds for hiding things in their own view.
You, as admin, don't decide what the household reads.

The two real admin-side levers:

- **Mail perimeter policy.** Spam thresholds — what gets delivered, junked,
  or rejected at the door — are admin policy, set on the admin
  Mail page. A filed message stays visible to its recipient in their Junk
  folder; a dedicated in-app appeal flow for a filing isn't built yet.
- **Legal takedown.** The one nest-wide removal power that exists is for
  content you are legally compelled to remove. It requires a legal reference,
  is visible to the author and leaves an audited tombstone — it is a
  compliance tool, not an opinion lever. If the post had already been
  published onward to an outside network your nest posts to, the takedown
  withdraws it there as well, rather than only hiding it locally — on
  networks where each server decides for itself whether to honor removals,
  that means publishing a signed withdrawal request, and copies others
  already fetched may persist beyond your nest's reach. Overturning
  a takedown restores the post on your nest, but deliberately does *not*
  re-publish it to those outside networks — putting it back out there is the
  author's call to make, not something an overturn does silently. (An appeal
  mechanism exists server-side; the app screen to use it is still landing.)

## Kids on the nest

Fauna's design for children is **supervised accounts**: a real, full account
carrying a guardianship link to a parent's account — guardianship is a
relationship between two accounts, deliberately *not* an admin power (you
pick the guardian when admitting the child, and that's where your role ends).
The guardian controls who can reach the child, can set per-category
content filters that collapse or hide flagged posts and messages on the
child's own device, and can set usable hours and a daily time budget that
lock the child's own device outside them. The model,
its transparency rules, and its current honest status are covered in
[Sharing files and running a family nest § Parental controls](cloud-sharing-family.md#parental-controls).
The short version as of mid-2026: the guardian link, reach controls, and
per-category content filters are built and live on the Family page in every
app; screen-time rules are rolling out.

## Keeping everyone's data safe

Three separate layers, from "automatic" to "yours to set up":

- **The nest backs up its own brain, automatically.** The nest keeps rolling
  restorable copies of its database — hourly exact copies and daily portable
  dumps — with no setup. This protects the box's own state (accounts, tiers,
  settings, the social graph) against corruption.
- **Members' snapshots are theirs.** Point-in-time backup of files and
  messages is a per-member feature ([the backup guide](cloud-backup.md)),
  sealed with their keys. You provide the disk; you can't read or delete
  their snapshots — and members' own "delete immediately" levers aren't
  yours to pull.
- **Off-box protection is the part to take seriously.** A member (you
  included) can add a **backup destination** — another nest, a friend's box —
  and replicate their encrypted data off your machine. Encourage it: as of
  mid-2026 the box's automatic self-backup does *not* yet extend to
  replicating mail and calendar data off-box, so **a nest that loses its disk
  can still lose mail** unless the underlying disk has its own redundancy or
  backup. Until that pipeline closes, treat host-level disk backup (RAID,
  VPS snapshots, disk images) as part of your job as the host.

## Where this stands today

| Feature | Status |
|---|---|
| Admin area in the app: dashboard, users, tiers, invites | **Available** on all seven apps |
| Invite codes (per-tier, limited-use) and join-request approval | **Available** |
| Tiers as quotas, enforced cleanly (storage, mailbox, devices, file size) | **Available** |
| Storage-only backup tier for friend-hosting | **Available** |
| Multiple admins; last-admin and admin-deletion protection | **Available** |
| Eviction ladder (warning/export window → suspension → deletion) + restore | **Available** |
| Immediate suspend button | **Available** |
| Member privacy from the admin (content sealed at rest, no admin read access) | **Available** |
| User-side moderation (labels, badges, per-user thresholds, training) | **Available** |
| Admin mail perimeter policy (spam thresholds) | **Available** |
| Legal takedown (audited tombstone) | **Available** — appeal is server-side only; no app screen to file one yet |
| Supervised accounts for kids | **Available** — guardian link, reach controls, and per-category content filters are built and live on the Family page in every app; screen-time hours and daily budgets are rolling out, live on some apps and landing on the rest |
| Automatic nest database self-backup (hourly + daily) | **Available** |
| Per-member snapshots & backup destinations | **Available** — your nest does the uploading itself, so it works the same from every app, including the web app, and keeps going with all of them closed |
| Off-box replication of mail & calendar data | **Planned** — until it lands, keep host-level disk backups |
