---
title: Fauna Social — Privacy
description: This website sets no cookies, runs no analytics and loads nothing
  from third parties, and the app stores your content encrypted on your own
  server, apart from your profile and what you publish to everyone.
---

# Privacy

*Illustration: turtle*

The short version: we designed both this website and Fauna itself so there is
almost nothing to write a privacy policy *about*.

## This website

- No cookies.
- No analytics, no trackers, no fingerprinting.
- No third-party requests at all — fonts, images, and styles are served from
  this site itself.
- No accounts, no forms, nothing to submit.

These are static pages. We learn nothing about you when you read them. The
company hosting the site may keep standard technical access logs (IP address,
time, page requested) for security and capacity purposes, as virtually all
hosting does; we add nothing on top and run no processing of our own.

## The Fauna app

Fauna is built so that nothing of yours rests with us:

- **{legalName} runs no servers for users.** Your data lives on your own nest —
  hardware at home, or a small server hosted wherever suits you. We hold no
  account of yours, no credentials, and no copies. We maintain three servers of
  our own, used to test new releases of the nest software and to let reviewers
  from the app stores try the apps; only one of them, fauna.build, has accounts
  that people sign in to: reviewers and our own testers.
- **No telemetry.** The app sends us no usage data, crash reports, or
  identifiers.
- **Encrypted on your own nest.** Your content is stored encrypted on the
  server you control, apart from your profile and what you publish to everyone,
  which are public by design; messages between people on Fauna are end-to-end
  encrypted by default; a group may instead choose to be a community room, which
  its home nest can read. Your nest sees what any server that delivers your
  messages must: who you're connected to — your contacts, whom you follow, the
  groups you're in — and when and how much you send. Beyond your profile, what
  you publish to everyone and the mail and messages your nest passes to and from
  email and other networks, what any machine may read is a permission you
  granted and can revoke in the app. Two exceptions: a link standing on a line
  of its own in a post or a message is fetched for its preview by the nest of
  each person who reads it, so that nest learns the link's address; and someone
  who can open one of your messages, or a post only some people can open, can
  attach its text to a report, which the admins it goes to can then read.
- **Nothing pulling the other way.** Our [bylaws](/organization/bylaws) forbid
  charging users, selling advertising or user data, and paying members — so
  there is no business model quietly pressing against this page.

**The apps.** Fauna's apps send data to nine places and nowhere else. To your
own nest: your handle and your identity's public key; an identifier each
install of the app makes for itself, so your nest knows which of your devices
is online; what you create and the settings you choose — posts, messages,
files, calendar, contacts — stored encrypted there, apart from your profile and
what you publish to everyone, which are public by design; the sign-in of an
account you link on another network, which your nest keeps readable so it can
read that network for you, until you unlink it; the age range your app reports
and how it was established, where it carries one (with a request to join it
stays on the request, where your nest's admin sees it, and with an invite code a
range only declared is checked and not kept); on a supervised account, the
minutes of use a guardian's screen-time limit counts, with your device's
time-zone offset and, when the guardian has switched it on, how many items their
category rules filtered in each category, never what; from the web, Android,
iPhone, Linux, Windows and terminal apps today, a registration so your nest can
wake the app with a notification (an iPhone's is used only by a nest holding our
key for Apple's service, which no nest you run holds); and, if you look after a
child's account, which of their devices is yours. To your device's own services:
on the web app the browser's notification service, and on an Android phone the
notification app you chose, which carry those notifications encrypted; on an
iPhone Apple's notification service, which carries them encrypted apart from a
generic alert and the number of unread items; on an Android phone, or an iPhone
on iOS 26 and later, the store's age signal — from an iPhone with an attestation
of the app whenever the phone can make one, and from an Android phone with one
once nests can check it — read on the device and sent only to your nest; and, on
an iPhone or Mac, a copy of your identity key to Apple's iCloud Keychain if you
turn that on, which is off by default. When the app rents a server, manages your
domain or imports your old mailbox for you, to the provider you chose, with the
credential you gave it (on the web app, where a provider refuses calls from a
browser, such a call will pass through a relay of ours, which parses and keeps
nothing it forwards); and when you link a Bluesky account, your browser signs in
at the server that holds that account. And, from the desktop and terminal apps,
to the site that publishes our releases — only to learn whether a newer version
is out, when you ask and once at sign-in. And, directly, from the Linux, Windows
and terminal apps to your own other devices, and from those and the Mac app to
the devices of people you share a folder with — from the iPhone and Android apps
only what pairing in person itself exchanges, never a file. And, when you enter
a handle or retire a nest — and, on the web app, when it reconnects to your nest
— to a public DNS service (Cloudflare's), only to look up that domain's records.
And, when you load the pictures a message or mail links to from elsewhere, or
open an article from someone you follow or a nest's web page, to the sites that
host those pictures. And, when something you look up, open, send or share rests
on another Fauna nest — the person you type to reach, a post someone there
wrote, a conversation someone there started, a room or a folder whose home is
there, or a backup you chose to keep there — to that nest, only to look that
person up, to read what rests there, to store there the files you attach to a
conversation and, from the Linux app, your changes to a shared folder, encrypted
on your device, and, for the backup, to sign in there as you to set it up and
manage it and to read it back, still encrypted, to check it or to restore it.
And, from the Linux app, and from the terminal app on Linux and macOS, today, to
a program on the same computer that you have connected to your account and
approved to read or send in your conversations — a program serving an AI
assistant, say: only what that approval covers — your rooms and their messages
as it asks for them, the matches of a search it runs, and a message it sends as
you — never your conversation keys; the app notes on your nest each thing it did
for that program — what kind, in which room, and when, never what was read,
searched or said; and what the program does with what it reads is its own:
Fauna's program for AI assistants passes it on to whichever assistant you
connected it to, and to that assistant's provider if the assistant runs on a
company's servers. Nothing stays with us.

**Your nest.** The nest is the server program that you, or the admin of the
nest you joined, run — on a rented machine, on a computer at home, or as the
optional nest service in the Windows installer (and in the Mac installer, once
it is published). It talks only to the kinds of place named below, and it never
reports to us.

To other Fauna nests, only for what its users and its admin do there and for
the upkeep between nests — among them these, over a connection it signs with
its own key. When you
talk to, knock on, follow, invite, share a folder with or report someone whose
account is on another nest, and when someone there follows you, it sends that
nest what you sent — your messages, encrypted on your device,
your follows, your posts (for a post only some people can open, only its record
and the preview you made public, while its encrypted text stays on your nest
for the reader's app to fetch — or, for a post written for a community room
whose home is that nest, for that nest to fetch, encrypted on the way, and to
read as the room's home) and your reports, without your name — with the
routing details the delivery needs. When you open a post or a profile whose
home is another nest, or search a community room whose home is there, it asks
that nest for it on your behalf. To
the nest of someone whose post or message your nest's admin removed or
blocked, that decision; to a nest whose decision you appeal, your appeal; to
the nest of someone who reported a user of your nest, the report's outcome;
and, when an account is recovered under a new key, word of that to the nests
that know the old one. To a public nest you paired a home nest with, the sync
between them; to the nests you back up to, copies of your data that your nest
encrypts with your own key, which those nests cannot read, uploaded beside that
connection under a short-lived permission it obtains over it; and, if your
nest's admin agrees to keep a copy of someone's data for them, it fetches that
copy from their nest every quarter hour, still encrypted, so it cannot read
it, apart from what that person has published to everyone. Before its first
contact with any nest it reads that nest's public description, and it fetches
the video a post from another nest names, and the encrypted text of a post
written for a community room it is the home of, from the nest the post came
from; and about hourly and whenever it starts it asks the home nest of each
account from elsewhere it holds anything of whether that account was recovered
under a new key — all plain public reads, outside that signed connection; and
about hourly it swaps counts of reports and trends, never the reports
themselves, with nests it already talks to, and fetches from them the public
posts those trends name that it does not yet have.

To the networks you connect it to, each only once you turn it on yourself,
from your own app: with Bluesky, it looks your account up in the public
directory, reads your timeline, notifications and messages, sends the messages
you write there and the likes, reposts, replies and quotes you make on Bluesky
posts, and, if you turned that on, posts your public posts for you, apart from
posts you imported from another service — and if
your nest hosts
your Bluesky identity, it registers that identity in the public directory,
announces itself to Bluesky's relay and passes your app's requests on to
Bluesky's servers; with the fediverse, it delivers your public posts, apart from
posts you imported from another service, and word when you delete one, to your
followers' servers, and your follows, replies, quotes, likes, boosts and
messages to the servers of the people they are for, and fetches the profiles and
posts those exchanges name; with Nostr, it reads from the relays you list, or a
default few until you do, and, if you turned that on, publishes your public
posts to them, apart from posts you imported from another service — a quote you
write of a Nostr post goes there in any case, and a reply to one when you have
turned replies on. To the site of a link standing on a line of its own in a post
or a message, to fetch the link's title and picture, and to the sites that host
the pictures in posts from other networks when you view them — so that your
device need not. To the service that wakes your phone or browser, a notice
encrypted so that the service cannot read it — apart from Apple's notification
service, which reads a generic alert and the number of unread items, and which a
nest can reach only with our key for it, which no nest you run holds. To an app
you connect: a read of its public description when it first asks, and later word
that you changed what it may do. A plugin your nest's admin installs is fetched,
with its public description, from the publisher's address the admin gives, and
reaches, through the nest, whatever it was built to reach.

When mail is on — it is off on a new nest until its admin turns it on, which
the first claim of an internet-facing nest at its own internet domain does by
default, unless the server was created without email — your nest
delivers your mail to your recipients' mail servers, signed with your nest's
key for your address's domain, after reading the security policy each
recipient's domain publishes; sends the mail of a mailing list a user of your
nest runs; forwards your incoming mail to the address you asked it to forward
to, and sends the automatic replies you set up; tells the sender of a message it
could not deliver; checks each incoming message's sender against its domain's
public records and a public spam blocklist; exchanges with the domains you write
to and those that write to you, where they ask for them, the daily encryption
and authentication reports mail servers send each other; checks once a day
whether its own address is on public spam blocklists; and, once a day and
whenever your admin runs the mail diagnostics, makes a test connection to a
large mail provider. The virus and spam scanners that run beside it fetch their
updates from their own projects and from the blocklist service SURBL; check that
public blocklists answer, and ask them about the senders, domains and links in
incoming mail; and check fingerprints of each incoming message's parts against
the spam scanner's shared database.

To learn whether a newer version is out, it asks the site that publishes our
releases once a day and whenever it starts and, while it follows an update
channel, the public image registry every few minutes. The nest itself installs
nothing; on a nest run from our container image, a companion updater follows
the channel your nest's admin chose and installs the new image when one
appears, and the machine it runs on downloads the nest's and the scanners'
images from their registries when it is set up. It looks domains up through
the network it sits on; asks its own domain's nameservers directly about
records your nest's admin was asked to publish, and checks every few minutes
for the record that proves a web address of its own while one is being set
up; and asks its own domain's registry once a day when the registration expires
— finding it through IANA's public list or, when that list cannot be read,
through the public lookup service rdap.org. When it has a public name or a
public address, it gets the certificate that secures it from the certificate
authority Let's Encrypt, which connects back to check that the address is the
nest's. It sends us nothing of its own: no telemetry, no crash reports, no
identifiers; our own three servers are reached only as any other nest is, when
someone talks to someone there.

The filters you set for posts and messages run on your own device, answer to
you, and report to no one. On a supervised account the guardian's rules run on
the device too, and when the guardian has switched Notify on the app tells them
how many items their category rules filtered in each category — hidden behind a
warning or blocked — never what. The rules you set for your mail under Email
Filters are the exception: they run on your nest as each message from outside it
arrives. Incoming mail is checked for spam and malware on your nest before it is
filed, under thresholds your nest's admin sets, as every mail server does; your
own spam filter runs where your mail is opened — in the Fauna app, or in your
nest's mail bridge while a mail app of yours is signed in — never at delivery.
Where an admin removes content under a legal obligation, or blocks a reported
item, on their nest, a notice stands in its place, the author can appeal, and
the act is recorded; nothing is removed silently. The [terms page](/terms) says
who can block what.

## Questions

Privacy questions, or something on this page that isn't true anymore? Tell
us: {email}.

