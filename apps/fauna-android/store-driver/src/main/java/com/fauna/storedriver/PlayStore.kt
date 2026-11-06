package com.fauna.storedriver

import android.app.UiAutomation
import android.os.SystemClock
import androidx.test.uiautomator.By
import androidx.test.uiautomator.BySelector
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.UiObject2
import androidx.test.uiautomator.Until
import java.util.regex.Pattern

/** The Fauna app's store identity (installers/android.md § Store identity). */
const val FAUNA_PACKAGE = "social.fauna.fauna"

/** The Play Store app — the installer of record a store install names. */
const val PLAY_PACKAGE = "com.android.vending"

/**
 * Shell-identity commands through the instrumentation's [UiAutomation]
 * (`executeShellCommand`, API 21+): what `adb shell` could run, never anything
 * of the app's UID. Each call waits for the command's output to close, so it
 * returns after the command has finished.
 */
class Shell(private val automation: () -> UiAutomation) {
    fun run(command: String): String {
        val pfd = automation().executeShellCommand(command)
        return android.os.ParcelFileDescriptor.AutoCloseInputStream(pfd).use { it.readBytes().decodeToString() }
    }
}

/**
 * The Fauna app, reached from outside: started from its launcher entry and
 * stopped, both as shell-identity commands — no launch intent extras, no seed,
 * no file of the app's touched (the store driver has none of the harness
 * bridge's same-UID powers).
 */
class AppControl(private val shell: Shell) {
    fun launch(): String = shell.run("monkey -p $FAUNA_PACKAGE -c android.intent.category.LAUNCHER 1")

    fun stop() {
        shell.run("am force-stop $FAUNA_PACKAGE")
    }
}

/**
 * The obtain verbs: open the app's listing in the Play Store and press Install,
 * as a user does (feature-catalog.md § Implementation status today, how the
 * Android store install is driven — "obtaining is the driver's act too").
 *
 * Completion is NOT read here: the host polls `pm list packages` (the
 * installer of record and the certificate are the host's subject facts), so
 * this side answers only what the listing page showed.
 *
 * The labels are the Play Store's own, in English — the venue AVD's locale. A
 * page that shows none of them is answered as `unknown` with the visible texts,
 * so a Play redesign reads as itself rather than as a missing button.
 */
class PlayStore(private val device: UiDevice, private val shell: Shell) {

    /** `POST /store/open` — the listing, in the Play Store, in front. */
    fun openListing(): Boolean {
        shell.run(
            "am start -a android.intent.action.VIEW " +
                "-d market://details?id=$FAUNA_PACKAGE -p $PLAY_PACKAGE",
        )
        return device.wait(Until.hasObject(By.pkg(PLAY_PACKAGE).depth(0)), OPEN_WAIT_MS) == true
    }

    /**
     * `POST /store/install` — press the listing's Install button, after reading
     * what the listing shows. Answers one of:
     * - `pressed` — Install was there and is pressed;
     * - `installed` — the listing already offers Open / Uninstall;
     * - `signed_out` — the Play Store asks for a Google account (the human
     *   inch: the one-time sign-in to an account on the internal testers list);
     * - `not_available` — the listing is not served to this account or device;
     * - `unknown` — none of these within the wait, with the page's texts.
     */
    fun install(): InstallAnswer {
        val deadline = SystemClock.uptimeMillis() + LISTING_WAIT_MS
        while (true) {
            find(INSTALL)?.let { button ->
                button.click()
                return InstallAnswer("pressed")
            }
            if (find(OPEN) != null || find(UNINSTALL) != null) return InstallAnswer("installed")
            if (find(SIGN_IN) != null) return InstallAnswer("signed_out")
            if (find(NOT_AVAILABLE) != null) return InstallAnswer("not_available")
            if (SystemClock.uptimeMillis() >= deadline) return InstallAnswer("unknown", visibleTexts())
            SystemClock.sleep(POLL_MS)
        }
    }

    private fun find(label: Pattern): UiObject2? =
        device.findObject(playSelector().text(label)) ?: device.findObject(playSelector().desc(label))

    private fun playSelector(): BySelector = By.pkg(PLAY_PACKAGE)

    private fun visibleTexts(): List<String> =
        device.findObjects(By.pkg(PLAY_PACKAGE)).mapNotNull { it.text?.takeIf(String::isNotBlank) }.distinct()

    private companion object {
        const val OPEN_WAIT_MS = 15_000L
        const val LISTING_WAIT_MS = 30_000L
        const val POLL_MS = 250L

        fun label(vararg words: String): Pattern =
            Pattern.compile(words.joinToString("|") { "^\\s*${Pattern.quote(it)}\\s*$" }, Pattern.CASE_INSENSITIVE)

        val INSTALL: Pattern = label("Install")
        val OPEN: Pattern = label("Open")
        val UNINSTALL: Pattern = label("Uninstall")
        val SIGN_IN: Pattern = label("Sign in")
        val NOT_AVAILABLE: Pattern = Pattern.compile(
            "item not found|isn.t available|not available for (this|your) device|couldn.t find",
            Pattern.CASE_INSENSITIVE,
        )
    }
}

/** What [PlayStore.install] found on the listing; [texts] only for `unknown`. */
data class InstallAnswer(val state: String, val texts: List<String> = emptyList())
