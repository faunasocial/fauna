package com.fauna.app

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
        }
        val appState = AppState()

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
