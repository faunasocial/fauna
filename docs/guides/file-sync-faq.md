# File sync FAQ — working together in shared folders

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
> exactly what is available now, what is landing, and what is planned.
>
> Part of the [Your own cloud](your-own-cloud.md) series. For the basics of
> folders see [file sync](cloud-sync.md); for sharing and roles see
> [sharing files](cloud-sharing-family.md).

---

## Can two people work in one shared folder straight from Explorer and Finder — without opening the Fauna app?

Yes — that's the intended everyday experience. Fauna shows up as a cloud
location in Windows Explorer and macOS Finder (the same way OneDrive and
iCloud Drive do), and Word, Excel, or any other program reads and writes those
folders directly.

You need the Fauna app itself only at the edges: accepting a share the first
time, choosing which location on your machine carries it, browsing version
history, and changing settings. The day-to-day open/edit/save loop is pure
Explorer/Finder.

## Does sharing work when the other person's account lives on a different nest?

Yes. You share to their handle exactly as you would with someone on your own
nest; because their account lives elsewhere, the share always arrives as an
invitation they confirm before anything happens (a safety choice — nobody can
silently attach a set to your machine). After they accept, their files come
to them through their own nest, end-to-end encrypted; neither nest can read
the content. Give them **Writer** access and they can bind the set to a
location of their own and edit; see the status table for where write access
across nests stands today.

## My collaborator saved a file four times before I opened it. Do I get all four saves?

You get the newest one — and the other three are not lost. Every save becomes
a **version**; your machine always materializes the latest, and the full
history (with who made each version) is in the folder's version list, where
any older version can be restored with one tap. Restoring never deletes
anything either — it just makes an older version current again.

By default every version is kept forever. If a folder churns a lot, you can
bound its history in the folder's row ("Keep at most (versions per file)" /
"Keep versions for at most (days)" — see [backups](cloud-backup.md) for how
the bounds behave); versions that go out of bounds are pruned gently, and a
recently pruned version can still be brought back from the version list via
"Show recently pruned" for thirty days.

## We're both working and saving frequently. Do our changes actually flow?

Continuously, in both directions. When you save, your machine uploads within
seconds, and if you and your collaborator are on the same nest their device
is nudged the moment your save lands — in practice their copy updates in
well under a couple of seconds, no waiting for the next scan. Sharing across
two different nests doesn't get that instant nudge yet: there, learning
about the other person's saves happens on the periodic catch-up, which runs
every few minutes on every device. Either way, that catch-up remains the
safety net that also picks up anything missed while a device was asleep or
offline — there is nothing to configure; it is the same on every folder.

## What happens if we both edit the same file at the same time?

Nothing blocks, nothing is lost, and you never get "conflicted copy" litter:

- **Text files** (code, notes, CSV, anything line-based): Fauna performs a
  real merge. If your edits don't overlap — you changed the top, they changed
  the bottom — **both survive** in the merged result. Genuinely overlapping
  edits fall back to "latest save wins", and so does a very large file: past a
  few thousand lines, comparing two versions line by line can take minutes,
  and Fauna takes the latest save rather than freeze your sync waiting for it.
  (If both sides only *added* lines — the usual shape for a long notes file or
  a log — both sets of additions still survive at any size, because that case
  needs no comparison.) In every fallback the other version is kept in
  version history.
- **Binary files** (Word, Excel, images, archives): the latest save wins,
  whole-file. The other version is kept in version history and can be
  restored or compared later.
- Fauna never writes conflict markers into your files and never invents
  side-by-side copy files — the folder stays clean; history holds the rest.

The folder's conflict review list has a one-tap **Use the other version** for
each entry. Your device first checks that the other version really is part of
that file's own history; if it cannot confirm that, it tells you so and changes
nothing — open the file's version history to restore from there instead.

One consequence worth knowing: for a Word or Excel file, two people's
*simultaneous* edits are never combined automatically — the losing side
re-applies their change on the winning version (or restores theirs from
history). For sustained same-file work, take turns or split the work across
files.

## What happens if I delete a file someone else is editing?

The edit wins. A deletion never destroys changes that haven't synced yet: if
another device has fresh edits to that file, your delete is declined and the
file stays. You'll see it come back, together with a **"Delete declined"**
entry on the folder's conflict review list explaining what happened — the
file reappearing is that entry, not a sync glitch. If you still want the file
gone, delete it again once the other side's edit has synced.

## What does a "Not applied" entry on the conflict review list mean?

One of your devices received a change it could never apply — for example a
file saved by a newer version of Fauna than that device runs, or a file whose
name that device cannot read. Rather than stop syncing everything after it,
the device skips that one change and says so: the entry names the file (or
shows that its name could not be read) and the device that is behind. Your
other devices are unaffected, and so is the device's own copy — it is simply
an older version. The entry clears by itself as soon as that device receives
a newer version of the file: edit the file on any device, or restore a
version from its history. If the device was behind on Fauna itself, update
it first.

## Can Word and Excel co-author live, like on OneDrive?

No — and not with any cloud folder other than Microsoft's own. Live
co-authoring (multiple cursors, AutoSave) uses a private Microsoft protocol
that only works against OneDrive/SharePoint; iCloud Drive and Dropbox can't
offer it either. Through a Fauna folder, Office behaves as it does on any
standard cloud folder: whole-file saves, with Fauna's versioning as the
safety net.

## Is it better with a code/text editor like VS Code or Zed?

Noticeably, for three reasons: text files get the real merge described above,
these editors don't hold Windows-style file locks, and they watch the disk —
when your collaborator's change arrives, an open (unmodified) file reloads by
itself. Two people editing different parts of the same source file genuinely
works. It's still not keystroke-level live collaboration — for that, use an
editor's own live-share feature.

## I shared a 20 GB virtual-machine disk image. Is the whole file re-uploaded on every change?

No. Files are stored as content-addressed **chunks** (roughly 2 MB each);
after a change, only the chunks whose content actually differs are uploaded —
for a VM disk, where writes happen in place, that means roughly "the blocks
the VM touched", compressed on the way. Your machine does re-read the whole
file locally to find the changed chunks, so a save of a huge file costs some
local disk/CPU time, but very little network.

Two practical cautions for VM disks specifically:

- A running VM writes its disk continuously and holds it locked, so the disk
  effectively syncs when it settles — suspend or shut the VM down, and the
  (small) delta uploads then.
- **Don't run the same VM from the shared folder on two machines at once.**
  That's true of every sync service, not just Fauna: two diverging copies of
  one disk image can't be merged, so whoever's session syncs last wins and
  the other's session state drops to version history. Take turns: shut down
  on one side, let it sync, power on at the other.

## Do junk files like `.DS_Store` or Office's `~$` temp files clutter the shared folder?

No. Hidden dotfiles (like `.DS_Store`) never sync — they're excluded outright.
Application temp litter (Office's `~$…` lock files, `*.tmp` save leftovers,
`Thumbs.db`, `desktop.ini`) is filtered too, out of the box. You can add your
own per-folder exclusion rules on top with a `.faunaignore` file
(gitignore-style patterns).

## My external drive was unplugged. Did that just wipe the folder from my nest?

No. When a synced folder is suddenly *empty* — the folder is still there but
everything it was syncing is gone at once — Fauna treats that as a drive that
went away, never as "the user deleted everything". Nothing is removed from your
nest, and the folder's row in **Settings → Folders** says so plainly:

> This folder looks empty. Deletions held: 12. Reconnect the folder, or apply
> them to your nest.

Plug the drive back in (or point the folder back where it belongs) and syncing
picks up exactly where it left off — nothing was lost, so there is nothing to
restore.

If you really did mean to empty it, there is an **Apply held deletions** button
on that same row. It is the only thing that will propagate them, and it always
acts on what is *actually* missing at the moment you press it — so if the drive
came back in the meantime, it deletes nothing.

One deliberate exception: deleting files *one at a time* is ordinary deleting,
and syncs immediately as you'd expect. This safety net is only for the
everything-at-once case, which is almost never something a person did on
purpose.

## Part of my folder stopped syncing and nothing was deleted. What happened?

Fauna could not *read* part of the folder — its permissions changed, or the
drive or network share it lives on stopped answering. A file Fauna cannot read
is never treated as a file you deleted, so nothing is removed from your nest or
from anyone else's copy. The folder's row in **Settings → Folders** tells you:

> Fauna couldn't read 12 items in this folder, so it has stopped syncing them.
> Check that the drive is connected and that Fauna can open the folder.

There is no button, because there is nothing to confirm: fix the permissions or
reconnect the drive, and the next sync picks those files up again — the note
disappears on its own.

## What if my access is downgraded while I have the set bound to a location?

Loudly and safely. Write access is enforced by the nest that owns the set the
moment the owner changes it — your next upload simply won't be accepted. Your
app then clearly marks that location as no longer syncing; nothing on your disk
is deleted or changed, and any local edits stay right where they are, visible.
A location is never left *silently* stale.

## Where this stands today

| Piece | Status |
|---|---|
| Explorer / Finder cloud folders (open/edit/save without the app) | **Available** (Windows on-demand files; macOS File Provider) |
| Sharing to someone on your nest, Reader/Writer roles, per-writer caps | **Rolling out** — live on Linux, macOS, and the terminal app; Windows landing (Android and web don't bind folder locations, so writer binding doesn't apply there) |
| Sharing to someone on **another nest** — browsing + downloading | **Available** (accept the invitation once, then it behaves like any other directory) |
| Write access for someone on another nest (their location binding) | **Landing** — the server side is built and proven; the app-side binding is in progress |
| Versions, restore, "who made this version" | **Available** |
| Automatic conflict handling (text merge / latest-wins + history) | **Available** |
| Instant appearance of the other side's saves (push notification) | **Available when you're both on the same nest** — typically well under a couple of seconds; across two different nests it still arrives on the periodic catch-up (every few minutes, the same on every folder) |
| Built-in filtering of app temp files (`~$…`, `*.tmp`, `Thumbs.db`, `desktop.ini`) | **Available** — dotfiles are also never synced; `.faunaignore` adds your own rules on top |
| Holding an all-at-once folder emptying instead of propagating it | **Available** — the protection itself works everywhere; the "Deletions held / Apply held deletions" row is live on the terminal app, desktop Linux, macOS, and Windows (Android and web don't bind folder locations, so the row doesn't apply there) |
| Telling you when part of a folder can't be read (instead of treating it as deleted) | **Available** — the protection works everywhere; the note on the folder's row is live on the terminal app, with desktop Linux, macOS, and Windows following |
| Office live co-authoring | **Not possible** on any third-party cloud (Microsoft-private protocol) |
