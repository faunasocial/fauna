package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.core.RegionPlaneHost
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.FfiRegionSource
import com.fauna.ffi.FfiRegionView
import social.fauna.generated.Ids

/**
 * The Settings region section (`settings-region-*`, `region-blocking.md` § The
 * blocked render and the transparency surface): the declared region and where it
 * came from (with its change path), each policy on the declared chain — the
 * authority, its version and issue time, an inert/malformed notice — when the
 * policies were last checked, and the stale warning. A paint of the shared
 * `FfiRegionPlane.view()`, never an app-side fold; linux's
 * `region::paint_settings`, web's `RegionSettingsSection.svelte`. Always present:
 * before the plane opens (a test, a preview) it reads as no region declared.
 */
@Composable
fun RegionSettingsSection() {
    val revision by RegionPlaneHost.revision.collectAsState()
    val view = remember(revision) { RegionPlaneHost.view() }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SETTINGS_REGION_SECTION)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(stringResource(R.string.region_section_title), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            RegionSettingsRows(view)
        }
    }
}

/** The section's rows for [view] (`null`: the plane is not open). */
@Composable
internal fun RegionSettingsRows(view: FfiRegionView?) {
    val declared = view?.declared
    if (declared == null) {
        Text(
            stringResource(R.string.region_none_declared),
            modifier = Modifier.testTag(Ids.SETTINGS_REGION_DECLARED),
        )
        return
    }
    Text(
        stringResourceFmt(R.string.region_declared, declared.code),
        modifier = Modifier.testTag(Ids.SETTINGS_REGION_DECLARED),
    )
    Text(
        stringResource(sourceText(declared.source)),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag(Ids.SETTINGS_REGION_SOURCE),
    )
    if (view.policies.isEmpty()) {
        Text(stringResource(R.string.region_no_policy))
    }
    for (policy in view.policies) {
        Column(
            modifier = Modifier.padding(vertical = 6.dp).testTag(Ids.SETTINGS_REGION_POLICY_ITEM),
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(
                stringResourceFmt(R.string.region_policy_authority, policy.region, policy.authorityName),
                modifier = Modifier.testTag(Ids.SETTINGS_REGION_POLICY_AUTHORITY),
            )
            Text(
                stringResourceFmt(
                    R.string.region_policy_version,
                    policy.sequence.toString(),
                    ValueFormat.absoluteLocal(policy.issuedAt.toLong()),
                ),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.SETTINGS_REGION_POLICY_VERSION),
            )
            val notice = when (policy.state) {
                "inert" -> stringResourceFmt(R.string.region_inert_notice, policy.inertVersion?.toString() ?: "")
                "malformed" -> stringResource(R.string.region_malformed_notice)
                else -> null
            }
            if (notice != null) {
                Text(
                    notice,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.testTag(Ids.SETTINGS_REGION_INERT_NOTICE),
                )
            }
        }
    }
    view.lastCheckedAt?.let { checked ->
        Text(
            stringResourceFmt(R.string.region_last_checked, ValueFormat.absoluteLocal(checked.toLong())),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.SETTINGS_REGION_LAST_CHECKED),
        )
    }
    if (view.stale) {
        Text(
            stringResource(R.string.region_stale_warning),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.testTag(Ids.SETTINGS_REGION_STALE_WARNING),
        )
    }
}

private fun sourceText(source: FfiRegionSource): Int = when (source) {
    FfiRegionSource.STOREFRONT -> R.string.region_source_storefront
    FfiRegionSource.SYSTEM_REGION -> R.string.region_source_system_region
    FfiRegionSource.SYSTEM_LOCALE -> R.string.region_source_system_locale
    FfiRegionSource.BROWSER_LOCALE -> R.string.region_source_browser_locale
}
