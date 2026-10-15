// The Devices machine's predecessor-chain hand-in, pinned as a source contract
// (testing.md convention 17; `devices-session.ts` imports SvelteKit-only
// modules, so — like `critical-alerts-source-wiring-contract.test.ts` — this
// reads the file as TEXT).
//
// Two properties, both load-bearing for a successor's Folders page listing the
// sets it inherited (`succession-aftermath.md` § Re-key scope, the `BackupKey`
// corpus row):
//   1. `setPredecessorChain(secretHex)` is called at all — the wasm side walks
//      the account registry itself, so the raw predecessor keys never enter JS.
//   2. It comes AFTER `setLabelCustody(secretHex)`: a later `setLabelCustody`
//      replaces the custody whole, chain included, so the reverse order wipes it.

import { stripComments } from './source-contract.ts';

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

const src = await Deno.readTextFile(new URL('./devices-session.ts', import.meta.url));

// Comments stripped so a doc mention can't satisfy the pin.
const code = stripComments(src);

Deno.test('devices-session hands the machine the predecessor chain from secretHex', () => {
  const n = code.match(/machine\.setPredecessorChain\(secretHex\)/g)?.length;
  assert(n === 1, `expected exactly one hand-in, found ${n}`);
});

Deno.test('the predecessor chain is set after the label custody, never before', () => {
  const custody = code.indexOf('machine.setLabelCustody(secretHex)');
  const chain = code.indexOf('machine.setPredecessorChain(secretHex)');
  assert(custody >= 0 && chain >= 0, 'both calls must be present');
  assert(chain > custody, 'setPredecessorChain must follow setLabelCustody');
});

Deno.test('no raw keys cross into JS: the export takes only the secret', () => {
  assert(!/setPredecessorChain\([^)]*(keys|actorIds)/.test(code), 'raw keys reached JS');
});
