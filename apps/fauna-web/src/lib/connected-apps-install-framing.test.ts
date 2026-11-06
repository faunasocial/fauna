// The install card's admin framing line (`third-party.md` § The runner
// contract → *The install-approval leg*): painted under the install heading
// only when the shared composition supplies it (`framing` is null on a bind
// card). A source contract over the section, like its sibling contract tests.

function assert(ok: boolean) {
  if (!ok) throw new Error("assertion failed");
}

const svelte = await Deno.readTextFile(
  new URL('./components/ConnectedAppsSection.svelte', import.meta.url),
);
const ts = await Deno.readTextFile(new URL('./wasm-connected-apps.ts', import.meta.url));

Deno.test('the install type carries a nullable framing line', () => {
  assert(/framing:\s*ComposedText \| null/.test(ts));
});

Deno.test('the framing line is guarded and sits between heading and form', () => {
  const heading = svelte.indexOf('IDS.CONNECTED_APPS_REQUEST_INSTALL}>');
  const framing = svelte.indexOf('{#if install.framing}');
  const form = svelte.indexOf('IDS.CONNECTED_APPS_REQUEST_INSTALL_FORM');
  assert(heading >= 0 && framing > heading && form > framing);
  assert(svelte.includes('resolveComposed(install.framing)'));
});
