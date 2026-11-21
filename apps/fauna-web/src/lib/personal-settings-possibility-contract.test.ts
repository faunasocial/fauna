// The personal settings controls wait for their reads and their sessions
// (`docs/goal/ui/README.md` § Offer only what is possible, rules 5–7; the
// rulings in `behavior/engagement-cues.md` § Layer B, `ui/connected-apps.md`
// and `ui/sessions.md` § Errors & edge cases, `behavior/succession-propagation.md`
// § Propagation and `behavior/subscriptions.md`). The web lift of tui's
// `personal_controls_refuse_before_their_reads_and_say_why`. Run via:
//
//     deno test --allow-read apps/fauna-web/src/lib/personal-settings-possibility-contract.test.ts
//
// A source-contract test (the components are not mountable under plain Deno):
// each control paints from its shared answer, and each handler asks the same
// answer again and refuses on the page's error instead of writing a guess or
// returning without a word. The answer → paint/refusal mapping itself is
// pinned by `form-gate.test.ts`.

const read = (path: string) => Deno.readTextFile(new URL(path, import.meta.url));
const personalization = await read('./components/PersonalizationSection.svelte');
const connectedApps = await read('./components/ConnectedAppsSection.svelte');
const sessions = await read('./components/SessionsSection.svelte');
const subscriptions = await read('./components/SubscriptionsSection.svelte');
const settingsPage = await read('../routes/settings/[[subpage]]/+page.svelte');

function check(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

/** The body of `async function <name>` / `function <name>` up to the next
 *  top-level function in the same script. */
function fnBody(src: string, name: string): string {
  const m = src.match(new RegExp(`function ${name}\\([\\s\\S]*?\\n  \\}\\n`));
  check(m !== null, `${name} not found`);
  return m![0];
}

/** The opening tag of the element carrying `data-testid={IDS.<id>}`. */
function tagOf(src: string, id: string): string {
  const m = src.match(new RegExp(`<(button|input)[^>]*?data-testid=\\{IDS\\.${id}\\}[\\s\\S]*?>`));
  check(m !== null, `${id} not found`);
  return m![0];
}

Deno.test('share-signals toggle: painted from its answer, the press asks it again', () => {
  check(
    /engagementShareSignalsPossibility\(managerReady, shareRead\)/.test(personalization),
    'the toggle must paint from engagementShareSignalsPossibility(managerReady, shareRead)',
  );
  check(/let shareRead = \$state<ReadWord>\('pending'\);/.test(personalization), 'the read starts pending');
  check(/disabled=\{signalBusy \|\| sharePaint\.disabled\}/.test(tagOf(personalization, 'PERSONALIZATION_SHARE_SIGNALS_TOGGLE')), 'toggle disabled from its answer');
  const toggle = fnBody(personalization, 'toggleShareSignals');
  check(/formRefusal\(/.test(toggle) && /engagementShareSignalsPossibility\(/.test(toggle), 'the press asks the answer again');
  check(/error = refusal/.test(toggle), 'a refusal goes on error-message');
  check(!/getFeedManager\(\)/.test(toggle), 'the press must not build a manager of its own');
});

Deno.test('clear-engagement button: painted from its answer, no silent return', () => {
  check(/disabled=\{clearPaint\.disabled\}/.test(tagOf(personalization, 'PERSONALIZATION_CLEAR_ENGAGEMENT_DATA_BUTTON')), 'button disabled from its answer');
  const clear = fnBody(personalization, 'clearEngagementData');
  check(/engagementClearPossibility\(manager !== null\)/.test(clear), 'the press asks the answer over the live manager');
  check(/error = refusal/.test(clear), 'a refusal goes on error-message');
  check(!/if \(!id\) return;/.test(clear), 'no silent identity return');
});

Deno.test('external-apps switch: still loading until the read, the press asks again', () => {
  check(/machine\.externalAppsPossibilityJson\(\)/.test(connectedApps), 'the switch asks the shared answer');
  check(/readPossibility\('pending'\)/.test(connectedApps), 'before the fold the answer is the pending read');
  check(/disabled=\{externalAppsPaint\.disabled\}/.test(tagOf(connectedApps, 'ATPROTO_EXTERNAL_APPS_ENABLE')), 'switch disabled from its answer');
  const toggle = fnBody(connectedApps, 'toggleExternalApps');
  check(/formRefusal\(externalAppsAnswer\(\)\)/.test(toggle), 'the press asks the answer again');
  check(!/!!snap\?\.external_apps_off/.test(toggle), 'no write of an unread default');
});

Deno.test('sign out everywhere else: arm and confirm both ask the answer', () => {
  check(/sessionsRevokeOthersPossibility\(snapshot, busy\)/.test(sessions), 'painted from the shared answer');
  check(/revokeOthersPaint\.disabled/.test(tagOf(sessions, 'SESSIONS_REVOKE_OTHERS_BUTTON')), 'button disabled from its answer');
  check(/revokeOthersAllowed\(\)/.test(fnBody(sessions, 'armRevokeOthers')), 'the arm asks again');
  check(/revokeOthersAllowed\(\)/.test(fnBody(sessions, 'revokeOthers')), 'the confirm asks again');
  check(/error = refusal/.test(fnBody(sessions, 'revokeOthersAllowed')), 'a refusal goes on error-message');
});

Deno.test('member-review Remove: painted from its answer, refuses without a session', () => {
  check(/memberReviewRemovePossibility\(memberReviewSessionUp\)/.test(settingsPage), 'painted from the shared answer');
  const tags = [...settingsPage.matchAll(/<button[^>]*?data-testid=\{IDS\.MEMBER_REVIEW_REMOVE_BUTTON\}[\s\S]*?>/g)];
  check(tags.length === 2, 'both Remove surfaces present');
  for (const tag of tags) check(/disabled=\{memberReviewRemovePaint\.disabled\}/.test(tag[0]), 'Remove disabled from its answer');
  const remove = fnBody(settingsPage, 'removeMemberReview');
  check(/memberReviewRemovePossibility\(conversationsManagerIfReady\(\) !== null\)/.test(remove), 'the press asks over the live session');
  check(/memberReviewError = refusal/.test(remove), 'a refusal goes on the page error');
});

Deno.test('subscriptions list: loading, then empty only after an answered read', () => {
  check(/let mineRead = \$state<ReadWord>\('pending'\);/.test(subscriptions), 'the read starts pending');
  check(/\{#if mineRead === 'pending'\}\s*<p class="muted">\{t\.common\.loading\}<\/p>/.test(subscriptions), 'pending paints loading');
  check(/\{:else if mineRead === 'resolved' && subscriptions\.length === 0\}/.test(subscriptions), 'empty only after an answered read');
  check(/mineRead = readFailed\(mineRead\)/.test(subscriptions), 'a failed read is tracked');
});
