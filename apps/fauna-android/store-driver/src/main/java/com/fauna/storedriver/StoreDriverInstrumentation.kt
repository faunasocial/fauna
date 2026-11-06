package com.fauna.storedriver

import android.app.Instrumentation
import android.os.Bundle
import androidx.test.uiautomator.UiDevice
import com.fauna.automation.UiElements

/**
 * The store driver's entry point: `am instrument -w -e port <p>
 * social.fauna.storedriver/com.fauna.storedriver.StoreDriverInstrumentation`.
 *
 * Self-instrumenting (its manifest targets its own package), so it runs in its
 * OWN process and UID and never attaches to the Fauna app — the property that
 * lets it drive an install the Play Store delivered, re-signed with Google's
 * key (feature-catalog.md § Implementation status today, how the Android store
 * install is driven). Everything it does to the app or to the Play Store goes
 * through [UiDevice] / `UiAutomation`: the accessibility tree and shell-identity
 * commands, the reach any accessibility service or `adb shell` already has.
 *
 * A plain [Instrumentation], not a JUnit runner: like the harness bridge
 * (`BridgeInstrumentation`), it serves until the host kills `am instrument`.
 */
class StoreDriverInstrumentation : Instrumentation() {
    private var port = DEFAULT_PORT

    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        port = arguments?.getString("port")?.toIntOrNull() ?: DEFAULT_PORT
        start() // runs onStart() on the instrumentation thread
    }

    override fun onStart() {
        val device = UiDevice.getInstance(this)
        val shell = Shell { uiAutomation }
        val server = StoreDriverHttpServer(
            port,
            UiElements(device),
            PlayStore(device, shell),
            AppControl(shell),
        )
        server.start()

        val result = Bundle()
        result.putString("bridge_port", port.toString())
        sendStatus(0, result)

        // Serve until the host ends `am instrument -w`.
        try { Thread.currentThread().join() } catch (_: InterruptedException) {}
        server.stop()
    }

    private companion object {
        /** Beside the harness bridge's 18500, so the two never collide on one device. */
        const val DEFAULT_PORT = 18510
    }
}
