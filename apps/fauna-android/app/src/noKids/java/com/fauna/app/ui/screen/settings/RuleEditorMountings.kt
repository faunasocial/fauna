package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.NestRulesHost
import com.fauna.app.ui.components.RuleEditor
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.FfiFamilyWardInfo
import com.fauna.ffi.FfiRuleEditor
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import social.fauna.generated.Ids
import javax.inject.Inject

/**
 * The shared rule editor's two app mountings on android (`nest-rules.md` § The
 * shared rule editor → *Three mountings, one shape*): the admin's in
 * `admin-nest-rules-section` and the guardian's in `family-policy-rules-section`.
 * Each owns its editor handle, its save and its status line; the editor itself
 * is the one [RuleEditor] paint. tui's `rule_editor.rs` mountings and web's
 * `admin/nest` and `family` pages are the siblings.
 *
 * In `src/noKids/`: `rule-editor` rides `store-safe`'s tail, and a kids account
 * is never an admin or a guardian — the kids twin paints nothing.
 */
@HiltViewModel
class RuleEditorHostVM @Inject constructor(val api: ApiClient) : ViewModel()

/** What `rule-editor-status` says, before it is resolved to a string. */
private sealed interface HostStatus {
    data object None : HostStatus
    data object Saved : HostStatus
    data class SavedVersion(val sequence: ULong) : HostStatus
    /** The nest's or the read's own words (a refusal names its defect). */
    data class Text(val text: String) : HostStatus
}

@Composable
private fun HostStatus.text(): String = when (this) {
    HostStatus.None -> ""
    HostStatus.Saved -> stringResource(R.string.rule_editor_saved)
    is HostStatus.SavedVersion -> stringResourceFmt(R.string.rule_editor_saved_version, sequence.toString())
    is HostStatus.Text -> text
}

/**
 * `admin-nest-rules-section` — the admin's mounting on `admin-nest`, directly
 * after the reports queue (`admin.md` § N. Nest → *Nest rules*): read on entry
 * over `fauna.admin.content_policy.get` beside the self-description's domain,
 * written through `.set`; the status is the reply's version or the nest's named
 * defect.
 */
@Composable
fun AdminNestRulesSection(vm: RuleEditorHostVM = hiltViewModel()) {
    val scope = rememberCoroutineScope()
    var editor by remember { mutableStateOf<FfiRuleEditor?>(null) }
    var status by remember { mutableStateOf<HostStatus>(HostStatus.None) }
    var saving by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) {
        try {
            val policy = vm.api.adminContentPolicy()
            val domain = NestRulesHost.sessionDomain(vm.api.nestRpc())
                ?: throw IllegalStateException("the nest's domain is not known yet")
            editor = FfiRuleEditor.forNest(domain, policy.document)
        } catch (e: Exception) {
            status = HostStatus.Text(e.message ?: "error")
        }
    }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_RULES_SECTION)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.rule_editor_nest_section), style = MaterialTheme.typography.titleMedium)
            val e = editor
            if (e == null) {
                Text(
                    status.text().ifEmpty { stringResource(R.string.rule_editor_loading) },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                RuleEditor(
                    editor = e,
                    writeKind = "fauna.admin.content_policy.set",
                    status = status.text(),
                    saving = saving,
                    onSave = {
                        scope.launch {
                            saving = true
                            status = try {
                                val reply = vm.api.adminSetContentPolicy(e.documentBytes())
                                reply.defect?.let { HostStatus.Text(it) } ?: HostStatus.SavedVersion(reply.sequence)
                            } catch (err: Exception) {
                                HostStatus.Text(err.message ?: "error")
                            } finally {
                                saving = false
                            }
                        }
                    },
                    onChange = { status = HostStatus.None },
                    loadLabelers = { vm.api.ruleLabelers() },
                    loadModule = { vm.api.ruleModule(it) },
                )
            }
        }
    }
}

/**
 * `family-policy-rules-section` — the guardian's mounting in the per-ward policy
 * editor, where the four content-floor selects stood (`family-safety.md` § App
 * surface → *Content rules*): loaded from the selected ward's policy
 * (`FfiRuleEditor.forGuardian`) and saved by its own button through the shared
 * `FfiFamilyClient.saveContentRules`; [onSaved] re-reads the page, so what
 * paints after a save is what the nest stored.
 */
@Composable
fun FamilyRulesSection(
    ward: FfiFamilyWardInfo,
    onSaved: () -> Unit,
    vm: RuleEditorHostVM = hiltViewModel(),
) {
    val scope = rememberCoroutineScope()
    val wardKey = HexUtil.bytesToHex(ward.actorId)
    val document = ward.policy.contentPolicy?.document
    // A fresh editor per ward and per stored document: a re-read that changed
    // nothing keeps the one being edited.
    val docKey = document?.let { HexUtil.bytesToHex(it) }
    val editor = remember(wardKey, docKey) { FfiRuleEditor.forGuardian(ward.policy.contentPolicy) }
    var status by remember(wardKey) { mutableStateOf<HostStatus>(HostStatus.None) }
    var saving by remember(wardKey) { mutableStateOf(false) }
    Column(
        modifier = Modifier.fillMaxWidth().testTag(Ids.FAMILY_POLICY_RULES_SECTION),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(stringResource(R.string.rule_editor_family_section), style = MaterialTheme.typography.titleSmall)
        RuleEditor(
            editor = editor,
            writeKind = "fauna.family.policy.update",
            status = status.text(),
            saving = saving,
            onSave = {
                scope.launch {
                    saving = true
                    status = try {
                        // `null`: the caller no longer guards the ward and nothing
                        // was written — the re-read drops it from the list (web's
                        // twin).
                        val entry = vm.api.familySaveContentRules(ward.actorId, editor.documentBytes())
                        onSaved()
                        if (entry == null) HostStatus.None else HostStatus.Saved
                    } catch (err: Exception) {
                        HostStatus.Text(err.message ?: "error")
                    } finally {
                        saving = false
                    }
                }
            },
            onChange = { status = HostStatus.None },
            loadLabelers = { vm.api.ruleLabelers() },
            loadModule = { vm.api.ruleModule(it) },
        )
    }
}
