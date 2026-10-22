# Photos and media on your own cloud

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
exactly what is available now and what is not.
>
> Part of the [Your own cloud](your-own-cloud.md) series.

---

Photos are most households' largest collection; this guide covers how Fauna keeps them.

## Automatic photo backup from your phone

Install the Fauna app, enable photo backup, and the camera roll flows to your
nest — encrypted on the phone before upload (unless you make the folder public — see [the privacy difference](#the-privacy-difference-concretely)), deduplicated so re-runs and
duplicates cost nothing.

Every phone and Mac **strips location and device metadata before upload**, for
the picture formats listed under
[What the metadata strip covers](#what-the-metadata-strip-covers) below — read
that first, because it does **not** cover every format yet. What does get
stripped is stripped *losslessly*: the metadata is lifted out of the file and
the picture itself is untouched, so the copy on your nest is the photo your
camera took and not a lower-quality re-encoding of it.

- **iPhone**: the app watches for new photos while it's open, and also backs
  up periodically in the background under iOS's own scheduling — Fauna asks
  the system for network connectivity but deliberately never requires the
  phone to be plugged in, so backup won't compete for battery life. While a
  backup is running the app shows how far it has got — how many photos of how
  many — and a **Back up now** button is there when you don't want to wait for
  the next scheduled pass.
- **Android**: turn on photo backup and the camera roll backs up on its own in
  the background — no need to open the app. Passes run only on an unmetered connection such as Wi-Fi, never on mobile data, and they run every 15 minutes — Android's floor for a repeating background job, and the same cadence on every device; there is no per-folder frequency to set. **Back up now** is still there when you don't
  want to wait.
- **Mac**: the desktop app watches the Photos library itself (it's an
  always-running app, so no scheduler gymnastics) — useful when the Mac, not a
  phone, is where the library lives.

### Your photo library is just a folder

On iPhone, Mac, and Android, your **Photos library is a folder named "Photo
Library"** — the phone's photo store acting as the folder that feeds it. Fauna sets it up as an archive: **Applies deletions** is off, and
your nest keeps 7 snapshots for 30 days until you change that. It appears on the Folders page like any folder you sync, and you
get the good stuff uniformly: the same snapshots, the same retention rules, the
same off-site [backup destinations and friend-hosted copies](cloud-backup.md),
and the same browsing, all applying to your photos with no photo-special cases.

Turning photo backup on creates that folder for you — there's no wizard to fill in.

On Windows and Linux, any folder of images can be kept the same way: switch **Applies deletions** off for it and set how many snapshots and how many days to keep under **What your nest keeps** — see [Backup](cloud-backup.md).

## Browsing your media anywhere

Every Fauna app — all seven platforms, web included — has a **Media** page: your files across all your folders in one place, with a list/thumbnail-grid toggle, sorting by name/size/date, and a per-folder filter. Thumbnails are
generated on your devices and stored encrypted like the pictures themselves, so
browsing is fast *and* the nest still can't see your pictures — unless you choose otherwise, as [below](#the-privacy-difference-concretely).

A folder someone has shared with you shows up in that filter too, with its
owner's name beside it — so a shared "Photos" and your own "Photos" are two
separate choices, never mixed together. Pick the shared one and you can upload
straight into it when its owner has let you add files; if they shared it for
reading only, the app tells you so instead of uploading.

Honest scoping, because photo apps are a high bar: this is a fast, private
**file-first browser** — think "your library, everywhere" — not a
Google-Photos-style product with face recognition, semantic search, memories,
or editing. A timeline/gallery view may come; treat everything beyond
browse/view/download as not promised. Pictures are never recompressed or converted. Photo backup and an upload from the Media page lift out the metadata described below and change nothing else; a file that reaches a folder through a synced location on your computer is kept byte for byte.

## The privacy difference, concretely

Commercial photo clouds scan your library — for features, for advertising
signals, and for whatever their next policy update says. On Fauna:

- photos are **encrypted before upload** (except in a folder you make public), and your nest stores them encrypted and can't read them, unless you choose otherwise: a folder you make public, serve over WebDAV or put behind a paywall is readable by the box that serves it;
- **thumbnails too**;
- location data is **removed before the photo leaves the device** when it comes in through photo backup or an upload from the **Media** page — for the formats covered below;
- and the pictures sit on hardware you chose, under a retention policy you
set, next to [backups](cloud-backup.md) that are encrypted too.

### What the metadata strip covers

Being precise about this matters more than making it sound complete, so:
**location and camera metadata are removed from JPEG, PNG, WebP, GIF and
HEIC/HEIF photos, and from videos** (MP4/MOV — the format phones record in).

That includes **HEIC**, which is what an iPhone camera saves by default — so
you no longer need to change **Settings → Camera → Formats** to "Most
Compatible" to keep locations out of your backed-up photos. (Earlier versions of
this guide suggested exactly that; it is no longer necessary.)

It also includes the **Motion Photos** many Android phones take by default —
those are a photo with a short video tucked inside the same file, and the video
half carries its own location. Both halves are cleaned.

Some files still keep their metadata: **PDFs**, **WebM** videos, and **RAW photos** (Apple ProRAW, Android RAW, and other DNG/TIFF files). If your camera app is set to save RAW, those shots keep their location. So does any picture that reaches a folder through a synced location on your computer: syncing keeps a file byte for byte, and for a folder the strip runs only on photo backup and an upload from the **Media** page.

One more case, rare but worth stating plainly: if a photo or video is built in a
way we cannot read confidently — a damaged file, or one written oddly by some
camera app — **it is backed up exactly as it is, with its metadata left in
place.** We would rather hand you back the original file untouched than risk
damaging the only copy you have; a backup that quietly corrupts a photo is worse
than one that keeps a location field.

Unless the folder is public, those are still stored encrypted, so it is not
something an outsider can read, nor your nest's host — unless you serve the folder over WebDAV or put it behind a paywall, which lets the box that serves it read it. It matters when you make the folder public, or later
**share or export** one of those files, because the location travels with it.

## Where this stands today

| Feature | Status |
|---|---|
| iPhone background photo backup (auto + on-demand, progress; network required, not plugged-in-gated) | **Available** |
| macOS Photos-library backup | **Available** |
| Photo Library folder (snapshots, retention, off-site copies) | **Available** on iPhone, Mac, and Android |
| Android photo backup (on-demand "back up now") | **Available** |
| Location/metadata strip for JPEG, PNG, WebP, GIF, HEIC/HEIF, video (MP4/MOV), and Motion Photos | **Available** |
| Location/metadata strip for PDFs, WebM video, and RAW photos (ProRAW/DNG) | **Not yet** — those files keep their metadata; see above |
| Android automatic background photo backup (no need to open the app) | **Available** — unmetered connections such as Wi-Fi only, every 15 minutes |
| Encrypted, deduplicated upload + encrypted thumbnails | **Available** — except in a folder you make public |
| Media browser (all folders, list/grid, sort, filter) on all 7 apps | **Available** |
| Gallery/timeline view, albums, search-by-content, editing | **Not promised** — file-first browsing is the committed scope today |
| Video streaming beyond basic playback; transcoding | **Not promised** |
