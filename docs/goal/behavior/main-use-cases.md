# Main use cases: the catalogue, the element score and the journeys

Owns: main-use-cases
Status: ratified — the process is the owner's direction of 2026-11-17 ("look at the main use cases and see which UI elements are involved in those, and score each UI element according to use case involvement"; "there need to be e2e tests for the common tasks a user would do"); the weights and bands in § The score are best-guessed settings, tuned by editing this doc
Authority: the catalogue of main use cases (its schema and the rules a use case obeys), the score a ui.yaml element gets from it, what a score may and may not decide about placement, and the rule that every main use case is one journey test. The catalogue's ROWS are data → `tests/e2e-unified/use-cases.yaml` (never restated here). Element IDs, per-page element scope and primary-nav membership → `tests/e2e-unified/ui.yaml` (a placement change is a ui.yaml change under its rule A). The navigation model and the layout a window size gets → [`../ui/README.md`](../ui/README.md) § Navigation model. How a journey drives the UI → [`../architecture/e2e-conventions.md`](../architecture/e2e-conventions.md) convention 8; the systematic walks beside the journeys → [`../architecture/e2e-systematic-ui-walks.md`](../architecture/e2e-systematic-ui-walks.md). The store reviewer's walk and its accounts → [`../architecture/installers/store-review.md`](../architecture/installers/store-review.md).

## Goal

What sits on a page's first view, what sits one step behind it and what a narrow window keeps are decided from what people do with the product, measured the same way on every page and re-measured when the product changes. One list of main use cases is the source for two things: the score of every UI element, and the set of end-to-end journeys that prove each common task can be done through the UI. The apps offer the same placement on all 7 apps; nothing here is per app.

## The catalogue

**One file, `tests/e2e-unified/use-cases.yaml`, lists the main use cases.** It sits beside ui.yaml because every step names a ui.yaml id and every use case names a test of that suite. A use case is something a person sets out to do and can tell they have finished ("send a mail to someone outside the nest", "add a calendar event"), never a screen or a feature.

Each use case carries:

| Field | Meaning |
|---|---|
| `id` | kebab-case, stable; the name the journey and the score output use |
| `title` | the task in the user's words |
| `actor` | `new-user`, `user`, `admin`; a use case met by two actors is two use cases |
| `reviewer` | `true` when a store reviewer does it during a review; a flag, not an actor: a reviewer is a new user with a prepared account |
| `frequency` | 5 daily, 4 weekly, 3 monthly, 2 a few times a year, 1 once per account or per nest |
| `earliness` | 3 met in the first session, 2 in the first week, 1 later |
| `steps` | the ordered gestures of the shortest path a person takes, each `{page: <ui.yaml page or onboarding stage>, element: <ui.yaml element id>}`, starting from the app's landing page after sign-in (a `new-user` use case, and an admin claiming a nest, starts at the first onboarding stage) and including the navigation steps; a gesture on a primary-nav tab or a shell rail row (`settings-nav-row[<page>]`, `admin-nav-row[<page>]`, `account-settings-link`) is written on the pseudo-page `navigation` |
| `journey` | the path of the test that walks it (§ The journeys) |

**Rules a use case obeys.**

1. Steps are gestures on elements the person acts on: a tab, a button, a field, a row. An element they only read is not a step.
2. The path is the one the product offers today, on tui (the lead app). A step that exists on tui only as a declared absence is written from web.
3. The catalogue stays small: between 10 and 25 use cases. It lists what most people do, not everything that can be done; the systematic walks cover the rest of the surface.
4. A frequency or an earliness is a judgement, written once in the file and reviewed like any other line. No usage telemetry exists or is planned as its source.
5. Adding, removing or re-weighting a use case is an edit of the data file; it moves scores, and a moved score moves no element by itself (§ What a score decides).

## The score

**A use case's weight is `frequency × earliness`** (1 to 15). A daily task met in the first session weighs 15; a once-per-account task met in the first session weighs 3; a yearly task met later weighs 2.

**An element's score on a page is the sum of the weights of the use cases with a step naming it on that page**, each use case counted once however many of its steps name the element. An indexed element (`contact-row[<id>]`) is scored as its base id. A page's scope for a step is its `elements`, `optional_elements`, `errors`, `platform_elements`, the elements of its `components` (their `children` included) and of its `sub_pages`, plus the `global` elements every authenticated page carries.

**A page's score is the sum of the weights of the use cases with a step on it**, and a primary-nav destination's score is the sum of the weights of the use cases with a step on its page or on a page reached through it, each counted once: a step counts for the destination of the last tab step before it, and a use case that does not start in onboarding is on the landing page (the feed) until its first tab step.

**Bands.** Within a page, an element is `primary` at a score of 8 or more, `secondary` from 1 to 7, and `unscored` at 0.

**Not scored — contract elements.** `page-heading`, `error-message`, `warning-message`, `info-message`, the back, close and cancel affordances (ids ending `-back`, `-back-button`, `-close-button`, `-cancel`, `-cancel-button`), empty states (ids ending `-empty`, `-empty-state`, `-empty-message`) and the primary-nav tabs' own existence are owed by other rules and are listed by the script as `contract`, never as `unscored`.

**Not scored — display elements.** An element whose ui.yaml type is one a person reads (`text`, `view`, `image`, `label`, `container`) and that no step names is listed as `display`, never as `unscored`: rule 1 makes it no step, so its absence from every use case says nothing about its placement. A step that does name one (a tappable `view` row) scores it like any other element.

**The score is computed by one script, never written by hand**. Its contract:

- Input: the catalogue and ui.yaml, both read whole by the script (a session reads the output, never ui.yaml).
- It refuses, naming each offender, when a step names an element id that ui.yaml does not define, a page that does not exist, an element outside that page's scope (§ The score, above), a use case with no `journey` or whose journey file does not exist or does not carry the use case's marker and its closing assertion, a marker or closing assertion naming no use case, a malformed field (`actor`, `frequency`, `earliness`, `reviewer`, a non-kebab `id`, a duplicate `id`, no `steps`), or a catalogue outside the 10–25 bound. A use case listed under the catalogue's `pending_journeys` — no test walks it by gestures yet — may name a journey file that does not exist yet or that reaches a step by a shortcut and so carries no marker, and an entry whose journey carries its marker is refused; the script prints the list on every run, and the tolerance is removed once the list is empty.
- Output, deterministic text, three views: with no argument, the destinations by score and the catalogue's weights; with a page, that page's elements by score with their band and the use cases behind each number; with `--unscored`, every non-contract element no use case touches, by page.
- With `--check` it runs the refusals alone, and the UI lint runs it first.
- It is pinned by its own test on a fixture catalogue and a fixture ui.yaml: the weight, the once-per-use-case count, the indexed-id fold, the three bands at their edges, the contract list and every refusal.

## What a score decides

A score is evidence for a placement proposal; it is never a placement. Every move of an element between surfaces is a ui.yaml change, proposed per page and approved by the owner under ui.yaml's rule A.

1. **First view.** A `primary` element is on its page's first view at every layout class: visible without a scroll, a menu or a sub-page.
2. **One step behind.** A `secondary` element is reachable from the first view in one gesture (an overflow menu, a disclosure, a sub-page row) and may be on the first view where the layout has room.
3. **Unscored is a review list, not a verdict.** An `unscored` element is a candidate to move behind an overflow, to a sub-page or to Settings. It is never removed from the product on its score, and three kinds stay where their own rules put them whatever they score: an element the person needs when something is wrong (recovery, sign out everywhere else, block and report), an element another goal doc places by rule, and the one unblocking step of a blocked surface ([`../ui/README.md`](../ui/README.md) § Offer only what is possible).
4. **Destinations.** The compact layout's primary bar carries the highest-scoring destinations and the overflow hub lists the rest in score order; the expanded sidebar lists every destination in score order. Which destinations exist stays ui.yaml `navigation`'s.
5. **A compact window keeps `primary`.** When a layout class has room for less, `secondary` elements go behind the page's overflow before any `primary` element does.
6. **A proposal is per page and whole.** It states, for one page, each element's score and band, where it sits today and where it would sit, on all 7 apps; it is put to the owner as one ask per page (several small pages may share one ask), and the ui.yaml edit, the per-page goal doc and tui land after the answer, the other six apps in trickle-down.

## The journeys

**Every main use case is one journey test, and every journey of a main use case says which.** The test walks the use case's steps through the app UI by gestures alone (convention 8), on tui first and then on every app like any other test, and ends by asserting the task's outcome where a person would see it.

1. **The marker.** The test carries `@pytest.mark.use_case("<id>")`; the catalogue's `journey` names the file. The test ends with the closing assertion of point 2, naming the same id. The score script checks marker, assertion and catalogue entry both ways, so a use case without a journey, a journey without its marker or assertion, and a marker without a use case all fail the UI lint.
2. **The steps are proved, not claimed.** The driver records the element id of every gesture it performs; a marked test ends by comparing that record with the catalogue's steps (`assert_use_case_walked`): every catalogued step was performed, in order, with only setup gestures the test declares in between. A catalogue path the UI no longer offers therefore reds its journey, and the score never rests on a path nobody can walk.
3. **An existing test is adopted before a new one is written.** A test that already walks the task by gestures gets the marker and the closing assertion; a new journey is written only for a use case no test walks, or whose test reaches the outcome through an API shortcut.
4. **A reviewer's session is the `reviewer: true` use cases in catalogue order**; the store-reviewer walk composes the same step lists rather than keeping its own.
5. A journey arranges its world as any test does (convention 8, carve-out (b)); the use case's own steps are never arranged.

## Implementation status today

Built: the catalogue `tests/e2e-unified/use-cases.yaml` with 16 use cases, the score script with its three views and `--check`, its test, and the UI lint running `--check` first. Each catalogued path was read from the test named as its journey; where that test reaches a step by setting app state rather than by a gesture (several reach a Settings sub-page or a profile that way), the catalogue carries the gesture a person makes; join a nest and claim a nest, read from tests that import a prepared key, are written through Create, the path a new person takes, and their journeys walk it, the secret read off the screen. The journey harness is built too: every driver records the element id of each gesture it performs, the `use_case` marker is registered, `assert_use_case_walked` compares the record with the steps, and the score script refuses a journey without its marker or its closing assertion. All sixteen use cases are walked by a journey: join a nest by `tests/e2e-unified/tests/test_pending_invite_journey.py` (the handle page walked by gestures on an invite-only handled nest), back up a folder and get a file back by `tests/e2e-unified/tests/test_use_case_backup_journey.py` (a local folder bound through the folder's row, the file uploaded by the real sync agent, a snapshot taken and the file restored byte for byte), claim a nest, save the recovery kit, restore on a new device, read the feed and post, send a mail, add a calendar event, share a folder, export my data and sign out other devices by adopted tests, report and block from a feed post by `tests/e2e-unified/tests/test_abuse_reporting.py` (the report sheet in `post-card`'s scope since 2026-11-19), approve an invite request and turn mail on as an admin by `tests/e2e-unified/tests/test_use_case_admin_journeys.py`, and add a contact and message someone by `tests/e2e-unified/tests/test_use_case_user_journeys.py`; where an adopted test reached a tab or a Settings sub-page by setting app state, it now opens it by its clicks.

Not built yet: `pending_journeys` is empty, but the score script's tolerance for a pending entry is still in place. On macOS the feed journey is an expected failure, because the e2e action posts there by setting the compose state. The store-reviewer walk names its use case per step group and asserts the one it walks (read the feed and post), but still keeps its own steps. ui.yaml carries no placement field: the only primary-against-overflow data is `navigation.layouts`, written by hand. No placement has changed yet: the first proposals (destinations, conversations, the handle-entry and identity-choice stages, feed, folders, settings) are written and put to the owner, one ask each, and nothing moves before an answer. The score script does not expand a component that a page lists among its optional elements, so that component's children are missing from the page's score.
