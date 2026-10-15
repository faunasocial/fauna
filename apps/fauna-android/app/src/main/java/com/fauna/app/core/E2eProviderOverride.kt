package com.fauna.app.core

/**
 * The cloud-provider base-URL override a page-bound machine reads at
 * construction — the one seam the `nest_retire` page's machine needs from the
 * e2e harness (`NestRetireMachine` takes `provider_base_url` when it is built,
 * unlike the wizard's machine, which the harness overrides in place).
 *
 * Production never sets it: [vps] answers `null`, so the machine talks to the
 * real provider. The debug-only `TestAgent` installs a reader of the wizard
 * machine's own `provider_base_url` (the value `set_provider_base_urls` put
 * there), so one harness gesture redirects both pages (testing.md convention 15
 * — the automation reader lives in the debug source set, never the release
 * artifact).
 */
object E2eProviderOverride {
    @Volatile
    var vps: () -> String? = { null }

    /** The public-DNS resolver override (`doh` entry), same release-build `null`. */
    @Volatile
    var doh: () -> String? = { null }
}
