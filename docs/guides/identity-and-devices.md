# Your identity, devices, and account recovery

> **Status — this guide describes Fauna as it is taking shape.** Most of it is built and tested; the [table at the end](#where-this-stands-today) says exactly what is available now and what is planned.

---

> **What this is.** How Fauna accounts actually work — one key, no account password — and what that means in practice: keeping the key safe, adding and removing devices, and what happens (honestly) in every loss scenario from "dropped my phone in a lake" to "someone stole my laptop."
>
> **Who it's for.** Everyone. This is the one guide worth reading *before*
> you need it.

---

## One key, no account password

Your Fauna identity is a cryptographic key pair, created on your device the first time you sign up — a key on your devices, not an account with anyone. The **secret** — shown to you at creation as a long hex string — is the private half. It leaves a device only when you move it yourself: by adding a device or exporting your identity, by turning on the iCloud Keychain backup on iPhone or Mac, or by making a recovery kit, which keeps a copy on your nest (and on a friend's nest, if one holds copies for you) encrypted so that only the kit can open it. Your device's own backup may also carry it, still locked — see [Where the key lives](#where-the-key-lives--and-where-you-should-put-a-copy). Your nest never holds the secret in a form it can use; it works with the public half. There is no account password, which means:

- **No sign-in password to phish or leak.** Signing in proves you hold the key; no server stores a password that would let someone sign in as you.
- **Nothing to reset.** No "forgot password" flow, no support desk that can be talked into handing over your account: nobody — not your nest's admin, not the people who make Fauna — can sign in as you or hand your identity to someone else. (An admin can still suspend or remove an account on their nest — see [What the nest admin can and can't do](#what-the-nest-admin-can-and-cant-do).)
- **The flip side, stated plainly: whoever holds your secret can act as you.** Everything in this guide follows from that.

Your **handle** (`you@yourdomain.com`) is your name on the network; it's
attached to your account on the nest, not baked into the key, and it can be
changed in settings without touching your identity.

## Where the key lives — and where you should put a copy

On each device the app stores the secret in the platform's secure storage: the keychain on iPhone/Mac, the equivalent stores on Windows, Linux, and Android. **The web app has no such store, and neither does the terminal app on a server or over SSH, so there the secret is kept encrypted with a passphrase you choose** (in a browser, in the browser's site storage) — the app asks you to choose it the first time it opens, and asks for it again at each launch (in a browser, once per session, as long as one Fauna tab stays open). Clearing the browser's site data still removes the secret from that browser, so you need the secret or your recovery kit somewhere else to sign back in; and a forgotten passphrase cannot be recovered: **Forgot the passphrase? Start over** on the unlock screen erases this browser's copy of your sign-in keys and nothing else, and you sign in again from another device where you're signed in, an exported copy of your key, or your recovery kit. Treat the web app as a window, not as the home of your only copy.

On iPhone and Mac you can optionally turn on **Settings → Account → Back up
identity to iCloud Keychain**. By default the app keeps your identity out of
iCloud altogether; switch this on and iCloud Keychain keeps an
encrypted copy, so a new iPhone or Mac restored from your Apple account already
has your identity. It's a convenience — being signed in on a second device
(below) is still the sturdier way to make sure you never lose access.

The app never sends your secret anywhere by itself — not into a backup, not
into a sync between your devices. What your device's *own* backup does with
the place the secret is kept depends on the device:

- **iPhone and Android phone:** the secret stays on the phone. A new phone
  set up from a backup of the old one does not have it — add the new phone
  as a device (below), or use your recovery kit.
- **Mac and Linux:** the secret sits in your keychain or keyring, which is
  part of your home folder. A backup of your home folder (Time Machine, or
  whatever you use) carries it, still locked under your login password.
- **Windows:** the secret sits in Windows' own credential store, protected by
  your Windows sign-in. It stays with that PC: Windows does not copy it to your
  other PCs, and a backup of your user profile carries it still locked under
  your sign-in.
- **Terminal app with a passphrase:** the encrypted file goes with any backup
  of the folder it is in, and opens only with your passphrase.
- **Web app:** the secret goes wherever a copy of your browser's data goes, still locked — encrypted with your passphrase.

Using the terminal app on a server or over SSH — anywhere without an OS
secure store — it asks you to choose a **passphrase** on first run and keeps
the secret in a file encrypted with it. You'll enter that passphrase at each
launch; there is no reset if you forget it (that's the point), so keep the
secret backed up per the rules below and you can always start fresh. You can change the passphrase later — in the terminal app and the web app alike — under **Settings → Account → Credential store → Change passphrase**: enter the current one, choose the new one, and from then on the old one no longer opens the store — back up your identity secret first (the dialog reminds you), because a forgotten new passphrase is just as unrecoverable as a forgotten old one.

The house rules, in order of importance:

1. **Keep a copy outside any device** — a password manager entry is ideal;
   paper in a drawer works too.
2. **Be signed in on at least two devices.** Any signed-in device can
   re-display the secret from its settings, so two devices means one can
   always rescue the other.

## Adding a device

Install the app, choose **Import from Another Device** (not **Create New Identity**), and give it your secret —
paste the hex string, or scan your identity as a QR code shown by a device you
are already signed in on (**Settings → Account → Export Identity**, then *Show
QR Code*). The QR carries your handle too, so the rest pre-fills. Same
identity, same handle, everywhere.

The QR stays hidden behind that button, with a warning beside it while it is
visible, because **anyone who scans it gets your identity** — reveal it only
when nobody can see your screen.

Your **Devices** page in settings lists every device: name, online dot, a
**Peer transfers** switch, and a remove button. The switch says whether that
device fetches shared files straight from other devices (see the sharing
guide); on the device you are holding it is yours to flip either way, and on
any other device you can only turn it off — that device turns itself back on,
never anyone else. Your account's tier allows a certain number of devices; if you sign in on one more than that, the new device is not added and the Devices page tells you so, with the way out: remove a device you no longer use from that page, or ask your admin for a bigger tier. A few details show the design's care for your data:

- Removing a device never loses what your nest holds: your nest keeps every synced folder's files, except a folder whose **Content kept on the nest** is set to **Metadata only — content stays on my devices**, whose files live only on your devices.
- The device you're holding can't be removed from this page — sign out on it
  instead. And before removing any other device, the app checks for itself
  which of your devices the row stands for rather than taking the server's
  word; if it can't confirm, nothing is removed and the page says so.
- Below the list, **Signed-in devices without a matching entry** appears only when a device still holds your account's keys but no entry above stands for it — for example one that has signed in but not yet registered, or one whose own record disagrees with its entry. Such a device has no name to show, so its card carries its key fingerprint and the sign-in time it claims. Each of your devices shows its own fingerprint on its own entry (**Key …**); the one to remove is the card that matches none of them. Removing it there can't be undone — that device would have to sign in again from scratch — so it asks you to confirm — and if two cards appear where you expected one, one of them is a device you hold.
- Removing a device shuts it out of your nest: its connection is refused, and new account data is no longer encrypted to it. It does not erase what is already on that device — your conversations are not re-keyed away from it — and a removed device that still holds your identity secret could sign back in. If you think someone copied your secret from a device that was taken, take your account back with your [recovery kit](#your-recovery-kit) — see [If a device is lost or stolen](#if-a-device-is-lost-or-stolen).
- A newly added device sometimes says it is waiting for another of your devices before it shows settings such as your mail, backups or folders: what it needs to read them is still on that other device. Opening the app there ends the
  wait; if that device is gone for good, removing it here ends it too.
- On a computer, the sync-agent line at the top of the sidebar reads **Not
  enrolled** when that computer is no longer one of your account's devices —
  it was removed from your devices on another device, or the server was reset.
  While Fauna is open there your files keep syncing; once you close it,
  nothing on that computer syncs in the background any more. Sign in again on
  that computer to put it back among your devices; the line then returns to
  **Running** by itself. If your tier's device limit is what is in the way, remove a device you no longer use first.

New conversations appear on all your devices: one you join or start on one device shows up on your other open devices within about half a minute, with no restart, and a new device brings your existing conversation history with it.

## Several identities on one device

The mirror image of one identity on several devices: **one device can hold
several identities**, and you switch between them without signing out. The
common reason is keeping your everyday identity separate from the one that
administers your nest, so ordinary browsing isn't done with the keys that can
change everyone's settings. A shared laptop is the other.

They live in settings under **Accounts** — *"Switch between the identities on
this device, or add another."* Each identity gets a row showing its handle,
with the one you're using marked as active. Tap another row to become it.

Notifications follow the identity you're using. When you switch, the device
stops alerting for the identity you left and starts alerting for the one you
switched to — so on a shared device, nobody's alerts show up while someone
else is signed in. Switch back and they resume on their own; sign out and they
stop altogether, returning automatically the next time you sign in. None of
this touches your notification choice itself: if you turned alerts off, they
stay off until you turn them on again.

**Add account** runs the same setup you saw the first time — create a new
identity, or import one you already have — except it runs *over* your live
session, and when it finishes you are on the new identity. Nothing about the
first one is disturbed: they are genuinely separate accounts on the nest, with
separate logins and nothing shared between them, so the switch is complete
rather than a change of view. If the new identity is setting up a nest of its own and you stop once its nest has started being built — or close the app — you are still on the identity you had; the unfinished one is listed among your identities, and selecting it picks the setup up where it left off (or lets you remove it). Stop before that, and nothing of the new identity is kept.

A device holds about a dozen identities. The exact number depends on how much
each one keeps here — fewer if they are signed in, and fewer still if they
replaced an older identity the device remembers. When the list is full, adding
one more is refused before anything is saved. The app says the account list is
full, and you stay on the identity you were using. Remove an identity this
device no longer needs (see **Remove** below), then add the new one again.

**Require confirmation to switch** is a per-identity toggle on every row,
including the one you're currently on — which is the point, because the
identity worth protecting is usually the one you're already using. Turning it
on never prompts you; only *switching to* a protected identity does. On iPhone, iPad, Mac, Windows and Android the prompt is your device's own screen unlock — Face ID, Touch ID, Windows Hello, your passcode — and if that device has no screen lock set up (on Windows, no Windows Hello), a protected identity can't be switched to there: set one up, or turn the toggle off first. On Linux, in the terminal app and in the browser, Fauna asks you to confirm in the app instead; that one is a deliberate second act rather than a password check, which is the honest description of what it can be there. Either way, declining just leaves you on
the identity you were using — no error, nothing changed, because nothing had
changed yet.

If an identity is a nest administrator, the app turns this on for it the first
time it notices. **Your own choice always wins**: turn it off and it stays off on this device for as long as the identity is listed here.

**Remove** appears on the identities you are not currently using. Read it as
*remove from this device* — it deletes that identity's stored keys **here**.
It does not delete the account, and it does not touch your other devices. Removing it also signs this device out of that account.

> ⚠️ **Before removing an identity, make sure its key survives somewhere
> else** — another signed-in device, an exported copy, or its recovery kit. A
> Fauna identity is its key. If this device holds the only copy, removing it
> is not undoable by anyone, including your nest's admin.

If this device can no longer sign in as one of the identities in the list —
its key is missing here, for example because the device's key storage lost
it — switching to that identity is refused, the app says so, and you stay on
the identity you were using. To use it on this device again, add it back with
its secret key or its recovery kit.

On a computer each row also offers **Open in new window** (in the terminal app it copies the command that opens the second window), so two identities can run side by side instead of taking turns — each window stays signed in as its
own identity, and the confirmation above applies to those windows too. The
full walkthrough of that, including what happens when you start Fauna again
while an identity is already open, is in the
[app tour](app-tour.md#settings).

In the browser, **tabs are the windows**: open Fauna in a second tab and switch
that tab to another identity, and the two tabs stay on their own identities
from then on. Switching in one tab no longer changes what the others are
showing — each tab remembers the identity you gave it, and keeps it until you
switch that tab yourself. A tab you open fresh starts on whichever identity you
used last.

## Signing out takes your data off the device

Signing out is not just forgetting your key. It also removes what that identity left on the machine — the local copies of your conversations, the account's sync data and its own records — so a device you are handing on, or a computer you share, does not keep them readable.

In a browser, a sign-out you have confirmed is finished even if you close the
tab straight away: the next time Fauna opens in that browser, it completes the
sign-out before it shows anything.

Occasionally one of those items will not go. Almost always this is another program holding the file open at that moment — on a computer, often a virus scanner or a search indexer. When that happens the
sign-out still finishes — you are signed out, and everything that *could* be
removed was — and Fauna tells you, on the screen it returns you to, how many
items are still on the device. It says so precisely because the alternative is
worse: a sign-out that looked spotless while your conversations were still
sitting there.

If you see that message, the fix is usually to close the other program and press
**Remove Again** beside it. Fauna tries once more to remove exactly what was
left, and the message goes away once nothing is. The message doesn't disappear
when you close Fauna, either: the next time you open it while signed out, Fauna
quietly tries again first, and only shows the message if something is still
there. If you are giving the machine away and want certainty rather than a retry, on a computer remove Fauna's data folder yourself, or wipe the disk.

In the web app the leftover is the copy of your account your browser keeps, and
what usually holds it is another Fauna tab in the same browser. Close that tab
and press **Remove Again**; the next time Fauna loads in that browser it tries
again on its own as well.

If Remove Again says another Fauna window is using some of this data, close that window and press it again. Fauna checks first, and won't remove data it finds another window using.
Anything you have used again since — say you signed back in to the same
identity — no longer counts as a leftover, and Fauna leaves it alone.

One case is different: **Fauna itself is still open somewhere else on this
device, using the same identity.** That can be a second window, or a different
Fauna app — the terminal app running beside the desktop one, for instance. They
share the same data, so removing it would pull it out from under the one still
running. Here Fauna does not sign you out at all. It tells you that you are
still signed in and that another Fauna window is using this identity; close that
one, then sign out again. Nothing is removed until you do. Removing a single
account from the account switcher works the same way: while another window is
using that account, Fauna leaves it where it is and says so — close that window,
then remove the account again. The same holds for the window you are in: the
account switcher marks the account this window is using, and never offers to
remove it. To remove it, close this window and remove the account from another
one.

Leftovers can include your sign-in credentials too — the key that lets this device act as you. Fauna keeps them in your system's secure storage — or, in the web app and in the terminal app without one, encrypted with your passphrase — and signing out removes them too. If that storage is locked or unavailable at the moment you sign out, the credentials can stay behind, and Fauna tells you so on the same screen rather than reporting a clean sign-out. Unlock your system's keychain or password manager, then press **Remove Again**.

## If the app can't read your saved identities

Very rarely, Fauna starts up and cannot read the list of identities it saved on
this device. When that happens it says so, rather than offering to set you up
from scratch — because your identities are almost certainly still there. There
are two versions of that screen, and they ask different things of you.

**Update needed** — *"Your accounts were saved by a newer version of this app, so this version can't read them."* You used a newer copy of Fauna on this device and have gone back to an older one. Nothing has
been lost: update the app and every identity will be where you left it. The
screen deliberately offers no other button, because nothing else helps — and
anything that cleared the device here would destroy exactly what the update
would have brought back.

**"Your saved accounts can't be read."** The saved list is damaged in a way no
version of Fauna can read, so updating won't help. Nothing has been changed or
deleted. **Start over on this device** is offered here, and pressing it does
nothing yet: first it tells you what starting over will and won't do, and only
**Remove everything and start over** acts on it.

> ⚠️ **Starting over removes every identity from this device.** Your accounts
> still exist on your nest, but to sign back in you'll need each one's secret
> key or recovery kit — this device's copy is gone. Some of what the damaged
> list pointed to can't be found to clear it, for the same reason the list
> couldn't be read.

Starting over removes the same things signing out does, so it stops for the same reason: if Fauna is still open somewhere else on this device — another window, or another Fauna app — it removes nothing and tells you so. Close the other one, then press **Remove everything and start over** again. In the browser there is no such check, so close any other Fauna tabs before you start over.

If this device might hold the **only** copy of an identity, don't start over yet. On a computer, copy Fauna's data folder somewhere safe first: the list may be unreadable
to this version and perfectly readable to another, and a copy keeps that chance
open.

Your conversations work the same way. If a newer copy of Fauna saved them on this
device and you have gone back to an older one, the Conversations page says the
app is too old to open them and that they are intact. Nothing on the device is
changed: update the app and they come back. Everything else keeps working in the
meantime.

## If a device is lost or stolen

Act in this order, from **Settings → Devices** and **Settings → Sessions** on any signed-in device:

1. **Revoke** its sessions and remove the device — the first fix. Removal disconnects it on the spot (revocation severs live connections immediately, not at next sign-in), refuses that device's connection from then on, and stops new account data being encrypted to it — though a device that still holds your secret could sign in again from scratch, which only your recovery kit settles (see below). If your account lives on more than one nest, the removed device loses access at every one of them: your other devices pass the removal on to each nest as they sync, so you never have to repeat it nest by nest. 2. **Emergency lockout**, if you cannot reach a signed-in device, or think the secret itself was copied. On a signed-in device it is on **Settings → Sessions** (**Lock for 24 Hours**); on any other device you can pull it with nothing but your identity secret and handle (**Lock my account for 24 hours**, on the screen where you create or import an identity), and nothing is saved on that device. It signs out every device, this one too, blocks every sign-in for 24 hours, yours included, and kicks any live connection off the nest immediately — so while it holds, no device of yours can sign in to remove anything, which is why it comes second. (On an account that is an admin of its nest, the lockout signs every device out but does not stop anyone signing in again.) It buys you calm time, but it does not remove someone who holds your secret: they come back when the lock ends. It is deliberately temporary, and it cannot be lifted early.

Then think about what the thief actually has. A phone with a lock screen
keeps the secret in hardware-backed storage — realistically fine after the
steps above. But **if you have reason to believe the secret itself was
copied** (an unlocked device, a compromised browser), the steps above buy
time rather than settle it: someone holding the secret can import it just
like you would, and a lockout can't un-leak it. What settles it is a
**recovery kit** — see below.

## Your recovery kit

A recovery kit is a second key, kept only by you and only offline, that
**outranks your identity secret**. It is what turns the two worst days —
losing your secret, or someone stealing it — from permanent into fixable.

You are offered a kit right after creating a new identity — a screen shows the code and encourages saving it before you go on (skipping costs one click, and the Recovery Kit section in Settings shows a standing warning until you make one). A kit saved there finishes activating when your account comes online at the end of setup; the screen says so, and until that happens the kit is not yet protection. You can also create one any time under **Settings → Account → Recovery Kit**. Either way you are shown a 64-character code and a QR, **once** — the app also calls the code your *recovery phrase*. Write it down or print it; put
it somewhere a burglar and a house fire won't reach the same day. Nothing on
your devices keeps a copy, deliberately — a recovery key your stolen laptop
also holds would protect you from nothing. There is no "show it to me again"
button, and there never will be.

**Copy** puts the full `fauna://recovery` link on your clipboard — the same thing the QR encodes, which is a little more than the 64 characters on screen: it also names your account, so restoring from it later asks you for nothing else. (The kit offered during sign-up is made before your handle is settled, so restoring from that one also asks for your handle.)
If you write the code down by hand instead, the 64 characters are all you
need; the app will simply ask which account it belongs to.

The status line under that heading tells you where you stand: whether a kit is active, whether a copy of your identity secret that only your kit can open is stored for it, or whether someone has asked to replace your kit (see below). It reads your
account rather than this device, so a kit you created on your phone shows up
on your laptop too.

Three things a kit lets you do:

- **Get your identity back after losing every device.** Creating the kit also stores a copy of your identity secret on your nest, encrypted *to the kit*, so your nest cannot read it and neither can anyone without the code.
- **Take your account back if your secret is stolen.** The kit can move your account to a fresh identity, which the thief cannot follow. Your handle moves with you, and the old key stops working — at your nest at once, and at each other nest your account is linked to once one of your devices next syncs with it.
- **Replace the kit itself**, either instantly with the kit in hand, or — if
  you lost it — using your identity secret alone. That second path waits
  **30 days** before taking effect, and every device you own says so loudly the whole time: a warning banner that stands out, across the top of *every* page once you are signed in, which you cannot dismiss and which appears as soon as the app has opened and signed you in — you do not have to go looking in Settings to find out. It shows how many days are
  left and the start of the new key's fingerprint, so if it *was* you, you can
  check it against the kit in your hand. That delay is the point: it is what
  stops a thief who has your identity secret from quietly issuing themselves a
  new recovery kit. If you ever see that warning and it wasn't you, cancel it (**Settings → Account → Recovery Kit**), and treat your identity secret as compromised.
  If your account is linked to more than one nest, the replacement and the
  cancel each reach all of them: one cancel stops the wait on every nest,
  including a replacement someone asked for at another of your nests.

One more step belongs to that 30-day path, and it needs you: when the wait ends and the new kit takes effect, the copy of your secret that your old kit could open is removed — so for the moment, your phrase alone cannot recover the account. The Recovery Kit section will say so, and your nest sends you a note
about it. Go there, enter the new kit you wrote down when you asked for the replacement, and press **Restore Phrase Recovery**. That stores a fresh copy, encrypted to your new kit — the kit itself stays exactly the one on your paper. (The same button appears in the rare case an interrupted kit setup left that copy missing; the fix is always the kit in your hand.)

Rarely, some of your account data can end up locked away for good: it was
saved under a key that none of your signed-in devices holds, and no copy of
that key is stored with your nest — for example when a device was lost right
after it made a new key, before it could store a copy. When that happens the
Recovery Kit section says how many items are affected and since when. If you
still have a device that has not been signed in since then, sign in there
first: it may still be able to read them. Otherwise, type **LET GO** into the
field below that line and press **Let Go Of Unreadable Data** to free the space
those items take. Nothing ever lets them go without you.

To do any of the things that need the kit you are holding — replacing it,
cancelling a replacement you did not ask for, restoring phrase recovery, or
taking your account back — paste the code into the field that appears in that
same Settings section, then press the button. It is the same box the restore
screen uses, and it accepts either form: the 64-character code or the
`fauna://recovery` link.

One thing worth knowing about replacing a kit: making a new one **retires the old one**, and the copy of your secret has to be re-encrypted to the new code in the same breath. If your app cannot read the copy it is about to replace — you are offline, or something about it does not open — it will say so
and **decline to go ahead** rather than replace it with a partial one. Nothing
changes when that happens: your current kit still works, and you can simply try
again on a connection that reaches your nest. This matters most if you have taken your account back from a thief at some point, because that copy then carries the *old* identity's secret too, which is the only thing that can still open files encrypted before the takeback.

If your account lives on more than one nest, your kit has to be known at each of
them — a nest that has never heard of your kit could not help you take the
account back there. You do not carry it by hand. When you link two nests by
address in **Settings → Nests**, the app brings them to the same recovery kit
before it links them, and afterwards it keeps them in step: a kit you make or
replace on one nest reaches your other linked nests the next time your app
syncs with them. (A nest only reachable on your home network catches up when a
device is next at home.)

Linking can be refused with *"These two nests hold different recovery keys for
this account, so they can't be linked."* That means each nest already has a
recovery kit on record for you and they are not the same one — nothing was
changed on either nest, and they stay unlinked. If you never made a second kit
yourself, treat it as a sign that someone else holds your identity secret, and
see the next section.

### Taking your account back from a stolen secret

Someone has your identity secret. They can post as you, read what arrives for you, and lock you out of your own sessions — and no password change exists to stop them, because there is no account password. What stops them is the kit, which they
do not have.

In **Settings → Account → Recovery Kit**, paste your recovery code, type
`SUCCEED` into the confirm box, and press **My Identity Was Stolen**. The
confirm box is there for the same reason deleting an account has one: this
cannot be undone.

What happens next, in one step: your account is moved to a brand-new identity that only you hold, the old key stops working — at your nest at once, the thief's sessions included, and at each other linked nest once one of your devices next syncs with it — and **your handle comes with you**. The app signs itself back in as
the new identity on its own; you don't have to paste anything.

Your group conversations move with you, in the same step. Your new identity
joins each one and the old key is removed from it, so the thief loses the
ability to read those conversations going forward. The Recovery Kit section
tells you how it went as soon as it is done — and if some group could not be
updated, it says so plainly rather than quietly counting it as finished. That usually happens because your conversations were not running at the moment you did this, and you can finish it yourself — see **If your groups did not all move**, below.

**If your account is supervised, it stays supervised.** Taking your account
back moves your guardianship link along with everything else, so a child who
has to recover their account does not quietly come out of it unsupervised —
and the reach and content settings their guardian chose come across untouched.
It works the same way from the other side: a guardian who takes their own
account back keeps every account they look after, and those accounts keep
showing the same guardian on their Family page. Nothing about supervision is
something you have to set up again afterwards.

**Anything that was scheduled to happen is called off.** Some things you ask
for don't happen straight away — deleting your account, deleting or pruning
snapshots, changing your handle — they sit for a waiting period first, so you
can change your mind. Taking your account back cancels every one of those that
is still waiting, because the point of the waiting period is that a thief who
got into your account could have started one, and the countdown would otherwise
keep running after you had locked them out. Snapshots that were queued for
deletion go back to counting normally. Nothing you scheduled yourself is lost
in any other sense — if you still want it, ask for it again from the same
place, and the wait starts fresh.

**What the other people in those groups see.** From their side, a stranger has
just joined and you have just left — which is exactly what a thief taking over
would look like, so their app does not take anyone's word for it. It checks the move against the record kept by the nest that hosts *your* handle — one it already holds, or fetches from that nest — and never against anything the message itself claims. When that checks out, the conversation simply carries on
with you still in it, under your handle, in the same place in the member list;
your older messages stay attributed to the identity that actually wrote them.
When it cannot be checked — their app is offline, your nest is unreachable, or
the claim does not hold up — they see the plain "someone joined, someone left"
version instead. Their app only shows you as the same person once it has proven the move.

**Community rooms move with you at once.** A community room keeps its member
list on the nest that hosts it, so taking your account back puts you in your
old place there in the same moment — the same rank, owner or admin included,
with nothing for anyone to approve — and the room shows your previous identity
as having left. The next time your app checks in on the room it hands over a
fresh key, so messages sent after the move are readable too; if you were the
room's owner or an admin, it turns the room's key over in the same moment, so
your previous identity reads nothing new. To the room you are a new member,
and what you can read there starts from the move.

**If your groups did not all move, you can finish it yourself.** Moving your groups is the last
thing taking your account back does, and it is the part most likely to be left half-done: your
conversations may not have been running at the time, or the connection may have dropped part-way
through. The Recovery Kit section says so plainly when it happens — how many of your groups moved
and how many did not — and offers **Finish Moving Your Groups**. Press it and the app picks up where
it left off. It is safe to press more than once: groups that already moved are left alone.

Do this from a device that still has the conversation history from your previous identity, which is what the move is made from. Press it on a device that doesn't, and the app tells you so — nothing breaks, and nothing is lost by trying: if another of your devices still has those conversations, press **Finish Moving Your Groups** there; otherwise ask another member of each group to remove your old identity and add your new one.

Until your groups have moved, your previous identity can still read new messages in the ones that
have not — which is why this is worth finishing rather than leaving.

**Read the part about members it could not vouch for.** Removing the stolen key
is the half that is certain. The half that isn't: someone who had your secret
could have added *themselves* to one of your groups earlier, under a name of
their own — and to your app that looks exactly like any other member you added
yourself. Nothing on your device can tell those apart, so it does not guess. It
tells you how many members are in that category and leaves the judgement to you.

Those members are marked for you, so you do not have to remember who they were.
Open a group and the people still waiting on your judgement carry a short note
beside their name, with a **Keep** button next to it. Press **Keep** for someone
you recognise and the note goes away everywhere they appear. For someone you do
not recognise, use the ordinary remove you would use for any member — the same
tap, in the same place. Contacts you happen to have saved carry a matching
reminder on their row, so you can spot them there too.

Right after a recovery, the Recovery Kit section also gathers everyone still waiting into one list, so you can work through them in one sitting — **Keep** and **Remove From My Groups** beside each name, and **Review The Rest Later**, which puts the rest off without deciding anything (their marks stay in the places above). That list is only shown once, in the recovery itself. Whoever you did not get to is waiting for you afterwards under **Settings → Members To Review**, with the same **Keep** and **Remove From My Groups** beside each name. That page is empty until a
recovery leaves something on it, and empty again once you have worked through
what it holds — so most of the time, opening it just tells you there is nobody
waiting.
**Remove From My Groups** means what it says — *remove this person from every group of mine*: the app
takes them out of each of your group conversations it can reach. If it cannot
reach every seat they hold, it says so and the row stays until each one is dealt
with, so nothing is quietly left behind — someone also in a shared folder is removed in that folder's own sharing settings (or leave the folder, if it is not yours), and a group chat that has not synced to this device yet can be finished after it syncs, or from a device that has it.

Two things worth knowing. Most of the people marked this way are your own
friends: the move can only vouch for the one member it added itself, so everyone
else lands on the list by default. It is a review list, not a list of suspects.
And **Keep** answers *this* recovery only — if you ever have to recover the
account again, the same people are raised again, because a judgement about one
break-in cannot speak for the next one.

**If the connection drops in the middle of it**, you have not lost anything. The
move can complete on the server at the very moment the reply back to your app
goes missing, so the app can no longer tell whether it worked — and it says so
in those words, rather than pretending it failed. The new identity is saved on
your device either way, before anything else happens, so the way forward is
always the same: reopen the app. If the move did go through, you come back in as
the new identity; if it did not, your old one still works and you can simply try
again with the same recovery code.

A few more things to expect afterwards:

- **Your own data unlocks itself, without you asking.** Everything you had was
  locked with the key you just retired — your settings and saved keys, the
  destinations your backups go to, your conversations, your files and photos,
  and the half-written messages and posts you never sent. As soon as you come
  back in, the app starts moving all of it across to the new identity on its
  own. Your settings come across with your account and need no line of their
  own; for the other parts the Recovery Kit section shows a line per part as
  each one finishes. You do not have to run anything.

  Your unsent drafts are part of that, and they come back on your **first**
  visit rather than the second — open a composer and what you had typed is
  still there. If a draft is still held under your previous identity, the app
  says so rather than showing you an empty box: the text is safe, just locked
  to a key this device does not have.

  If you took your account back on a *different* device from the one you are
  looking at, a part may say it is still held under your previous identity and
  that another device will finish it. That is a wait, not damage — nothing is
  lost, and it completes the next time you sign in on the device you used to
  take the account back. Conversations can also report that only *some* of them
  came across, which happens if your account has been moved more than once; the
  same fix applies.

- **Your mail apps stop working, on purpose, and you set them up again.** If you
  use Fauna for email, calendar or contacts, the app passwords you gave to Mail,
  Thunderbird, your phone's calendar and so on are dead the moment you come back
  in. That is deliberate: whoever had your previous identity could read those
  passwords, and any one of them would have let them keep reading your mail from
  then on. So the app replaces your mailbox's encryption key and revokes every
  one of those passwords, and the Recovery Kit section tells you it has done it.

  **Your mailbox keeps receiving mail throughout** — nothing is lost, and nothing
  bounces once you have signed back in. What you have to do is go to Settings →
  Mail & Calendar and set each mail app up again with a new password. The old
  entries stay in your credential list, marked as revoked, precisely so you can
  see which apps you still have to redo; remove each one once you have.

  **What the passwords opened is untouched.** Only the passwords are replaced —
  your mail itself, which folders it is filed in, what you had read, your
  calendars and their events, and your address book and its contacts all come
  across with you. Set an app up again with its new password and it finds
  everything where you left it, including the things you had deleted staying
  deleted. Your sending allowance for the day comes across too, so a recovery
  does not hand you a fresh one.

- **What you had hidden from search stays hidden.** If you had turned off showing a bridge's content in search, it stays off after you take your account back.

- **If you sell subscriptions, your payment setup needs redoing — your record of
  sales does not.** The connection to your payment provider was tied to the
  identity you just retired, so it does not come across: the **Payment
  providers** section starts empty again, and any claim codes that had been
  issued but never redeemed now show as voided. That is deliberate, for the same
  reason your mail passwords are — whoever had your previous identity could have
  connected a provider of their own, or minted codes and kept them, and neither
  would have stopped working on its own.

  Reconnecting is mostly what you would have had to do anyway: the webhook
  address contains your identity, so it changes with it and has to be registered
  at your provider again either way, and the verification secret is the one you
  copy from their dashboard while you are there. **Every sale you have made is
  still listed** under **Manual claim codes**, so a buyer who paid and never
  redeemed their code is not lost — check the payment against your provider's own
  records and mint them a fresh one.
- **Your mailing lists come with you — and so does today's sending allowance.**
  A list you run keeps its address, its members, its history and its settings,
  and it is there on the Lists page under the identity you just recovered, to
  edit, send from and delete exactly as before. The one thing that does *not*
  start over is the daily send quota: if much of today's allowance had already
  been used, what is left is what you have until the day rolls over. That is
  deliberate — an allowance you could clear by taking your account back would
  not be much of an allowance — and the same goes for the quieter limits that
  keep mail from your domain trusted.
- **Your feeds and the labellers you follow come with you.** Every feed you
  built is still yours — same name, same rules, same ordering — and it is on
  your feeds list under the identity you just recovered, to edit and delete as
  before. The weightings you set for what you want to see more and less of come
  across with them, so your feeds read the way they did yesterday rather than
  quietly re-sorting themselves. Community labellers you subscribed to stay
  subscribed, and keep filtering your incoming mail. As with your mailing lists, the limit on how many feeds your tier allows counts the feeds you already have — taking your account back does not hand you a fresh set of slots.
- **Your Fediverse name comes with you; your Bluesky connection is dropped on
  purpose.** These two look alike and are treated differently, for one reason.
  Your Mastodon-style address is *made from your handle* — it is yours because
  the handle is yours — so it moves across with you: people who followed you
  still follow you, posts sent to your address still arrive, and you keep the
  name rather than being pushed onto a slightly different one. A Bluesky
  connection is the opposite: it points at an account on Bluesky's side, and
  whoever had your previous identity could have pointed it at *their* account.
  Left connected, everything you posted from then on would have been copied
  there. So the connection is dropped, along with the cross-posting setting and
  the saved feeds that came with it. Reconnecting takes one sign-in on the
  Bridges page, and nothing on the Fauna side is lost — your posts and messages
  are yours throughout.
- **Your Nostr key comes with you too — no reconnecting, but you're asked to
  double check it.** Unlike Bluesky, your Nostr key never moves to someone
  else's account: whoever had your previous identity could only ever *use* it
  while they had it, never take it away from you, so it stays exactly as it
  was and there is nothing to redo. Every app you had connected to sign in
  through your nest (Nostr Connect's Connected apps list) is disconnected the
  same way your mail passwords are, for the same reason — reconnecting is one
  visible step on that app's side, and a connection you did not make staying
  invisible is the risk this closes. The one thing worth a quick look is the
  key itself: if whoever took your account also relinked Nostr with a key of
  their own before you got it back, the Nostr page would still be showing
  *theirs*. So the first time you visit it after a recovery, it asks you to
  confirm the public key on screen is really yours. If it is, one tap clears
  the question. If it is not — or nothing is linked at all — the same button
  that always lets you link a fresh key is right there.
- **Your other devices will ask you to import the new identity.** Each one gets
  turned away the next time it connects, explains why, and sends you to the
  import screen. That is the same flow as adding a new device.
- **A new recovery kit is made for you, and shown once.** The old kit retires
  with the old identity, so the moment you come back in as the new one the app
  makes a fresh kit and takes you straight to it — you do not have to remember to
  ask. **Write the new code down while it is on the screen.** This is the same
  once-only display as the kit you made when you first created your account, and
  the same warning applies: leave the screen without copying it and the app
  cannot show it to you again. Until you have it written down, your account is
  protected against a stolen secret but not against losing every device.
- **Your history stays yours but stays signed by the old key.** Old posts and
  messages are not rewritten — they were genuinely written by that key — and
  people who have seen the move will show them as yours. Staying yours includes
  control: you can still delete any of your old posts after the move, exactly
  as before.
- **Your profile comes with you, re-published under the new identity.** Your
  name, bio, links and pictures carry over. The device you took the account
  back on re-publishes them under the new identity when it makes your new kit.
  The previous identity's recovery details and nest address are not copied over. The new kit publishes fresh ones.
- **You are asked to look over the people in your group chats** — the members your app could not vouch for, described under **Read the part about members it could not vouch for**, above.

- **You are asked to look over your email filter rules.** Your filter rules move
  across with the rest of your account — a rule that files mail into a folder or
  quietly bins it keeps working after the recovery. Anyone who had reached your
  account before could have added one, and a filter is a quiet thing: a rule that
  discards or redirects incoming mail leaves no trace in your inbox to notice.

  So the Recovery Kit section tells you how many rules were set up before the
  recovery and are still unchecked, and **Settings → Privacy → Email Filters**
  marks each one. Press **I recognise this** on the rules that are yours, and use
  the ordinary **Delete** beside a rule you do not recognise — the same button
  you would use any other day. The mark disappears as you answer each one.

  As with the group members above, expect nearly every rule on the list to be
  your own, and expect the answer to cover *this* recovery only: if you ever have
  to recover the account again, you will be asked about your rules again, because
  a rule you vouched for last year says nothing about what happened this time.

- **Push notifications are switched off on every device, and reconnect on their
  own.** Taking your account back cuts every push registration the old identity
  had. That is on purpose: a registration is just an address your nest sends
  alerts to, anyone holding your stolen secret could have added one pointing at
  their own device, and nothing about a registration says who added it — so the
  only safe answer is to drop them all rather than guess. Nothing is lost, and on a device that already had notifications on, they turn back on by themselves the next time you open the app there. If a device stays quiet, switching **Notify me on this device** off and on once re-arms it ([where the switch is](app-tour.md#notifications)).

Other access you had granted comes across too: the app re-issues each grant under your new identity on its own, with the same expiry — a grant that had already expired stays expired. The mail app passwords, the Nostr connected apps and any unredeemed claim codes above are the exceptions, cut on purpose. And a folder you sell to subscribers on the web shows its visitors only the teaser until you set its paywall again.

### Getting your account back after losing every device

This is the day the kit exists for: no phone, no laptop, nothing signed in
anywhere — just the code you wrote down.

Install the app on a new device. On the screen where you choose **Create New Identity**, choose **Restore my account from a recovery phrase** (careful: the entry right above it, **Recover a lost box**, is a different repair — that one is for when your *nest* is gone, not your identity). Paste the code, press **Restore**, and you are signed back
in with the same identity you had before. Everything under it comes back with
it: your handle, your encrypted conversations, your files.

If what you saved is the QR or the `fauna://recovery` link — what **Copy**
gives you from Settings — that is all you type: it names your account, so the
app knows which nest to ask. If it is the bare 64-character code you wrote
down, or a kit from the setup-time screen (no account name is chosen yet at
that point), it doesn't say *whose* account it is, so the screen asks: enter
your handle including the domain, like `alice@fauna.social`. That handle is
how your nest gets found in the first place.

**If the domain itself is gone** — it lapsed, or someone took it — put your
nest's own address after the `@` instead: `alice@192.0.2.10`,
`alice@nest.local`, or an address with a port on the end. Your nest, your account and the copy of your secret that your kit opens are all still there; only the name that pointed at them stopped working, and this is the way in that doesn't need it. The part
before the `@` is still your handle, exactly as before.

**If your nest itself is gone** — not just its name, but the nest and
everything on it — your account can still come back if a friend's nest was
keeping a copy of it for you (you ask a friend to hold copies on the Devices
page, and they choose to keep them on their nest). Paste the copied kit — the
`fauna://recovery` link, because it names your account; the bare code alone
does not work here — and in the account field enter your handle with your
friend's nest address after the `@`, like `alice@friends-nest.example`. Their nest checks the kit and hands back the copy of your secret that only your kit opens, just as your own nest would have, and you carry on from there as on any new device. If you replaced your kit after your friend started holding the copy, use the newest one: their nest stops accepting the old one once it has caught up with the replacement. That gets your *secret* back and leaves you at the handle screen. Your account's records — what their nest was holding for you — come back in a second step once you are signed in to a new nest: carry on from step 3 of [If you lose every device and your nest](#if-you-lose-every-device-and-your-nest) below.

Two answers you may get instead, both plain-spoken rather than a generic
failure:

- **"This account has no sealed backup to restore from."** The kit is real but no copy of your secret, encrypted to it, is waiting for it — usually because the kit was made on a device that never finished coming online. Nothing here can fix it;
  it takes a device that is still signed in, making a new kit.
- **"This identity was replaced after it was compromised."** Someone already
  used a recovery kit to move this account to a fresh identity. The app sends
  you to the import screen — you continue with the new identity, not this one.

**Being straight about what's ready:** every one of the kit's jobs works today — creating it (the copy of your secret that only the kit opens is stored as soon as your account is online — for the kit offered at sign-up, at the end of setup), restoring from the code after losing every device, replacing the kit, cancelling a replacement you did not ask for, taking your account back from a thief, moving your groups across with a way to finish if some did not move, and re-issuing the other access you had granted.

## If you lose every device and your nest

If someone you trust holds copies of your account on their nest — they chose **My nest** when they agreed to hold for you, under [Sealed copies on a friend's device](#sealed-copies-on-a-friends-device) — you can bring your account's records back even after every device and your own nest are gone. This route needs your identity secret. If what you have is your recovery kit instead, start with the **If your nest itself is gone** paragraph under [Your recovery kit](#your-recovery-kit), which gets the secret back from their nest first, and then carry on from step 3 here.

1. Ask them for the restore address. Their app shows it on the card for the copies they hold for you, under **Settings → Devices**. Nothing sends it to you; they read it out or write it down for you. (Copies held on one of their *devices* rather than their nest have no address — this route is for a nest.)
2. On a new device, choose **Import from Another Device** and give it your secret.
3. Claim a new nest or join one with an invite, and finish signing in.
4. Open **Settings → Nests**, choose **Restore from a custodian nest…**, enter the address, and choose **Restore**.

The app tells you how many records came back, and normally every one of them is readable. If any came back sealed, it says how many: they stay unreadable until this device has their key. If that nest doesn't let you in, the address may be wrong, the nest may be down, or it may hold nothing for your account — the app can't tell which, and says so.

Afterwards your list of nests shows their nest again, and **Restore from this nest now** on its row runs the same restore at any time.

## If you lose everything

You're signed in nowhere and have no saved copy of the secret. **Without a
recovery kit**, the identity — and any end-to-end-encrypted data under it —
is **unrecoverable. Full stop.** Not by you, not by the nest admin, not by
Fauna. You create a new identity and start again; the nest admin can remove the old account, which frees its handle for your new one, but can't hand you its contents.

This is not a missing feature. Every "we can recover your account" promise a
big provider makes is another door into your data, for them and for anyone
who can impersonate you convincingly. Fauna's promise is the opposite one —
and rule 1 above ("a copy outside any device"), plus a recovery kit, is what
makes it comfortable to live with.

## What the nest admin can and can't do

Can: approve your account, set your tier (how much storage and inbox space, how many devices and feeds, and how large a single message delivered to you can be), suspend your account, or remove it from the nest after a warning period — which also frees its handle. On their nest they can also publish rules your app applies, with a notice naming the nest and the rule's reason, and by hand remove content under a legal obligation or block a single reported item, each with a visible notice, a permanent record and, for a post, an appeal. Can't: read content you have not shared with their nest or published to everyone — beyond the mail and the messages the nest passes to and from email and other networks, a community room whose members chose to let its home nest read it, what you have given the nest permission to read, the address of a link standing on a line of its own in a post or a message, which the nest of each person who reads it fetches for its preview, and the text of a message or a post only some people can open that someone attaches to a report — nor reset your key, recover your data, or sign in as you. Their nest does see what any server that delivers your messages must: who you're connected to — your contacts, whom you follow, the groups you're in — and when and how much you send. If you run your own
nest, the same applies to you with your family's accounts — see
[Running a nest for friends and family](nest-for-friends-and-family.md).

## Sealed copies on a friend's device

A friend can agree to keep **sealed copies** of your data on one of their devices — extra redundancy for the day your own hardware fails. Their device stores your data in its encrypted form and cannot read it, apart from what you have published to everyone, which is public by design; of the rest they see only the shape (that data exists, roughly how much, and when it changes), never the content.

Both sides manage this from **Settings → Devices**:

- **To ask a friend**, use *Ask a friend to hold sealed copies*: pick the
  conversation with them — the request travels over it, so start one first
  if you haven't — read what they would and would not see, and send. They
  accept or decline on their own device.
- **If someone asks you to hold for them**, the request appears as a card
  stating exactly what your device would and would not see, with **Hold for
  them** and **Decline** buttons. Accepting commits the device you accept
  on, and you pick how much space it may use — you can change that budget
  later on the same row, and **Stop holding** at any time. Two controls, two
  different effects: **Stop holding** pauses the arrangement but keeps what
  is already stored, so the space stays used; **Remove and free the space**
  ends it and gives the space back. Removing is not undoable from here —
  they would have to ask again.
- **You can hold on your nest instead of a device.** When the request
  offers it, a *Where to hold* choice appears on the card: *This device*
  (the default) or *My nest*. Choosing your nest means the copies live on your always-on box rather than the device you happen to be using — your nest quietly keeps them current and confirms to your friend
  on its own, with none of your devices needing to be awake. The same
  budget, **Stop holding** and **Remove and free the space** controls apply.
  One thing to know if you hold on your nest: whoever administers that nest
  can also see the arrangement listed, and can end it themselves — it is
  their disk it uses.
- **If a friend holds for you**, the holder appears in its own list —
  never mixed in with your own devices — with how much it currently holds
  and a status line that is honest about freshness rather than collapsing
  everything into one "connected" dot. Right after they accept, before
  their first check-in arrives, it says **No confirmation yet**. Once
  they've confirmed at least once, it says **Last confirmed** and a time.
  If a while passes with no fresh confirmation, it says so plainly —
  **Stale — last confirmed ‹time›. Treat this copy as degraded.** — rather
  than pretending all is well. Three different states, three different
  lines, never silently merged. A holding **device** appears under
  Devices; a holding **nest** appears under Nests. You can stop trusting a holder at any time: that ends future copies and serving on honest holders, but copies already held stay where they are, no more readable to the holder than before.
- **If a holder's device ran short of space**, its row says some copies
  were dropped under the budget — you always see reduced coverage, never
  discover it later.

## Where this stands today

| Feature | Status |
|---|---|
| Key-based sign-up, sign-in, import — no account password | **Available** on all seven apps |
| Secret stored in platform secure storage | **Available** — where a device has none (the web app; the terminal app on a server or over SSH) the secret is encrypted with a passphrase you choose |
| Re-display your secret from a signed-in device | **Available** |
| QR identity export (scan to sign in a new device) | **Available** — hidden behind *Show QR Code*, with a warning while it is on screen |
| Back up identity to iCloud Keychain (iPhone/Mac) | **Available** — off by default (your identity stays out of iCloud); opt in under Settings → Account |
| QR/URI import (paste or scan) | **Available** on all seven apps |
| Create a recovery kit (Settings → Account) | **Available** on all seven apps. Creating it also stores the copy of your secret that only the kit opens |
| Replace a lost kit using your identity secret (30-day wait, loud warning) | **Available** on all seven apps |
| Replace your kit with the one in your hand (takes effect at once) | **Available** on all seven apps |
| Cancel a replacement you did not ask for | **Available** on all seven apps |
| Recover your identity from the kit's code after losing every device | **Available** on all seven apps |
| Take an account back from a stolen secret (handle and all) | **Available** on all seven apps |
| Restore your account's records from a friend's nest after losing every device and your own nest | **Available** |
| Re-join group conversations after taking an account back, with a retry if some groups didn't move | **Available** on all seven apps |
| Several identities on one device (add, switch, remove) | **Available** on all seven apps |
| Require confirmation before switching to an identity | **Available** — your device's own screen unlock on iPhone, iPad, Mac, Windows and Android (with no screen lock set up — on Windows, no Windows Hello — a protected identity can't be switched to there); an in-app confirmation on Linux, in the terminal app and in the browser |
| Two identities open side by side in separate windows | **Available** on Linux, macOS, Windows and in the terminal app — and in the browser, where separate tabs each keep their own identity |
| Devices list with online status and removal | **Available** |
| "This device" tag on your own device's row, reliable even on a computer where a different app already set up your account | **Available** |
| Sole-source guard on device removal | **Available** |
| Session list, revoke, revoke-all (Settings → Sessions) | **Available** |
| Emergency lockout (no sign-in needed, locks for 24 hours) | **Available** |
| Revocation kicks live connections immediately | **Available** |
| A removed device refused by your nest and left out of new account data (conversations are not re-keyed) | **Available** |
| Full conversation history on a newly-added device | **Available** |
| Restricted-capability secondary devices (e.g. a posting-only device) | **Planned** |
| Sealed copies held on a friend's device (ask, consent, budget, stop, revoke) | **Available** |
| "Relay only — holds no keys" marker on your own devices that hold no keys | **Available** |
| Sealed copies held on a friend's nest (their always-on box holds for you; shown under Settings → Nests) | **Available** — shown under Settings → Nests |
