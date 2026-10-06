package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.WebSettingsVM
import com.fauna.ffi.FfiPublishedPost
import uniffi.fauna_client_web.SiteLinkDisabledReason
import uniffi.fauna_client_web.SubdomainDisabledReason
import uniffi.fauna_client_web.SubdomainView
import social.fauna.generated.Ids

/**
 * The user `web-settings` page (`web-content-hosting.md` § Published-post
 * management): the per-user subdomain opt-in. One control — the subdomain toggle
 * (`web-settings-subdomain-toggle`, default OFF) that serves the user's `web`
 * content at `https://<handle>.<domain>/`; the live URL (or a disabled reason)
 * renders below it, plus a static explainer pointing at the two content sources
 * (a `web`-mode folder + web-published posts). The admin uses this same page for
 * their own site; the nest-wide apex designation is the separate `admin-web` page.
 *
 * Stateless [WebSettingsContent] is split out for the Compose test harness; the
 * VM-bound [WebSettingsScreen] is the wrapper the NavHost mounts. Per priority #2
 * the shell holds no web logic — the shared `web_subdomain_view` projection drives
 * the render. Mirrors the Linux lead (apps/fauna-linux/src/settings/web.rs);
 * `page-heading` is this screen's TopAppBar title, `error-message` the global
 * MessageBanner.
 */
@Composable
fun WebSettingsScreen(
    navController: NavController,
    vm: WebSettingsVM = hiltViewModel(),
) {
    val view by vm.view.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val posts by vm.posts.collectAsState()
    val hydrated by vm.hydrated.collectAsState()
    val renderedPagesDown by vm.renderedPagesDown.collectAsState()
    val origin by vm.origin.collectAsState()
    val disabledReason by vm.disabledReason.collectAsState()
    val copied by vm.copied.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    val publishError by vm.publishError.collectAsState()
    val publishErrorText = publishError?.let { (kind, message) ->
        when (kind) {
            "unpublish" -> stringResourceFmt(R.string.web_publish_error_unpublish, message)
            "paywall" -> stringResourceFmt(R.string.web_publish_error_paywall_link, message)
            else -> stringResourceFmt(R.string.web_publish_error_publish, message)
        }
    }
    LaunchedEffect(publishErrorText) { publishErrorText?.let { appMessages.showError(it) } }

    WebSettingsContent(
        view = view,
        onBack = { navController.popBackStack() },
        onSetEnabled = vm::setEnabled,
        posts = posts,
        hydrated = hydrated,
        renderedPagesDown = renderedPagesDown,
        origin = origin,
        disabledReason = disabledReason,
        copied = copied,
        onCopyWebLink = vm::copyWebLink,
        onCopyPaywallLink = vm::copyPaywallLink,
        onUnpublish = vm::unpublish,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WebSettingsContent(
    view: SubdomainView,
    onBack: () -> Unit,
    onSetEnabled: (Boolean) -> Unit,
    posts: List<FfiPublishedPost> = emptyList(),
    hydrated: Boolean = false,
    renderedPagesDown: Boolean = false,
    origin: String? = null,
    disabledReason: SiteLinkDisabledReason? = null,
    copied: Triple<String, String, String>? = null,
    onCopyWebLink: (FfiPublishedPost) -> Unit = {},
    onCopyPaywallLink: (FfiPublishedPost) -> Unit = {},
    onUnpublish: (FfiPublishedPost) -> Unit = {},
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.web_settings_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
                    }
                },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            // ── Subdomain opt-in toggle (default OFF) ──
            // The toggle below is the commit; hoisted here so the reason can
            // render under the whole row.
            val subdomainGate = faunaGate("fauna.web.set_subdomain_enabled")
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(stringResource(R.string.web_settings_subdomain_toggle_label), style = MaterialTheme.typography.bodyLarge)
                    Text(
                        stringResource(R.string.web_settings_subdomain_toggle_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                // A dispatch-on-change toggle IS the commit — there is no Save
                // beside it to carry the declaration (the rule batch 1 earned on
                // `admin-service-pairing-toggle`).
                Switch(
                    checked = view.enabled,
                    onCheckedChange = onSetEnabled,
                    enabled = subdomainGate.enabled,
                    modifier = Modifier.testTag(Ids.WEB_SETTINGS_SUBDOMAIN_TOGGLE),
                )
            }
            DisabledControlReasonText(subdomainGate.reason)

            // ── Live URL / disabled-reason row ──
            // The live URL renders whether the toggle is on or off — it's where the
            // site would serve; else the reason it can't (no handle / reserved label).
            val urlText = view.url ?: when (view.disabledReason) {
                SubdomainDisabledReason.NO_HANDLE -> stringResource(R.string.web_settings_subdomain_no_handle)
                SubdomainDisabledReason.RESERVED_LABEL -> stringResource(R.string.web_settings_subdomain_reserved)
                // The nest serves no web content at any host — say so rather
                // than leave the row blank.
                SubdomainDisabledReason.NO_SERVING_DOMAIN ->
                    stringResource(R.string.web_settings_subdomain_no_serving_domain)
                null -> ""
            }
            Column {
                Text(
                    stringResource(R.string.web_settings_subdomain_url_label),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(urlText, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.testTag(Ids.WEB_SETTINGS_SUBDOMAIN_URL))
            }

            // ── Blanked-site status line ──
            // Information only, no button: the nest restores the site by itself
            // (web-content-hosting.md § Routing, render, serving → *A blanked
            // site tells its author*). Absent on a healthy site.
            if (renderedPagesDown) {
                Text(
                    stringResource(R.string.web_settings_render_status_down),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.WEB_SETTINGS_RENDER_STATUS),
                )
            }

            // ── Static content explainer ──
            Text(
                stringResource(R.string.web_settings_content_info),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.WEB_SETTINGS_CONTENT_INFO),
            )

            // ── Published-posts management section (web-content-hosting.md
            //    § Published-post management) — painted only once the page
            //    has actually read the list; a pre-read frame must not claim
            //    "no published posts" about a list nobody asked for. ──
            if (hydrated) {
                HorizontalDivider()
                Text(stringResource(R.string.web_settings_published_posts_title), style = MaterialTheme.typography.titleMedium)
                if (posts.isEmpty()) {
                    Text(
                        stringResource(R.string.web_settings_published_posts_empty),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.WEB_PUBLISHED_POSTS_EMPTY),
                    )
                } else {
                    Column(
                        modifier = Modifier.testTag(Ids.WEB_PUBLISHED_POSTS_LIST),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        if (posts.any { it.gatedTier != null }) {
                            Text(
                                stringResource(R.string.web_publish_paywall_link_note),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        if (origin == null) {
                            Text(
                                linkDisabledText(disabledReason),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        posts.forEach { post ->
                            PublishedPostRow(
                                post = post,
                                origin = origin,
                                copied = copied,
                                onCopyWebLink = { onCopyWebLink(post) },
                                onCopyPaywallLink = { onCopyPaywallLink(post) },
                                onUnpublish = { onUnpublish(post) },
                            )
                        }
                    }
                }
            }
        }
    }
}

/** The three reasons published content has no public address, shown in place
 *  of the copy affordances rather than handing out a link that cannot load
 *  (`web-content-hosting.md`: "legal but unreachable — the UI must say so").
 *  The key mapping is the shared `fauna_client_web::disabled_reason_text` door
 *  (web-content-hosting.md § Published-post management) — tui/linux/macOS/iOS/web
 *  resolve the same door. */
@Composable
private fun linkDisabledText(reason: SiteLinkDisabledReason?): String =
    localized(com.fauna.ffi.webDisabledReasonText(reason)).orEmpty()

/**
 * One `web-published-posts-list` row: slug + a gated-tier badge, the copy-link
 * / copy-paywall-link (gated rows only) / unpublish verbs. Both copy
 * affordances disable when [origin] is `null` — publishing with no origin is
 * legal but unreachable, and the doc is explicit that the UI must say so
 * rather than hand out a link that cannot load. Mirrors linux's
 * `build_published_row` / web's row markup.
 */
@Composable
private fun PublishedPostRow(
    post: FfiPublishedPost,
    origin: String?,
    copied: Triple<String, String, String>?,
    onCopyWebLink: () -> Unit,
    onCopyPaywallLink: () -> Unit,
    onUnpublish: () -> Unit,
) {
    val postIdHex = HexUtil.bytesToHex(post.postId)
    Column(
        modifier = Modifier.testTag(Ids.WEB_PUBLISHED_POST_ITEM),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(post.slug, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f).testTag(Ids.WEB_PUBLISHED_POST_SLUG))
            post.gatedTier?.let {
                Text(
                    stringResourceFmt(R.string.web_settings_published_post_gated_badge, it),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = onCopyWebLink, enabled = origin != null, modifier = Modifier.testTag(Ids.WEB_PUBLISHED_POST_COPY_LINK_BUTTON)) {
                Text(stringResource(R.string.web_publish_copy_web_link))
            }
            if (post.gatedTier != null) {
                // ⚠ Reads like a local clipboard action, but it is a COMMIT: it
                // calls `paywallMintToken` and mints a capability token on the
                // nest (`WebPublishStore.copyPaywallLink`). The trap row 253
                // records is about grading from the issuer's name; this is its
                // mirror — the CONTROL's name understates what it does. Its
                // plain copy-link sibling beside it is genuinely local (it
                // formats an already-known origin) and stays live.
                val paywallGate = faunaGate("fauna.web.paywall.mint_token", enabled = origin != null)
                TextButton(onClick = onCopyPaywallLink, enabled = paywallGate.enabled, modifier = Modifier.testTag(Ids.WEB_PUBLISHED_POST_COPY_PAYWALL_LINK_BUTTON)) {
                    Text(stringResource(R.string.web_publish_copy_paywall_link))
                }
            }
            TextButton(onClick = onUnpublish, modifier = Modifier.testTag(Ids.WEB_PUBLISHED_POST_UNPUBLISH_BUTTON)) {
                Text(stringResource(R.string.web_publish_unpublish))
            }
        }
        // The copied value, painted back under the row — the devices-page
        // lesson: an unasserted copy affordance rots invisibly.
        if (copied != null && copied.first == postIdHex) {
            Text(
                if (copied.second == "web") {
                    stringResourceFmt(R.string.web_publish_copied_link, copied.third)
                } else {
                    stringResourceFmt(R.string.web_publish_copied_paywall_link, copied.third)
                },
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}
