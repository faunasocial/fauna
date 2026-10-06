# Calendar and contacts on your own server

> **Status — this guide describes Fauna as it is taking shape.** Most of it is
> built and tested; the [table at the end](#where-this-stands-today) says
> exactly what is available now, what is landing, and what is planned.

---

> **What this is.** Replacing Google/Apple calendar and contacts with your
> nest: a full calendar in the Fauna app, event invitations that reach
> Gmail and Outlook users, and standard-protocol access (CalDAV/CardDAV) so
> Apple Calendar, Thunderbird, and DAVx⁵ keep working exactly as they do now.
>
> **Who it's for.** Anyone with a nest. Calendar and contacts don't require
> mail to be enabled — though invitations to non-Fauna people ride on it.

---

## The calendar in the Fauna app

Every Fauna app has a full **Events** page: multiple named calendars, agenda
/ month / week / day views, recurring events, locations and descriptions, a
per-event reminder, and `.ics` import/export for moving calendars in and out.

The back and forward arrows above the calendar move by whatever you're
currently looking at — a month in month view, a week in week view, a day in
day view. The agenda is a running list rather than a fixed span, so the
arrows do nothing there.

Your calendars are listed down the side, each with a checkbox. By default
you see everything at once; clear a calendar's checkbox to take it off the
view and tick it again to bring it back. Nothing is deleted or hidden from
anyone else — it only changes what *you* are looking at, on *this* device,
and it isn't a sharing or privacy setting. Clicking a calendar's name is a
different thing: that narrows the page to just that calendar, and while one
is picked the checkboxes step aside.

To move a calendar in or out of Fauna, click its name first — the import and
export controls appear beside the calendar list and act on the calendar you
picked. **Export** saves that calendar as a single `.ics` file in your
Downloads folder and tells you the file's name; exporting again makes a new
file rather than overwriting the last one. **Import** takes an `.ics` file
(choose it, or in the terminal app type its path) and adds its events to the
picked calendar, then tells you how many it imported and how many it had to
skip. Pressing Import before choosing a file just tells you to choose one.

The quickest way to add something is to click where it belongs. In week or
day view, click an empty stretch of the timetable and the new-event form
opens already filled in with that day and that time — click 9:15 on Tuesday
and you get Tuesday at 9:15. In month view, double-click a day to do the same
thing for the whole day; a single click there takes you into that day.

An event you start writing but don't finish is kept for you. Close the app
halfway through filling in the form and the next time you open **New event**
it is still there, on whichever of your devices you pick up — the title, the
times, the description and the location, exactly as you typed them. Creating
the event clears it, and so does starting a fresh one by clicking a day or a
time. Like everything else you write, an unfinished event is encrypted before
it leaves the device: your nest keeps it for you but cannot read it.

The week starts on whichever day your system's language and region setting
says it should — Monday in most of the world, Sunday in the United States,
Japan and much of Latin America, Saturday across much of the Middle East.
There is nothing to configure: change your device's region and the calendar
follows it.

Invite people to an event and RSVPs show on the event for everyone —
*Going*, *Interested*, or *Declined* (Fauna adds "Interested" to the
traditional yes/no/maybe; to outside calendar systems it appears as
"Tentative").

The important design fact: the app's calendar and the standard-protocol
calendar below are **the same data**. An event created in Apple Calendar
appears in the Fauna app and vice versa — there's no "sync between two
calendars," there's one calendar.

## Invitations that reach everyone

Calendar invitations use the same open standard (iMIP — invitations over
email) that Exchange, Google Calendar, and Apple Calendar speak among
themselves. If your nest has [mail enabled](own-your-mail.md):

- **Invite anyone by email address.** A Gmail invitee gets the normal
  invitation with inline Yes/Maybe/No; an Outlook invitee sees a normal
  meeting request. Their responses come back and update the roster on your
  event.
- **Get invited from anywhere.** Invitations mailed to `you@yourdomain.com`
  land on your calendar — in any calendar app you've connected, too — and
  your RSVP flows back, whether you answer in the Fauna app or in that
  calendar app. A mailed invitation only ever *adds* an event: if the same
  event is already on your calendar, it is left exactly as it is and the
  message stays in your inbox. An invitation that went to Junk, or that is
  waiting for a guardian's approval, stays with its mail and doesn't reach
  your calendar.
- **Even a stock calendar app with no Fauna involved works:** the server
  implements calendar auto-scheduling, so someone using only Apple Calendar
  pointed at your nest can organize meetings and answer invitations — the
  nest sends and processes the invitation mail for them.

Between Fauna users, invitations don't need mail at all — they travel
Fauna's own end-to-end-sealed channel, including across nests.

Anyone can invite you, but only an event's organizer can change or cancel
it on your calendar. If someone else — even another invitee who holds the
same invitation — sends a change or a cancellation, your Fauna app ignores
it and the event stays as it is.

## Connecting Apple Calendar, Thunderbird, DAVx⁵…

Your nest serves **CalDAV** (calendar) and **CardDAV** (contacts) — the same
protocols behind Fastmail and iCloud calendar sharing — read-write, with the
**same credential you use for mail** (one credential per app/device, created
in *Settings → Mail & Calendar*, shown once, individually revocable):

- **Automatic setup** (Apple Calendar/Contacts, DAVx⁵): enter
  `you@yourdomain.com` + the credential password; the apps discover the
  server via your domain's DNS records (they're part of the standard record
  set on the admin DNS page).
- **Manual setup**: server address `mail.yourdomain.com`, port 443. (Use
  this on Apple apps if discovery doesn't kick in — they're picky about it.)

Apple Calendar and Contacts (Mac + iPhone), Thunderbird, Evolution, and
DAVx⁵ (Android) are the tested set. Outlook and Google Calendar can't mount
external CalDAV calendars — that's their limitation — but as invitees they
work fully via the invitation flow above.

## Contacts: two different address books, on purpose

Fauna keeps two things other platforms blur together:

- **Contacts** — your social graph on the network: Fauna people, contact
  requests ("knocks") you accept, block, or dismiss. This is what messaging
  and sharing check against.
- **The Address Book** — ordinary contact cards: grandma, the plumber, the
  school. Names, phones, emails, addresses, birthdays — most of them not
  Fauna users at all.

The Address Book is what CardDAV serves: point Apple Contacts or DAVx⁵ at
your nest and your phone's contacts live on your hardware instead of
Google's, fully editable from those apps. The Fauna app itself shows your
address book read-only today; editing happens in the native contact apps it
syncs with. A card you add there shows up in the Fauna app's Address Book as
soon as it syncs, even while you have the Address Book open.

Events and address-book cards share one storage allowance with your mail.
The storage figure your mail app shows for your account counts mail, calendar
and contacts together, so a large calendar leaves less room for mail, and
deleting old events frees it again. Once the allowance is used up, a new
event or card is refused with a clear "storage is full" message and nothing
is saved. Making an event or card smaller and deleting things always work,
so you can make room again. An emailed invitation that arrives while you
are full still reaches your inbox, but it isn't added to your calendar.

## When someone tries to change your calendar

Invitations arrive from strangers — that's what invitations are. Changing an
event you already have is different: only the person who organized it can
cancel it or move it, and only the guest an RSVP speaks for can answer it.
Fauna checks that on every incoming message, and anything that fails the check
is refused: your event is left exactly as it was.

You're told when it happens. The Events page lists refused changes — what was
tried, to which event, who tried it, and why it was refused. A repeated attempt
shows as one entry with a count, not a pile of notices. When the message came
through someone else's server, the entry says which server vouched for the
sender, and it shows a name you know only when that name belongs to that
server, so another server can't pass itself off as one of your contacts. An
RSVP that arrives by email is held to the same rule: it counts only when the
mail system that delivered it could confirm who sent it, and only for the guest
that sender is. A refused one names the confirmed email address, or says the
sender could not be confirmed.
Nothing here needs an
answer from you; there is deliberately **no way to apply a refused change**,
because the question it raises — *is this person really the organizer?* — is
one your app already answered with better evidence than you have. Press
**Dismiss** once you've read an entry. If the same person tries again
afterwards, it comes back.

The list appears only when there's something on it, and what it shows comes
from your nest's own record of who sent the message — not from the message,
which a sender can write however they like.

## Whose eyes: the privacy model

Events and contact cards are **sealed to your keys before they're stored, on
every nest** — no nest ever has your calendar or address book lying readable
on disk, and there's no setup choice that changes that. When a standard app
connects over CalDAV/CardDAV, the bridge unseals items only inside that
authenticated session (the same model ProtonMail's bridge uses for mail apps
that don't speak encryption).

One practical consequence: your DAV credential password is a real key to
this data — treat the generated one with the respect it deserves, and prefer
generated over invented passwords.

And a deployment note worth knowing: **calendar + contacts without mail is a
genuinely quieter server** — it's just an HTTPS surface, with no
mail-receiving port open to the world. A home box can serve your family's
calendars with mail off entirely.

## On a home nest (no domain)

A home nest ([Set up a nest at home](nest-home-setup.md)) serves CalDAV and
CardDAV too, on its own port (8443 by default, changeable in the admin app).
There's no DNS on a bare IP address, so apps need manual setup
(`https://<the-box's-IP>:8443`), and Fauna's own apps handle the
self-signed-certificate story for you — third-party calendar apps may need
you to accept the certificate once. Fair warning: this path is younger than
the public-domain one — the pieces are tested individually, but the
end-to-end packaged proof is still being closed out.

## Where this stands today

| Feature | Status |
|---|---|
| Events page: calendars, agenda/month/week/day, recurring events, `.ics` import/export | **Available** (a few refinements still landing on some apps) |
| RSVPs (Going / Interested / Declined) visible to all attendees | **Available** |
| Per-event reminder | **Available** as a setting — reminder *notifications* on your devices are still landing; don't rely on them for wake-up calls yet |
| Invitations to/from Gmail, Outlook/Exchange, Apple users (iMIP over mail) | **Available** — proven live, including stock-calendar-only organizers |
| Fauna-to-Fauna invitations without mail (sealed, cross-nest) | **Available** |
| CalDAV read-write for Apple Calendar, Thunderbird, Evolution, DAVx⁵ | **Available** |
| CardDAV read-write for Apple Contacts, Thunderbird, DAVx⁵ | **Available** |
| DNS-based autodiscovery (enter address + password, done) | **Available** (Apple apps sometimes need manual `mail.<domain>`) |
| Contacts page: requests, accept/block, contact list | **Available** |
| Address Book view in the Fauna app (read-only) | **Available** |
| Editing address-book cards inside the Fauna app | **Planned** — edit via the synced native apps meanwhile |
| Calendar/contacts on a bare-IP home nest (port 8443) | **Landing** — works in testing; final packaged end-to-end proof still closing |
| Refused changes to your events, checked and reported | **Available** — the check runs for every app; the list is on the terminal app now and landing on the rest |
| Events & contacts sealed at rest on every nest, unconditionally | **Available** |
