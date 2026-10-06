package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.AdminUsersVM
import com.fauna.ffi.FfiAdminInviteCode
import com.fauna.ffi.FfiAdminInviteRequest
import com.fauna.ffi.FfiAdminUser
import com.fauna.ffi.FfiAdminUserRowControls
import com.fauna.ffi.FfiAgeBandOption
import com.fauna.ffi.FfiRegistrationMode
import uniffi.fauna_core.LocalizedText
import social.fauna.generated.Ids

/**
 * The consolidated `admin-users` hub (admin.md § Users): four sections —
 * Pending requests / Registration / Invite / Users — over the shared
 * `fauna.admin.*` WS-RPC kinds (via [AdminUsersVM] → `FfiAdminClient`).
 * Admission is always choosing a *tier* (the tier is the quota): a per-row
 * [TierDropdown] on each section.
 *
 * Stateless [AdminUsersContent] is split out so it renders under the Compose
 * test harness with seeded state — the VM-bound [AdminUsersScreen] is the thin
 * wrapper the navigation graph mounts.
 *
 * The Users rows carry the cut-off ladder (evict / suspend / cancel-eviction),
 * rendered from the shared `admin_user_row_controls` decision (admin.md § 2 Users
 * → *Cutting a user off*). Still out of scope here: tier-definition editing.
 */
@Composable
fun AdminUsersScreen(
    navController: NavController,
    vm: AdminUsersVM = hiltViewModel()
) {
    val pendingRequests by vm.pendingRequests.collectAsState()
    val inviteCodes by vm.inviteCodes.collectAsState()
    val users by vm.users.collectAsState()
    val allUsers by vm.allUsers.collectAsState()
    val userTotal by vm.userTotal.collectAsState()
    val userOffset by vm.userOffset.collectAsState()
    val tiers by vm.tiers.collectAsState()
    val mintedCode by vm.mintedCode.collectAsState()
    val actionError by vm.actionError.collectAsState()
    val registrationMode by vm.registrationMode.collectAsState()
    val unknownRegistrationMode by vm.unknownRegistrationMode.collectAsState()
    val maxFreeUsers by vm.maxFreeUsers.collectAsState()
    val ageVerificationRequired by vm.ageVerificationRequired.collectAsState()

    LaunchedEffect(Unit) { vm.refresh() }

    // Default audit reasons for the one-click cut-off controls — resolved here
    // (the Composable has a Context) and handed to the VM, which is Context-free.
    // Same shared `admin.users_page.*_default_reason` strings linux passes.
    val evictReason = stringResource(R.string.admin_users_page_evict_default_reason)
    val suspendReason = stringResource(R.string.admin_users_page_suspend_default_reason)
    // Same Context-free-VM idiom for the Admit section's malformed-actor-id
    // hint (tui/linux's `ADMIT_ACTOR_HINT`, surfaced on `admin-users-action-error`).
    val admitActorHint = stringResource(R.string.admin_users_page_admit_actor_hint)

    AdminUsersContent(
        pendingRequests = pendingRequests,
        inviteCodes = inviteCodes,
        users = users,
        allUsers = allUsers,
        userTotal = userTotal,
        userOffset = userOffset,
        tiers = tiers,
        mintedCode = mintedCode,
        actionError = actionError,
        registrationMode = registrationMode,
        unknownRegistrationMode = unknownRegistrationMode,
        maxFreeUsers = maxFreeUsers,
        ageVerificationRequired = ageVerificationRequired,
        onBack = { navController.popBackStack() },
        onSetUserTier = vm::setUserTier,
        onCreateCode = vm::createInviteCode,
        onDeleteCode = vm::deleteInviteCode,
        onApprove = vm::approveRequest,
        onDeny = vm::denyRequest,
        onEvictUser = { vm.evictUser(it, evictReason) },
        onSuspendUser = { vm.suspendUser(it, suspendReason) },
        onCancelEviction = vm::cancelEviction,
        onMakeAdmin = vm::makeAdmin,
        onRemoveAdmin = vm::removeAdmin,
        onNextPage = vm::nextPage,
        onPrevPage = vm::prevPage,
        onSaveRegistration = vm::saveRegistration,
        onAdmitUser = { actor, handle, tier -> vm.admitUser(actor, handle, tier, admitActorHint) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminUsersContent(
    pendingRequests: List<FfiAdminInviteRequest>,
    inviteCodes: List<FfiAdminInviteCode>,
    users: List<FfiAdminUser>,
    // Every account on the nest — the guardian pickers' source (Pending
    // requests / Invite sections), kept separate from the paginated [users]
    // page above (`fauna_client_admin::users_list_all`; admin.md § 2 →
    // *Which accounts a picker offers*).
    allUsers: List<FfiAdminUser>,
    userTotal: Long,
    // 0-based offset of the currently-loaded users page (admin.md § Users —
    // limit/offset on `fauna.admin.users.list`).
    userOffset: Long,
    tiers: List<String>,
    mintedCode: String?,
    actionError: String?,
    // Section 2 — Registration (admin.md § 2 Users). `registrationMode == null`
    // means this client cannot name the nest's posture (a newer nest's
    // mode string); the section then renders read-only off [unknownRegistrationMode]
    // instead of a picker (public-mode.md § Registration Modes — never coerce
    // an unrecognized posture to a guessed variant).
    registrationMode: FfiRegistrationMode?,
    unknownRegistrationMode: String?,
    maxFreeUsers: String,
    // The age require-knob as persisted (`fauna.setup.status
    // .age_verification_required`) — seeds the Registration section's
    // `admin-users-registration-age-verification-toggle` draft.
    ageVerificationRequired: Boolean,
    onBack: () -> Unit,
    onSetUserTier: (FfiAdminUser, String) -> Unit,
    // (tier, uses, guardian, age band) — the band is the select's wire token,
    // `null` for *not set*.
    onCreateCode: (String, Long, ByteArray?, String?) -> Unit,
    onDeleteCode: (String) -> Unit,
    // (request, tier, guardian, age band) — same band contract as [onCreateCode].
    onApprove: (FfiAdminInviteRequest, String, ByteArray?, String?) -> Unit,
    onDeny: (FfiAdminInviteRequest, String?) -> Unit,
    onEvictUser: (FfiAdminUser) -> Unit,
    onSuspendUser: (FfiAdminUser) -> Unit,
    onCancelEviction: (FfiAdminUser) -> Unit,
    onMakeAdmin: (FfiAdminUser) -> Unit,
    onRemoveAdmin: (FfiAdminUser) -> Unit,
    // Users-section pagination (admin-users-pagination / -prev-page /
    // -next-page): step to the next/previous page, or no-op at a boundary —
    // the shared stepper (`fauna_core::format::{next,prev}_page_offset`)
    // decides that, the VM owns the offset, this is just the click.
    onNextPage: () -> Unit,
    onPrevPage: () -> Unit,
    // Save mode + cap TOGETHER — one `fauna.admin.set_registration_mode` call
    // (admin-users-registration-save-button) — plus the age require-knob's
    // draft, which the VM sends only when it changed. Not reachable while the
    // section is read-only (no Save button renders then).
    onSaveRegistration: (FfiRegistrationMode, String, Boolean) -> Unit,
    // Section: Admit — direct admission, one `fauna.admin.users.create` call
    // (actorHex, handle, tier). Validation (64-hex actor id) is a VM concern
    // (mirrors tui/linux); this screen passes through whatever was typed.
    onAdmitUser: (actorHex: String, handle: String, tier: String) -> Unit,
    // Shared `fauna_core::format::hex_full` display encoder (value-formatting.md
    // § Hex id display), injected FFI-free so the Robolectric harness stays off
    // the native path.
    hexFull: (ByteArray) -> String = { com.fauna.ffi.hexFull(it) },
    // Shared `fauna_core::format::mail_serving_status_label` (admin.md § Users;
    // value-formatting.md § Implementation status), injected FFI-free alongside
    // [hexFull] so the read-only serving indicator single-sources its label.
    mailServingStatusLabel: (Boolean) -> LocalizedText = { com.fauna.ffi.mailServingStatusLabel(it) },
    // Shared `fauna_client_admin::admin_user_row_controls` — which of the three
    // Users-row lifecycle controls (evict / suspend / cancel-eviction) a row
    // offers (admin.md § 2 Users → Cutting a user off). Injected FFI-free so the
    // Robolectric harness stays off the native path; do NOT re-derive it from
    // `eviction`/`isAdmin` client-side — the shared decision is unit-tested.
    rowControls: (FfiAdminUser) -> FfiAdminUserRowControls = { com.fauna.ffi.adminUserRowControls(it) },
    // Shared `fauna_client_admin::admin_picker_option` — the option text a
    // guardian picker offers: the handle, falling back to the full actor hex
    // (admin.md § 2 → What identifies a user in an admin picker). Injected
    // FFI-free so the Robolectric harness stays off the native path; do NOT
    // re-derive the handle-or-hex fallback client-side (linux/tui/web all call
    // the same shared owner).
    guardianOption: (FfiAdminUser) -> String = { com.fauna.ffi.adminPickerOption(it) },
    // Shared `fauna_core::format::{total,current}_page` — the
    // `"Page {current} of {pages}"` indicator's arithmetic, one source of
    // truth with the other six apps' pagination (value-formatting.md §
    // Pagination). Injected FFI-free so the Robolectric harness stays off the
    // native path.
    totalPages: (Long, Long) -> Long = { total, pageSize -> com.fauna.ffi.totalPages(total, pageSize) },
    currentPage: (Long, Long) -> Long = { offset, pageSize -> com.fauna.ffi.currentPage(offset, pageSize) },
    // The age-band surfaces' shared Rust (family-safety.md § App surface →
    // *Age-band surfaces*: the vocabulary and its labels are spelled once),
    // injected FFI-free so the Robolectric harness stays off the native path:
    // the two band selects' catalog (`fauna_client_admin::age_band_options`)
    // and its *not set* value, the request row's seed from the applicant's
    // claim (`claimed_age_band_option`), the row's claim text
    // (`age_claim_label`, total — nested args), and the invite-code row's
    // band echo (`age_band_label`).
    ageBandOptions: () -> List<FfiAgeBandOption> = { com.fauna.ffi.ageBandOptions() },
    ageBandNotSet: String = com.fauna.ffi.ageBandNotSetValue(),
    claimedAgeBandOption: (String?) -> String = { com.fauna.ffi.claimedAgeBandOption(it) },
    ageClaimLabel: (String?, String?) -> LocalizedText = { band, provenance -> com.fauna.ffi.ageClaimLabel(band, provenance) },
    ageBandLabel: (String) -> LocalizedText? = { com.fauna.ffi.ageBandLabel(it) },
) {
    val bandSelect = remember(ageBandNotSet) { AgeBandSelectSpec(ageBandOptions, ageBandNotSet) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_users_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_USERS_HEADING)
                    )
                },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK)
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                }
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(horizontal = 16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(8.dp)
        ) {
            RequestsSection(
                pendingRequests, tiers, allUsers, onApprove, onDeny, hexFull, guardianOption,
                bandSelect, claimedAgeBandOption, ageClaimLabel,
            )
            HorizontalDivider()
            RegistrationSection(
                registrationMode, unknownRegistrationMode, maxFreeUsers, ageVerificationRequired, onSaveRegistration,
            )
            HorizontalDivider()
            AdmitSection(tiers, onAdmitUser)
            HorizontalDivider()
            InviteSection(
                inviteCodes, tiers, allUsers, mintedCode, onCreateCode, onDeleteCode, hexFull, guardianOption,
                bandSelect, ageBandLabel,
            )
            HorizontalDivider()
            UsersSection(
                users, userTotal, userOffset, tiers, onSetUserTier, hexFull, mailServingStatusLabel,
                onEvictUser, onSuspendUser, onCancelEviction, onMakeAdmin, onRemoveAdmin,
                rowControls, onNextPage, onPrevPage, totalPages, currentPage,
            )

            actionError?.let {
                Text(
                    it,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .padding(vertical = 8.dp)
                        .testTag(Ids.ADMIN_USERS_ACTION_ERROR)
                )
            }
        }
    }
}

// ── Section 1: Pending requests ──────────────────────────────────────────────

@Composable
private fun RequestsSection(
    requests: List<FfiAdminInviteRequest>,
    tiers: List<String>,
    allUsers: List<FfiAdminUser>,
    onApprove: (FfiAdminInviteRequest, String, ByteArray?, String?) -> Unit,
    onDeny: (FfiAdminInviteRequest, String?) -> Unit,
    hexFull: (ByteArray) -> String,
    guardianOption: (FfiAdminUser) -> String,
    bandSelect: AgeBandSelectSpec,
    claimedAgeBandOption: (String?) -> String,
    ageClaimLabel: (String?, String?) -> LocalizedText,
) {
    Column(modifier = Modifier.testTag(Ids.ADMIN_USERS_REQUESTS_SECTION)) {
        Text(
            stringResource(R.string.admin_users_page_section_requests),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp)
        )
        if (requests.isEmpty()) {
            Text(
                stringResource(R.string.admin_invite_requests_page_empty),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
        } else {
            Column(modifier = Modifier.testTag(Ids.ADMIN_INVITE_REQUESTS_LIST)) {
                requests.forEach { req ->
                    InviteRequestRow(
                        req, tiers, allUsers, onApprove, onDeny, hexFull, guardianOption,
                        bandSelect, claimedAgeBandOption, ageClaimLabel,
                    )
                }
            }
        }
    }
}

@Composable
private fun InviteRequestRow(
    req: FfiAdminInviteRequest,
    tiers: List<String>,
    allUsers: List<FfiAdminUser>,
    onApprove: (FfiAdminInviteRequest, String, ByteArray?, String?) -> Unit,
    onDeny: (FfiAdminInviteRequest, String?) -> Unit,
    hexFull: (ByteArray) -> String,
    guardianOption: (FfiAdminUser) -> String,
    bandSelect: AgeBandSelectSpec,
    claimedAgeBandOption: (String?) -> String,
    ageClaimLabel: (String?, String?) -> LocalizedText,
) {
    var selectedTier by remember(req.id) { mutableStateOf(tiers.firstOrNull() ?: "free") }
    var selectedGuardianActorId by remember(req.id) { mutableStateOf<ByteArray?>(null) }
    // Seeded from the applicant's claim when the request carries a nameable
    // band, else *not set* — the shared `claimed_age_band_option` decides (D5:
    // the claim corroborates, the admitting adult decides).
    var selectedBand by remember(req.id) { mutableStateOf(claimedAgeBandOption(req.ageBand)) }
    var denyReason by remember(req.id) { mutableStateOf("") }
    Card(modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(req.handle, modifier = Modifier.testTag(Ids.INVITE_REQUEST_ROW_HANDLE),
                style = MaterialTheme.typography.titleSmall)
            Text(hexFull(req.actorId),
                modifier = Modifier.testTag(Ids.INVITE_REQUEST_ROW_ACTOR),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(req.message, modifier = Modifier.testTag(Ids.INVITE_REQUEST_ROW_MESSAGE),
                style = MaterialTheme.typography.bodyMedium)
            // The applicant's recorded claim — band + provenance, or "No app
            // age verification" (D6's absence-as-signal); total, so always
            // painted.
            Text(
                localizedNested(ageClaimLabel(req.ageBand, req.ageBandProvenance)) ?: "",
                modifier = Modifier.testTag(Ids.INVITE_REQUEST_ROW_AGE_CLAIM),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            TierDropdown(
                testId = "invite-request-row-tier-select",
                tiers = tiers,
                selected = selectedTier,
                onSelect = { selectedTier = it },
            )
            // Guardian for a supervised admission (family-safety.md § Client
            // surface; default "none"). UX-only filter (non-suspended users) —
            // the nest re-validates existence/suspension/non-supervision on submit.
            GuardianDropdown(
                testId = "invite-request-row-guardian-select",
                users = allUsers,
                selectedActorId = selectedGuardianActorId,
                onSelect = {
                    selectedGuardianActorId = it
                    if (it == null) selectedBand = bandSelect.notSet
                },
                guardianOption = guardianOption,
            )
            AgeBandSelect(
                testId = Ids.INVITE_REQUEST_ROW_AGE_BAND_SELECT,
                spec = bandSelect,
                selected = selectedBand,
                enabled = selectedGuardianActorId != null,
                onSelect = { selectedBand = it },
            )
            OutlinedTextField(
                value = denyReason,
                onValueChange = { denyReason = it },
                label = { Text(stringResource(R.string.admin_invite_requests_page_deny_reason_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.INVITE_REQUEST_ROW_DENY_REASON_FIELD)
            )
            // Both verdicts on this row are commits — a DENY dispatches
            // `fauna.admin.invite_requests.deny` just as an approve dispatches
            // its own kind, the same "a refusal is a commit" shape the family
            // transfer-decline taught (batch 4). The three buffers above (tier,
            // guardian, deny reason) are pure local editing state and stay live,
            // which is what makes the pair discriminating.
            val approveGate = faunaGate("fauna.admin.invite_requests.approve")
            val denyGate = faunaGate("fauna.admin.invite_requests.deny")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = {
                        onApprove(req, selectedTier, selectedGuardianActorId, bandSelect.wire(selectedBand))
                    },
                    enabled = approveGate.enabled,
                    modifier = Modifier.testTag(Ids.INVITE_REQUEST_ROW_APPROVE_BUTTON)
                ) { Text(stringResource(R.string.admin_invite_requests_page_approve)) }
                OutlinedButton(
                    onClick = { onDeny(req, denyReason) },
                    enabled = denyGate.enabled,
                    modifier = Modifier.testTag(Ids.INVITE_REQUEST_ROW_DENY_BUTTON)
                ) { Text(stringResource(R.string.admin_invite_requests_page_deny)) }
            }
            DisabledControlReasonText(approveGate.reason ?: denyGate.reason)
        }
    }
}

// ── Section 2: Registration (the nest's registration posture) ───────────────

/**
 * The nest's registration posture — who, if anyone, may create an account
 * without the admin — plus the orthogonal free-tier ceiling (admin.md § 2
 * Users → *Section 2 — Registration*; public-mode.md § Registration Modes).
 * Saved TOGETHER by one `admin-users-registration-save-button` tap → one
 * `fauna.admin.set_registration_mode { mode, max_free_users }` call. Mirrors
 * web's `+page.svelte` § Section 2: local editing state until Save, then the
 * VM re-reads `fauna.setup.status` so what the admin sees after saving is the
 * *persisted* posture, not their typing (no separate confirmation element).
 *
 * `registrationMode == null` means this client cannot name the posture (a
 * mode string a *newer* nest added
 * that this client predates — real under the bidirectional-compat
 * invariant, version-compatibility.md). The section renders READ-ONLY in that
 * case — no picker, no Save — rather than guessing a variant and risking an
 * overwrite of the nest's real posture with this client's guess.
 */
@Composable
private fun RegistrationSection(
    registrationMode: FfiRegistrationMode?,
    unknownRegistrationMode: String?,
    maxFreeUsers: String,
    ageVerificationRequired: Boolean,
    onSave: (FfiRegistrationMode, String, Boolean) -> Unit,
) {
    Column(modifier = Modifier.testTag(Ids.ADMIN_USERS_REGISTRATION_SECTION)) {
        Text(
            stringResource(R.string.admin_users_page_section_registration),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp)
        )
        if (registrationMode == null) {
            Text(
                stringResourceFmt(
                    R.string.admin_users_page_registration_mode_unknown,
                    unknownRegistrationMode ?: "—",
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            // Local editing state, reset whenever the VM's persisted values
            // change (initial hydrate, or the re-seed after a save) — the
            // InviteSection create-form idiom, not the live-apply UsersSection
            // tier picker: this section has an explicit, deliberate Save.
            var selectedMode by remember(registrationMode) { mutableStateOf(registrationMode) }
            var capInput by remember(maxFreeUsers) { mutableStateOf(maxFreeUsers) }
            var requireAgeVerification by remember(ageVerificationRequired) {
                mutableStateOf(ageVerificationRequired)
            }

            RegistrationModeDropdown(
                testId = "admin-users-registration-mode-select",
                selected = selectedMode,
                onSelect = { selectedMode = it },
            )
            OutlinedTextField(
                value = capInput,
                // Filter to digits-only as typed (mirrors the invite-code
                // max-uses field) so the field is always "blank or digits" —
                // blank round-trips to `null` (no cap) on save, never `0`.
                onValueChange = { capInput = it.filter(Char::isDigit) },
                label = { Text(stringResource(R.string.admin_users_page_max_free_users_label)) },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 8.dp)
                    .testTag(Ids.ADMIN_USERS_MAX_FREE_USERS_INPUT),
            )
            Text(
                stringResource(R.string.admin_users_page_max_free_users_hint),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // The D5+D6 require-knob — "accept only signups carrying app age
            // verification", default off (family-safety.md § App surface →
            // *Age-band surfaces*; admin.md § 2 → Registration). A draft like
            // the two controls above, saved by the same button; its `on`/`off`
            // word rides `stateDescription`, the driver's `state` attribute.
            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    stringResource(R.string.admin_users_page_age_verification_required_label),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = requireAgeVerification,
                    onCheckedChange = { requireAgeVerification = it },
                    modifier = Modifier
                        .testTag(Ids.ADMIN_USERS_REGISTRATION_AGE_VERIFICATION_TOGGLE)
                        .semantics { stateDescription = if (requireAgeVerification) "on" else "off" },
                )
            }
            // Mode + cap (+ the knob) are edited locally and committed TOGETHER
            // by this one Save (one `fauna.admin.set_registration_mode` call,
            // plus `set_age_verification_required` only when the knob changed),
            // so the Save carries the whole section's declaration and the
            // controls above stay live as ordinary buffers.
            val saveGate = faunaGate("fauna.admin.set_registration_mode")
            Button(
                onClick = { onSave(selectedMode, capInput, requireAgeVerification) },
                enabled = saveGate.enabled,
                modifier = Modifier
                    .padding(top = 8.dp)
                    .testTag(Ids.ADMIN_USERS_REGISTRATION_SAVE_BUTTON),
            ) { Text(stringResource(R.string.admin_users_page_registration_save)) }
            DisabledControlReasonText(saveGate.reason)
        }
    }
}

/**
 * The registration-mode picker — a native Compose `ExposedDropdownMenuBox`,
 * like [TierDropdown], but the FIRST admin-users picker where the wire value
 * ("open") differs from its localized display label ("Anyone"): [TierDropdown]
 * and [GuardianDropdown] both show the value verbatim (a tier/handle IS its
 * own label), so neither needed a value/label split. Here the anchor and each
 * `DropdownMenuItem` render the i18n LABEL (matching web's `<option>` text) —
 * the enum [FfiRegistrationMode] is the actual selection state; each option's
 * wire value is resolved to the enum via the shared
 * `com.fauna.ffi.registrationModeFromWire`, never a second local
 * value-to-enum map.
 *
 * KNOWN GAP (flagged, not silently papered over): the shared cross-app e2e
 * `actions/admin.py::_pick_tier` / `registration_mode()` drive/read a picker
 * by its WIRE value ("open"), which on web works because `<select>` decouples
 * DOM `.value` from the displayed `<option>` text. Android's on-device bridge
 * (`androidTest/.../bridge/ElementOps.kt`) has no such split today —
 * `select()` clicks a visible-text match (`By.text(value)`) and `getText()`
 * reads the anchor's visible text — so driving/reading this specific element
 * by wire value will need `ElementOps.kt` extended (e.g. matching the
 * per-option `testTag` below by resource id, and a value-carrying semantics
 * readback distinct from the visible label) before real device e2e can drive
 * it. Each `DropdownMenuItem` below already carries `"$testId-option-$value"`
 * as a forward-compatible hook for that fix. Out of scope here: android e2e
 * is emulator-gated (needs a dedicated emulator host) and unverifiable on the primary dev VM —
 * Robolectric (which queries the semantics tree directly, not visible text)
 * is today's achievable proof, per this session's Verify steps.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RegistrationModeDropdown(
    testId: String,
    selected: FfiRegistrationMode,
    onSelect: (FfiRegistrationMode) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val options = remember { com.fauna.ffi.registrationModeOptions() }
    val selectedOption = options.first { com.fauna.ffi.registrationModeFromWire(it.value) == selected }
    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = !expanded },
    ) {
        OutlinedTextField(
            value = localized(selectedOption.label) ?: selectedOption.value,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.admin_users_page_registration_mode_label)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(testId)
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { opt ->
                val mode = com.fauna.ffi.registrationModeFromWire(opt.value) ?: return@forEach
                DropdownMenuItem(
                    text = { Text(localized(opt.label) ?: opt.value) },
                    onClick = {
                        expanded = false
                        onSelect(mode)
                    },
                    modifier = Modifier.testTag("$testId-option-${mode.name.lowercase()}"),
                )
            }
        }
    }
}

// ── Section: Admit (direct admission) ────────────────────────────────────────

/**
 * The Admit section — direct admission, the third account-creation path
 * (`public-mode.md` § Registration & Identity; user-approved IDs 2026-08-15;
 * tui leads, `admin/users.rs::admit_section` is the reference; linux/web
 * follow the same shape). The admin types a known actor id, names the handle
 * the actor is admitted under (there is no set-later — `clear_handle` can
 * only strip one; a blank handle admits the deliberate handle-less state,
 * which cannot send deployment-domain mail), picks a tier ("admission is
 * always choosing a tier"), and admits — one `fauna.admin.users.create`
 * call. The form is never cleared here, on success or failure (tui/linux's
 * own idiom) — the new row in the Users-section refetch below is the
 * feedback; errors (including a malformed actor id, validated VM-side) land
 * on `admin-users-action-error`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AdmitSection(
    tiers: List<String>,
    onAdmit: (actorHex: String, handle: String, tier: String) -> Unit,
) {
    var actorInput by remember { mutableStateOf("") }
    var handleInput by remember { mutableStateOf("") }
    var selectedTier by remember { mutableStateOf(tiers.firstOrNull() ?: "free") }

    Column(modifier = Modifier.testTag(Ids.ADMIN_USERS_ADMIT_SECTION)) {
        Text(
            stringResource(R.string.admin_users_page_section_admit),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp)
        )
        OutlinedTextField(
            value = actorInput,
            onValueChange = { actorInput = it },
            label = { Text(stringResource(R.string.admin_users_page_admit_actor_label)) },
            singleLine = true,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.ADMIN_USERS_ADMIT_ACTOR_INPUT),
        )
        OutlinedTextField(
            value = handleInput,
            onValueChange = { handleInput = it },
            label = { Text(stringResource(R.string.admin_users_page_admit_handle_label)) },
            singleLine = true,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 8.dp)
                .testTag(Ids.ADMIN_USERS_ADMIT_HANDLE_INPUT),
        )
        TierDropdown(
            testId = Ids.ADMIN_USERS_ADMIT_TIER_SELECT,
            tiers = tiers,
            selected = selectedTier,
            onSelect = { selectedTier = it },
            modifier = Modifier.padding(top = 8.dp),
        )
        val admitGate = faunaGate("fauna.admin.users.create")
        Button(
            onClick = { onAdmit(actorInput, handleInput, selectedTier) },
            enabled = admitGate.enabled,
            modifier = Modifier
                .padding(top = 8.dp)
                .testTag(Ids.ADMIN_USERS_ADMIT_BUTTON),
        ) { Text(stringResource(R.string.admin_users_page_admit_button)) }
        DisabledControlReasonText(admitGate.reason)
    }
}

// ── Section 3: Invite (mint a code) ──────────────────────────────────────────

@Composable
private fun InviteSection(
    inviteCodes: List<FfiAdminInviteCode>,
    tiers: List<String>,
    allUsers: List<FfiAdminUser>,
    mintedCode: String?,
    onCreateCode: (String, Long, ByteArray?, String?) -> Unit,
    onDeleteCode: (String) -> Unit,
    hexFull: (ByteArray) -> String,
    guardianOption: (FfiAdminUser) -> String,
    bandSelect: AgeBandSelectSpec,
    ageBandLabel: (String) -> LocalizedText?,
) {
    var formVisible by remember { mutableStateOf(false) }
    var selectedTier by remember { mutableStateOf(tiers.firstOrNull() ?: "free") }
    var maxUses by remember { mutableStateOf("1") }
    var selectedGuardianActorId by remember { mutableStateOf<ByteArray?>(null) }
    var selectedBand by remember { mutableStateOf(bandSelect.notSet) }

    Column(modifier = Modifier.testTag(Ids.ADMIN_USERS_INVITE_SECTION)) {
        Text(
            stringResource(R.string.admin_users_page_section_invite),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp)
        )
        if (inviteCodes.isEmpty()) {
            Text(
                stringResource(R.string.admin_settings_page_no_codes),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
        } else {
            inviteCodes.forEach { code -> InviteCodeRow(code, onDeleteCode, ageBandLabel) }
        }

        if (formVisible) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp).testTag(Ids.ADMIN_SETTINGS_INVITE_CREATE_FORM),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically
            ) {
                TierDropdown(
                    testId = "admin-settings-tier-select",
                    tiers = tiers,
                    selected = selectedTier,
                    onSelect = { selectedTier = it },
                    modifier = Modifier.weight(1f),
                )
                OutlinedTextField(
                    value = maxUses,
                    onValueChange = { maxUses = it.filter(Char::isDigit) },
                    label = { Text(stringResource(R.string.admin_settings_page_max_uses)) },
                    singleLine = true,
                    modifier = Modifier.width(96.dp).testTag(Ids.ADMIN_SETTINGS_MAX_USES_INPUT)
                )
            }
            // Guardian for a supervised admission (family-safety.md § Client
            // surface; default "none"). UX-only filter (non-suspended users) —
            // the nest re-validates existence/suspension/non-supervision on submit.
            GuardianDropdown(
                testId = "admin-users-invite-guardian-select",
                users = allUsers,
                selectedActorId = selectedGuardianActorId,
                onSelect = {
                    selectedGuardianActorId = it
                    if (it == null) selectedBand = bandSelect.notSet
                },
                modifier = Modifier.padding(top = 8.dp),
                guardianOption = guardianOption,
            )
            AgeBandSelect(
                testId = Ids.ADMIN_USERS_INVITE_AGE_BAND_SELECT,
                spec = bandSelect,
                selected = selectedBand,
                enabled = selectedGuardianActorId != null,
                onSelect = { selectedBand = it },
                modifier = Modifier.padding(top = 8.dp),
            )
        }

        mintedCode?.let { code ->
            Text(
                stringResourceFmt(R.string.admin_users_page_minted_code, code),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(top = 8.dp)
            )
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp)
        ) {
            mintedCode?.let { code ->
                CopyButton(
                    testTag = Ids.ADMIN_USERS_INVITE_CODE_COPY_BTN,
                    text = code,
                    label = stringResource(R.string.admin_users_page_copy_code),
                )
            }
            if (!formVisible) {
                Button(
                    onClick = { formVisible = true },
                    modifier = Modifier.testTag(Ids.CREATE_INVITE_CODE_BTN)
                ) { Text(stringResource(R.string.admin_settings_page_create_code)) }
            } else {
                OutlinedButton(
                    onClick = { formVisible = false },
                    modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_INVITE_CANCEL_BUTTON)
                ) { Text(stringResource(R.string.admin_settings_page_factory_reset_cancel)) }
                // Only the CONFIRM mints — `create-invite-code-btn` reveals the
                // form and `admin-settings-invite-cancel-button` hides it again,
                // both pure-local (the arming/disarming split tui states in its
                // admin kind map), so neither declares and both stay live with
                // no nest.
                val mintGate = faunaGate("fauna.admin.invite_codes.create")
                Button(
                    onClick = {
                        onCreateCode(
                            selectedTier, maxUses.toLongOrNull() ?: 1L, selectedGuardianActorId,
                            bandSelect.wire(selectedBand),
                        )
                        formVisible = false
                    },
                    enabled = mintGate.enabled,
                    modifier = Modifier.testTag(Ids.CREATE_INVITE_CONFIRM_BTN)
                ) { Text(stringResource(R.string.admin_settings_page_create_code)) }
                DisabledControlReasonText(mintGate.reason)
            }
        }
    }
}

@Composable
private fun InviteCodeRow(
    code: FfiAdminInviteCode,
    onDeleteCode: (String) -> Unit,
    ageBandLabel: (String) -> LocalizedText?,
) {
    // The minted band echoes on the same row — richer text, no new id
    // (family-safety.md § App surface → *Age-band surfaces*); a band this
    // client cannot name adds nothing.
    val usesLeft = stringResourceFmt(R.string.admin_settings_page_uses_left_n, code.usesLeft.toString())
    val band = code.ageBand?.let { localized(ageBandLabel(it)) }
    val supporting = listOfNotNull(usesLeft, band).joinToString(" · ")
    ListItem(
        headlineContent = {
            Text(code.code, modifier = Modifier.testTag(Ids.INVITE_CODE_VALUE))
        },
        supportingContent = { Text(supporting) },
        trailingContent = {
            val deleteGate = faunaGate("fauna.admin.invite_codes.delete")
            IconButton(
                onClick = { onDeleteCode(code.code) },
                enabled = deleteGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_INVITE_DELETE_BUTTON)
            ) { Icon(Icons.Default.Delete, stringResource(R.string.common_delete)) }
        },
        // The row's text rides the id'd element's OWN semantics (the
        // `snapshot-prune-preview` idiom): a tagged ListItem has no text of its
        // own, and merging descendants would fold `invite-code-value` and the
        // delete button's tags into this node.
        modifier = Modifier
            .testTag(Ids.INVITE_CODE_ITEM)
            .semantics { text = AnnotatedString("${code.tier} · ${code.code} · $supporting") }
    )
}

// ── Section 4: Users ─────────────────────────────────────────────────────────

@Composable
private fun UsersSection(
    users: List<FfiAdminUser>,
    userTotal: Long,
    userOffset: Long,
    tiers: List<String>,
    onSetUserTier: (FfiAdminUser, String) -> Unit,
    hexFull: (ByteArray) -> String,
    mailServingStatusLabel: (Boolean) -> LocalizedText,
    onEvictUser: (FfiAdminUser) -> Unit,
    onSuspendUser: (FfiAdminUser) -> Unit,
    onCancelEviction: (FfiAdminUser) -> Unit,
    onMakeAdmin: (FfiAdminUser) -> Unit,
    onRemoveAdmin: (FfiAdminUser) -> Unit,
    rowControls: (FfiAdminUser) -> FfiAdminUserRowControls,
    onNextPage: () -> Unit,
    onPrevPage: () -> Unit,
    totalPages: (Long, Long) -> Long,
    currentPage: (Long, Long) -> Long,
) {
    Column(modifier = Modifier.testTag(Ids.ADMIN_USERS_LIST_SECTION)) {
        Text(
            stringResource(R.string.admin_users_page_section_users),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp)
        )
        Text(
            stringResourceFmt(R.string.admin_users_page_total, userTotal.toString()),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.USER_COUNT_TEXT)
        )
        users.forEach { user ->
            UserRow(
                user, tiers, onSetUserTier, hexFull, mailServingStatusLabel,
                onEvictUser, onSuspendUser, onCancelEviction, onMakeAdmin, onRemoveAdmin,
                rowControls,
            )
        }
        // Pagination (admin.md § Users — limit/offset on `fauna.admin.users.list`).
        // Buttons stay enabled at a boundary too (the shared stepper no-ops the
        // click, mirrors web/apple); the `enabled` flags below are a visual nicety
        // (mirrors linux's `set_sensitive`), not a correctness dependency.
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 8.dp)
                .testTag(Ids.ADMIN_USERS_PAGINATION),
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedButton(
                onClick = onPrevPage,
                enabled = userOffset > 0,
                modifier = Modifier.testTag(Ids.ADMIN_USERS_PREV_PAGE),
            ) { Text(stringResource(R.string.admin_users_page_prev_page)) }
            Text(
                stringResourceFmt(
                    R.string.admin_users_page_page_indicator,
                    currentPage(userOffset, AdminUsersVM.PAGE_SIZE).toString(),
                    totalPages(userTotal, AdminUsersVM.PAGE_SIZE).toString(),
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 12.dp),
            )
            OutlinedButton(
                onClick = onNextPage,
                enabled = userOffset + AdminUsersVM.PAGE_SIZE < userTotal,
                modifier = Modifier.testTag(Ids.ADMIN_USERS_NEXT_PAGE),
            ) { Text(stringResource(R.string.admin_users_page_next_page)) }
        }
    }
}

@Composable
private fun UserRow(
    user: FfiAdminUser,
    tiers: List<String>,
    onSetUserTier: (FfiAdminUser, String) -> Unit,
    hexFull: (ByteArray) -> String,
    mailServingStatusLabel: (Boolean) -> LocalizedText,
    onEvictUser: (FfiAdminUser) -> Unit,
    onSuspendUser: (FfiAdminUser) -> Unit,
    onCancelEviction: (FfiAdminUser) -> Unit,
    onMakeAdmin: (FfiAdminUser) -> Unit,
    onRemoveAdmin: (FfiAdminUser) -> Unit,
    rowControls: (FfiAdminUser) -> FfiAdminUserRowControls,
) {
    val controls = rowControls(user)
    Column(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp).testTag(Ids.USER_ROW),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp)
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    user.label.ifBlank { stringResource(R.string.admin_users_page_no_handle) },
                    style = MaterialTheme.typography.bodyMedium
                )
                Text(
                    hexFull(user.actorId),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.USER_ACTOR_ID)
                )
            }
            // Read-only IMAP/CalDAV-serving audit indicator (admin.md § Users;
            // deployment-home-with-public-relay.md § MUA reach). The admin only
            // *sees* this — the user sets it from their own mail-settings serve-here
            // toggle, so there is no control here (admin.md § Don't do these: no
            // per-user controls). The enabled→label decision is single-sourced in
            // shared `fauna_core::format::mail_serving_status_label` (mirrors linux
            // `build_user_row`).
            Text(
                resolveLocalized(LocalContext.current, mailServingStatusLabel(user.mailServingEnabled)).orEmpty(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.ADMIN_USERS_MAIL_SERVING_STATUS)
            )
            // Dispatch-on-pick: picking a tier IS the `fauna.admin.users.update`
            // commit (there is no Save beside it), so the select itself carries
            // the declaration — the same shape `admin-web-apex-actor-select`
            // has. The two sibling pickers in the invite sections above are
            // buffers and stay live; this one is not.
            TierDropdown(
                testId = "admin-users-tier-select",
                tiers = tiers,
                selected = user.tier,
                onSelect = { onSetUserTier(user, it) },
                enabled = faunaGate("fauna.admin.users.update").enabled,
            )
        }

        // Lifecycle controls (admin.md § 2 Users → *Cutting a user off*). Which of
        // the three a row offers is the SHARED `admin_user_row_controls` decision
        // (three eviction states crossed with the admin-role guard — easy to get
        // wrong, so all seven apps render from the one unit-tested rule and do NOT
        // re-read `eviction`/`isAdmin` here). One click acts immediately, no confirm
        // dialog: both cut-off paths are reversible by the Cancel-eviction control
        // beside them. Descendants of `user-row`, so the e2e queries them scoped.
        if (controls.evict || controls.`suspend` || controls.restore) {
            // Each of the three is its own wire kind, so each declares its own —
            // NOT one gate for the ladder. `cancel_eviction` in particular is
            // the reversal of the other two and could look like a local undo;
            // it is a nest call like the rest.
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (controls.evict) {
                    TextButton(
                        onClick = { onEvictUser(user) },
                        enabled = faunaGate("fauna.admin.users.evict").enabled,
                        colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_USERS_EVICT_BUTTON),
                    ) { Text(stringResource(R.string.admin_users_page_evict)) }
                }
                if (controls.`suspend`) {
                    TextButton(
                        onClick = { onSuspendUser(user) },
                        enabled = faunaGate("fauna.admin.users.suspend").enabled,
                        colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_USERS_SUSPEND_BUTTON),
                    ) { Text(stringResource(R.string.admin_users_page_suspend)) }
                }
                if (controls.restore) {
                    TextButton(
                        onClick = { onCancelEviction(user) },
                        enabled = faunaGate("fauna.admin.users.cancel_eviction").enabled,
                        modifier = Modifier.testTag(Ids.ADMIN_USERS_CANCEL_EVICTION_BUTTON),
                    ) { Text(stringResource(R.string.admin_users_page_cancel_eviction)) }
                }
            }
        }
        // Roster controls (admin.md § Admin continuity and succession, instrument
        // 1) — same shared `controls` decision as the cut-off ladder above, own
        // row so a plain row's Make Admin never sits inside the (empty) cut-off
        // `Row` above and get skipped by its `if` guard.
        if (controls.makeAdmin || controls.removeAdmin) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (controls.makeAdmin) {
                    TextButton(
                        onClick = { onMakeAdmin(user) },
                        enabled = faunaGate("fauna.admin.admins.add").enabled,
                        modifier = Modifier.testTag(Ids.ADMIN_USERS_MAKE_ADMIN_BUTTON),
                    ) { Text(stringResource(R.string.admin_users_page_make_admin)) }
                }
                if (controls.removeAdmin) {
                    TextButton(
                        onClick = { onRemoveAdmin(user) },
                        enabled = faunaGate("fauna.admin.admins.remove").enabled,
                        colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_USERS_REMOVE_ADMIN_BUTTON),
                    ) { Text(stringResource(R.string.admin_users_page_remove_admin)) }
                }
            }
        }
    }
}

// ── Shared tier picker ───────────────────────────────────────────────────────

/**
 * The cross-section tier picker — a native Compose `ExposedDropdownMenuBox`
 * (ui.yaml types `*-tier-select` as `select`, 2026-05-31). The anchor's display
 * value is the selected tier *name* so the e2e `actions/admin.py` `_pick_tier`
 * (which reads `get_text(test_id)` and drives `driver.select`) sees the readable
 * value. The bridge actuates it via `/element/select` (opens the box, clicks the
 * matching item).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TierDropdown(
    testId: String,
    tiers: List<String>,
    selected: String,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
    // Defaults to live, because most uses of this picker are BUFFERS (the two
    // invite forms) and only the users-row one dispatches on pick. The caller
    // that dispatches hands its gate verdict in; desensitizing the anchor closes
    // the whole control, menu included, because the menu only opens from it —
    // the same reasoning `admin-web-apex-actor-select` records.
    enabled: Boolean = true,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded && enabled,
        onExpandedChange = { if (enabled) expanded = !expanded },
        modifier = modifier
    ) {
        OutlinedTextField(
            value = selected,
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            label = { Text(stringResource(R.string.admin_settings_page_tiers)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .widthIn(min = 140.dp)
                .testTag(testId)
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            tiers.forEach { tier ->
                DropdownMenuItem(
                    text = { Text(tier) },
                    onClick = {
                        expanded = false
                        onSelect(tier)
                    }
                )
            }
        }
    }
}

/**
 * The guardian picker for a supervised admission (family-safety.md § Client
 * surface — `admin-users-invite-guardian-select` / `invite-request-row-
 * guardian-select`, default "none"). Same `ExposedDropdownMenuBox` shape as
 * [TierDropdown]; lists every non-suspended user by handle (falling back to
 * the full actor hex, via [guardianOption] — `fauna_client_admin::admin_picker_option`,
 * the one shared owner tui/linux/web also call) plus a leading "None" option.
 * Binding is already injective on actor id ([onSelect] carries the raw id,
 * never the display text) — the handle-else-hex text additionally makes the
 * DISPLAYED option itself unique, matching every other app's picker. UX-only
 * filtering — the nest re-validates existence/suspension/non-supervision on
 * submit.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun GuardianDropdown(
    testId: String,
    users: List<FfiAdminUser>,
    selectedActorId: ByteArray?,
    onSelect: (ByteArray?) -> Unit,
    modifier: Modifier = Modifier,
    guardianOption: (FfiAdminUser) -> String = { com.fauna.ffi.adminPickerOption(it) },
) {
    val noneLabel = stringResource(R.string.admin_users_page_guardian_none)
    val options = users.filter { !it.suspended }
    val selectedLabel = options.find { selectedActorId?.contentEquals(it.actorId) == true }
        ?.let { guardianOption(it) } ?: noneLabel

    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = !expanded },
        modifier = modifier,
    ) {
        OutlinedTextField(
            value = selectedLabel,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.admin_users_page_guardian_label)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(testId)
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text(noneLabel) },
                onClick = { expanded = false; onSelect(null) },
            )
            options.forEach { user ->
                DropdownMenuItem(
                    text = { Text(guardianOption(user)) },
                    onClick = { expanded = false; onSelect(user.actorId) },
                )
            }
        }
    }
}

/**
 * The two admission age-band selects' shared catalog: the options
 * (`fauna_client_admin::age_band_options` — *not set* + the four bands, in the
 * ratified order) and the *not set* value, which [wire] maps to "no band" on
 * the admission call. Built once per screen so the Invite form and every
 * request row draw the same list.
 */
private class AgeBandSelectSpec(options: () -> List<FfiAgeBandOption>, val notSet: String) {
    val options: List<FfiAgeBandOption> = options()

    /** The admission carry's `age_band` for a selected value — `null` for *not set*. */
    fun wire(selected: String): String? = selected.takeIf { it != notSet }
}

/**
 * `admin-users-invite-age-band-select` / `invite-request-row-age-band-select`
 * (family-safety.md § App surface → *Age-band surfaces*): a [TokenSelect] by
 * VALUE — the driver picks and reads the wire token (`u13`, `not-set`, …),
 * the human sees the shared label. [enabled] only while a guardian is selected
 * (the nest refuses a band without one; the client gate is UX), and the
 * caller resets [selected] to *not set* when the guardian is cleared.
 */
@Composable
private fun AgeBandSelect(
    testId: String,
    spec: AgeBandSelectSpec,
    selected: String,
    enabled: Boolean,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    TokenSelect(
        testTagValue = testId,
        selected = selected,
        options = spec.options.map { it.value to (localized(it.label) ?: it.value) },
        onSelect = onSelect,
        enabled = enabled,
        modifier = modifier,
    )
}
