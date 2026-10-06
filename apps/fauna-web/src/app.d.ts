// Ambient declarations for the SPA.

/**
 * Compile-time constant injected by `vite.config.ts` `define` (testing.md
 * § Test-agent build exclusion): true only in the `just web-test` e2e build
 * flavor (FAUNA_WEB_E2E_AUTOMATION=1); a production build folds it to false
 * and strips every automation hook gated behind it out of the bundle.
 */
declare const __FAUNA_E2E_AUTOMATION__: boolean;

/**
 * Compile-time constant injected by `vite.config.ts` `define` — the web family's
 * half of the `payments` compile-time excision (`dynamic-features.md`
 * § Platform-family surface excision: *"web: a vite define + the isolated-module
 * pattern so production builds emit neither code nor chunk"*).
 *
 * POSITIVE and defaulting to **true**, unlike `__FAUNA_E2E_AUTOMATION__` above and
 * unlike apple's negative `FAUNA_EXCISE_PAYMENTS`: a flavor root's plain build
 * ships the plane, and excision is what a *flavor* asks for. `just
 * web-store-safe` sets `FAUNA_WEB_PAYMENTS=0`, which folds this to false and
 * removes both the render and — through the isolated-module pattern — the glue
 * chunk behind it.
 *
 * ⚠ Gate the **render**, not only the resolver feeding it: `PostSummary.tips` is
 * a deliberately ungated inert record that stays `null` forever in an excised
 * build, so an ungated tip section paints nothing and still ships every
 * `post-tip-*` id. Dead is not absent (`dynamic-features.md` § Platform-family
 * surface excision, the ⚠ paragraph).
 */
declare const __FAUNA_PAYMENTS__: boolean;
