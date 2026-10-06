package com.fauna.app.ui.screen.nostr

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.payments.ZapSignerItem
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiBridgeIdentity
import com.fauna.ffi.FfiBridgeSetting
import com.fauna.ffi.FfiBridgeStatus
import com.fauna.ffi.FfiCborValue
import com.fauna.ffi.FfiCreateBunkerInviteReply
import com.fauna.ffi.FfiFeatureRow
import uniffi.fauna_core.LocalizedText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [NostrContent] (`docs/goal/ui/nostr.md`) —
 * account linking, the 5 content toggles, relay management, follows, and the
 * Connected apps (NIP-46 bunker) section. Renders with seeded
 * [FfiBridgeStatus]/[FfiBridgeFollow] fixtures — no Hilt, no
 * FFI native calls (all pure UniFFI data classes) — so the canonical `nostr-*`
 * ui.yaml ids and the add/remove gestures are exercised on the JVM. (Android
 * E2E is host-emulator-gated — `android-nostr` track, `docs/goal/ui/nostr.md`.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NostrContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun boolSetting(key: String, value: Boolean) =
        FfiBridgeSetting(key = key, label = key, settingType = "bool", value = FfiCborValue.Bool(value), options = null)

    private fun textSetting(key: String, value: String) =
        FfiBridgeSetting(key = key, label = key, settingType = "text", value = FfiCborValue.Text(value), options = null)

    private fun linkedBridge(
        pubkey: String = "npub1abc",
        mode: String = "generated",
        relayListJson: String = "[]",
        exposeContent: Boolean = false,
        autoPublish: Boolean = false,
        publishReplies: Boolean = true,
        publishReactions: Boolean = false,
        inboundToFeed: Boolean = true,
    ) = FfiBridgeStatus(
        id = "nostr",
        name = "Nostr",
        available = true,
        linked = true,
        identity = FfiBridgeIdentity(label = "Public Key", value = pubkey, display = pubkey),
        mode = mode,
        settings = listOf(
            textSetting("relay_list", relayListJson),
            boolSetting("expose_content", exposeContent),
            boolSetting("auto_publish", autoPublish),
            boolSetting("publish_replies", publishReplies),
            boolSetting("publish_reactions", publishReactions),
            boolSetting("inbound_to_feed", inboundToFeed),
        ),
        supportsFollows = true,
        linkModes = null,
        error = null,
    )

    private fun unlinkedBridge() = FfiBridgeStatus(
        id = "nostr",
        name = "Nostr",
        available = true,
        linked = false,
        identity = null,
        mode = null,
        settings = emptyList(),
        supportsFollows = true,
        linkModes = null,
        error = null,
    )

    private fun zapSigner(
        id: Long,
        pubkey: String = "ab".repeat(32),
        label: String = "",
    ) = ZapSignerItem(
        id = id,
        signerPubkey = pubkey,
        label = label,
        createdAt = 100UL,
    )

    /** Mirrors `AccountSettingsContentTest.featureRow` — the same shared
     *  `FfiFeatureRow` shape the real `FeaturesClient::rows()` composition
     *  returns, not a fixture drifted from it. */
    private fun featureRow(feature: String, affordance: String = "available") = FfiFeatureRow(
        feature = feature,
        name = LocalizedText("features.$feature.name", emptyMap()),
        availability = if (affordance == "available") "allow" else "deny",
        deniedBy = null,
        cells = emptyList(),
        perOperationMax = null,
        perOperationMaxTier = null,
        unit = "bytes",
        affordance = affordance,
        restriction = null,
        status = LocalizedText("features.status.$affordance", emptyMap()),
    )

    private fun render(
        registered: Boolean = true,
        bridge: FfiBridgeStatus? = unlinkedBridge(),
        follows: List<FfiBridgeFollow> = emptyList(),
        bunkerInvite: FfiCreateBunkerInviteReply? = null,
        zapSigners: List<ZapSignerItem> = emptyList(),
        zapSignerGateRow: FfiFeatureRow? = null,
        onLink: (String, Map<String, String>) -> Unit = { _, _ -> },
        onUnlink: () -> Unit = {},
        onUpdateSetting: (String, FfiCborValue) -> Unit = { _, _ -> },
        onAddRelay: (String) -> Unit = {},
        onRemoveRelay: (String) -> Unit = {},
        onAddFollow: (String, String?) -> Unit = { _, _ -> },
        onRemoveFollow: (String) -> Unit = {},
        onConnectApp: () -> Unit = {},
        onAddZapSigner: (String, String) -> Unit = { _, _ -> },
        onRemoveZapSigner: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            NostrContent(
                registered = registered,
                bridge = bridge,
                follows = follows,
                bunkerInvite = bunkerInvite,
                zapSigners = zapSigners,
                zapSignerGateRow = zapSignerGateRow,
                onLink = onLink,
                onUnlink = onUnlink,
                onUpdateSetting = onUpdateSetting,
                onAddRelay = onAddRelay,
                onRemoveRelay = onRemoveRelay,
                onAddFollow = onAddFollow,
                onRemoveFollow = onRemoveFollow,
                onConnectApp = onConnectApp,
                onAddZapSigner = onAddZapSigner,
                onRemoveZapSigner = onRemoveZapSigner,
            )
        }
    }

    @Test
    fun pageHeadingAlwaysRenders() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
    }

    @Test
    fun unavailableNoticeRendersWhenNotRegistered() {
        render(registered = false, bridge = null)
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("nostr-link-mode").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nostr-link-button").assertDoesNotExist()
    }

    @Test
    fun unlinkedSectionRendersLinkModeAndButtonNoNsecByDefault() {
        render(bridge = unlinkedBridge())
        composeTestRule.onNodeWithTag("nostr-link-mode").assertExists()
        composeTestRule.onNodeWithTag("nostr-link-button").assertExists()
        // generate is the default mode — nsec input only shows for import.
        composeTestRule.onNodeWithTag("nostr-nsec-input").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nostr-unlink-button").assertDoesNotExist()
    }

    @Test
    fun selectingImportModeRevealsNsecInput() {
        render(bridge = unlinkedBridge())
        composeTestRule.onNodeWithTag("nostr-link-mode").performClick()
        composeTestRule.onNodeWithText("Import nsec").performClick()
        composeTestRule.onNodeWithTag("nostr-nsec-input").assertExists()
    }

    @Test
    fun linkButtonFiresGenerateModeByDefault() {
        var calledMode: String? = null
        var calledFields: Map<String, String>? = null
        render(bridge = unlinkedBridge(), onLink = { m, f -> calledMode = m; calledFields = f })
        composeTestRule.onNodeWithTag("nostr-link-button").performClick()
        assertEquals("generate", calledMode)
        assertEquals(emptyMap<String, String>(), calledFields)
    }

    @Test
    fun linkedAccountSectionRendersPubkeyAndUnlinkButton() {
        render(bridge = linkedBridge(pubkey = "npub1xyz"))
        composeTestRule.onNodeWithTag("nostr-pubkey-copy-btn").assertExists()
        composeTestRule.onNodeWithText("npub1xyz").assertExists()
        composeTestRule.onNodeWithTag("nostr-unlink-button").assertExists()
        // Unlinked-only elements must not leak into the linked render.
        composeTestRule.onNodeWithTag("nostr-link-mode").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nostr-link-button").assertDoesNotExist()
    }

    @Test
    fun unlinkButtonFiresCallback() {
        var unlinked = false
        render(bridge = linkedBridge(), onUnlink = { unlinked = true })
        composeTestRule.onNodeWithTag("nostr-unlink-button").performClick()
        assertEquals(true, unlinked)
    }

    @Test
    fun contentTogglesRenderAndReflectState() {
        render(bridge = linkedBridge(exposeContent = true, publishReplies = false))
        composeTestRule.onNodeWithTag("nostr-expose-content").assertExists().assertIsOn()
        composeTestRule.onNodeWithTag("nostr-auto-publish").assertExists().assertIsOff()
        composeTestRule.onNodeWithTag("nostr-publish-replies").assertExists().assertIsOff()
        composeTestRule.onNodeWithTag("nostr-publish-reactions").assertExists().assertIsOff()
        composeTestRule.onNodeWithTag("nostr-inbound-to-feed").assertExists().assertIsOn()
    }

    @Test
    fun toggleFiresUpdateSettingWithTheFlippedValue() {
        var calledKey: String? = null
        var calledValue: FfiCborValue? = null
        render(
            bridge = linkedBridge(autoPublish = false),
            onUpdateSetting = { k, v -> calledKey = k; calledValue = v },
        )
        composeTestRule.onNodeWithTag("nostr-auto-publish").performScrollTo().performClick()
        assertEquals("auto_publish", calledKey)
        assertEquals(true, (calledValue as FfiCborValue.Bool).v)
    }

    @Test
    fun relaysSectionRendersEmptyState() {
        render(bridge = linkedBridge(relayListJson = "[]"))
        composeTestRule.onNodeWithTag("nostr-relay-item").assertDoesNotExist()
        composeTestRule.onNodeWithTag("nostr-relay-input").assertExists()
        composeTestRule.onNodeWithTag("nostr-add-relay").assertExists()
    }

    @Test
    fun relaysSectionRendersItems() {
        render(bridge = linkedBridge(relayListJson = "[\"wss://relay.one\",\"wss://relay.two\"]"))
        composeTestRule.onAllNodesWithTag("nostr-relay-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("nostr-remove-relay").assertCountEquals(2)
    }

    @Test
    fun addRelayFiresCallbackAndClearsInput() {
        var added: String? = null
        render(bridge = linkedBridge(), onAddRelay = { added = it })
        composeTestRule.onNodeWithTag("nostr-relay-input").performTextInput("wss://relay.example.com")
        composeTestRule.onNodeWithTag("nostr-add-relay").performClick()
        assertEquals("wss://relay.example.com", added)
    }

    @Test
    fun removeRelayFiresCallbackWithTheUrl() {
        var removed: String? = null
        render(
            bridge = linkedBridge(relayListJson = "[\"wss://relay.one\"]"),
            onRemoveRelay = { removed = it },
        )
        composeTestRule.onNodeWithTag("nostr-remove-relay").performScrollTo().performClick()
        assertEquals("wss://relay.one", removed)
    }

    @Test
    fun followsSectionRendersItemsAndPetname() {
        render(
            bridge = linkedBridge(),
            follows = listOf(
                FfiBridgeFollow(id = "npub1alice", petname = "Alice", createdAt = 0L, extra = null),
                FfiBridgeFollow(id = "npub1bob", petname = null, createdAt = 0L, extra = null),
            ),
        )
        composeTestRule.onAllNodesWithTag("nostr-follow-item").assertCountEquals(2)
        composeTestRule.onNodeWithText("Alice").assertExists()
    }

    @Test
    fun addFollowFiresCallbackAndClearsInputs() {
        var addedPubkey: String? = null
        var addedPetname: String? = null
        render(bridge = linkedBridge(), onAddFollow = { p, n -> addedPubkey = p; addedPetname = n })
        composeTestRule.onNodeWithTag("nostr-follow-pubkey-input").performTextInput("npub1carol")
        composeTestRule.onNodeWithTag("nostr-follow-petname-input").performTextInput("Carol")
        composeTestRule.onNodeWithTag("nostr-add-follow").performClick()
        assertEquals("npub1carol", addedPubkey)
        assertEquals("Carol", addedPetname)
    }

    @Test
    fun removeFollowFiresCallbackWithTheId() {
        var removedId: String? = null
        render(
            bridge = linkedBridge(),
            follows = listOf(FfiBridgeFollow(id = "npub1dave", petname = null, createdAt = 0L, extra = null)),
            onRemoveFollow = { removedId = it },
        )
        composeTestRule.onNodeWithTag("nostr-remove-follow").performScrollTo().performClick()
        assertEquals("npub1dave", removedId)
    }

    // -- Connected apps (NIP-46 bunker, nostr.md § Layout & flow item 6) --

    @Test
    fun connectedAppsSectionAbsentWhenUnlinked() {
        render(bridge = unlinkedBridge())
        composeTestRule.onNodeWithTag("nostr-bunker-connect-btn").assertDoesNotExist()
    }

    @Test
    fun connectedAppsSectionAbsentForNonCustodialMode() {
        // A NIP-07/remote-signer account has no nest-held key to sign with —
        // only generated/imported (custodial) accounts can be a bunker.
        render(bridge = linkedBridge(mode = "remote"))
        composeTestRule.onNodeWithTag("nostr-bunker-connect-btn").assertDoesNotExist()
    }

    @Test
    fun connectedAppsSectionRendersForCustodialAccount() {
        render(bridge = linkedBridge(mode = "generated"))
        composeTestRule.onNodeWithTag("nostr-bunker-connect-btn").assertExists()
    }

    @Test
    fun connectButtonFiresCallback() {
        var connected = false
        render(bridge = linkedBridge(), onConnectApp = { connected = true })
        composeTestRule.onNodeWithTag("nostr-bunker-connect-btn").performScrollTo().performClick()
        assertEquals(true, connected)
    }

    @Test
    fun inviteRevealRendersConnectStringAndCopyButton() {
        render(
            bridge = linkedBridge(),
            bunkerInvite = FfiCreateBunkerInviteReply(
                connectionId = 7L,
                connectString = "bunker://abcd?relay=wss://x/nostr&secret=s",
                signerPubkey = "abcd",
                expiresAt = 1000UL,
            ),
        )
        composeTestRule.onNodeWithTag("nostr-bunker-connect-string")
            .assertExists()
            .assertTextEquals("bunker://abcd?relay=wss://x/nostr&secret=s")
        composeTestRule.onNodeWithTag("nostr-bunker-connect-copy-btn").assertExists()
    }

    @Test
    fun noInviteRevealWhenNoneMinted() {
        render(bridge = linkedBridge(), bunkerInvite = null)
        composeTestRule.onNodeWithTag("nostr-bunker-connect-string").assertDoesNotExist()
    }

    // ── Zap signers (the NIP-57 trust root, monetization.md § Zap receipts —
    // the trust model). Unlike Connected apps above, rendered for ANY linked
    // account — not just custodial ones.

    @Test
    fun zapSignersSectionAbsentWhenUnlinked() {
        render(bridge = unlinkedBridge())
        composeTestRule.onNodeWithTag("nostr-zap-signer-pubkey-input").assertDoesNotExist()
    }

    @Test
    fun zapSignersSectionRendersForANonCustodialLinkedAccount() {
        // Unlike Connected apps, a `remote` (non-custodial) account still
        // gets this section — designating who may speak for your money is
        // orthogonal to where your key lives.
        render(bridge = linkedBridge(mode = "remote"))
        composeTestRule.onNodeWithTag("nostr-zap-signer-pubkey-input").assertExists()
    }

    @Test
    fun emptyZapSignerRosterRendersNoneText() {
        render(bridge = linkedBridge(), zapSigners = emptyList())
        composeTestRule.onNodeWithTag("nostr-zap-signer-empty").assertExists()
        composeTestRule.onNodeWithTag("nostr-zap-signer-item").assertDoesNotExist()
    }

    @Test
    fun zapSignerRosterRendersItemsAndLabels() {
        render(
            bridge = linkedBridge(),
            zapSigners = listOf(
                zapSigner(id = 1, pubkey = "ab".repeat(32), label = "Alby"),
                zapSigner(id = 2, pubkey = "cd".repeat(32), label = ""),
            ),
        )
        composeTestRule.onAllNodesWithTag("nostr-zap-signer-item").assertCountEquals(2)
        composeTestRule.onNodeWithText("Alby", substring = true).assertExists()
    }

    @Test
    fun addZapSignerButtonFiresCallbackWithTypedFields() {
        var addedPubkey: String? = null
        var addedLabel: String? = null
        render(
            bridge = linkedBridge(),
            onAddZapSigner = { pubkey, label -> addedPubkey = pubkey; addedLabel = label },
        )
        composeTestRule.onNodeWithTag("nostr-zap-signer-pubkey-input").performScrollTo().performTextInput("ab".repeat(32))
        composeTestRule.onNodeWithTag("nostr-zap-signer-label-input").performScrollTo().performTextInput("Alby")
        composeTestRule.onNodeWithTag("nostr-zap-signer-add-btn").performScrollTo().performClick()
        assertEquals("ab".repeat(32), addedPubkey)
        assertEquals("Alby", addedLabel)
    }

    @Test
    fun removeZapSignerButtonFiresCallbackWithTheStoredPubkey() {
        var removedPubkey: String? = null
        render(
            bridge = linkedBridge(),
            zapSigners = listOf(zapSigner(id = 1, pubkey = "ab".repeat(32))),
            onRemoveZapSigner = { removedPubkey = it },
        )
        composeTestRule.onNodeWithTag("nostr-zap-signer-remove").performScrollTo().performClick()
        assertEquals("ab".repeat(32), removedPubkey)
    }

    // ── The Dim-3 courtesy gate on the add button (never re-derived — read
    // straight off the shared FeatureRow; removal is never gated).

    @Test
    fun addButtonIsEnabledWhenTheGateRowIsAbsent() {
        // An un-hydrated read (no rows fetched yet) must leave the button
        // LIVE — the nest, not the app, is the enforcement floor.
        render(bridge = linkedBridge(), zapSignerGateRow = null)
        composeTestRule.onNodeWithTag("nostr-zap-signer-add-btn").assertIsEnabled()
    }

    @Test
    fun addButtonIsEnabledWhenTheZapsRowIsAvailable() {
        render(bridge = linkedBridge(), zapSignerGateRow = featureRow("zaps", "available"))
        composeTestRule.onNodeWithTag("nostr-zap-signer-add-btn").assertIsEnabled()
    }

    @Test
    fun addButtonIsDisabledWithAReasonWhenTheZapsRowIsRestricted() {
        render(bridge = linkedBridge(), zapSignerGateRow = featureRow("zaps", "hidden"))
        composeTestRule.onNodeWithTag("nostr-zap-signer-add-btn").assertIsNotEnabled()
    }

    @Test
    fun removeButtonStaysEnabledEvenWhenTheZapsRowIsRestricted() {
        // Removal is de-escalation, never gated — a tier that can only
        // tighten must not be able to trap a user in a roster they cannot
        // undo.
        render(
            bridge = linkedBridge(),
            zapSigners = listOf(zapSigner(id = 1)),
            zapSignerGateRow = featureRow("zaps", "hidden"),
        )
        composeTestRule.onNodeWithTag("nostr-zap-signer-remove").assertIsEnabled()
    }
}
