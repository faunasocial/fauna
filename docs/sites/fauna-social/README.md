# The site's copy — edit here

These files are the **editable source of every word** on fauna.social's
story pages (the short edition won the two-edition bake-off on 2026-08-03 and
became the site; the copy left its historical `short/` subdirectory on
2026-08-04):

| File | Page |
|---|---|
| `index.md` | `/` — the pitch |
| `what.md` | `/what` — what you get |
| `who.md` | `/who` — who's behind it |
| `features.md` | `/features` — what works where (prose only; the table itself is generated) |
| `privacy.md` | `/privacy` — the privacy page (no nav entry, by design) |
| `official-apps.md` | `/official-apps` — which apps are Fauna's (prose only; the identity tables are generated from `installer/official-apps.json`; no nav entry) |
| `code-signing-policy.md` | `/code-signing-policy` — the code signing policy (no nav entry; linked from the footer and `/official-apps`) |
| `terms.md` | `/terms` — the terms and content guidelines (no nav entry; linked from `/privacy`) |

The pages import this markdown **at build time**, so there is no sync step:
edit a file here, run `just site-preview`, and the change is live at
`http://127.0.0.1:4321`. Never edit copy in the `.astro` page files — they
own structure and styling only, and any prose found there is a bug. The long
edition's text is archived.

## How the markdown becomes the page

One rule: **the body is the page.** Everything below the second `---` renders
in order as the page's prose — headings, links, `**bold**`, `` `code` `` and
tables all work, and nothing in the body is skipped or reordered. A
header-less table is written with an empty header row (`|  |  |` then
`|---|---|`) — the idiom `who.md`'s facts and `what.md`'s costs tables use.
So every word is ordinary markdown in ordinary reading order, editable in any
markdown editor; there is no copy in the frontmatter and none in the `.astro`
pages.

Three conventions give the designed pieces their shape, and that is all of
them:

- **The `#` heading and the paragraph under it are the page's opening** — the
  hero on `index.md`, the heading and lead elsewhere.
- **`*Illustration: fox*` on its own line under a heading** gives that heading
  its animal, and never renders as text. Under the `#` it is the page's own
  illustration (`who.md`'s hedgehog); under a `##` on `what.md` it makes that
  section one of the illustrated feature blocks. Names are listed in *The
  feature cards' critters* below.
- **A `> blockquote` is a boxed aside** — `index.md`'s closing
  call-to-action and `what.md`'s "The honest part". Start it with a
  `**bold**` line to give the box a heading.

**Frontmatter** (the `---` block) carries no copy at all — only metadata and
navigation: `title` (browser tab + search result) and `description` (search
snippet), neither ever visible on the page itself, plus `next` (the label and
target of the button at the foot of `index.md` and `what.md`).


## Tokens — org facts never appear literally

`{email}`, `{legalName}`, `{orgForm}`, `{registry}`, `{orgNumber}`,
`{founded}`, `{domain}`, `{repo}` and `{maintainer}` are filled at build time
from `sites/fauna-social/src/data/org.ts`, the single source of organization
identity — write the token, never the literal address, name, number, URL or
account handle, so those facts can't drift. Tokens work in the body and in
every frontmatter copy string alike (`sites/fauna-social/src/data/fill.ts` is
the one filler); `{email}` renders as a working mailto link, `{repo}` as a
link to the public source repository, and `{maintainer}` as the linked
account(s) of the code-signing team (`code-signing-policy.md`'s roles table).
Inline HTML also works in both places (e.g. `<strong>closed alpha</strong>` in
`index.md`'s `cta`).

## The feature cards' critters

An `*Illustration: …*` line names one of:

`badger` · `bear` · `beaver` · `bee` · `cat` · `duckling` · `fawn` · `fox` ·
`frog` · `hedgehog` · `mammoth` · `mouse` · `otter` · `owl` · `parrot` ·
`penguin` · `rabbit` · `snail` · `squirrel` · `turtle`

They are drawn in `sites/fauna-social/src/components/critters/` and registered
in `src/components/Critter.astro` — that registry is the list. An unknown name
fails the build loudly rather than rendering a blank.

Two things a new drawing must respect, both learned the hard way: it has to
read at the size the meadow uses it (down to 38px on a phone), and it must
hold its own against **both** backgrounds — the penguin left the front page
because near-black plumage disappeared into the dark theme. Mid-tone colours
from the palette in `src/styles/global.css` are the safe ground.

## The rules that bind this copy

Six corrections the association ruled bind every word here — don't overclaim the bylaws, no trademark
claims, the pre-App-Store constraints, … . Read them before rewriting anything,
and re-read them before deleting a sentence that looks over-careful: each one
is there because the "obvious" enthusiastic version was written and corrected
once already.
