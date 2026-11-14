# The owner's reading pages — what a page of the manual prompt injection check shows — target state

Owns: owner-reading-pages
Status: ratified — the three demands of § The owner's words are owner-ruled 2026-11-12, in guided bump round 26, verbatim; the design that meets them (§ The rules) is this doc's own, written the same day, and refutable: the owner's confirmation of it is asked; built the same day (§ Implementation status today)
Authority: owns **what a page the owner reads shows, what it leaves out, and what is handed to a session afterwards**, for the three readings of the manual prompt injection check: the reading of a pre-scan RED (`bump-round injection-check`), the owner's diff of a package move (`bump-round owner-diff`) and the whole-file clearance (`injection-clear pages`). Neighbours that do **not** own this: [`release-integrity.md`](release-integrity.md) § Reviewing untrusted source without being subverted → *The manual prompt injection check* owns the check itself — when it runs, the three passes of the RED reading, the verdicts, the pre-scan ledger and the clearance record, the limits on non-Latin text, the rule that an ask states its page count; [`dependency-bump-report.md`](dependency-bump-report.md) § A round of bumps owns the round and the verbs.

*Written as its own doc on 2026-11-12 because [`release-integrity.md`](release-integrity.md) stood at 220,973 B, 84% of the 262,144 B whole-file read ceiling, and the paragraph this extends is already its longest.*

## The owner's words

Guided bump round 26, 2026-11-12. On a page of the owner's diff of a path-handling crate, which showed a hunk header, a `<2 code lines left out>` marker and doc-comment lines each behind a diff sign, a line number, a bar and the comment marker: "You asked me to look at files like the following, which contain a lot of code. All the code should be stripped and you should only ask me about the comments and special chars (not \"lines containing comments and special chars\"). The code is noise here" … "I want this without any special chars".

The same minute ("Also:"), on a second page of the same diff: a hunk whose every shown line was unchanged context — two code lines shown whole because each held a one-character string, two doc comments nobody had changed, and a left-out marker between each pair.

On the reading of a pre-scan RED, after 100 pages had been opened: "this is too much and too difficult to review manually … the rest have too many non-ASCII characters."

## What a reading is for

A reading rules out prompt injection: text in a dependency that would instruct a session that later reads it, or — for a pre-scan RED — text that shows the package means to. The owner is the one reader no text in a dependency can instruct. A reading says nothing about what code does.

Two of the three readings end in a **hand-off**: after the owner's diff a session may read that change's diff, after the whole-file clearance it may read the cleared files. For those the one invariant is: **every character handed to a session is either text the owner approved on a page, or code** — a character the lexer read as code, printable ASCII, no quote, no identifier that reads like a sentence. Each rule below keeps that invariant by one of two moves: it shows the text, or it **withholds the text from the hand-off as well**. Nothing is ever handed over that was only counted, with one stated exception (rule 1, a string with no letter in it).

The reading of a pre-scan RED ends in no hand-off: its clearance lets a bump be taken and lets no session read anything. Its two guarantees are the check's own: nothing on a page can hide or reorder text (a page is printable ASCII), and nothing flagged goes unshown or uncounted.

## The rules

### 1. A page shows text, never lines of code

Applies to the owner's diff and, for a code file, to the whole-file clearance.

- **A text part** is a comment, a doc comment, a string, a character literal, a regular expression, a template literal's text, or an identifier of five words or more. The lexer finds text parts as spans of characters, where today it marks the lines they touch. A file that is not code, or that the lexer cannot read to its end, is text in every line, as today (fail closed).
- **The page shows the words of each text part and nothing else.** No diff sign, no line number, no gutter, no hunk header. The delimiters are removed: a comment's opener and closer and the leading star of a block comment's inner lines, a string's quotes and prefix. What stays is the part's own characters, line by line, leading and trailing spaces trimmed, wrapped by indentation alone. Parts on consecutive lines form one paragraph; a blank line parts paragraphs.
- **Code is never shown.** It is counted once per file, in the file's heading: lines added, lines removed. There is no left-out marker inside the text.
- **A line's code must still be code.** Outside its text parts a line may hold only printable ASCII with no quote character. A line that fails this is shown whole, as text.
- **A text part with no letter in it is counted, not shown** — `"."`, `"/"`, `": "`, `"\n"`, `""`. Precisely: printable ASCII, no letter but the letter of one of the escapes `\n` `\t` `\r` `\0`, and no backslash but those and `\\` `\"` `\'`. This is the one thing handed over uncounted-as-read, and it adds nothing a session could not already meet: a run of digits and punctuation is what code is made of, and the code of the diff is handed over whole.
- **Removed and added text are told apart in words.** Under each file's heading: `Removed text`, then its paragraphs; `Added text`, then its paragraphs. A whole-file clearance has the one heading `Text`.
- **A file's heading** is its path, whether it was added, removed or changed, and its counts: code lines not shown, text lines left out as unchanged (rule 2), strings with no letter.

### 2. Text the old version already holds is neither shown nor handed over

Applies to the owner's diff.

- **Unchanged text.** A line of a text part on the added side is unchanged when the same words — delimiters removed, runs of spaces made one — stand in a text part anywhere in the old version's archive; on the removed side, when they stand anywhere in the new version's. A context line's text is unchanged by definition. So a moved, re-indented or re-wrapped comment, a constant's doc comment in a reordered table, the old entries of a changelog and the untouched lines of a manifest are all unchanged.
- **Unchanged text is counted on the page and replaced in the hand-off.** The diff handed to a session carries the fixed marker `<unchanged text>` where the words stood, and one line `<N lines of unchanged text>` for a run of lines holding nothing else. The session reads the changed text and all the code; it does not read text the owner was not shown. The handed diff is a reading aid, not a patch to apply.
- **Why withholding, not trust.** The old version is in the lock because the scanner passed it, not because anyone read it. Text counted as "already there" and then handed over would let a package plant text in one version and move it in the next. Withheld, it reaches nobody, so how finely "the same words" is matched is a question of page count only, never of safety.
- **Machine-written files are named and counted, like a binary file.** `Cargo.lock`, `.cargo_vcs_info.json`, `package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock`, `pnpm-lock.yaml`, `deno.lock`, `bun.lock` and any `*.map`, by file name, anywhere in the archive: on no page and not in the hand-off. What a round needs from them it has from its own resolver. The list is this doc's; a name is added here first.

### 3. Bulk non-Latin text is summarized

Applies to all three readings unless a bullet says otherwise.

- **A word, not a letter, is the marker.** Two or more letters of one non-Latin script in a row are one marker: the script, the count, the first and the last letter — `[Cyrillic: 12 letters, U+041F CYRILLIC CAPITAL LETTER PE to U+0442 CYRILLIC SMALL LETTER TE]`. A single such letter keeps today's full marker, so a look-alike letter inside a Latin word stays visible.
- **A long run shows its ends.** A run of more than 12 words of one script shows its first three and last three word markers and, between them, `<N more words of <script>, M letters>`.
- **The limits hold on every page.** The check's limits (more than one Chinese, Japanese or Korean character in a row; three words or more of an alphabetic script) are marked wherever a run crosses them, and the page's header counts the runs over the limit on it.
- **In a hand-off reading, a run over the limit is withheld.** The owner's diff and the whole-file clearance do not ask the owner to judge a sentence they cannot read: the run is counted on the page and replaced in the hand-off by `<N words of <script> not handed over>`. A session reviewing a change has no use for translated messages, and the standing default for such a run is deny. A run under the limit is shown and handed over, as today.
- **In the RED reading, a long line is a window.** The scanner reports where on its line each finding is. A flagged line of more than 400 characters is shown as the 40 words before and after that place; the rest of the line is one counted marker — `<N more characters of this line not shown: a ASCII, b letters of <script>, …, c odd characters>`. A context line that long shows its first 40 words and the same marker. A finding with no place shows the line's start and says so.
- **In the RED reading, the third pass groups as the second does.** Per package and script: the runs under the limit are one item — their count, up to 12 places, three of them with words around; the runs over the limit are an item each, longest first, up to 20, and past that one item with the count, the words and the files they are in. Over the limit keeps deny as the default answer, for an item and for a group alike. So a table of code points — letters standing alone between punctuation — is one item per script, and a file of translated messages is at most 21.
- **Nothing flagged goes unshown or uncounted.** Every distinct odd character is still its own item in the second pass, over the whole flagged text and not the window alone. The coverage check is exact to the character for a windowed line (shown plus counted equals the line's length) and to the item for a grouped pass.

## The coverage check

Each reading keeps a check that counts from the written pages, never from the list they were written from, and refuses to offer the reading on any difference: per file, the text lines the lexer found against the text lines shown, plus those counted as unchanged, with no letter, or withheld; per file, the diff's lines against code lines counted plus lines holding text; for the RED reading, the items and characters of rule 3. A page stays printable ASCII plus newline.

## What is given up, and what bounds it

- The owner no longer sees where in a file a text part stands, nor the code around it. A deny is of the whole reading, as before, so nothing depended on the place.
- The session no longer reads unchanged text or foreign-language sentences in a change. Both are outside what it reads a change for.
- For a pre-scan RED, the owner reads the place of the finding and a count of the rest of a very long line, where before every character was on some page. Every distinct odd character and every run over the limit in that rest is still an item of its own pass.

## Implementation status today

**Built (2026-11-12).** The three renderers follow § The rules and § The coverage check. Where the code fills in what the rules leave open: a text line is one text part's characters on one source line, so a line holding two strings is two text lines; in the owner's diff the files follow one another on the pages, each file's heading numbered (`File 3 of 23`) so the coverage check splits the written pages by file; a withheld run stands on the page as `<OVER THE LIMIT, not handed over: N words of <script>>`, counted in the page's header with the runs over the limit; a run of one unchanged line keeps its line in the hand-off with `<unchanged text>` in it, two or more are one `<N lines of unchanged text>` line; a windowed line's counted rest is two marker lines, `<characters a-b of L shown, N more not shown>` — the one the coverage check adds up from the page — and its classes, whose sum the renderer checks as it writes them; a carriage return outside a text part fails rule 1's code test, so its line is a text line (fail closed, as before). Measured on round 26's stored inputs (counts only, the tools' own output): the path-handling crate's diff, 25 pages and 8,063 words on the first renderer, is 3 pages and 2,449 words; the object-file crate pair (0.40.0 / 0.39.1), 307 pages and 213,932 words, is 28 pages and 25,486 words. Not yet read by the owner: the confirmation of the design's readings is the open ask above.
