// Deno tests for the shell banner's update rule. Run via:
//
//     deno test apps/fauna-web/src/lib/message-update.test.ts
//
// The custody leg's cleared mark (`box-recovery.md` § The plane-era recovery
// floor, *(c) The writes*) takes its own warning down, and only that one.

import { applyMessageUpdate, type BannerTexts } from './message-update.ts';

const CUSTODY = 'Off-box recovery custody not saved';
const MISMATCH = 'The recovery seed did not match this box';
const empty: BannerTexts = { error: '', warning: '', info: '' };

function eq(got: BannerTexts, want: BannerTexts) {
  if (JSON.stringify(got) !== JSON.stringify(want)) {
    throw new Error(`got ${JSON.stringify(got)}, want ${JSON.stringify(want)}`);
  }
}

Deno.test('a custody warning, then the cleared mark, takes the warning down', () => {
  const warned = applyMessageUpdate(empty, { warning: CUSTODY });
  eq(warned, { ...empty, warning: CUSTODY });
  eq(applyMessageUpdate(warned, { withdrawWarning: [MISMATCH, CUSTODY] }), empty);
});

Deno.test('the cleared mark leaves an unrelated warning standing', () => {
  const warned = applyMessageUpdate(
    applyMessageUpdate(empty, { warning: CUSTODY }),
    { warning: 'Your connection was lost' },
  );
  eq(
    applyMessageUpdate(warned, { withdrawWarning: [MISMATCH, CUSTODY] }),
    { ...empty, warning: 'Your connection was lost' },
  );
});

Deno.test('the cleared mark touches neither the error nor the info text', () => {
  const shown = { error: 'Failed', warning: MISMATCH, info: 'Saved' };
  eq(applyMessageUpdate(shown, { withdrawWarning: [MISMATCH] }), {
    error: 'Failed',
    warning: '',
    info: 'Saved',
  });
});
