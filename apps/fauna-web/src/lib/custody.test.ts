// Deno tests for the two `custody-holder-card` line composers (the web leg). Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/custody.test.ts
//
// These pin the composition, not the wording: which state maps to which key is
// decided in shared Rust and already folded into the row, so what can still go
// wrong on this side is the substitution — a `{when}` left unfilled, a
// `LocalizedText` argument substituted before it was resolved (rendering the raw
// key inside the sentence), or the degraded marker replacing the status line
// instead of riding beside it.

import { custodyHeldBytesText, custodyReceiptStatusText } from './custody.ts';
import type { CustodyReceiptRowView } from './devices-machine.ts';

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

const DEGRADED_KEY = 'devices.custody_degraded_badge';

function receipt(over: Partial<CustodyReceiptRowView> = {}): CustodyReceiptRowView {
  return {
    status_label: { key: 'devices.custody_receipt_fresh', args: {} },
    attested_at_secs: 1_700_000_000,
    held_bytes_label: { key: 'devices.custody_held_bytes', args: {} },
    held: { key: 'size.kb', args: { value: '1' } },
    cap: { key: 'size.kb', args: { value: '4' } },
    degraded: false,
    held_bytes: 1024,
    attested_cap: 4096,
    ...over,
  };
}

Deno.test('the receipt status substitutes {when} through the injected formatter', () => {
  const line = custodyReceiptStatusText(receipt(), () => 'FORMATTED');
  assert(line.includes('FORMATTED'), `expected the formatted stamp, got: ${line}`);
  assert(!line.includes('{when}'), `an unfilled placeholder reached the UI: ${line}`);
});

// The A7 honesty rule: three states, three different lines, none empty. A
// collapse here is exactly the mistake the shared decision exists to prevent.
Deno.test('the three receipt states render three distinct non-empty lines', () => {
  const lines = [
    ['devices.custody_receipt_fresh', 1_700_000_000],
    ['devices.custody_receipt_stale', 1_700_000_000],
    ['devices.custody_receipt_none', null],
  ].map(([key, secs]) =>
    custodyReceiptStatusText(
      receipt({
        status_label: { key: key as string, args: {} },
        attested_at_secs: secs as number | null,
      }),
      () => 'WHEN',
    ),
  );
  assert(lines.every((l) => l.length > 0), `an empty status line: ${JSON.stringify(lines)}`);
  assert(new Set(lines).size === 3, `states collapsed: ${JSON.stringify(lines)}`);
});

// A receipt-less row carries no timestamp and its key has no `{when}` — the
// formatter must not be reached at all, rather than being handed a 0 that would
// render as 1970.
Deno.test('a receipt-less row never calls the timestamp formatter', () => {
  let called = false;
  const line = custodyReceiptStatusText(
    receipt({ status_label: { key: 'devices.custody_receipt_none', args: {} }, attested_at_secs: null }),
    () => {
      called = true;
      return 'NOPE';
    },
  );
  assert(!called, 'the formatter was called for a row with no receipt');
  assert(line.length > 0 && !line.includes('NOPE'), `unexpected line: ${line}`);
});

// A `LocalizedText` argument is a flat string, so `held`/`cap` must be resolved
// BEFORE they are substituted — otherwise the raw key lands inside the sentence.
Deno.test('held and cap are resolved before substitution, never as raw keys', () => {
  const line = custodyHeldBytesText(receipt(), DEGRADED_KEY);
  assert(!line.includes('size.kb'), `an unresolved key reached the UI: ${line}`);
  assert(!line.includes('{held}') && !line.includes('{cap}'), `unfilled placeholder: ${line}`);
});

// `degraded` is ORTHOGONAL to freshness — a fresh receipt can honestly report
// dropped payload — so the marker appends to this line and never replaces the
// status one.
Deno.test('the degraded marker rides the held-bytes line rather than replacing it', () => {
  const plain = custodyHeldBytesText(receipt(), DEGRADED_KEY);
  const degraded = custodyHeldBytesText(receipt({ degraded: true }), DEGRADED_KEY);
  assert(degraded.startsWith(plain), `the marker replaced the line: ${degraded}`);
  assert(degraded.length > plain.length, 'the degraded marker did not render');
});

Deno.test('an undegraded receipt carries no marker', () => {
  const line = custodyHeldBytesText(receipt({ degraded: false }), DEGRADED_KEY);
  assert(!line.includes('—'), `an em-dash marker leaked onto a healthy row: ${line}`);
});
