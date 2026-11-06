// The SPA's opener for an external address — web's twin of the native apps'
// one opener (windows `Services/UrlOpener.cs`, android `core/UrlOpener.kt`).
//
// Under the e2e harness (`__FAUNA_E2E_AUTOMATION__`, a build made for testing)
// it opens nothing and logs the address it would have opened, in the windows
// opener's own words, so a journey reads the browser console the bridge
// captures as the opener's record (`tests/e2e-unified/conftest.py`'s
// `opened_urls`) — a real tab would only leave the harness's browser for the
// open internet.

export function openUrl(url: string): void {
  if (__FAUNA_E2E_AUTOMATION__) {
    console.info(`open-url: suppressed under the e2e harness: ${url}`);
    return;
  }
  window.open(url, '_blank', 'noopener');
}
