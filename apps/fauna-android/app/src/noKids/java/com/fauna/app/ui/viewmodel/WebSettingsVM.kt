package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SessionAccount
import com.fauna.app.core.HexUtil
import com.fauna.app.core.ShellLog
import com.fauna.app.core.WebPublishStore
import com.fauna.ffi.FfiPublishedPost
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_web.SubdomainView
import uniffi.fauna_client_web.WebDomainRow
import javax.inject.Inject

/**
 * Renders the shared `fauna.web.*` authoring surface for the user
 * `web-settings` page (`web-content-hosting.md` § Published-post
 * management): the per-user subdomain toggle (`web-settings-subdomain-toggle`,
 * default OFF) that opts this actor's `web` content into serving at
 * `https://<handle>.<domain>/`, and the **Published-posts management
 * section** — every post this actor published to the web
 * (`fauna.web.publish.list`), each offering copy-link / copy-paywall-link
 * (gated rows only) / unpublish. Per priority #2 the VM holds **no**
 * URL/reserved-label/origin logic — the shared `web_subdomain_view` /
 * `web_site_link_view` projections (`fauna_core::web`, one source of truth
 * with the nest's routing) build the live URL and the copy affordances'
 * origin. Non-optimistic: the toggle re-renders only from the nest-echoed
 * state, so the rendered view doubles as the round-trip proof. The origin +
 * copy state is [WebPublishStore], the SAME singleton the feed ⋯-menu reads,
 * so the two surfaces can never disagree about a creator's address. Linux
 * lead: apps/fauna-linux/src/settings/web.rs.
 */
@HiltViewModel
class WebSettingsVM @Inject constructor(
    private val api: ApiClient,
    private val sessionAccount: SessionAccount,
    private val webPublishStore: WebPublishStore,
) : ViewModel() {

    private val web = api.webClient()

    /** The subdomain-toggle render state (enabled + live URL or disabled reason). */
    val view = MutableStateFlow(EMPTY_VIEW)
    val errorMessage = MutableStateFlow<String?>(null)

    /** The Published-posts rows (`fauna.web.publish.list`). */
    val posts = MutableStateFlow<List<FfiPublishedPost>>(emptyList())

    /** `false` until the page's first hydrate lands — a pre-read frame must
     *  not claim "no published posts" about a list nobody asked for. */
    val hydrated = MutableStateFlow(false)

    /** The nest blanked the caller's rendered pages and has not restored them
     *  yet (`fauna.web.publish.list`'s `rendered_pages_down`) — painted as
     *  `web-settings-render-status`, information only. */
    val renderedPagesDown = MutableStateFlow(false)

    /** The origin every copy affordance builds on, resolved from the SAME
     *  cached inputs the feed ⋯-menu reads. */
    val origin: StateFlow<String?> get() = webPublishStore.origin

    /** Set exactly when [origin] is `null` — why the actor's content has no
     *  public origin. */
    val disabledReason: StateFlow<uniffi.fauna_client_web.SiteLinkDisabledReason?>
        get() = webPublishStore.disabledReason

    /** `(postId, kind, url)` for the last web/paywall link this page copied. */
    val copied: StateFlow<Triple<String, String, String>?> get() = webPublishStore.copied

    /** `(kind, message)` for the last publish/unpublish/paywall-mint failure —
     *  `kind` is `"publish"` / `"unpublish"` / `"paywall"`. */
    val publishError: StateFlow<Pair<String, String>?> get() = webPublishStore.error

    // The actor handle (bare local part) + the nest's SERVING domain feed the
    // display URL; resolved once on hydrate and reused for the set-echo re-render.
    //
    // ⚠ `domain` is `fauna.nest.info`'s `web_serving_domain` — the host the nest's
    // own resolver routes on. It is deliberately NOT `setup.status`'s `domain`
    // (which this page used to read): that is `handle_domain()`, whose
    // `"localhost"` placeholder on a domainless box composes `<handle>.localhost`,
    // exactly the host the resolver never strips. `setup.status` is not consulted for
    // the domain (`web-content-hosting.md` § Published-post management).
    private var handle: String? = null
    private var domain: String = ""
    private var domainRows: List<WebDomainRow> = emptyList()

    init { hydrate() }

    /** Read the full `web-settings` page and render both surfaces from one
     *  answer. `getSubdomainEnabled` is a single NestClient RPC — the
     *  transport already parks it while the socket comes up (transport.md §
     *  Request lifecycle step 3). */
    private fun hydrate() {
        val w = web ?: return
        viewModelScope.launch {
            handle = bareHandle()
            domain = runCatching { w.servingDomain() }
                .onFailure { ShellLog.w("WebSettingsVM", "serving-domain fetch failed: ${it.message}") }
                .getOrDefault("")
            try {
                val enabled = w.getSubdomainEnabled()
                domainRows = runCatching { w.domainGet() }.getOrDefault(emptyList())
                val site = runCatching { w.publishedSite() }.getOrNull()
                posts.value = site?.posts ?: emptyList()
                renderedPagesDown.value = site?.renderedPagesDown ?: false
                hydrated.value = true
                webPublishStore.applyWebPage(domainRows, enabled, handle, domain)
                render(enabled)
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that
                // has not landed yet (NestClient::request_inner).
            }
        }
    }

    /** Flip the opt-in, then re-render from the nest's echoed state (non-optimistic). */
    fun setEnabled(enabled: Boolean) {
        val w = web ?: return
        viewModelScope.launch {
            try {
                val echoed = w.setSubdomainEnabled(enabled)
                render(echoed)
                webPublishStore.applyWebPage(domainRows, echoed, handle, domain)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Copy the public page URL — purely local, no round trip. */
    fun copyWebLink(post: FfiPublishedPost) {
        webPublishStore.copyWebLink(HexUtil.bytesToHex(post.postId), post.slug)
    }

    /** Mint + copy a short-lived full-access link for a gated, published post. */
    fun copyPaywallLink(post: FfiPublishedPost) {
        webPublishStore.copyPaywallLink(HexUtil.bytesToHex(post.postId), post.slug)
    }

    /** Take a published post down, then re-read `publish.list` so the section
     *  reflects the nest's state — a takedown that half-applied then shows up
     *  as a row that stayed rather than one that vanished from a screen the
     *  nest disagrees with (mirrors linux's `unpublish`). */
    fun unpublish(post: FfiPublishedPost) {
        val w = web ?: return
        viewModelScope.launch {
            webPublishStore.unpublish(HexUtil.bytesToHex(post.postId))
            // The whole answer, not just the rows: a takedown's render can be
            // what restores the pages, and the status line follows the same read.
            runCatching { w.publishedSite() }.getOrNull()?.let { site ->
                posts.value = site.posts
                renderedPagesDown.value = site.renderedPagesDown
            }
        }
    }

    private fun render(enabled: Boolean) {
        view.value = runCatching { com.fauna.ffi.webSubdomainView(enabled, handle, domain) }
            .getOrDefault(SubdomainView(enabled = enabled, url = null, disabledReason = null))
    }

    private fun bareHandle(): String? =
        sessionAccount.handle?.substringBefore("@")?.ifBlank { null }

    private companion object {
        val EMPTY_VIEW = SubdomainView(enabled = false, url = null, disabledReason = null)
    }
}
