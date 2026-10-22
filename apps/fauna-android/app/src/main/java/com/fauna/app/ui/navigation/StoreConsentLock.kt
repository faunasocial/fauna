package com.fauna.app.ui.navigation

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.ui.viewmodel.StoreConsentLockVM
import social.fauna.generated.Ids

/**
 * The store-consent lock (`account-age-band.md` § The account age band → the
 * store-law developer duties bullet, item (3); § App surface → *Store-consent
 * surfaces*; ui.yaml `global.platform_elements.android` `store-consent-lock` /
 * `store-consent-lock-message`, user-approved 2026-10-19) — the
 * `screen-time-lock` twin, but **whole-app**: it wraps everything the activity
 * renders, onboarding and the launch gates included, so while the shared round
 * says `Locked` no page can be reached, read or touched. The app underneath
 * stays composed (its state survives the lift) but is hidden from semantics,
 * and the lock swallows touch and back.
 *
 * No ask button — Play raises the parent's request itself and the store
 * re-checks on every foreground — and no sign-out: the lock is about the
 * person the store knows on this device, not the Fauna account. The message is
 * the shared round's own lines, rendered in order (the first as the heading),
 * as one text node.
 */
@Composable
fun StoreConsentLockHost(vm: StoreConsentLockVM = hiltViewModel(), content: @Composable () -> Unit) {
    val lines by vm.lockLines.collectAsState()
    val locked = lines
    Box(Modifier.fillMaxSize()) {
        Box(Modifier.fillMaxSize().then(if (locked != null) Modifier.clearAndSetSemantics {} else Modifier)) {
            content()
        }
        if (locked != null) StoreConsentLock(locked)
    }
}

@Composable
private fun StoreConsentLock(lines: List<String>) {
    BackHandler(enabled = true) {}
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) awaitPointerEvent().changes.forEach { it.consume() }
                }
            }
            .testTag(Ids.STORE_CONSENT_LOCK),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.padding(24.dp),
        ) {
            val heading = MaterialTheme.typography.headlineSmall.toSpanStyle()
            Text(
                buildAnnotatedString {
                    lines.forEachIndexed { i, line ->
                        if (i > 0) append("\n\n")
                        if (i == 0) withStyle(heading) { append(line) } else append(line)
                    }
                },
                modifier = Modifier.testTag(Ids.STORE_CONSENT_LOCK_MESSAGE),
                style = MaterialTheme.typography.bodyMedium,
                textAlign = TextAlign.Center,
            )
        }
    }
}
