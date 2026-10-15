package com.fauna.app

import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.fragment.app.FragmentActivity
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.*
import androidx.compose.material3.windowsizeclass.ExperimentalMaterial3WindowSizeClassApi
import androidx.compose.material3.windowsizeclass.calculateWindowSizeClass
import androidx.compose.runtime.Composable
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import com.fauna.app.core.AppState
import com.fauna.app.ui.navigation.FaunaNavHost
import dagger.hilt.android.AndroidEntryPoint

@AndroidEntryPoint
// FragmentActivity (not the bare ComponentActivity) so androidx BiometricPrompt
// can host its dialog fragment — the multi-account re-auth gate (AccountReauth).
class MainActivity : FragmentActivity() {
    @OptIn(ExperimentalMaterial3WindowSizeClassApi::class, ExperimentalComposeUiApi::class)
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Must run before setContent(): Hilt resolves the launch-persistence
        // chain (LaunchModule.provideSecretBackend et al.) during the very
        // first composition to drive initial nav routing, so the e2e
        // credential-file choice has to be visible before that point --
        // earlier than the FAUNA_E2E_BRIDGE handling below, which runs
        // TestAgent.start() after setContent() (fine there; see
        // TestAgent.credentialFilePath's doc for why this one differs).
        //
        // BuildConfig.DEBUG-gated (testing.md convention 15): the automation
        // surface is compiled out of release artifacts, so this whole branch
        // — and TestAgent.start()'s body below — is dead in a release build
        // and R8 strips it.
        if (BuildConfig.DEBUG) {
            intent.getStringExtra("FAUNA_E2E_CREDENTIAL_FILE")?.let {
                com.fauna.app.testing.TestAgent.credentialFilePath = it
            }
            // Re-export as a real process env var: android hosts its account
            // runtime IN-PROCESS (libs/fauna-ffi/src/account_runtime.rs), and
            // the shared Rust trust seed (fauna_client::trust::
            // trusted_escrow_holders, e2e-automation-surface-gating.md § The
            // e2e trust seed) reads FAUNA_E2E_TRUST_NEST_IDENTITY via
            // std::env::var -- an Intent extra alone never becomes process
            // env. Os.setenv is the one door through, and it must land here,
            // before the launch-persistence chain below can trigger an
            // auto-login that assembles the runtime.
            intent.getStringExtra("FAUNA_E2E_TRUST_NEST_IDENTITY")?.let {
                try {
                    android.system.Os.setenv("FAUNA_E2E_TRUST_NEST_IDENTITY", it, true)
                } catch (e: android.system.ErrnoException) {
                    android.util.Log.w("MainActivity", "R14 trust seed setenv failed: ${e.message}")
                }
            }
            // The launch clock's e2e offset, through the same door and for the
            // same reason: `fauna_launch_machine::launch_clock` reads
            // FAUNA_E2E_CLOCK_OFFSET_SECS via std::env::var once, on first use,
            // so it must be process env before the launch chain below starts
            // the machine (e2e-automation-surface-gating.md § The convention).
            intent.getStringExtra("FAUNA_E2E_CLOCK_OFFSET_SECS")?.let {
                try {
                    android.system.Os.setenv("FAUNA_E2E_CLOCK_OFFSET_SECS", it, true)
                } catch (e: android.system.ErrnoException) {
                    android.util.Log.w("MainActivity", "launch clock offset setenv failed: ${e.message}")
                }
            }
            // The region content plane's two test-capable seams
            // (`fauna_client_region`'s registry seed and declared-region
            // override, convention 15), through the same door: both are read
            // via std::env::var inside `FfiRegionPlane.open` below, so they must
            // be process env first. A relaunch that re-delivers them into a
            // surviving process reopens the plane so they take effect.
            var regionSeeded = false
            for (key in REGION_E2E_ENV_KEYS) {
                intent.getStringExtra(key)?.let {
                    try {
                        android.system.Os.setenv(key, it, true)
                        regionSeeded = true
                    } catch (e: android.system.ErrnoException) {
                        android.util.Log.w("MainActivity", "$key setenv failed: ${e.message}")
                    }
                }
            }
            if (regionSeeded) com.fauna.app.core.RegionLeaf.openPlane(this, reopen = true)
            // The admin DNS page's two seams, through the same door: the
            // fake-provider gate and the compressed re-check clock are read via
            // std::env inside `fauna-client-dns` each time the page builds its
            // machine or ticks a clock, so process env before the page opens is
            // enough — no reopen.
            for (key in DNS_E2E_ENV_KEYS) {
                intent.getStringExtra(key)?.let {
                    try {
                        android.system.Os.setenv(key, it, true)
                    } catch (e: android.system.ErrnoException) {
                        android.util.Log.w("MainActivity", "$key setenv failed: ${e.message}")
                    }
                }
            }
        }
        // The device's region plane, opened at launch AHEAD of the first fetch
        // (region-blocking.md § Fail posture) — idempotent, the leaf reads one
        // OS setting per launch.
        com.fauna.app.core.RegionLeaf.openPlane(this)
        val appState = AppState()
        this.appState = appState

        setContent {
            FaunaTheme {
                val windowSizeClass = calculateWindowSizeClass(this@MainActivity)
                // Expose every Modifier.testTag() as a UiAutomator resource-id
                // (Compose's documented opt-in for black-box accessibility
                // bridges — androidTest/bridge's ElementOps drives the app via
                // UiDevice.findObjects(By.res(...)), not a hosted ComposeTestRule).
                // Root-level: covers every screen without a per-composable tag.
                Surface(
                    modifier = Modifier
                        .fillMaxSize()
                        .semantics { testTagsAsResourceId = true },
                ) {
                    FaunaNavHost(
                        widthSizeClass = windowSizeClass.widthSizeClass,
                        appState = appState,
                    )
                }
            }
        }

        // Start test agent if bridge URL provided via intent extra
        // (BuildConfig.DEBUG-gated — see the note above).
        if (BuildConfig.DEBUG) {
            intent.getStringExtra("FAUNA_E2E_BRIDGE")?.let { bridgeUrl ->
                // Read before start(): ConversationsManagerHost is a Hilt @Singleton
                // that may be constructed by the same composition pass start() kicks
                // off, and its E2E gate checks this flag.
                com.fauna.app.testing.TestAgent.isRealConversationsActive =
                    intent.getBooleanExtra("FAUNA_E2E_REAL_CONVERSATIONS", false)
                com.fauna.app.testing.TestAgent.start(this, bridgeUrl, appState)
            }
        }

        // A `fauna://` route that launched the app. Only on a fresh create: a
        // recreation (rotation, process restore) re-delivers the same launch
        // intent, and re-offering a spent consent handle would paint the
        // expired-link message over a card the user already answered.
        if (savedInstanceState == null) receiveRoute(intent)
    }

    // `singleTask`: a route opened while the app runs arrives here.
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        receiveRoute(intent)
    }

    /**
     * Hand a `fauna://` route to the one door (`android.md` § Navigation →
     * *In-app routes*): it parses through the shared `parseAppRoute`, drops what
     * it does not take, holds a route that arrives signed out, and otherwise
     * navigates to the route's page. A relaunch from Recents re-delivers the old
     * intent, so that one is not a route.
     */
    private fun receiveRoute(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        if (intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return
        val uri = intent.dataString ?: return
        com.fauna.app.core.ConsentHandoff.shared.offer(uri, appState)
    }

    private lateinit var appState: AppState

    private companion object {
        /** `fauna_client_region::registry::E2E_REGISTRY_ENV` and
         *  `fauna_client_region::source::E2E_DECLARED_ENV` — the keys the driver's
         *  `declare_region_for_relaunch` writes (`drivers/http_bridge.py`). */
        val REGION_E2E_ENV_KEYS = listOf("FAUNA_E2E_REGION_REGISTRY", "FAUNA_E2E_REGION_DECLARED")

        /** `fauna_client_dns`'s fake-provider gate, DNS-page clock override and
         *  ACME directory seed (`e2e_acme_seam`, read when an order starts) —
         *  the keys conftest's android block sets for the admin DNS journeys;
         *  the whole seed, its name server a reversed loopback port. */
        val DNS_E2E_ENV_KEYS = listOf(
            "FAUNA_DNS_PROVIDER_FAKE",
            "FAUNA_DNS_RECHECK_SECS",
            "FAUNA_E2E_ACME_DIRECTORY_URL",
            "FAUNA_E2E_ACME_CA_PEM",
            "FAUNA_E2E_ACME_NAMESERVER",
        )
    }
}

@Composable
fun FaunaTheme(content: @Composable () -> Unit) {
    val context = LocalContext.current
    val darkTheme = isSystemInDarkTheme()
    val colorScheme = when {
        Build.VERSION.SDK_INT >= 31 && darkTheme -> dynamicDarkColorScheme(context)
        Build.VERSION.SDK_INT >= 31 -> dynamicLightColorScheme(context)
        darkTheme -> darkColorScheme()
        else -> lightColorScheme()
    }
    MaterialTheme(colorScheme = colorScheme, content = content)
}
