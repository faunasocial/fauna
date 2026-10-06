package com.fauna.app.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

/**
 * [FileSecretBackend] is pure file I/O but uses `org.json.JSONObject`, whose
 * real behavior only exists under Robolectric's shadow (the compile-time
 * android.jar's org.json classes are `Stub!`-throwing placeholders) -- a
 * plain, non-Robolectric JVM test silently swallows that via this class's own
 * catch-all and reads back null everywhere, which is exactly what the first
 * version of this test did (every round-trip case failed `expected X but was
 * null`, verified 2026-07-19). Mirrors the round-trip + degrade cases windows'
 * `FileSecretBackendTests` covers for its C# twin.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class FileSecretBackendTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private fun backend(): Pair<FileSecretBackend, File> {
        val file = File(tmp.newFolder(), "e2e_credentials.json")
        return FileSecretBackend(file) to file
    }

    @Test
    fun missingFileReadsAsAbsent() {
        val (backend, _) = backend()
        assertNull(backend.get("fauna/index"))
    }

    @Test
    fun setThenGetRoundTrips() {
        val (backend, _) = backend()
        backend.set("fauna/index", """{"active":"abc","accounts":[]}""")
        assertEquals("""{"active":"abc","accounts":[]}""", backend.get("fauna/index"))
    }

    @Test
    fun setPreservesOtherKeys() {
        val (backend, _) = backend()
        backend.set("fauna/abc/secret", "s1")
        backend.set("fauna/index", "idx")
        assertEquals("s1", backend.get("fauna/abc/secret"))
        assertEquals("idx", backend.get("fauna/index"))
    }

    @Test
    fun deleteRemovesOnlyThatKey() {
        val (backend, _) = backend()
        backend.set("fauna/abc/secret", "s1")
        backend.set("fauna/index", "idx")
        backend.delete("fauna/abc/secret")
        assertNull(backend.get("fauna/abc/secret"))
        assertEquals("idx", backend.get("fauna/index"))
    }

    @Test
    fun corruptFileDegradesToAbsentNotCrash() {
        val file = File(tmp.newFolder(), "e2e_credentials.json")
        file.writeText("not valid json{{{")
        val backend = FileSecretBackend(file)
        assertNull(backend.get("fauna/index"))
    }

    @Test
    fun preSeededFlatMapIsReadableVerbatim() {
        // Mirrors what AppLauncher writes: a flat JSON map produced by
        // tests/common/accounts.py::build_registry_seed, written whole before
        // the app (and this backend) ever reads it.
        val file = File(tmp.newFolder(), "e2e_credentials.json")
        file.writeText(
            """{"fauna/index":"{\"active\":\"a1\",\"accounts\":[]}","secret_key":"deadbeef"}""",
        )
        val backend = FileSecretBackend(file)
        assertEquals("""{"active":"a1","accounts":[]}""", backend.get("fauna/index"))
        assertEquals("deadbeef", backend.get("secret_key"))
    }
}
