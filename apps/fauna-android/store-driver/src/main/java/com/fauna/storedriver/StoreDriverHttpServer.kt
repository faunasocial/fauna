package com.fauna.storedriver

import com.fauna.automation.ElementNotFound
import com.fauna.automation.UiElements
import com.fauna.automation.UnsupportedKey
import com.fauna.automation.parseScope
import com.fauna.automation.scopeOf
import fi.iki.elonen.NanoHTTPD
import org.json.JSONArray
import org.json.JSONObject

/**
 * The store driver's routes. The element routes are the harness bridge's own
 * wire shape (`tests/e2e-unified/drivers/http_bridge.py`) over the shared
 * [UiElements]; the rest is what an out-of-process driver can honestly do —
 * obtain the app from the Play Store, start it from its launcher entry, stop it.
 *
 * Deliberately absent, because they need the app's UID or seed what a user
 * cannot: `/session` and its launch-environment seeds, `/credentials`,
 * `/app-log*`, `/download-*`, `/clipboard/text`, `/input-file`,
 * `/reauth-result`, `/app/state`, `/app/commands`, and the raw-tree reads
 * (`/element/attr`, `/element/select`) the harness bridge's `ElementOps` adds.
 * Served on the device's loopback only; the host reaches it through
 * `adb forward`.
 */
class StoreDriverHttpServer(
    port: Int,
    private val elements: UiElements,
    private val play: PlayStore,
    private val app: AppControl,
) : NanoHTTPD("127.0.0.1", port) {

    override fun serve(session: IHTTPSession): Response = try {
        route(session)
    } catch (e: UnsupportedKey) {
        json(Response.Status.BAD_REQUEST, error(e))
    } catch (e: ElementNotFound) {
        json(Response.Status.NOT_FOUND, error(e))
    } catch (e: Exception) {
        json(Response.Status.INTERNAL_ERROR, error(e))
    }

    private fun route(session: IHTTPSession): Response {
        val uri = session.uri
        if (session.method == Method.GET) return when (uri) {
            "/health" -> ok(JSONObject().put("ready", true).put("platform", "android").put("driver", "store"))
            "/element/text" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                val idx = session.parms["index"]?.toIntOrNull() ?: 0
                ok(JSONObject().put("text", elements.text(id, idx, parseScope(session.parms["scope"]))))
            }
            "/element/enabled" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                val idx = session.parms["index"]?.toIntOrNull() ?: 0
                ok(JSONObject().put("enabled", elements.isEnabled(id, idx, parseScope(session.parms["scope"]))))
            }
            "/element/visible" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                ok(JSONObject().put("visible", elements.isVisible(id, parseScope(session.parms["scope"]))))
            }
            "/element/count" -> {
                val id = session.parms["id"] ?: return badReq("id required")
                ok(JSONObject().put("count", elements.count(id, parseScope(session.parms["scope"]))))
            }
            else -> json(Response.Status.NOT_FOUND, """{"error":"Unknown: GET $uri"}""")
        }

        if (session.method == Method.POST) {
            val body = readBody(session).ifEmpty { "{}" }
            val d = JSONObject(body)
            return when (uri) {
                "/store/open" -> ok(JSONObject().put("opened", play.openListing()))
                "/store/install" -> {
                    val answer = play.install()
                    ok(JSONObject().put("state", answer.state).put("texts", JSONArray(answer.texts)))
                }
                "/app/launch" -> ok(JSONObject().put("output", app.launch()))
                "/app/stop" -> {
                    app.stop()
                    ok(JSONObject())
                }
                "/element/click" -> {
                    elements.click(d.getString("id"), d.optInt("index", 0), scopeOf(d))
                    ok(JSONObject())
                }
                "/element/type" -> {
                    elements.type(d.getString("id"), d.getString("text"), d.optInt("index", 0), scopeOf(d))
                    ok(JSONObject())
                }
                "/element/clear" -> {
                    elements.clear(d.getString("id"), d.optInt("index", 0), scopeOf(d))
                    ok(JSONObject())
                }
                "/element/key" -> {
                    elements.pressKey(d.getString("id"), d.getString("key"), d.optInt("index", 0), scopeOf(d))
                    ok(JSONObject())
                }
                "/device/back" -> {
                    elements.pressBack()
                    ok(JSONObject())
                }
                "/screenshot" -> ok(JSONObject().put("path", elements.screenshot(d.getString("name"))))
                else -> json(Response.Status.NOT_FOUND, """{"error":"Unknown: POST $uri"}""")
            }
        }
        return json(Response.Status.NOT_FOUND, """{"error":"Unknown: ${session.method} $uri"}""")
    }

    private fun readBody(session: IHTTPSession): String {
        val map = HashMap<String, String>()
        session.parseBody(map)
        return map["postData"] ?: ""
    }

    private fun ok(body: JSONObject) = json(Response.Status.OK, body.toString())

    private fun json(status: Response.Status, body: String): Response =
        newFixedLengthResponse(status, "application/json", body)

    private fun badReq(msg: String) = json(Response.Status.BAD_REQUEST, JSONObject().put("error", msg).toString())

    private fun error(e: Exception) = JSONObject().put("error", e.message ?: e.javaClass.simpleName).toString()
}
