package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.payments.ClaimItem
import com.fauna.app.payments.ProviderItem
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.ProfileTiersVM
import com.fauna.ffi.FfiPendingRequest
import com.fauna.ffi.FfiSubscriberEntry
import com.fauna.ffi.FfiTierItem
import uniffi.fauna_core.LocalizedText
import social.fauna.generated.Ids

/**
 * The profile Tiers-tab **SELF** author-management sections (subscriptions Slice
 * A — `monetization.md` § Pillar 1 + `profile.md` § Layout & flow). Lifts the
 * linux lead (`apps/fauna-linux/src/views/profile/tiers.rs`) onto Compose
 * (priority #1 uniform; same ui.yaml IDs). Three stacked sections render over the
 * shared Rust:
 *
 * - **§1 My tiers** (`subscription-tiers-section`): list + create/edit
 *   (`subscription-tier-form`) + delete.
 * - **§2 Pending requests** (`subscription-requests-section`): approve
 *   (transparent mint+upload, showing `subscription-request-busy`) + reject.
 * - **§3 Subscribers** (`subscription-subscribers-section`): the
 *   `subscription-subscribers-tier-select` tier's roster + remove.
 * - **§4 Payment providers** (`subscription-provider-section` —
 *   `monetization.md` § Pillar 3): configured providers (kind/tier/status —
 *   literal "configured" first cut, no secret ever rendered) + the add form
 *   (kind select from the shared registry, webhook-secret never pre-filled,
 *   entitled tier from the author's own tiers, live webhook-URL preview +
 *   copy button) + per-row remove.
 * - **§5 Manual claim codes** (`subscription-claim-section` —
 *   `monetization.md` § Pillar 3): mint a `"manual"`-provider claim code for
 *   one of the author's tiers, and audit every code (manually- or
 *   webhook-minted) with its `claim_status_label` (redeemed wins over voided).
 *
 * The OTHER-profile browse (`subscription-offers-section` / `profile-follow-button`)
 * is Slice B — not built here.
 *
 * FFI-free [ProfileTiersContent] is split out for the Robolectric harness; the
 * VM-bound [ProfileTiersSection] is what the Profile page mounts.
 */
@Composable
fun ProfileTiersSection(
    /** Bumped by every `profile-tiers-tab` activation — the SELF half of the same
     * ruled door the OTHER branch uses ([ProfileOffersSection]). */
    reloadToken: Int = 0,
    vm: ProfileTiersVM = hiltViewModel(),
) {
    val tiers by vm.tiers.collectAsState()
    val requests by vm.requests.collectAsState()
    val subscribers by vm.subscribers.collectAsState()
    val providers by vm.providers.collectAsState()
    val claims by vm.claims.collectAsState()
    val selectedTier by vm.selectedTier.collectAsState()
    val approving by vm.approving.collectAsState()
    val working by vm.working.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }
    // Re-reads on every tab activation, not only on first composition — the
    // ruled uniform door (`monetization.md` § Pillar 1 → *The Tiers-tab re-read
    // door*); an author re-clicking Tiers is asking whether a new request landed.
    LaunchedEffect(reloadToken) { vm.refreshAll() }

    ProfileTiersContent(
        tiers = tiers,
        requests = requests,
        subscribers = subscribers,
        providers = providers,
        providerKinds = vm.knownProviderKinds(),
        claims = claims,
        selectedTier = selectedTier,
        approving = approving,
        working = working,
        onCreate = vm::createTier,
        onUpdate = vm::updateTier,
        onDelete = vm::deleteTier,
        onApprove = vm::approve,
        onReject = vm::reject,
        onSelectTier = vm::selectTier,
        onRemove = vm::remove,
        onSetProvider = vm::setProvider,
        onRemoveProvider = vm::removeProvider,
        webhookUrl = vm::webhookUrl,
        onMintClaim = vm::mintClaim,
    )
}

@Composable
fun ProfileTiersContent(
    tiers: List<FfiTierItem>,
    requests: List<FfiPendingRequest>,
    subscribers: List<FfiSubscriberEntry>,
    selectedTier: Int,
    approving: Boolean,
    working: Boolean,
    onCreate: (String, UInt, String?, String?, String?, Boolean, ULong?) -> Unit,
    onUpdate: (String, UInt?, String?, String?, String?, Boolean?, ULong?) -> Unit,
    onDelete: (String) -> Unit,
    onApprove: (FfiPendingRequest) -> Unit,
    onReject: (Long) -> Unit,
    onSelectTier: (Int) -> Unit,
    onRemove: (String, ByteArray) -> Unit,
    providers: List<ProviderItem> = emptyList(),
    providerKinds: List<String> = emptyList(),
    claims: List<ClaimItem> = emptyList(),
    onSetProvider: (String, String, String) -> Unit = { _, _, _ -> },
    onRemoveProvider: (String) -> Unit = {},
    onMintClaim: (String) -> Unit = {},
    // Shared `fauna_core::format::hex_full` display encoder (value-formatting.md
    // § Hex id display), injected FFI-free so the Robolectric harness stays off
    // the native path.
    hexFull: (ByteArray) -> String = { com.fauna.ffi.hexFull(it) },
    // §4 live webhook-URL preview — pure local compute (no nest round-trip),
    // injected FFI-free so the Robolectric harness stays off the native path.
    webhookUrl: (String) -> String = { "" },
    // Shared `fauna_core::format::claim_status_label` 3-state decision
    // (redeemed wins over voided), injected FFI-free for the same reason.
    claimStatusLabel: (Boolean, Boolean) -> LocalizedText =
        { redeemed, voided -> com.fauna.ffi.claimStatusLabel(redeemed, voided) },
    // Shared `fauna_core::format::provider_status_label` evidence-based 3-state
    // decision (configured/verified/error; monetization.md § Pillar 3 →
    // "Provider status — evidence-based, no ping"), injected FFI-free for the
    // same reason.
    providerStatusLabel: (ULong?, ULong?) -> LocalizedText =
        { lastVerifiedAt, lastRejectedAt ->
            com.fauna.ffi.providerStatusLabel(lastVerifiedAt, lastRejectedAt)
        },
    // Shared `fauna_core::format::parse_count` tier-rank validation
    // (value-formatting.md § Tier rank — the § Mail-knob validation parser reused,
    // no new fn), injected FFI-free for the same reason as the labels above.
    //
    // This swap is deliberately BEHAVIOUR-PRESERVING, and it is worth saying why
    // rather than leaving the next reader to re-derive it: the two parsers differ
    // only on surrounding whitespace (the shared one trims, `toUIntOrNull` does
    // not), and the rank field's own `onValueChange` strips every non-digit at
    // input time — so no reachable input can tell them apart. Unlike web's and
    // tui's legs of this row, which each closed a live bug, android's is pure
    // uniformity (priority #2/#4): one parser, seven apps. Do NOT "simplify" it
    // back to `toUIntOrNull()`, and do NOT drop the digit filter on the theory
    // that the shared parser now covers it — the filter is what keeps the two
    // equivalent, and removing it would make this a real behaviour change.
    parseRank: (String) -> UInt? = { com.fauna.ffi.parseCount(it) },
) {
    var form by remember { mutableStateOf<FormMode>(FormMode.Closed) }
    var providerFormOpen by remember { mutableStateOf(false) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
    ) {
        // ── §1 My tiers ────────────────────────────────────────────────────
        Column(
            modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIERS_SECTION),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.subscriptions_my_tiers),
                style = MaterialTheme.typography.titleMedium,
            )
            Button(
                onClick = { form = FormMode.Create },
                enabled = !working,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_CREATE_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_create_tier)) }

            when (val mode = form) {
                is FormMode.Create -> TierForm(
                    editing = null,
                    working = working,
                    onSubmit = { rank, desc, price, pay, auto, name, askingPrice ->
                        onCreate(name, rank, desc, price, pay, auto, askingPrice); form = FormMode.Closed
                    },
                    onCancel = { form = FormMode.Closed },
                    parseRank = parseRank,
                )
                is FormMode.Edit -> TierForm(
                    editing = mode.tier,
                    working = working,
                    onSubmit = { rank, desc, price, pay, auto, _, askingPrice ->
                        onUpdate(mode.tier.name, rank, desc, price, pay, auto, askingPrice); form = FormMode.Closed
                    },
                    onCancel = { form = FormMode.Closed },
                    parseRank = parseRank,
                )
                FormMode.Closed -> {}
            }

            // A designated (`unlocks_post`-carrying) tier is an auto-minted
            // "sell this post" tier, not one the author manages — excluded
            // from §1 the same way `offers.rs`'s FOLLOWERS_TIER filter works
            // on linux (monetization.md:128). §3/§4/§5 below still need to
            // pick a designated tier, so the underlying `tiers` list itself
            // is left unfiltered.
            val manageableTiers = tiers.filter { it.unlocksPost == null }
            if (manageableTiers.isEmpty()) {
                Text(
                    stringResource(R.string.subscriptions_no_tiers),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                manageableTiers.forEach { tier ->
                    TierRow(
                        tier = tier,
                        working = working,
                        onEdit = { form = FormMode.Edit(tier) },
                        onDelete = { onDelete(tier.name) },
                    )
                }
            }
        }

        // ── §2 Pending requests ────────────────────────────────────────────
        Column(
            modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_REQUESTS_SECTION),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.subscriptions_pending_requests),
                style = MaterialTheme.typography.titleMedium,
            )
            if (approving) {
                Text(
                    stringResource(R.string.subscriptions_approving),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_REQUEST_BUSY),
                )
            }
            if (requests.isEmpty()) {
                Text(
                    stringResource(R.string.subscriptions_no_requests),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                requests.forEach { req ->
                    RequestRow(
                        request = req,
                        working = working,
                        onApprove = { onApprove(req) },
                        onReject = { onReject(req.requestId) },
                        hexFull = hexFull,
                    )
                }
            }
        }

        // ── §3 Subscribers roster ──────────────────────────────────────────
        Column(
            modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_SUBSCRIBERS_SECTION),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.subscriptions_subscribers),
                style = MaterialTheme.typography.titleMedium,
            )
            TierSelect(
                tierNames = tiers.map { it.name },
                selectedIndex = selectedTier,
                onSelect = onSelectTier,
            )
            if (subscribers.isEmpty()) {
                Text(
                    stringResource(R.string.subscriptions_no_subscribers),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                val tierName = tiers.getOrNull(selectedTier)?.name.orEmpty()
                subscribers.forEach { sub ->
                    SubscriberRow(
                        subscriber = sub,
                        working = working,
                        onRemove = { onRemove(tierName, sub.subscriberId) },
                        hexFull = hexFull,
                    )
                }
            }
        }

        // ── §4 + §5 — the `payments` registry feature's author surface ─────
        //
        // `BuildConfig.PAYMENTS` is android's family compile condition
        // (`dynamic-features.md` § Platform-family surface excision): `false`
        // in the `storeSafe` build type, where R8 folds the constant branch and
        // strips both sections' element ids out of the artifact. The condition
        // is on the RENDER, not merely on the glue — the section that reads
        // [providers]/[claims] would otherwise compile perfectly, paint an
        // empty list, and still ship every `subscription-provider-*` /
        // `subscription-claim-*` id as a string literal (dead is not absent;
        // § What "completely compiled away" means, criterion 1).
        if (BuildConfig.PAYMENTS) {
            // ── §4 Payment providers (monetization.md § Pillar 3) ─────────
            Column(
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_PROVIDER_SECTION),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    stringResource(R.string.subscriptions_payment_providers),
                    style = MaterialTheme.typography.titleMedium,
                )
                Button(
                    onClick = { providerFormOpen = true },
                    enabled = !working,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_PROVIDER_ADD_BUTTON),
                ) { Text(stringResource(R.string.subscriptions_add_provider)) }

                if (providerFormOpen) {
                    ProviderForm(
                        kinds = providerKinds,
                        tierNames = tiers.map { it.name },
                        working = working,
                        onSubmit = { kind, secret, tier ->
                            onSetProvider(kind, secret, tier); providerFormOpen = false
                        },
                        onCancel = { providerFormOpen = false },
                        webhookUrl = webhookUrl,
                    )
                }

                if (providers.isEmpty()) {
                    Text(
                        stringResource(R.string.subscriptions_no_providers),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    providers.forEach { provider ->
                        ProviderRow(
                            provider = provider,
                            working = working,
                            onRemove = { onRemoveProvider(provider.kind) },
                            providerStatusLabel = providerStatusLabel,
                        )
                    }
                }
            }

            // ── §5 Manual claim codes (monetization.md § Pillar 3) ────────
            ClaimSection(
                claims = claims,
                tierNames = tiers.map { it.name },
                working = working,
                onMint = onMintClaim,
                claimStatusLabel = claimStatusLabel,
            )
        }
    }
}

/** Create/edit form mode (`subscription-tier-form`). */
private sealed interface FormMode {
    object Closed : FormMode
    object Create : FormMode
    data class Edit(val tier: FfiTierItem) : FormMode
}

/** One `subscription-tier-row` — name / rank / price + edit & delete. */
@Composable
private fun TierRow(
    tier: FfiTierItem,
    working: Boolean,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_ROW)) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                tier.name,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_TIER_NAME),
            )
            Text(
                tier.rank.toString(),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_RANK),
            )
            Text(
                tier.priceHint.orEmpty(),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_PRICE),
            )
            OutlinedButton(
                onClick = onEdit,
                enabled = !working,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_EDIT_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_edit)) }
            OutlinedButton(
                onClick = onDelete,
                enabled = !working,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_DELETE_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_delete)) }
        }
    }
}

/** One `subscription-request-row` — subscriber / tier / kind (+ paid badge when
 *  payment-verified) + approve & reject. */
@Composable
private fun RequestRow(
    request: FfiPendingRequest,
    working: Boolean,
    onApprove: () -> Unit,
    onReject: () -> Unit,
    hexFull: (ByteArray) -> String,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_REQUEST_ROW)) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                hexFull(request.subscriberId),
                style = MaterialTheme.typography.bodySmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_REQUEST_SUBSCRIBER),
            )
            Text(
                request.tierName,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_REQUEST_TIER),
            )
            Text(
                request.kind,
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_REQUEST_KIND),
            )
            // monetization.md § Pillar 3 — shown only for a payment-verified
            // request (PendingRequest.payment_entitled).
            if (request.paymentEntitled) {
                Text(
                    stringResource(R.string.subscriptions_paid),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_REQUEST_PAID_BADGE),
                )
            }
            Button(
                onClick = onApprove,
                enabled = !working,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_REQUEST_APPROVE_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_approve)) }
            OutlinedButton(
                onClick = onReject,
                enabled = !working,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_REQUEST_REJECT_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_reject)) }
        }
    }
}

/** One `subscription-subscriber-row` — handle (hex actor_id) + remove. */
@Composable
private fun SubscriberRow(
    subscriber: FfiSubscriberEntry,
    working: Boolean,
    onRemove: () -> Unit,
    hexFull: (ByteArray) -> String,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_SUBSCRIBER_ROW)) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                hexFull(subscriber.subscriberId),
                style = MaterialTheme.typography.bodySmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_SUBSCRIBER_HANDLE),
            )
            OutlinedButton(
                onClick = onRemove,
                enabled = !working,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_SUBSCRIBER_REMOVE_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_remove)) }
        }
    }
}

/** The §3 tier picker (`subscription-subscribers-tier-select`). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TierSelect(
    tierNames: List<String>,
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val empty = tierNames.isEmpty()
    val label = tierNames.getOrNull(selectedIndex).orEmpty()

    ExposedDropdownMenuBox(
        expanded = expanded && !empty,
        onExpandedChange = { if (!empty) expanded = !expanded },
    ) {
        OutlinedTextField(
            value = label,
            onValueChange = {},
            readOnly = true,
            enabled = !empty,
            label = { Text(stringResource(R.string.subscriptions_tier_select_label)) },
            trailingIcon = {
                ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded && !empty)
            },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(Ids.SUBSCRIPTION_SUBSCRIBERS_TIER_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded && !empty, onDismissRequest = { expanded = false }) {
            tierNames.forEachIndexed { index, name ->
                DropdownMenuItem(
                    text = { Text(name) },
                    onClick = { onSelect(index); expanded = false },
                )
            }
        }
    }
}

/**
 * One `subscription-provider-row` — kind / entitled tier / status + remove.
 * Status renders the shared evidence-based `provider_status_label`
 * (configured/verified/error; monetization.md § Pillar 3 → "Provider status —
 * evidence-based, no ping"). No secret ever rendered (the nest deliberately
 * omits it from the list reply).
 */
@Composable
private fun ProviderRow(
    provider: ProviderItem,
    working: Boolean,
    onRemove: () -> Unit,
    providerStatusLabel: (ULong?, ULong?) -> LocalizedText,
) {
    val status = resolveLocalized(
        LocalContext.current,
        providerStatusLabel(provider.lastVerifiedAt, provider.lastRejectedAt),
    ).orEmpty()

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_PROVIDER_ROW)) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                provider.kind,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_PROVIDER_KIND),
            )
            Text(
                provider.tier,
                style = MaterialTheme.typography.bodySmall,
            )
            Text(
                status,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_PROVIDER_STATUS),
            )
            // `fauna.payments.providers.remove` is OnlineOnly. The row's kind,
            // tier and status beside it are pure reads and stay rendered.
            val removeGate = faunaGate(
                "fauna.payments.providers.remove",
                enabled = !working,
            )
            Column {
                OutlinedButton(
                    onClick = onRemove,
                    enabled = removeGate.enabled,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_PROVIDER_REMOVE_BUTTON),
                ) { Text(stringResource(R.string.subscriptions_remove)) }
                DisabledControlReasonText(removeGate.reason)
            }
        }
    }
}

/**
 * The §4 provider add form (`subscription-provider-form`): kind select from
 * the shared `fauna-payments` registry, the webhook-verification secret
 * (password transform, never pre-filled — the nest doesn't echo it back), the
 * entitled tier (single-select from the author's own tiers), and a read-only
 * webhook-URL preview (+ copy button) that recomputes as the kind selection
 * changes — the exact URL to register at the provider's dashboard, owned by
 * the same constant the nest builds its ingress route from (never
 * hand-assembled here). No client-side validation — the nest rejects unknown
 * kinds / dangling tiers / empty secrets with typed `fauna.payments.*` errors.
 */
@Composable
private fun ProviderForm(
    kinds: List<String>,
    tierNames: List<String>,
    working: Boolean,
    onSubmit: (String, String, String) -> Unit,
    onCancel: () -> Unit,
    webhookUrl: (String) -> String,
) {
    var kind by remember { mutableStateOf(kinds.firstOrNull().orEmpty()) }
    var secret by remember { mutableStateOf("") }
    var tier by remember(tierNames) {
        mutableStateOf(tierNames.firstOrNull().orEmpty())
    }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_PROVIDER_FORM)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            LabeledSelect(
                label = stringResource(R.string.subscriptions_provider_kind_label),
                options = kinds,
                selected = kind,
                onSelect = { kind = it },
                testTag = Ids.SUBSCRIPTION_PROVIDER_FORM_KIND,
            )
            OutlinedTextField(
                value = secret,
                onValueChange = { secret = it },
                singleLine = true,
                visualTransformation = androidx.compose.ui.text.input.PasswordVisualTransformation(),
                label = { Text(stringResource(R.string.subscriptions_webhook_secret)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_PROVIDER_FORM_SECRET),
            )
            LabeledSelect(
                label = stringResource(R.string.subscriptions_provider_tier_label),
                options = tierNames,
                selected = tier,
                onSelect = { tier = it },
                testTag = Ids.SUBSCRIPTION_PROVIDER_FORM_TIER_MAP,
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedTextField(
                    value = webhookUrl(kind),
                    onValueChange = {},
                    readOnly = true,
                    singleLine = true,
                    label = { Text(stringResource(R.string.subscriptions_webhook_url_label)) },
                    modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL),
                )
                CopyButton(
                    testTag = Ids.SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL_COPY_BUTTON,
                    text = webhookUrl(kind),
                )
            }
            // The commit gates, not the buffer: this form's kind select, secret
            // field, tier map and webhook-url copy all stay live with no nest —
            // only the save issues `fauna.payments.providers.set`. The cancel
            // beside it declares nothing; closing a form is pure local UI.
            val saveGate = faunaGate(
                "fauna.payments.providers.set",
                enabled = !working && tier.isNotEmpty(),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onSubmit(kind, secret, tier) },
                    enabled = saveGate.enabled,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_PROVIDER_FORM_SAVE),
                ) { Text(stringResource(R.string.subscriptions_save)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_PROVIDER_FORM_CANCEL),
                ) { Text(stringResource(R.string.subscriptions_cancel)) }
            }
            DisabledControlReasonText(saveGate.reason)
        }
    }
}

/** A labeled exposed-dropdown single-select (the §4 form's kind + tier pickers). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LabeledSelect(
    label: String,
    options: List<String>,
    selected: String,
    onSelect: (String) -> Unit,
    testTag: String,
) {
    var expanded by remember { mutableStateOf(false) }
    val empty = options.isEmpty()

    ExposedDropdownMenuBox(
        expanded = expanded && !empty,
        onExpandedChange = { if (!empty) expanded = !expanded },
    ) {
        OutlinedTextField(
            value = selected,
            onValueChange = {},
            readOnly = true,
            enabled = !empty,
            label = { Text(label) },
            trailingIcon = {
                ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded && !empty)
            },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(testTag),
        )
        ExposedDropdownMenu(expanded = expanded && !empty, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(option) },
                    onClick = { onSelect(option); expanded = false },
                )
            }
        }
    }
}

/**
 * The inline create/edit tier form (`subscription-tier-form`). The same form
 * serves create + edit; on edit the tier name is the server key (read-only,
 * mirrors linux). [onSubmit] hands back `(rank, description, priceHint,
 * paymentUrl, autoApprove, name, askingPriceSats)` — name is meaningful only
 * on create.
 */
@Composable
private fun TierForm(
    editing: FfiTierItem?,
    working: Boolean,
    onSubmit: (UInt, String?, String?, String?, Boolean, String, ULong?) -> Unit,
    onCancel: () -> Unit,
    parseRank: (String) -> UInt?,
) {
    var name by remember(editing) { mutableStateOf(editing?.name ?: "") }
    var rank by remember(editing) { mutableStateOf(editing?.rank?.toString() ?: "") }
    var description by remember(editing) { mutableStateOf(editing?.description ?: "") }
    var priceHint by remember(editing) { mutableStateOf(editing?.priceHint ?: "") }
    // The reverse of the create/update sats conversion — pre-fill with the
    // tier's current price, or empty for an unpriced tier / a unit this
    // build cannot interpret (fail-closed).
    var askingPrice by remember(editing) { mutableStateOf(editing?.askingPriceSats?.toString() ?: "") }
    var paymentUrl by remember(editing) { mutableStateOf(editing?.paymentUrl ?: "") }
    var autoApprove by remember(editing) { mutableStateOf(editing?.autoApprove ?: false) }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_FORM)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                readOnly = editing != null,
                singleLine = true,
                label = { Text(stringResource(R.string.subscriptions_tier_name)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_FORM_NAME),
            )
            OutlinedTextField(
                value = rank,
                onValueChange = { rank = it.filter(Char::isDigit) },
                singleLine = true,
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                    keyboardType = KeyboardType.Number,
                ),
                label = { Text(stringResource(R.string.subscriptions_rank)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_FORM_RANK),
            )
            OutlinedTextField(
                value = description,
                onValueChange = { description = it },
                singleLine = true,
                label = { Text(stringResource(R.string.subscriptions_description)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_FORM_DESCRIPTION),
            )
            OutlinedTextField(
                value = priceHint,
                onValueChange = { priceHint = it },
                singleLine = true,
                label = { Text(stringResource(R.string.subscriptions_price_hint)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_FORM_PRICE_HINT),
            )
            OutlinedTextField(
                value = askingPrice,
                onValueChange = { askingPrice = it },
                singleLine = true,
                keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                    keyboardType = KeyboardType.Number,
                ),
                label = { Text(stringResource(R.string.subscriptions_asking_price)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_FORM_ASKING_PRICE),
            )
            OutlinedTextField(
                value = paymentUrl,
                onValueChange = { paymentUrl = it },
                singleLine = true,
                label = { Text(stringResource(R.string.subscriptions_payment_url)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_TIER_FORM_PAYMENT_URL),
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(stringResource(R.string.subscriptions_auto_approve), modifier = Modifier.weight(1f))
                Switch(
                    checked = autoApprove,
                    onCheckedChange = { autoApprove = it },
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_FORM_AUTO_APPROVE),
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = {
                        onSubmit(
                            parseRank(rank) ?: 0u,
                            description.ifBlank { null },
                            priceHint.ifBlank { null },
                            paymentUrl.ifBlank { null },
                            autoApprove,
                            name.trim(),
                            // Same shape as the fields above (and their
                            // shared "no clear verb yet" gap): a parsed sats
                            // value, `null` for empty OR unparseable. `null`
                            // on an edit means "keep the current price",
                            // never "clear it".
                            askingPrice.trim().toULongOrNull(),
                        )
                    },
                    enabled = !working && name.isNotBlank(),
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_FORM_SAVE),
                ) { Text(stringResource(R.string.subscriptions_save)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_TIER_FORM_CANCEL),
                ) { Text(stringResource(R.string.subscriptions_cancel)) }
            }
        }
    }
}

/**
 * The §5 manual-claims section (`subscription-claim-section`,
 * `monetization.md` § Pillar 3): mint a `"manual"`-provider claim code for one
 * of the author's tiers, plus the audit list of every claim code — manually-
 * or webhook-minted alike (`subscription-claim-list`). The tier picker resets
 * to the registry's first tier whenever [tierNames] changes, matching §4's
 * `ProviderForm` tier-map picker (no cross-reload selection memory needed —
 * minting is a one-shot action, not an ongoing roster view).
 */
@Composable
private fun ClaimSection(
    claims: List<ClaimItem>,
    tierNames: List<String>,
    working: Boolean,
    onMint: (String) -> Unit,
    claimStatusLabel: (Boolean, Boolean) -> LocalizedText,
) {
    var tier by remember(tierNames) { mutableStateOf(tierNames.firstOrNull().orEmpty()) }

    Column(
        modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_CLAIM_SECTION),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            stringResource(R.string.subscriptions_manual_claims),
            style = MaterialTheme.typography.titleMedium,
        )
        // LabeledSelect's field is `.fillMaxWidth()` internally — stack it above
        // the button rather than sharing a Row (which would starve the button
        // of layout space), matching how §4's LabeledSelect calls each sit on
        // their own line.
        LabeledSelect(
            label = stringResource(R.string.subscriptions_tier_select_label),
            options = tierNames,
            selected = tier,
            onSelect = { tier = it },
            testTag = Ids.SUBSCRIPTION_CLAIM_TIER_SELECT,
        )
        // The commit gates, not the buffer: the tier select above stays live so
        // a tier can be picked with no nest — only the mint issues
        // `fauna.payments.claims.mint`.
        val mintGate = faunaGate(
            "fauna.payments.claims.mint",
            enabled = !working && tier.isNotEmpty(),
        )
        Button(
            onClick = { onMint(tier) },
            enabled = mintGate.enabled,
            modifier = Modifier.testTag(Ids.SUBSCRIPTION_CLAIM_MINT_BUTTON),
        ) { Text(stringResource(R.string.subscriptions_mint_claim)) }
        DisabledControlReasonText(mintGate.reason)
        if (claims.isEmpty()) {
            Text(
                stringResource(R.string.subscriptions_no_claims),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            claims.forEach { claim ->
                ClaimRow(claim = claim, claimStatusLabel = claimStatusLabel)
            }
        }
    }
}

/**
 * One `subscription-claim-row` — bare code/tier/status labels, no captions
 * (matches the §4 provider-row idiom). Status is the shared 3-state decision
 * (`fauna_core::format::claim_status_label` — redeemed wins over voided) —
 * redeemed/voided are hard nest-side facts, unlike §4's evidence-based
 * `provider-status` (`provider_status_label`, no ping).
 */
@Composable
private fun ClaimRow(
    claim: ClaimItem,
    claimStatusLabel: (Boolean, Boolean) -> LocalizedText,
) {
    val status = resolveLocalized(
        LocalContext.current,
        claimStatusLabel(claim.redeemedBy != null, claim.voidedAt != null),
    ).orEmpty()

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_CLAIM_ROW)) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                claim.code,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_CLAIM_CODE),
            )
            Text(
                claim.tier,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_CLAIM_TIER),
            )
            Text(
                status,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_CLAIM_STATUS),
            )
        }
    }
}
