# Fauna documentation

Fauna is open-source software for social communication — an app for every
device and a small server you control — where you own your data, control your
experience, and no single actor holds power over the network. The
[project README](../README.md) explains the principles and architecture in five
minutes.

This page is the front door to everything else.

## I want to try Fauna

Fauna is in closed alpha: everything below works, but there are **no packaged
releases yet** — trying it means building from source, and there is no public
server to join. You bring (or rent) the box.

1. **[Getting started](guides/getting-started.md)** — create your identity,
   claim or join a nest (a Fauna server), and find your way around the app.
2. **[Installing the app](guides/install.md)** — what you can build and run
   today on each platform: web, Linux, macOS, iOS, Android, Windows.
3. **[Set up a nest from the app](guides/nest-app-setup.md)** — the easiest way
   to get your own server: the app builds it in your cloud account, no terminal.
   Prefer to do it yourself? **[Set up a nest on the internet](guides/nest-internet-setup.md)**.

## Guides

The **[guides](guides/)** directory holds task-oriented documentation written
for users, not developers:

| Guide | What it covers |
|---|---|
| [Getting started](guides/getting-started.md) | First run: identity, joining a nest, the basics. |
| [A tour of the app](guides/app-tour.md) | Every screen, from Feeds to Settings — the same app on every platform. |
| [Installing the app](guides/install.md) | Building/running each app today. |
| [Your identity, devices & recovery](guides/identity-and-devices.md) | The one-key, no-password model: keeping the key safe, adding and losing devices, what is (and isn't) recoverable. |
| [Set up a nest from the app](guides/nest-app-setup.md) | The no-terminal path: the app builds the server in your own cloud account, and your provider bills you directly. |
| [Set up a nest on the internet](guides/nest-internet-setup.md) | The same server, built by hand with Docker: your own domain, real certificates, email. |
| [Set up a nest at home](guides/nest-home-setup.md) | A nest on a machine on your own network — never reachable from the internet. |
| [Link a home nest to an internet nest](guides/nest-relay-setup.md) | Connect the two so your email arrives at home instead of on a rented server. |
| [Running a nest for friends & family](guides/nest-for-friends-and-family.md) | The admin's guide: letting people in, capacity, moderation, kids' accounts, keeping everyone's data safe. |
| [A tour of the admin area](guides/admin-tour.md) | Every admin screen: users, tiers, mail policy, DNS, and the rest. |
| [Your own cloud](guides/your-own-cloud.md) | Replacing Dropbox/iCloud/Google Drive with Fauna — an overview series covering [file sync](guides/cloud-sync.md), [backup](guides/cloud-backup.md), [photos](guides/cloud-photos.md), and [family sharing](guides/cloud-sharing-family.md). |
| [Own your email](guides/own-your-mail.md) | Self-hosted mail at your own domain, with the deliverability liturgy automated — plus IMAP for your regular mail apps. |
| [Calendar and contacts](guides/calendar-and-contacts.md) | Events and invitations that reach Gmail/Outlook users; CalDAV/CardDAV for Apple Calendar, Thunderbird, DAVx⁵. |
| [Bluesky & Nostr from your nest](guides/bridges-bluesky-nostr.md) | Bridges to the wider social world, so self-hosting isn't an empty room. |
| [Take a nest down from the app](guides/retire-a-nest.md) | Retire a server the app created in your cloud account, DNS records included. |
| [Bring your posts from Facebook or Instagram](guides/import-your-social-archive.md) | Import your old posts, photos and events from the export archive those services give you. |
| [File sync FAQ](guides/file-sync-faq.md) | Working together in shared folders: what is available now, what is landing, what is planned. |
| [Who can see what](guides/who-can-see-what.md) | Every way your data reaches the outside world — per feature, who can see it, and what stays private. |

Some guides describe Fauna as it is taking shape: each one states plainly, up
front or in a closing table, what is available now, what is landing, and what
is planned.

## Design documents

**[`docs/goal/`](goal/README.md)** is the project's internal design
specification — the target state of every feature and subsystem, published for
transparency. It is written for the people building Fauna, not for users:
expect implementation detail, target-state prose ahead of the code, and open
questions. Start at its [registry](goal/README.md) if you want to know how a
particular subsystem is designed.

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the build workflow, the
contribution process, and how this published repository relates to upstream
development. Security reports: [SECURITY.md](../SECURITY.md).
