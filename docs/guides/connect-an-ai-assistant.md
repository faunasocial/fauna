# Connect an AI assistant

> **Status — this guide describes Fauna as it is taking shape.** The feature
> is built and tested; the [table at the end](#where-this-stands-today) says
> exactly what is available now, what is landing, and what is planned.

---

> **What this is.** How to let an AI assistant work in Fauna as you — read
> your feeds, post, keep up with notifications, read and reply in your
> conversations, tidy your profile, and, if you run a nest, do an admin's
> daily work — through one permission you grant once and can take away in
> one place.
>
> **Who it's for.** Anyone who uses an AI assistant on their own computer and
> wants it to reach their Fauna account without handing it a password or a
> key. There is no password to hand over, and the key never leaves your Fauna
> apps.

---

## How it works

Fauna has a small program, `fauna-cli`, that an AI assistant runs on your
computer. Assistants talk to such programs over the **Model Context
Protocol** (MCP), so any assistant that can start a local MCP server — a
desktop assistant app, a coding assistant, an assistant you run yourself —
can use it. The assistant sees a set of tools ("list my feeds", "post
this", "read that conversation"); each tool does exactly what one of your
Fauna apps would do for the same request, and nothing a Fauna app would not.

The program is not a Fauna app and holds none of your keys. It connects the
way any outside app connects to Fauna: it asks, you approve the request in
one of your Fauna apps, and from then on it acts within the permission you
approved. The approval card names it **Fauna for AI assistants** and lists,
in plain words, everything it will be allowed to do. You can disconnect it at
any time from the same page where you approved it.

## Before you start

- **A Fauna account**, and any one of your Fauna apps signed in — your phone
  is enough. You need it once, to approve the connection.
- **The `fauna-cli` program** on the computer where the assistant runs. It is
  not yet part of any release download; for now it is built from the source
  code, the same way the terminal app is built in its *Building from source*
  section (the package is `fauna-cli-device`, the program `fauna-cli`). The
  plan is to ship it beside the terminal app.
- **An assistant that can run a local MCP server.** Two kinds work: one that
  starts the program itself, and one that connects to a web address on the
  same computer. An assistant that runs only in the cloud and can connect
  only to an address on the public internet cannot use this yet (see the
  table at the end).

## Setting it up

1. **Add the program to your assistant** as a local MCP server. Most
   assistants take the command and its arguments in their settings; the
   command is `fauna-cli` and the one argument is `mcp`:

   ```json
   { "command": "fauna-cli", "args": ["mcp"] }
   ```

   If your assistant connects to an address instead, start the program
   yourself in a terminal with `fauna-cli mcp --listen`. It prints one line,
   an address such as `http://127.0.0.1:49152/mcp`; paste that into the
   assistant. The address answers only programs on the same computer.

2. **Ask the assistant to connect to Fauna.** It asks for your handle — your
   name on the network, not a secret — and then shows you a short code.

3. **Type the code into Fauna.** In any of your Fauna apps open **Settings →
   Connected apps**, type the code under **Connect an app**, and the approval
   card opens. It names the app, **Fauna for AI assistants**, lists each
   thing it will be allowed to do, and tells you that whatever you allow here
   reaches the assistant you connect. Read the list, then **Approve**.

4. **Done.** The assistant tells you it is connected. The connection is
   stored on that computer, so the next time the assistant starts the
   program it carries on without a new code. Where the computer has no
   keychain or secret store it can use, the connection is stored on its
   disk, protected by file permissions alone, and the assistant tells you so
   when you connect.

**More than one account.** Connect again with another handle. When the
assistant holds several accounts, its requests name the account they are
for, and it asks you which one when it cannot tell.

**If you run a nest.** Tell the assistant you administer your nest when you
ask it to connect. The approval card then also lists the admin permissions,
page by page. Only an admin's approval counts for those; from a member's
account the request simply never completes.

## What it can do

Each item below is one line on the approval card. You approve the whole list
or nothing; there is no partial approval.

- **Read your feeds** — the feeds you built, their posts, the public posts on
  your own nest, and what is trending there.
- **Post publicly as you, and delete the posts it made.** It can post only
  to everyone — never to subscribers, buyers or a room — and it can take
  back only its own posts. The posts are signed by a key your nest keeps for
  posting that your approval set up, never by your own key.
- **Read any public profile, and edit yours** — your display name, your bio
  and your links. It cannot change your pictures.
- **Read your notifications**, including how many are unread, and **mark
  them read**. It does not know your muted words, so a notification your
  apps would fold away is shown to it in full.
- **Read and reply in your conversations** — list your rooms, read and
  search their messages, and send a message as you into one. This works only
  while a Fauna app is open and signed in on the same computer: that app does
  the reading and the sending, and your conversation keys never leave it. It
  cannot invite, remove or rename anyone, and every room stays the kind of
  room it was.
- **For an admin, the daily work of running a nest** — admitting members,
  invite codes and requests, suspending and reinstating, tiers and quotas,
  the nest's settings and services, logs and the audit record, reports and
  blocks, domains and DNS, mail settings and reports, forwarders, and the
  calendar, contacts and files switches. Nothing that widens authority or
  touches a key: it cannot add or remove an admin, approve a bridge, mint,
  rotate or reveal any key, certificate or secret, rename the nest's domain,
  clear a handle, delete a user, or reset the nest.

## What it can never do

Some things stay with you, on one of your Fauna apps, however you set the
assistant up:

- **Your identity.** It never holds your identity key, your recovery kit or
  any recovery secret, and it is never asked for one. Creating, importing or
  recovering an identity, and changing your handle, are done in your apps.
- **Your devices, sessions, nests and backups**, and the **Connected apps**
  page itself — including its own permission. It cannot widen what it was
  allowed to do.
- **Deleting your account** or resetting a nest.
- **Posting to a paying or closed audience**, and anything in your mail,
  calendar, contacts or files — those are not among its tools today (see the
  table at the end).

Where it does act for you — posting, editing your profile — it never sees
your key: your nest signs for it, and the Connected apps row counts the times
it did.

## What the assistant's provider sees

Whatever you allow on the approval card, the assistant you connect can read
or do — and if that assistant runs on a company's servers, that company's
service can read or do it too. The program does not change this: it reaches
exactly what you approved, and hands it to whichever assistant you chose.
An assistant that runs entirely on your own computer keeps everything on it.
Choose the assistant with that in mind, and keep the permission to the
account you mean to share.

## Disconnecting

Open **Settings → Connected apps** in any of your Fauna apps. The row
**Fauna for AI assistants** says what the assistant may reach, when it was
last used, and — once your nest has signed anything for it — how many times
it did and what the most recent action was. **Disconnect** on that row,
confirm, and the very next thing the assistant tries is refused. Nothing on
the assistant's side can undo that.

## Using it yourself at the terminal

The same program works without an assistant. Run `fauna-cli connect`
followed by your handle. It shows a short code: type that into **Connect an
app** in one of your Fauna apps and approve it, and the command finishes once
you have. After that, `fauna-cli feed list` lists your feeds,
`fauna-cli post create --text "…"` posts publicly as you, and
`fauna-cli --help` lists every command. Add `--help` after any command to see
what it takes. Each command can do exactly what an assistant you connect
could do, and nothing more. When its output goes to another program rather
than your screen, or when you add `--json`, it prints JSON for scripts. A
command that can't be undone, such as `fauna-cli post delete`, asks first. In
a script it runs only when you add `--yes`.

## Where this stands today

| Feature | Status |
|---|---|
| Connecting an assistant with a code typed into **Connect an app** | **Available** on all seven apps |
| Reading feeds, posting and deleting its own posts | **Available** |
| Reading profiles and editing your own name, bio and links | **Available** |
| Reading notifications and marking them read | **Available** |
| Reading, searching and replying in conversations while a Fauna app is open on the same computer | **Available** |
| The admin's daily work, for an admin who asks for it | **Available** |
| Assistants that start the program themselves, and assistants that connect to an address on the same computer | **Available** |
| Using the same commands yourself at the terminal, with `--json` for scripts | **Available** |
| Getting the program from a release download | **Not yet** — built from source for now |
| Staying connected between launches on a computer with no keychain or secret store it can use | **Available** — the connection is stored on its disk, protected by file permissions alone, and the assistant says so when you connect |
| Mail, calendar, contacts, files and search for an assistant | **Planned** — one permission at a time, each a new line on the approval card |
| An assistant that runs only in the cloud and connects to a public address | **Planned** — a form of the program that runs on your nest |
