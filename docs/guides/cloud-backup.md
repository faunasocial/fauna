# Backup with Fauna — your own, and with friends

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
exactly what is available now, what is not yet, and what is planned.
>
> Part of the [Your own cloud](your-own-cloud.md) series.

---

Backup in Fauna answers three separate questions, and it helps to keep them
apart:

1. **"I deleted / mangled a file last Tuesday"** → point-in-time **snapshots**
   kept on your nest, per folder, with retention you set.
2. **"My nest's disk died"** → **backup destinations**: your nest's data, encrypted (apart from a folder you have made public), mirrored to *another* location — a second nest you control, a friend's, or one of your own devices.
3. **"I lost my devices"** → your **key**, and the **recovery kit** that stands behind it. No backup can stand in for them, so they get their own section below.

## Snapshots: the time machine on your nest

Every folder can have **point-in-time snapshots** kept on your nest — you don't
create a special kind of folder for it. Make a folder (or point one at a folder
that already matters — see [photo backup](cloud-photos.md) for the camera-roll
case), open its row in Settings → Folders, and set how your nest should keep it.

- **Automatic.** A snapshot is taken shortly after changes settle (about a
  minute), plus whenever you press the button.
- **You decide what your nest keeps, per folder.** Its row has "Keep snapshots"
  — *use the default*, *keep*, or *don't keep* — a quiet period (how long the
  folder must be still before a snapshot is cut), and how many snapshots and how
  many days to keep. **Leaving a box empty is itself a choice**: it means "use
  the default", which is how you take a setting back once you've set one. All of
  them save together.
- **File-version history has its own pair of bounds**, on the same row: "Keep at
  most (versions per file)" and "Keep versions for at most (days)". These bound
  the per-file save history (every save is a version — see the
  [file sync FAQ](file-sync-faq.md)), never your snapshots; empty boxes keep
  every version forever, which is also the starting state of every folder. A
  bound never touches the current file or your most recent older version, and
  pruning is gentle: versions going out of bounds wait seven days (you can
  cancel), then stay recoverable for thirty more — in a file's version list,
  switch on "Show recently pruned" and press *Recover* to bring one back.
- **Deletes need not follow you.** Whether a device applies deletions that
  happened elsewhere is a per-device choice on the folder — leave it off and that
  device keeps every file it has ever seen. That is what an archive is, and it no
  longer requires a whole separate kind of folder.
- **A richer schedule** — "keep hourly for a day, daily for a month, monthly for a year", the pattern borg/restic users know — is planned.
- **Browsable and verifiable.** The Backups page lists every snapshot with when
  it was taken, how many files it holds and how big it is; open one to see the
  files inside. Running the integrity check marks the snapshots it found a
  problem in, so a bad one is obvious in the list rather than buried in a
  report — and a check that finishes with findings is shown as a *result*, not
  as an error.
- **Tidying up is a preview, then a confirmation.** "Apply retention policy"
  first shows you exactly which snapshots would go and how many would remain,
  and only then offers to delete them. If the folder has no retention policy yet, it says so and points you at the folder settings where you set one, instead of quietly doing nothing.
- **Snapshots on the way out say so.** One you have deleted shows the deadline
  it can still be cancelled before, and one already in its recovery window
  shows how long it stays recoverable — so the month of regret-room below is
  visible on the row, not just a promise in this guide.
- **Restore** a single file straight back — open a snapshot, find the file in its list, press the download button next to it, and pick where to save it — or, on a computer, restore a whole snapshot into a folder you choose (the terminal app takes the folder's path); phones and the web app have no folder tree to write a whole snapshot into, so there you download files one at a time. When it finishes, the page tells you how many files it wrote and where, names any file it could not restore, and says everything else was written. Either way the files are decrypted on your device as they land. If a download cannot go ahead, the page says why rather than looking like it worked.

### Deleting backups is deliberately hard

Ransomware's favorite move is deleting the backups first. Fauna's deletion
path is built against it:

- Deleting a snapshot starts a **48-hour cancellable countdown**, then a
  **30-day soft-delete**, then the purge — about a month of regret-room end to
  end. A snapshot in that 30-day window stays in the list, labelled with the
  date it stops being recoverable, and carries a **Recover** button that brings
  it straight back. Snapshots the retention policy prunes on its own wait seven days (you can cancel), then enter the same 30-day recovery window; ones you prune with **Apply retention policy** go straight into that window. Either way, an over-eager retention setting is undoable too.
- The nest **refuses to drop below the last 3 snapshots** of a folder, ever.
- The owner-only "delete immediately" override exists (for the day you really
  mean it), but it demands re-typing the snapshot ID and an acknowledgement
  sentence — never one click.

## Backup destinations: surviving the loss of the nest itself

Snapshots on the nest don't help if the nest's disk dies. A **backup destination** is another location your data is continuously mirrored to, encrypted apart from a folder you have made public. Add one on the Backups page with just a URL; from then on the
uploads run in the background on their own schedule, and each destination row
shows when it last synced and how much is still queued.

What travels is **encrypted**: your mail, calendar, contacts and posts with a backup key derived from your identity, which you grant your own nest so it can keep the uploads going on its own, and any folder you attach exactly as it is already stored, encrypted — a nest destination stores blocks it cannot open, ever, apart from a folder you have made public, which is public by design.

A destination keeps your account's backup from **one box at a time**. If you
try to add a destination that already keeps your backup from another of your
boxes, the add stops and says so, and nothing is added. And if a destination
stops accepting this box — you froze its backup there from the Nests page, or
it keeps your backup from another box now — its row says **Not accepted**
instead of showing a queue, so a destination that turns your box away never
looks like one that is merely busy. Once the destination accepts this box
again, the row goes back to showing what is queued.

Remove a destination and your nest tells it so on its next pass, and the space
your backup was using there is released. That release is deliberately not
instant: for about a month afterwards the destination still holds what it had,
so a removal you regret — or one you didn't make — is recoverable rather than
final. Nothing about it needs your attention; it happens whether or not the app
is open.

### After you take an account back, the list asks you to look at it once

If you have used your recovery kit to take an account back — because someone
got hold of your identity — your backups restart on their own the moment you
sign in as the recovered account. Nothing is switched off while you sort things
out, and you don't have to re-add anything.

There is one thing only you can decide, though. The list of destinations was
stored under the *old* identity, so anyone who held it could have added one —
and a destination they added looks exactly like one you added. Your app can't
tell them apart, so it doesn't guess: it keeps every destination running and
marks each one for you to look at, with **Keep** next to the usual **Remove**.
Keep it if you recognise it; remove it if you don't.

Three things worth knowing. Most of the time every row on that list will be
yours — the mark means "not yet checked", never "something is wrong". Your
answer travels with you: keep or remove a destination on your laptop and your
phone agrees the next time it syncs, even if it was still showing the unanswered
list — you only work through the list once, on whichever device you happen to
have. And a *Keep* only settles the recovery that raised it: if you ever have to
take the account back a second time, the same destinations are raised again,
because your answer the first time can't vouch for what happened since.

### Your app checks the backup for itself

A backup you never verify is a hope, not a backup — and the row above only
reports what your *nest* says about its own uploads. So your app runs its own
check, on its own schedule, talking **directly to each destination** rather than
taking the nest's word for anything. Each destination row shows when that check
last passed ("Last checked: …"), so a healthy page gives you positive evidence
it is running — not just silence.

If a destination falls behind your actual data, stops accepting backups, or
simply can't be confirmed for a week, a banner appears at the top of the Backups
page naming which destination and why. That is the whole point: you find out
because the app went and looked, not because you eventually needed the backup.

Nothing is checked *by* being restored, and nothing is repaired automatically —
the check reports, and the decision stays yours. A destination that's merely
offline right now stays quiet, because a laptop on a plane isn't a data-loss
emergency; only sustained silence escalates to a warning.

**A device holding a copy checks itself.** When the destination is one of your
own devices, your app can't go and look: the device is often asleep, and it has
no address to knock on. So that device checks its own copy instead — opening a
sample of what it holds and verifying it — and reports the result back. Its row
says "Self-checked: …" rather than "Last checked: …", so you can always tell
which kind of assurance you're reading: one your app confirmed for itself, or one
the device told you. A device that hasn't got round to its first check yet says
so plainly rather than claiming to be fine. And if a device reports that its own
copy failed the check, you get the same loud banner at the top of the page — a
copy that can't verify itself is one you shouldn't count on.

**When your nest goes back in time, the backup says so.** If your nest is ever
restored from an older copy of its data — a snapshot, a disk image, an older
backup — it has quietly lost everything written since that copy was made. A
destination that was keeping up still holds it. Left alone, your nest would
carry on and reuse the space those newer pieces sat in, overwriting them one by
one. Your app notices the moment it happens (your nest confirms it is the one
that went backwards, so the destination is not blamed), tells your nest to stop
reusing that space, and shows a banner at the top of the Backups page naming
the destination. Whatever was not yet overwritten is then kept for as long as
you keep the destination, and the banner says it is held until it is recovered.
Pieces that were overwritten before your app noticed are kept only for the
destination's usual grace period, and while any of those remain the banner
tells you roughly how many days are left. The banner goes away by itself once
the copy holds nothing your nest lacks. Until then, the copy is the one place
the lost data still exists — if whoever restored your nest has a newer copy,
now is the time to ask for it.

**Getting it back.** When the destination is another nest, the banner carries
a **Recover what your nest lost** button. After a plain confirm, your app copies
everything the backup holds that your nest no longer has back into your nest —
as new items, each where it was: a lost message returns to its folder with its
flags, a lost post, event or contact comes back as it was. Nothing is deleted on
either side, and anything you changed or deleted since the rollback stays the
way you left it — a message you deleted afterwards is not brought back. The
result lists what came back for each kind of data, and anything that could not
be put back (an event or contact with no record of where it belonged). Running
it again is always safe: it only fetches what is still missing and never brings
anything back twice. Once it is complete the banner goes away. Mail apps see
the recovered messages as new ones.

One of your own devices keeping a copy says so too. A device you set up as a
backup destination fetches from your nest by itself, and if it finds your nest
has gone backwards it stops fetching rather than throw away what it holds — for
a nest rebuilt after a total loss, that device may hold the only copy left. The
same banner then appears on that device's Backups page, naming the device and
saying its copy is held until it is recovered. The device then carries on
backing up whatever your nest writes from now on, while keeping everything your
nest lost. On that device the banner carries the same **Recover what your nest
lost** button, with the same confirm and the same result: the device copies what
your nest no longer has back into it from its own copy, each item where it was.
Only the device that holds the copy offers the button — another of your devices
shows the banner without it. The banner goes away once the recovery is complete,
or by itself if your nest gets everything back some other way.

Three kinds of destination today, and one more planned:

- **A second nest you control** — if you run a nest at home with an internet nest
  in front of it, that internet nest is the obvious one: already yours, already
  on, already paid for. Otherwise a box at your parents', an office machine, a
  second small VPS. Off-site in the real sense.
- **A friend's nest** — the interesting one, next section.
- **One of your own devices** — a laptop or tablet with room to spare can hold a copy of everything a destination keeps itself, encrypted apart from a folder you have made public. See just below.
- **S3-compatible object storage** (any provider) is a planned destination kind, for people who want a commercial deep-archive as the last-resort copy.

**Your own device can be a destination, and it is the only kind that can
restore with no nest alive anywhere.** Pick **This device** when you add a destination, set a storage limit, and that device quietly pulls a copy of everything a destination keeps — your mail, calendar, contacts and posts, and any folder you attach, encrypted apart from a folder you have made public — whenever it is awake. Two things to know before you
lean on it:

- **A device copy is not off-site.** Devices get lost, wiped, stolen and
  replaced. If every destination you have is one of your own devices, the app
  says so with a standing warning — that warning is asking you to add a nest
  destination as well, not instead.
- **It is an offline copy of everything it holds.** Anyone who can unlock that device can read
  all of it, not just what is on screen. That is a genuinely different
  exposure from an ordinary signed-in device, and it is the trade you are making
  for a copy that survives your nest.

**Removing that destination does not delete the copy on the device**, and the
reason is the whole point of the kind: it is the copy that still works when
nothing else does, so it isn't thrown away as a side effect of tidying a list.
The remove dialog offers **Also delete this device's copy now** if that is what
you meant. If you didn't tick it, the device keeps the copy and says so on the
Backups page — a row telling you how much space it is using, with **Free up
this space** next to it. Deleting the copy asks you to confirm once, because
what you lose is the ability to restore from that device with no nest reachable;
the space comes back, and making the device a destination again rebuilds the
copy from scratch.

Note that *holding* a copy and *syncing* a folder are different things. A
folder whose devices all apply deletions gives every device a live copy, which
is useful redundancy but is not a backup: delete a file on one device and it
disappears from all of them. Avoiding exactly that is what your nest's snapshots
— and a device with **Applies deletions** switched off — are for, along with
backup destinations.

**Using your internet nest as the destination is worth spelling out**, because
it sounds like it shouldn't work. Linked as [the relay guide](nest-relay-setup.md) shows, that nest keeps no readable copy of the mail you receive — that's the point of the relay design. Adding it as a backup destination is a separate, deliberate choice, and it doesn't weaken that: what lands there is encrypted with a key it does not have (apart from any folder you have made public), so it stores your backup blind, exactly like a friend's nest would. The one thing to check is
disk — a front-door nest is usually provisioned small.

**Your folders can ride along too.** Out of the box a destination protects your mail, calendar, contacts and posts. An ordinary folder (a photo library, a documents folder) joins only
when you ask it to: open Settings → Folders, expand the folder, and under
**Destination places** attach one of your enrolled destinations. From then on your nest keeps that folder's current contents mirrored at the destination — still encrypted, so a nest destination cannot read it (a folder you have made public stays public there too) — and **Detach** stops the mirroring again. It's per-folder on purpose: folders can be large, and which
ones deserve an offsite copy is your call, folder by folder.

## Friend-hosted backup: "you hold mine, I'll hold yours"

This is a feature no commercial cloud offers, and it falls out of Fauna's
design naturally: because backups are encrypted — apart from a folder you have
made public, which is public by design — **anyone with spare disk can hold them
for you, including someone you'd never show the files themselves.**

How it works, both directions:

- **A friend stores your backup.** Your friend (the admin of their own nest)
  admits you as a **storage-only guest**: an account that can hold backup
  bytes up to a quota they choose — and do nothing else: no mailbox, no feeds,
  no logging into their apps. You add their nest as a backup destination, and your backup flows over, encrypted apart from a folder you have made public. Two households doing this for each other
  get true off-site backup for the one-time price of some extra disk.
- **You store a friend's.** The same picture from the admin chair: create a
  storage-only guest for them, set the quota ("you get 200 GB"), done. To stop hosting, **Evict** the guest. Nothing goes at once: the account is closed in stages, and about four weeks later the backup you held for them is deleted from your nest — until then, **Cancel Eviction** undoes it. Their own data is unaffected (they still have their originals and any other destinations).

**What the friend can and cannot see** — stated honestly, because this is the
whole point:

- **Cannot, ever:** read any file's contents, see your file or folder names, restore your data, or decrypt anything — no grant they could hold would change that, because everything you send them is encrypted before it reaches their nest, with keys their nest never has — except a folder you have made public, which is public by design. Your file names, folder structure, and the rest of the catalogue travel *inside* the encrypted backup, as unreadable to your friend as the file contents themselves.
- **Can see:** that it's you, how much space you use, and the minimal bookkeeping a storage service needs to keep your backup alive — how many blocks you store, how big each one is, and when it last changed. The blocks holding your mail, calendar, contacts and posts carry generated names (your account identifier plus a running number), and an attached folder's file names stay encrypted — except in a folder you have made public, whose names are public by design.

Your own nest — the one your devices sync with day to day — keeps your file names and folder structure too, encrypted with your key, and your apps decrypt them to show you pages like Media — unless you choose otherwise: a folder you make public has public names by design, and a folder you serve over WebDAV or put behind a paywall is readable by the box that serves it. A nest you back up to receives them only inside the encrypted backup, where it cannot read them (a public folder's names are public anyway), whether it's a friend's nest or another of your own.

## Restoring after disaster

If your nest dies, the plan is: stand up a fresh nest (same guides you used
the first time) — or rebuild the lost one with its own identity, which a
copy held by a nest you back up to needs — sign in to it with your key, and bring your
data back from the copy a destination holds.

**From a copy on one of your own devices**, it is one button. Sign in on the
device that holds the copy and open Backups. The device's copy shows up there,
either as its own destination row or as a row saying how much the device is
holding, and next to it is **Restore my data to this nest**. Confirm once and
the device sends its copy to the new nest, which makes it live again. Your
mail comes back into the folders it was in, with its read and flagged marks.
Nothing is deleted along the way,
either on the device or on the nest. If the nest already holds your data, it
stops rather than mixing the two. When the copy has arrived in full, the device
signs up again as that nest's backup, so you keep an offline copy from then on.

**From a copy a nest you back up to holds**, it is the same button, once your lost nest is rebuilt with its own identity (**Recover a lost box** in the app does this — for a server you set up by hand, it gives you the line its setup guide's *Rebuilding after a total loss* uses). Sign in to it and open Backups: the nest you back up to is still listed, and next to it is **Restore my data to this nest**. Confirm once and the app copies that nest's backup onto the rebuilt nest and makes it live again, the mail back in the folders it was in. Nothing is deleted on either nest. When the copy has arrived in full, that nest goes back to backing up the rebuilt one. (Copies of your account that a friend's nest holds for you under Settings → Nests come back another way, onto a new nest too: see [If you lose every device and your nest](identity-and-devices.md#if-you-lose-every-device-and-your-nest).)

If something is missing, for example the copy ran into the new nest's storage
limit or part of it had not finished backing up, the Backups page lists what
did not come back and what to do about it. Running the restore again picks up
where it left off. That holds even if the restore was cut off halfway, say the
nest restarted, and new mail arrived before you ran it again: the restore
finishes, and the new mail stays in the folder it arrived in, beside the mail
that came back.

One honest note on today's state. A destination receives your mail, calendar, contacts and posts, and any folder you attach to it — a photo library included; conversations are not copied to a destination yet. A restore brings back what the copy holds. (Snapshots on your own nest cover your files and photos; this note is about the *off-site* copy.)

## The key that matters more than the backup

Every encrypted byte in this guide is opened by keys derived from your one
identity key. That leads to the only rule in this series worth printing out:

> **A backup of everything + a lost key and no recovery kit = a backup of nothing.** > Keep your key on at least two devices, and keep your recovery kit > somewhere a house fire doesn't take along with the devices.

There is deliberately no back door here: no "forgot password" flow, no support desk that can be talked into resetting your account — those would be exactly the holes attackers use. What you have instead is your **recovery kit**, which only you hold: kept offline, it gets your account back if you lose every device, and takes it back if someone steals your key. See [Your recovery kit](identity-and-devices.md#your-recovery-kit).

## Where this stands today

| Feature | Status |
|---|---|
| Per-folder snapshots on your nest, automatic + manual | **Available** |
| Retention (per folder: count + age) | **Available**; richer keep-hourly/daily/monthly schedule **planned** |
| Snapshot browse, integrity check, prune | **Available** |
| Single-file restore; full-snapshot restore into a folder on computers | **Available** |
| Delete protection (48 h + 30-day soft-delete + 3-snapshot floor + friction on immediate delete) | **Available** |
| Backup destinations (add/edit/remove + status rows, across the apps) | **Available** |
| Your app's own check of each destination ("Last checked", plus a banner when one falls behind) | **Available** |
| Continuous encrypted upload to destinations | **Available** — your nest keeps the uploads flowing on its own, even with every app closed; mail, calendar, contacts, posts and any folder you attach go through the pipe — encrypted, apart from a folder you have made public (see the mirroring row below) |
| Friend-hosted storage (storage-only guest + quota; unreadable by the host, apart from a folder you have made public) | **Available** |
| Restore onto a fresh nest from a copy on your own device | **Available**: **Restore my data to this nest** on the Backups page brings your mail, calendar, contacts and posts, and any folder you attached, back onto the nest, the mail into the folders it was in |
| Restore onto your rebuilt nest from a copy held by a nest you back up to | **Available**: **Restore my data to this nest** on that nest's row brings the copy back |
| Snapshot coverage on your own nest: files/photos | **Available**; mail, calendar, contacts, posts and conversations **not yet** |
| Destination mirroring, by data type | Mail, calendar, contacts, posts and any folder you attach (photos included) **available**; conversations **not yet** |
| One of your own devices as a destination | **Available** on computers and phones — a device enrolled as a destination pulls and keeps a real copy, encrypted apart from a folder you have made public (a web browser cannot hold one) |
| S3-compatible destinations | **Planned** |
