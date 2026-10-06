package com.fauna.app.core

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import com.fauna.app.BuildConfig
import com.fauna.app.testing.TestAgent

/**
 * Hand a URL to the OS default handler — the ONE opener resolution every
 * open-something-external call site in this app shares: the hosted-auth
 * verification link and the DNS/VPS provider "open in browser" button
 * (`DnsConfigScreen.kt`, `VpsConfigScreen.kt`), and the Bluesky/bridge OAuth
 * authorize links (`AtprotoSettingsScreen.kt`, `BridgesScreen.kt`).
 *
 * Mirrors linux's `url_opener::open` / windows' `Services/UrlOpener.cs` /
 * apple's `OpenURL.open`: the OS already owns "which program opens a URL", so
 * fauna adds no program-picker knob — the only injection point is the e2e
 * suppression below, gated on the SAME harness-presence signal
 * ([TestAgent.isE2EActive]) every other android automation seam reads, not a
 * dedicated env var.
 *
 * ⚠ Under a harness launch this must NOT reach the OS
 * (`e2e-conventions.md` point 10: an app launch isolates every inherited
 * channel). Measured on windows: opening a real browser on the bundled
 * provider's verification URL wedges the e2e `fake_cloud` fixture (it stops
 * answering past ~6 idle connections), stalling the app's own next request
 * for 60-90+ seconds. The handoff stays observable rather than silent: the
 * URL is logged at this seam instead of being silently dropped.
 */
object UrlOpener {
    /** Hand [url] to the OS default handler, or — under e2e automation — log
     *  it instead of launching. */
    fun open(context: Context, url: String) {
        // Fauna Kids opens nothing outside the app (family-safety.md § The
        // account age band, the kids-app bullet, item (4): outbound link
        // opening is excised). The release shrinker folds the constant, so the kids artifact
        // carries no ACTION_VIEW launch at all.
        if (BuildConfig.KIDS) {
            ShellLog.i("UrlOpener", "open-url: outbound links are not opened in Fauna Kids")
            return
        }
        openWith(url, TestAgent.isE2EActive) {
            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        }
    }

    /**
     * The gate plus the launch, with both the e2e state and the OS call
     * injected — the unit test drives this directly, so it never reads the
     * real [TestAgent] state or launches a real activity. Mirrors linux
     * `url_opener::open_with(url, e2e, launch)`.
     */
    internal fun openWith(url: String, e2e: Boolean, launch: () -> Unit) {
        if (e2e) {
            ShellLog.i("UrlOpener", "open-url: suppressed under the e2e harness: $url")
            return
        }
        try {
            launch()
        } catch (e: ActivityNotFoundException) {
            ShellLog.w("UrlOpener", "open-url: no activity handled $url: ${e.message}")
        }
    }
}
