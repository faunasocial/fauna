# Your files on every device — file sync with Fauna

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
exactly what is available now, what is on its way, and what is planned.
>
> Part of the [Your own cloud](your-own-cloud.md) series.

---

## The idea: folders

Everything in Fauna's storage world is built on one concept, the **folder**:
a named collection of files that lives on your nest and can follow you to any
of your devices. You create one from a directory — "Documents", "Projects",
"Music" — and decide, per device, whether that device carries it.

A folder doesn't have a fixed job. What happens is decided **per place** — each
device that carries it, plus your nest — so one folder can behave like live sync
on your laptop and like an archive on the machine in the basement, without you
having to pick a category up front.

For each device you add to a folder, three checkboxes:

- **Uploads what I change here** — files you add or edit on this device are sent
  to the rest of the folder.
- **Receives changes from elsewhere** — changes made on your other devices land
  on this one.
- **Applies deletions** — when a file is deleted somewhere else, delete it here
  too. Leave this off and the device keeps every file forever: that is what an
  archive is, and it is the setting the [backup guide](cloud-backup.md) is about.

Tick all three and you get ordinary two-way sync, which is what most devices
want and what a new device starts as. Every other combination works too — untick
"Uploads what I change here" and you have a **read-only** device that mirrors
your changes without ever pushing its own; untick "Applies deletions" and you
have an archive. They are three independent switches, not a menu of presets.

None of this is decided once and for all. Open a folder's row later and you'll
find the same three checkboxes under **Device places**, one line per device that
carries the folder — so the laptop you set up as ordinary sync becomes an
archive by unticking a box, with no re-creating and no re-copying. The change
takes effect once your nest has it, which is why the boxes settle a moment after
you click rather than instantly.

Your nest is a place too, with its own settings on the folder: whether it keeps
**snapshots**, how long it waits for things to go quiet before taking one, and
how many (or how old) to keep. Every one of those can be left at "use the
default", which is where they start — see the [backup guide](cloud-backup.md)
for what snapshots give you.

## Setting up a second device

There is no pairing dance. Install the Fauna app, choose **Import from Another Device**, and give it your key — paste it, or scan it as a QR code from a device you are already signed in on. That's it — the nest already knows your folders. Pick which ones this device should carry and where to put them on disk.

Every device you add shows up on the **Devices** page in Settings with its name and whether it's online right now. From there you can remove a device you've retired or lost: removal disconnects it on the spot, and your nest keeps every folder's files — except a folder whose **Content kept on the nest** is set to **Metadata only — content stays on my devices**, whose files live only on your devices. If a device is stolen, remove it here first; [Your identity and devices](identity-and-devices.md#if-a-device-is-lost-or-stolen) walks through the steps, including when to use **Lock for 24 Hours** on the **Sessions** page next to it.

What each device does with a folder is set per folder under **Device places** in Settings → Folders, as above: untick **Uploads what I change here** there to make a device **read-only** for that folder — right for a media center or a display kiosk.

## What syncing feels like

- **Real time.** Save a file; other online devices have it seconds later. Under the hood only the *changed pieces* travel: files are split into content-addressed chunks, compressed, encrypted on your device (except in a folder you make public), and uploaded — the nest stores them encrypted and can't read a file's contents, unless you choose otherwise: a folder you make public, serve over WebDAV (below) or put behind a paywall is readable by the box that serves it.
- **Deduplicated.** The same content — a copied subfolder, the same photo twice — is stored once on the nest, within a folder.
- **Offline is normal.** A laptop that spent the flight editing files just
  catches up when it reconnects: it fetches what it missed, uploads what it
  queued. No "resolve 400 conflicts" fireworks — changes apply in order.
- **Per-file status.** Files show their state — synced, uploading,
  downloading, local-only, or in conflict — so "is it safe to close the
  laptop?" has a visible answer.
- **A quiet heads-up when uploads land.** On desktop, a low-key system
  notification tells you when a file has finished uploading — rapid batches
  collapse into a single notice rather than flooding your notification
  center.

## Conflicts, honestly

If two devices edit the same file while apart, Fauna resolves it automatically
— nothing blocks on you. Text files get a real three-way merge when the edits
don't overlap; anything that can't merge cleanly (overlapping text edits,
binary files) falls back to latest-save-wins. Either way, the losing version is never thrown away — it's kept in the file's version history, and the folder's conflict review list, in Settings → Folders, lists every auto-resolved conflict with a one-tap **Use the other version** if the automatic choice wasn't the one you wanted.

For folders holding files that fundamentally can't merge — a
password-manager database, a video project, a disk image — turn on **One
device at a time may edit this folder** in the folder's settings. From then on
only one of your devices uploads to that folder at a time: while one is
writing, the folder's row says which device is editing (by the name you gave
it), and your other devices hold their changes. Nothing is lost — edits you
make on a waiting device stay on that device and upload as soon as the other
one finishes. A device that is offline keeps editing freely and uploads when it
reconnects. This is the honest tool for the job other sync services
approximate with prayer.

## On-demand files ("free up space")

A folder on a desktop can run in **on-demand** mode: every file is visible in
your file manager with its name and size, but occupies no disk space until you
open it (it *hydrates* on first access, then stays local until you tell it to
free the space again). This is the same experience as OneDrive's Files
On-Demand or Dropbox's online-only files — a 2 TB library browsable on a
256 GB laptop — except the server holding the dehydrated bytes is yours.

On Windows this is native File Explorer integration: an on-demand folder shows
up as a cloud location (with the folder's name), files carry the familiar
cloud-status marks, and the right-click menu has **Free up space** — reclaim a
file's disk space while keeping it visible — and **Always keep on this
device** — pin it so it's available offline. Your edits upload either way; if
a file has changes that haven't reached your nest yet, freeing its space
waits until they have. A folder you sync to a location of your own choosing
starts on-demand: files already in that folder stay put and upload, and the
rest of the folder appears as placeholders. If you would rather keep the whole
folder on this machine, flip the folder's **on-demand** switch in Settings →
Folders; Fauna downloads everything and remembers the choice for that folder
on that device.

On Linux, a folder you sync to a location of your own choosing starts fully
downloaded. Flip its **on-demand** switch in Settings → Folders and the folder
stays exactly where it is: files already there remain on disk, and the files
this device doesn't hold yet appear in your file manager — and to every other
program — with their names and sizes, downloading when you open them. Turn
the switch off and Fauna downloads everything again; either way the choice is
remembered for that folder on that device. Two things to know. On-demand on
Linux needs the `fuse3` package: the .deb installs it for you, and with any
other install the switch is greyed out, with a note saying so, until it is
there (Flatpak and Snap installs can't offer on-demand at all). And the folder
has to be somewhere your system allows it — on Ubuntu, inside your home folder
or under `/mnt` or `/media`; anywhere else the folder's row tells you
on-demand can't be used there and keeps syncing the files already on the
device. On Linux there is no right-click menu for this, so the folder's row in Settings → Folders carries it: it shows how much of the folder is on this device and has a **Free up space** button. Pressing it frees every file your nest or your other devices can give back, and keeps any file this device may hold the only copy of — one with changes still uploading, or one you added or changed in a folder whose **Content kept on the nest** is set to **Metadata only — content stays on my devices**. To keep the whole folder on the device instead, turn on-demand off.

On Mac and iPhone/iPad, a folder appears as a **Fauna location** — in the
Finder sidebar on a Mac, and under Browse in the Files app on iOS. It's on by
default for every folder this device **receives changes from elsewhere** in:
the files show up as
placeholders (names and sizes, no disk space) and download when you open
them. Each folder has a **"Show in Finder"** / **"Show in Files"** switch in Settings → Folders if you'd rather keep a folder out of your file browser on that device; a folder you've synced to a location of your own choosing shows up there instead of as a Fauna location. Every folder you own has the switch,
including one you left this device out of when you created it: turning the
switch on there adds this device to the folder's devices, with the usual
defaults, and does nothing more on later flips. Your edits are never lost to flipping
the switch: if you turn a folder off (or sign out) while an edit hasn't reached your nest yet — say you edited in the Files app offline — Fauna keeps it
safe, either holding the location open until the upload finishes or setting
the file aside on disk, so nothing you wrote is discarded. A folder someone
has shared with you appears as a Fauna location too once you have accepted
it, named with whoever shared it — "Trip photos (alice)" — and its row in
Settings → Folders carries the same switch, which only hides or shows it on
this device. If it was shared with you to read only, you can browse and open
its files but Finder and Files won't let you change them. Right-clicking a file in the Fauna
location offers **Share** and **Version history**, which open the Fauna app
on the matching page — the same menu vocabulary Windows Explorer gets.

On Android, the folders this device receives changes in appear under **Fauna** in the system file browser
and in any app's file picker. Files show up with their names and sizes and
download when you open them. When storage runs low Android may clear
downloaded files — they stay listed and download again when you open them.
A folder someone has shared with you appears there too once you have
accepted it, named with whoever shared it — "Trip photos (alice)" — so it
can't be mistaken for a folder of your own. If it was shared with you to
read only, you can browse and open its files but not change them.
Each folder's row in Settings → Folders has a **Show in Files** switch, on by
default: turn it off to hide that folder from the file browser on this phone
only, and back on to bring it back — your other devices aren't affected. On a
folder shared with you the switch only hides or shows it. A folder this phone
already keeps in sync from one of your own phone folders shows a note instead
of the switch, because that folder is already on the phone.
You can edit, create, rename and delete files there too, and your changes reach your nest — an edit as soon as you close the file.

Platform status differs today: Windows has the full experience, the macOS/iOS Finder and Files integration is built and being polished, the Android file browser integration lets you browse, open and edit files, and Linux has the per-folder on-demand switch and a **Free up space** button on the folder's row — see the [status table](#where-this-stands-today).

## Mount it like a network drive (WebDAV)

Beyond the Fauna apps, your nest can serve any of your folders over
**WebDAV** — the standard protocol that file managers already speak. Flag a
folder "serve over WebDAV" (off by default, per folder), and then:

- **macOS Finder** (Go → Connect to Server), **GNOME Files**, **KDE Dolphin**,
  **Cyberduck**, and **rclone** connect with your Fauna mail credentials and
  read *and write* the files;
- **Windows Explorer** can browse read-only for now (native write needs file
  locking, which is on the list);
- anything that can speak WebDAV — scanners, backup tools, media players — gets a doorway to exactly the folders you flagged, and nothing else. Un-flag a folder and access is cryptographically revoked, not just password-walled.

Flagging a folder that already holds files makes those files reachable too: the app you flip the switch in re-locks the folder's existing files to its WebDAV key before it finishes, even on a phone or in the browser, with no computer syncing the folder.

Your storage quota is visible over WebDAV too, so tools that show "space
remaining" work.

## A NAS or server as a sync device

A NAS or home server can carry all your folders around the clock. A natural pattern: laptop and phone carry the small folders, the server carries everything, and doubles as an always-on source so two rarely-on devices never have to be awake at the same time to sync.

You set one up the same way you set up any other device — there is no config
file to hand-edit. Fauna's **terminal app** runs over SSH and gives you the
full interface: sign in with your key, create or bind your folders, and the
sync engine is installed and running behind it.

One box-specific setting matters on a machine you reach only over SSH. By
default your background programs stop when you log out — which would mean the
server syncs only while you are connected. Under **Settings → Folders** you
will find **Keep syncing when logged out**; turn it on and the server keeps
syncing after you disconnect. You can turn it back off the same way.

## Keep a folder's contents off the nest entirely

By default your nest keeps a copy of everything in a folder, so a device can
catch up even when your other devices are asleep. For a folder you'd rather
keep only on your own machines, open it under **Settings → Folders** and set
**Content kept on the nest** to **Metadata only**. A public folder keeps its
content on the nest so anyone following it can read it, so a folder can't be
both public and metadata-only: make it private or shared first, or set it back
to full before making it public.

With that on, the file names, edits and snapshots still travel through your nest
as usual — but the file *contents* never rest on it. They move directly between
your devices when they can reach each other, and are otherwise relayed through
the nest, only while a device holding them is online. Turning it on asks you to confirm one thing plainly: the nest deletes
its copy now, so if your devices lose the files, the nest can't bring them back.
Turn it back to **Full** any time and your devices re-fill the nest's copy.
Because the nest keeps no copy of such a folder's contents, the Media page's
**Upload** won't put a file into it — it tells you so instead. Add the file to
the folder on a device that syncs it.

A file you add or change in such a folder stays on the device you made it on,
because that device may hold the only copy: freeing its space is refused, and
on Windows, File Explorer shows it as always kept on this device. A file that
arrived from another of your devices can still have its space freed. If you
turn the folder back to **Full**, those files stay marked as kept until you
free their space yourself.

A file in such a folder opens wherever you look for it — on a computer that
syncs the folder, or from the Media page of any app, a phone or the web app
included — while a device holding it is switched on and signed in; the Media
page tells you whether one is.

**One limit today:** only a computer can be the device holding the files. A
phone can open them, but a file you add there can't yet be passed on to your
other devices. Keep a folder on **Full** if its files start out on a phone.

It's the strongest privacy setting for a folder — the contents touch the nest's
disk in no form at all — in exchange for needing one of your own devices awake
for another to pull a file. (A folder you're serving as a website, over WebDAV,
or behind a paywall keeps its contents on the nest, since that's what serves
them; Fauna will tell you to turn serving off first.)

## What Fauna deliberately does differently

- **No trash can on synced folders.** A delete propagates — that's what sync means.
  The safety net is real instead: your nest keeps **snapshots** of anything
  you'd grieve, with retention you control, and a device set not to apply
  deletions keeps every file it ever received. See the [backup guide](cloud-backup.md). (Every save
  to a synced file is also kept as a version on your nest: open its detail view
  in the Media browser on any app to restore any older version in place, or on
  Windows, right-click a synced file → **Fauna → Version history**.)
- **No LAN-scan magic.** Devices find each other through your nest, encrypted,
  wherever they are — nothing broadcasts or scans your network. When two of your
  computers can reach each other directly (on the same network, say), a file's
  contents travel straight from one to the other, encrypted, and your nest never
  sees them; the file names and edits still go through your nest. There is
  nothing to set up, and when the direct path isn't there the file simply comes
  through your nest instead, so "works at home" is never different from "works
  from a hotel".

## Where this stands today

| Feature | Status |
|---|---|
| Real-time encrypted sync across devices | **Available** — including offline catch-up and delta upload |
| Zero-config second-device setup (sign in with your key) | **Available** |
| Per-folder device participation + read-only devices | **Available** |
| Device list, removal, session revocation, emergency lockout | **Available** |
| A NAS or server as a sync device, set up from the terminal app over SSH | **Available** — it keeps syncing after you log out once you turn that on |
| Keep a folder's contents off the nest ("Metadata only") | **Available** on every app. Contents are relayed through the nest without resting there, from a computer that holds them to any app that opens them; a phone as the device holding them is **on its way** |
| Per-file status badges | **Available** — on Windows also as File Explorer's own cloud-status icons |
| One device at a time (exclusive editing) for unmergeable files | **Available** — switched on per folder; the folder's row names the device editing it |
| Conflict detection + automatic resolution | **Available** — text merge or latest-wins, with a per-folder review list to override the automatic choice |
| On-demand files / free-up-space | **Available on Windows**; **macOS Finder + iOS Files-app locations built** (per-folder Show in Finder / Show in Files switch, on by default); **Android file-browser location built** (per-folder Show in Files switch, on by default; browse, open and edit); **Linux per-folder on-demand switch built** (needs the `fuse3` package; **Free up space** on the folder's row) |
| WebDAV serving (Finder/Dolphin/rclone read-write) | **Server complete & tested**; the per-folder toggle is available on every app; Windows Explorer's own native write **planned** (needs locking) |
| Selective sync (include/exclude paths inside a folder) | **Available** — an edit reaches every signed-in device within a scan cycle, no restart |
| Per-file version history browser | **Available on every app** — Windows also gets a File Explorer right-click → Fauna → Version history shortcut; every app can restore an older version in place |
