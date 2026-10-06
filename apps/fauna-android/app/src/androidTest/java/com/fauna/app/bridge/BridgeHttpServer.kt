package com.fauna.app.bridge

import com.fauna.app.core.downloadsDir
import fi.iki.elonen.NanoHTTPD
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.ConcurrentLinkedQueue

class BridgeHttpServer(
    port: Int,
    private val elementOps: ElementOps,
    private val launcher: AppLauncher,
) : NanoHTTPD("127.0.0.1", port) {

    private val commandQueue = ConcurrentLinkedQueue<JSONObject>()
    @Volatile private var cachedAppState: JSONObject? = null

    override fun serve(session: IHTTPSession): Response {
        return try {
            route(session)
        } catch (e: SelectOptionNotOffered) {
            json(Response.Status.CONFLICT, """{"error":"${esc(e.message ?: "")}"}""")
        } catch (e: UnsupportedKey) {
            json(Response.Status.BAD_REQUEST, """{"error":"${esc(e.message ?: "")}"}""")
        } catch (e: ElementNotFound) {
            json(Response.Status.NOT_FOUND, """{"error":"${esc(e.message ?: "")}"}""")
        } catch (e: Exception) {
            json(Response.Status.INTERNAL_ERROR, """{"error":"${esc(e.message ?: "")}"}""")
        }
    }

    private fun route(session: IHTTPSession): Response {
        val uri = session.uri
        val method = session.method

        // GET routes. A scoped read carries `scope` as a JSON-encoded string
        // query parameter (`drivers/http_bridge.py` `json.dumps(wire)`).
        if (method == Method.GET) return when (uri) {
            "/health" -> json(Response.Status.OK, """{"ready":true,"platform":"android","version":"1.0.0"}""")
            "/app/commands" -> {
                val cmd = commandQueue.poll()
                if (cmd == null) newFixedLengthResponse(Response.Status.NO_CONTENT, MIME_PLAINTEXT, "")
                else json(Response.Status.OK, cmd.toString())
            }
            "/app/state" -> {
                val s = cachedAppState
                if (s == null) newFixedLengthResponse(Response.Status.NO_CONTENT, MIME_PLAINTEXT, "")
                else json(Response.Status.OK, s.toString())
            }
            "/element/text" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                val idx = session.parms["index"]?.toIntOrNull() ?: 0
                val scope = parseScope(session.parms["scope"])
                json(Response.Status.OK, JSONObject().put("text", elementOps.getText(id, idx, scope)).toString())
            }
            "/element/attr" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                val attr = session.parms["attr"] ?: return badReq("attr required")
                val idx = session.parms["index"]?.toIntOrNull() ?: 0
                val scope = parseScope(session.parms["scope"])
                // `JSONObject.put(key, null)` REMOVES the key; `NULL` keeps an
                // explicit `"value": null`, which the driver reads as `None`.
                val value: Any = elementOps.getAttr(id, attr, idx, scope) ?: JSONObject.NULL
                json(Response.Status.OK, JSONObject().put("value", value).toString())
            }
            "/element/enabled" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                val idx = session.parms["index"]?.toIntOrNull() ?: 0
                val scope = parseScope(session.parms["scope"])
                json(Response.Status.OK, """{"enabled":${elementOps.isEnabled(id, idx, scope)}}""")
            }
            "/element/visible" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                val scope = parseScope(session.parms["scope"])
                json(Response.Status.OK, """{"visible":${elementOps.isVisible(id, scope)}}""")
            }
            "/element/count" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                val scope = parseScope(session.parms["scope"])
                json(Response.Status.OK, """{"count":${elementOps.count(id, scope)}}""")
            }
            // The read half of the `seed_credentials` crossing: the on-device
            // e2e credential file, verbatim, for the host's registry
            // assertions (`drivers/android.py::credential_map`). No host path
            // reaches the app's filesDir, so this is the only view of it.
            "/credentials" -> json(
                Response.Status.OK,
                JSONObject().put("credentials", launcher.readCredentials()).toString(),
            )
            // The download observation seam (`drivers/android.py::download_dir`):
            // the app's own download directory, listed flat, then one file's
            // bytes at a time. Every other app saves e2e downloads onto the
            // pytest host; android's land in its cache, which no host path
            // reaches, so they cross here — the read twin of `/input-file`.
            "/download-dir" -> {
                val files = JSONArray()
                launcher.downloadFiles().forEach { f ->
                    files.put(
                        JSONObject().put("name", f.name).put("size", f.length()).put("mtime_ms", f.lastModified())
                    )
                }
                json(Response.Status.OK, JSONObject().put("files", files).toString())
            }
            "/download-file" -> {
                val name = session.parms["name"] ?: return badReq("name required")
                val file = launcher.downloadFile(name)
                    ?: return json(Response.Status.NOT_FOUND, """{"error":"no such download"}""")
                val stream = try {
                    java.io.FileInputStream(file)
                } catch (_: java.io.FileNotFoundException) {
                    // Renamed or deleted between the listing and this read.
                    return json(Response.Status.NOT_FOUND, """{"error":"no such download"}""")
                }
                newFixedLengthResponse(Response.Status.OK, "application/octet-stream", stream, file.length())
            }
            else -> json(Response.Status.NOT_FOUND, """{"error":"Unknown: GET $uri"}""")
        }

        // DELETE routes
        if (method == Method.DELETE) return when (uri) {
            "/session" -> {
                launcher.stopApp()
                json(Response.Status.OK, """{"closed":true}""")
            }
            else -> json(Response.Status.NOT_FOUND, """{"error":"Unknown: DELETE $uri"}""")
        }

        // POST routes. A scoped action carries `scope` as a JSON array in the
        // body (`drivers/http_bridge.py` `body["scope"] = wire`).
        // `set_input_files`'s crossing (`drivers/android.py::push_input_file`):
        // the picked file's RAW bytes, so it is answered before `readBody`,
        // which would decode them as a string. The written file's device path
        // is the answer; the host path never reaches the device.
        if (method == Method.POST && uri == "/input-file") {
            val name = session.parms["name"] ?: return badReq("name required")
            val length = session.headers["content-length"]?.toLongOrNull()
                ?: return badReq("content-length required")
            val file = launcher.writeInputFile(name, session.inputStream, length)
            return json(Response.Status.OK, JSONObject().put("path", file.absolutePath).toString())
        }

        if (method == Method.POST) {
            val body = readBody(session)
            return when (uri) {
                "/session" -> {
                    val data = JSONObject(body)
                    val env = data.optJSONObject("environment") ?: JSONObject()
                    val seedCredentials = data.optJSONObject("seed_credentials")
                    launcher.launchApp(
                        env.optString("FAUNA_E2E_BRIDGE", ""),
                        seedCredentials,
                        // "1"/"true"/etc, matching conftest.py's
                        // _apply_real_conversations_env string-flag convention
                        // (env values arrive as JSON strings, never booleans).
                        env.optString("FAUNA_E2E_REAL_CONVERSATIONS", "").isNotEmpty(),
                        env.optString("FAUNA_E2E_TRUST_NEST_IDENTITY", ""),
                        env.optString("FAUNA_E2E_CLOCK_OFFSET_SECS", ""),
                    )
                    json(Response.Status.CREATED, """{"session_id":"1"}""")
                }
                "/element/click" -> {
                    val d = JSONObject(body)
                    elementOps.click(d.getString("id"), d.optInt("index", 0), scopeOf(d))
                    json(Response.Status.OK, """{}""")
                }
                "/element/type" -> {
                    val d = JSONObject(body)
                    elementOps.type(d.getString("id"), d.getString("text"), d.optInt("index", 0), scopeOf(d))
                    json(Response.Status.OK, """{}""")
                }
                "/element/key" -> {
                    val d = JSONObject(body)
                    elementOps.pressKey(d.getString("id"), d.getString("key"), d.optInt("index", 0), scopeOf(d))
                    json(Response.Status.OK, """{}""")
                }
                "/element/clear" -> {
                    val d = JSONObject(body)
                    elementOps.clear(d.getString("id"), d.optInt("index", 0), scopeOf(d))
                    json(Response.Status.OK, """{}""")
                }
                "/element/select" -> {
                    val d = JSONObject(body)
                    elementOps.select(d.getString("id"), d.getString("value"), d.optInt("index", 0), scopeOf(d))
                    json(Response.Status.OK, """{}""")
                }
                // `driver.scroll_to` (`drivers/http_bridge.py`): bring one
                // element into the viewport; `found` false (with the reason)
                // when it is absent or no scroller would show it.
                "/element/scroll-into-view" -> {
                    val d = JSONObject(body)
                    val error = elementOps.scrollIntoView(d.getString("id"), d.optInt("index", 0), scopeOf(d))
                    val out = JSONObject().put("found", error == null)
                    if (error != null) out.put("error", error)
                    json(Response.Status.OK, out.toString())
                }
                // The system back — what the phone's back gesture/button
                // sends, so the screen's own `BackHandler` handles it
                // (`drivers/android.py::press_system_back`).
                "/device/back" -> {
                    elementOps.pressBack()
                    json(Response.Status.OK, """{}""")
                }
                "/screenshot" -> {
                    val d = JSONObject(body); val p = elementOps.screenshot(d.getString("name"))
                    json(Response.Status.OK, """{"path":"$p"}""")
                }
                "/app/commands" -> {
                    commandQueue.add(JSONObject(body))
                    json(Response.Status.CREATED, """{"queued":true}""")
                }
                "/app/state" -> {
                    cachedAppState = JSONObject(body)
                    json(Response.Status.OK, """{"received":true}""")
                }
                // The re-auth seam's verdict (`AccountReauth.e2eVerdict`),
                // written beside the credential file; a null verdict removes
                // it, which the app reads as decline (fail-closed).
                "/reauth-result" -> {
                    val d = JSONObject(body)
                    val verdict = if (d.isNull("verdict")) null else d.getString("verdict")
                    launcher.writeReauthVerdict(verdict)
                    json(Response.Status.OK, """{"written":${verdict != null}}""")
                }
                else -> json(Response.Status.NOT_FOUND, """{"error":"Unknown: POST $uri"}""")
            }
        }

        return json(Response.Status.NOT_FOUND, """{"error":"$method $uri"}""")
    }

    private fun readBody(session: IHTTPSession): String {
        val map = HashMap<String, String>()
        session.parseBody(map)
        return map["postData"] ?: ""
    }

    private fun json(status: Response.Status, body: String): Response =
        newFixedLengthResponse(status, "application/json", body)

    private fun badReq(msg: String) = json(Response.Status.BAD_REQUEST, """{"error":"$msg"}""")

    private fun esc(s: String) = s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n")
}

/** The `scope` of a POST body — an array of `{"id", "index"}` steps, or none. */
internal fun scopeOf(body: JSONObject): List<ScopeStep> =
    body.optJSONArray("scope")?.let(::scopeSteps) ?: emptyList()

/** The `scope` of a GET query — the same array, JSON-encoded as a string, or none. */
internal fun parseScope(raw: String?): List<ScopeStep> {
    if (raw.isNullOrEmpty()) return emptyList()
    return scopeSteps(JSONArray(raw))
}

private fun scopeSteps(steps: JSONArray): List<ScopeStep> =
    (0 until steps.length()).map { i ->
        val step = steps.getJSONObject(i)
        ScopeStep(step.getString("id"), step.optInt("index", 0))
    }

class AppLauncher(private val instrumentation: android.app.Instrumentation) {
    /**
     * Fixed on-device path for the e2e credential seed
     * ([com.fauna.app.core.FileSecretBackend]'s file). The instrumentation
     * runs in the SAME process/UID as the target app (standard `am
     * instrument` same-process attach), so writing via
     * `instrumentation.targetContext.filesDir` here lands in exactly the
     * directory the launched app's own `Context.filesDir` resolves to — no
     * adb push, no `run-as`, no host↔device filesystem boundary to cross;
     * the boundary was already crossed by the `/session` HTTP POST itself.
     */
    private fun credentialFile(): java.io.File =
        java.io.File(instrumentation.targetContext.filesDir, "e2e_credentials.json")

    /**
     * The re-auth seam's verdict file: `reauth-result` in the credential
     * file's own directory, which is where `AccountReauth.e2eSeamDir` looks —
     * the android `{FAUNA_E2E_CREDENTIAL_DIR}/reauth-result`.
     */
    private fun reauthResultFile(): java.io.File =
        java.io.File(credentialFile().parentFile, "reauth-result")

    /** The credential file's flat map, `{}` when absent or unreadable. */
    fun readCredentials(): JSONObject {
        val file = credentialFile()
        if (!file.exists()) return JSONObject()
        return runCatching { JSONObject(file.readText()) }.getOrDefault(JSONObject())
    }

    /**
     * Where `set_input_files`' picked files land: the app's own cache dir,
     * which the instrumentation (same UID) writes and the app's `TestAgent`
     * reads. One fresh subdirectory per file, so the basename — which the
     * agent turns into the attachment's name and MIME — stays exactly the
     * host's, and two picks of the same name never overwrite each other.
     */
    private fun inputFileRoot(): java.io.File =
        java.io.File(instrumentation.targetContext.cacheDir, "e2e-input")

    /**
     * The app's download directory — where every android download surface
     * writes a file before offering it to the share sheet
     * ([com.fauna.app.core.downloadsDir]). Read here, by the same UID, so
     * the host can observe what a Download press really saved.
     */
    private fun downloadRoot(): java.io.File = instrumentation.targetContext.downloadsDir()

    /** The regular files directly in [downloadRoot], or none when it is absent. */
    fun downloadFiles(): List<java.io.File> =
        downloadRoot().listFiles()?.filter { it.isFile }?.sortedBy { it.name } ?: emptyList()

    /**
     * The file named [name] directly in [downloadRoot], or `null` when there is
     * none. A [name] that is not a bare file name is treated as absent (it
     * could escape the root).
     */
    fun downloadFile(name: String): java.io.File? {
        if (name.isEmpty() || name == "." || name == ".." || '/' in name || '\u0000' in name) return null
        return java.io.File(downloadRoot(), name).takeIf { it.isFile }
    }

    private val inputFileSeq = java.util.concurrent.atomic.AtomicInteger()

    /**
     * Write exactly [length] bytes of [source] to a fresh file named [name]
     * under [inputFileRoot] and return that file. A [name] that is not a bare file name
     * is refused (it could escape the root), and so is a body shorter than
     * its declared length (a truncated attachment would pass as a real one).
     */
    fun writeInputFile(name: String, source: java.io.InputStream, length: Long): java.io.File {
        require(name.isNotEmpty() && name != "." && name != ".." && '/' !in name && '\u0000' !in name) {
            "input-file name must be a bare file name, got \"$name\""
        }
        val dir = java.io.File(inputFileRoot(), inputFileSeq.incrementAndGet().toString())
        check(dir.mkdirs() || dir.isDirectory) { "could not create $dir" }
        val file = java.io.File(dir, name)
        file.outputStream().use { out ->
            val buf = ByteArray(64 * 1024)
            var left = length
            while (left > 0) {
                val n = source.read(buf, 0, minOf(buf.size.toLong(), left).toInt())
                check(n >= 0) { "input-file body ended ${length - left} of $length bytes in" }
                out.write(buf, 0, n)
                left -= n
            }
        }
        return file
    }

    /** Write `verdict` to the seam file, or remove the file for `null`. */
    fun writeReauthVerdict(verdict: String?) {
        val file = reauthResultFile()
        if (verdict == null) file.delete() else file.writeText(verdict)
    }

    fun launchApp(
        bridgeUrl: String,
        seedCredentials: JSONObject? = null,
        realConversations: Boolean = false,
        trustNestIdentity: String = "",
        clockOffsetSecs: String = "",
    ) {
        val pkg = instrumentation.targetContext.packageName
        Runtime.getRuntime().exec(arrayOf("am", "force-stop", pkg)).waitFor()
        Thread.sleep(500)

        // A verdict outlives the session that wrote it (filesDir survives the
        // force-stop), so every launch starts from none — absent is decline,
        // the strictest arm — as the other apps' fresh per-launch credential
        // dir does by construction.
        reauthResultFile().delete()
        // Picked files are per-launch too: a relaunched app holds no staged
        // attachment that could still name one.
        inputFileRoot().deleteRecursively()

        val credFile = credentialFile()
        if (seedCredentials != null) {
            // Full overwrite (matches build_registry_seed's producer contract:
            // one complete flat map per seed, not a merge).
            credFile.writeText(seedCredentials.toString())
        }

        val intent = instrumentation.targetContext.packageManager.getLaunchIntentForPackage(pkg)
            ?: throw IllegalStateException("No launch intent for $pkg")
        if (bridgeUrl.isNotEmpty()) {
            intent.putExtra("FAUNA_E2E_BRIDGE", bridgeUrl)
        }
        if (realConversations) {
            intent.putExtra("FAUNA_E2E_REAL_CONVERSATIONS", true)
        }
        if (trustNestIdentity.isNotEmpty()) {
            // The R14 (account-data-plane.md § The ratified decisions) escrow-holder trust seed (e2e-automation-surface-gating.md
            // § The e2e trust seed) — the only door into the account runtime's
            // trust set MainActivity's Os.setenv re-export can reach.
            intent.putExtra("FAUNA_E2E_TRUST_NEST_IDENTITY", trustNestIdentity)
        }
        if (clockOffsetSecs.isNotEmpty()) {
            // The launch clock's e2e offset (`fauna_launch_machine::launch_clock`,
            // read once from process env on first use) — the wrong-clock launch
            // witness's seed. Forwarded as a string and re-exported verbatim by
            // MainActivity's Os.setenv door, beside the trust seed above.
            intent.putExtra("FAUNA_E2E_CLOCK_OFFSET_SECS", clockOffsetSecs)
        }
        // Forward whenever a seed file exists on disk -- not only on the
        // launch that wrote it -- so a later relaunch with no fresh seed
        // still finds the previously-seeded registry instead of silently
        // falling back to the real encrypted store.
        if (credFile.exists()) {
            intent.putExtra("FAUNA_E2E_CREDENTIAL_FILE", credFile.absolutePath)
        }
        intent.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK or android.content.Intent.FLAG_ACTIVITY_CLEAR_TASK)
        instrumentation.targetContext.startActivity(intent)
    }

    fun stopApp() {
        val pkg = instrumentation.targetContext.packageName
        Runtime.getRuntime().exec(arrayOf("am", "force-stop", pkg)).waitFor()
    }
}
