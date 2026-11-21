// The AT Protocol settings page paints from the machine's answers
// (`docs/goal/ui/atproto.md` § Errors & edge cases).
import { assert, assertEquals } from 'jsr:@std/assert@1';
import {
  authorizePaint,
  distinctReasons,
  pageMode,
  rungPaint,
  type AtprotoPossibilitySource,
} from './atproto-possibility.ts';

const loading = { state: 'blocked', reason: { key: 'common.still_loading' } };
const noIdentity = { state: 'blocked', reason: { key: 'atproto_settings.error_no_identity' } };

function source(rung: unknown, authorize: unknown): AtprotoPossibilitySource {
  return {
    rungPossibilityJson: () => JSON.stringify(rung),
    authorizePossibilityJson: () => JSON.stringify(authorize),
    contestPossibilityJson: () => JSON.stringify({ state: 'absent' }),
    retireIdentityPossibilityJson: () => JSON.stringify({ state: 'possible' }),
  };
}

Deno.test('every rung is disabled with still_loading until the level read resolves', () => {
  const m = source(loading, { state: 'possible' });
  const paints = ['off', 'linked', 'hosted_visible', 'hosted_full'].map((l) => rungPaint(m, l));
  assert(paints.every((p) => p.shown && p.disabled));
  assertEquals(paints[0].reason?.key, 'common.still_loading');
  assertEquals(distinctReasons(paints).length, 1);
});

Deno.test('authorize is disabled with the no-identity reason when no identity is held', () => {
  const paint = authorizePaint(source({ state: 'possible' }, noIdentity));
  assert(paint.shown && paint.disabled);
  assertEquals(paint.reason?.key, 'atproto_settings.error_no_identity');
});

Deno.test('a failed machine build paints only the unavailable line', () => {
  assertEquals(pageMode(true), 'unavailable');
  assertEquals(pageMode(false), 'controls');
});
