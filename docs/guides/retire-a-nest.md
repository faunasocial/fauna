# Retire a nest — take a server down from the app

> **If the app set up your nest, the app can take it down again.** It finds the
> servers Fauna created in your cloud account, removes the DNS records that point
> at the one you pick, and then deletes the server — no provider dashboard, no
> separate trip to your DNS settings. It works even when the nest itself no
> longer answers, which is usually why you want it gone.
>
> This is for a nest you set up with *[Set up a nest from the app](nest-app-setup.md)*.
> A server you created by hand (or one from before 8 July 2026, before Fauna
> started marking the servers it creates) is retired from your cloud provider's
> own dashboard.

---

## Before you start

- **Deleting a server destroys everything on it** — its disk, and every
  account's posts, mail and files. It cannot be undone. The only way back is
  from a backup, so if the data matters, check *[Cloud backup](cloud-backup.md)*
  first. The app does not insist — you may be retiring a server you never used.
- **Have your cloud provider's token ready.** The app asks for it each time and
  forgets it when you leave the page; it is never saved. It is the same kind of
  token you gave the app when it created the server.

## Where to find it

- **Signed in as the admin:** open the admin area, go to **Nest**, and choose
  **Retire this server…** — it sits beside **Factory reset**. The two are
  opposites: a factory reset wipes a server you keep; retiring destroys the
  server.
- **When your nest can't be reached:** the app's "Couldn't reach your nest"
  screen has a **Retire a server** button.

## Step by step

1. **Pick your cloud provider and enter your token**, then choose **Verify
   credentials**. For a provider you sign in to (rather than paste a token),
   type the provider's address and choose **Sign in at the provider…**.
2. **Pick the server.** The list shows only the servers Fauna created in that
   account — a server someone else made there is never shown. Each row shows the
   server's name, its address, and the domain it served, when the app could
   confirm it (the domain's address has to match the server's; the app never
   guesses from the name). The server you are signed into is labelled as such.
3. **Choose Delete this server…** The next screen spells out what goes: the
   server, its provider and address, every domain it served, which DNS records
   will be removed first, and which you'll have to remove yourself.
4. **Type the server's name exactly**, then choose **Delete the server and clean
   up DNS**. The button stays greyed out until the name matches.
5. **Watch the two steps.** First the DNS records that point at the server are
   removed — only those whose value is this server's address, so any record you
   have pointed elsewhere is left alone. Then the server is deleted.
6. **Done** shows what's left for you to remove by hand, with a button to copy
   the list.

## If something goes wrong

- **The DNS step fails:** the server is **not** deleted. Choose **Try again**, or
  **Delete the server anyway** if you'd rather clean up DNS yourself. If you do,
  remove the listed records straight away: once the server is gone your provider
  can give its address to someone else, and a record still pointing there would
  point at them.
- **The app closes part-way:** open the retire page again. The server is still
  listed until it has actually been deleted, and running it again finishes the
  job — anything already removed is simply found gone.
- **You back out** of the confirmation or the page: nothing happens, and the
  choice is forgotten. You'll have to type the name again next time.

## What's left for you to remove by hand

Some records share their names with things that aren't Fauna's — the mail
policy records (SPF, DMARC, MTA-STS, TLS reporting). They don't point at the
server and do no harm left behind, so the app lists them rather than touching
them: remove them when convenient.

If the app has no way into your domain's DNS — your cloud provider doesn't host
DNS, and you didn't set up Fauna-managed DNS — nothing is removed automatically.
The list then starts with the records that **still point at the server's
address**; those are the ones to remove first, ideally before the address is
reused.

## Your domain's transfer code

If you're moving your domain elsewhere, select its server in the list. Where
your provider can hand out the domain's transfer code, a **Get the domain
transfer code** button appears; the code shows up with a button to copy it. If
the registry still locks the domain against transfer, the app shows the date
the lock lifts — ask again after that. For other providers, the row tells you
to get the code from your registrar's own dashboard. Fetching the code deletes
nothing.

## After retiring

- Retiring the server you were signed into ends that nest: the app goes back to
  its start screen.
- Your nest's saved recovery key is **not** thrown away. If you later want the
  same nest back somewhere else, from your backups, *[recovering a lost
  nest](cloud-backup.md)* still works.
