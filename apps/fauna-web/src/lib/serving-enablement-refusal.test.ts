// Behavioural cover for the post-claim serving enablement's refusal routing
// (`onboarding.md` § 3b *The step completes; it is not fired once*): a refusal
// for good reaches the shell's `error-message` through the
// `fauna-message-update` channel, and a completed run says nothing.
//
//     deno test apps/fauna-web/src/lib/serving-enablement-refusal.test.ts

import { assert, assertEquals } from 'jsr:@std/assert';
import { applyMessageUpdate, type MessageUpdate } from './message-update.ts';
import {
  servingEnablementRefusalUpdate,
  surfaceServingEnablementRefusal,
} from './serving-enablement-refusal.ts';

function listen(target: EventTarget): MessageUpdate[] {
  const seen: MessageUpdate[] = [];
  target.addEventListener('fauna-message-update', (e) =>
    seen.push((e as CustomEvent).detail as MessageUpdate));
  return seen;
}

Deno.test('a refusal for good is painted as the shell banner error', () => {
  const target = new EventTarget();
  const seen = listen(target);
  assert(surfaceServingEnablementRefusal('Mail could not be turned on.', target));
  assertEquals(seen, [{ error: 'Mail could not be turned on.' }]);
  const shown = applyMessageUpdate({ error: '', warning: 'w', info: '' }, seen[0]);
  assertEquals(shown, { error: 'Mail could not be turned on.', warning: 'w', info: '' });
});

Deno.test('a completed run dispatches nothing and leaves the banner alone', () => {
  const target = new EventTarget();
  const seen = listen(target);
  assert(!surfaceServingEnablementRefusal(undefined, target));
  assert(!surfaceServingEnablementRefusal('', target));
  assertEquals(seen, []);
  assertEquals(servingEnablementRefusalUpdate(undefined), null);
});
