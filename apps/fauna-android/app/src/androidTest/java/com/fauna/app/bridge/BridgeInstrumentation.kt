package com.fauna.app.bridge

import android.app.Instrumentation
import android.os.Bundle
import androidx.test.uiautomator.UiDevice

class BridgeInstrumentation : Instrumentation() {
    private var port = 18500

    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        port = arguments?.getString("port")?.toIntOrNull() ?: 18500
        start() // triggers onStart() on the instrumentation thread
    }

    override fun onStart() {
        val device = UiDevice.getInstance(this)
        // The same UiAutomation UiDevice attached to (default flags → the
        // cached connection), for the raw accessibility-tree reads UiObject2
        // has no accessor for (`stateDescription`).
        val elementOps = ElementOps(device) { uiAutomation }
        val launcher = AppLauncher(this)

        val server = BridgeHttpServer(port, elementOps, launcher)
        server.start()

        val result = Bundle()
        result.putString("bridge_port", port.toString())
        sendStatus(0, result)

        // Block forever — bridge stays alive until killed
        try { Thread.currentThread().join() } catch (_: InterruptedException) {}

        // Cleanup
        server.stop()
    }
}
