// Deno test for `resolveComposed` — how web resolves a shared-Rust
// `ComposedText` (`fauna_client_connected_apps::compose`) through its string
// table. The composition itself (order, keys, args) is pinned in Rust; this pins
// only the JS half, with hand-built values. Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/composed-text.test.ts

import { resolveComposed, type ComposedText } from './i18n/localized.ts';

function assertEq(actual: string, expected: string) {
  if (actual !== expected) throw new Error(`expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

const verbatim = (text: string) => ({ key: 'connected_apps.verbatim', args: { text } });

Deno.test('a plain text fills its own arguments', () => {
  const ct: ComposedText = {
    text: { key: 'connected_apps.created', args: { time: '2023-11-14 22:13' } },
    nested: {},
  };
  assertEq(resolveComposed(ct), 'Added 2023-11-14 22:13');
});

Deno.test('a nested text resolves first and fills its argument', () => {
  const ct: ComposedText = {
    text: { key: 'connected_apps.row_head' },
    nested: { name: verbatim('Graysky'), badge: { key: 'connected_apps.class_device' } },
  };
  assertEq(resolveComposed(ct), 'Graysky · App on a device');
});

Deno.test('a bullet indents its nested text', () => {
  const ct: ComposedText = {
    text: { key: 'connected_apps.bullet' },
    nested: { text: verbatim('Post as you') },
  };
  assertEq(resolveComposed(ct), '  • Post as you');
});

Deno.test('a nested text wins over a plain argument of the same name', () => {
  const ct: ComposedText = {
    text: { key: 'connected_apps.bullet', args: { text: 'plain' } },
    nested: { text: verbatim('nested') },
  };
  assertEq(resolveComposed(ct), '  • nested');
});

Deno.test('an unknown key surfaces rather than rendering blank', () => {
  assertEq(resolveComposed({ text: { key: 'connected_apps.no_such_key' }, nested: {} }), 'connected_apps.no_such_key');
});
