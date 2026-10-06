package com.fauna.app.ui.components

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.ffi.FfiException
import com.fauna.ffi.identityQrEncode
import com.fauna.ffi.qrMatrix
import com.fauna.ffi.qrQuietZoneModules
// The QrMatrix *record* is a fauna-core type, so UniFFI emits it into that crate's own
// namespace — only the fns live in `com.fauna.ffi`.
import uniffi.fauna_core.QrMatrix
import social.fauna.generated.Ids

/**
 * Identity export — the Settings/Account section that reveals the identity QR a second
 * device scans to import (`docs/goal/ui/settings.md` § Identity export). The counterpart of
 * onboarding's `identity_import` step.
 *
 * Both halves are shared Rust: [identityQrEncode] builds exactly the URI the import parser
 * accepts, and [qrMatrix] turns it into a boolean module grid. The only Android-specific
 * part is painting that grid on a Compose [Canvas] — no client links a platform QR library
 * (priorities #1/#2).
 *
 * The description and the toggle are always visible; the warning and the QR appear **only
 * after the user presses show**. Hiding is not a security control — it exists so the secret
 * is never on screen by accident when a user opens Settings (e.g. while sharing a screen).
 * Nothing is persisted and no server call is made; the toggle is pure view state.
 *
 * @param secretHex the 64-hex identity secret from the platform secure store, or `null`.
 * @param handle the cached handle, ridden into the payload so a scanned import pre-fills the
 *   handle step. A handle-less client writes the bare secret form.
 */
@Composable
fun IdentityExportSection(secretHex: String?, handle: String?) {
    // `null` = collapsed. Holding the matrix (not a boolean) means hiding drops the encoded
    // secret rather than retaining it off-screen.
    var matrix by remember { mutableStateOf<QrMatrix?>(null) }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.IDENTITY_EXPORT_SECTION)) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                stringResource(R.string.settings_identity_export_title),
                style = MaterialTheme.typography.titleMedium
            )
            Spacer(Modifier.height(8.dp))

            Text(
                stringResource(R.string.settings_identity_export_desc),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.IDENTITY_EXPORT_DESCRIPTION)
            )

            matrix?.let { m ->
                Spacer(Modifier.height(8.dp))
                // The QR carries the full Ed25519 identity secret — whoever scans it gains
                // the identity — so the warning renders adjacent to the code, never below
                // the fold (settings.md § Identity export, "Risk").
                Text(
                    stringResource(R.string.settings_identity_export_warning),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.testTag(Ids.IDENTITY_EXPORT_WARNING)
                )
                Spacer(Modifier.height(8.dp))
                QrCanvas(
                    matrix = m,
                    modifier = Modifier.size(220.dp).testTag(Ids.IDENTITY_EXPORT_QR)
                )
            }

            Spacer(Modifier.height(8.dp))
            Button(
                onClick = {
                    matrix = if (matrix != null) {
                        null
                    } else {
                        encodeIdentityQr(secretHex, handle)
                    }
                },
                enabled = secretHex != null,
                modifier = Modifier.testTag(Ids.IDENTITY_EXPORT_SHOW_QR_BUTTON)
            ) {
                Text(
                    stringResource(
                        if (matrix != null) {
                            R.string.settings_identity_export_hide_qr
                        } else {
                            R.string.settings_identity_export_show_qr
                        }
                    )
                )
            }
        }
    }
}

/**
 * Encode the `(identity, handle)` URI as a QR matrix, or `null` when there is no secret /
 * the payload is unencodable. A failure leaves the section collapsed rather than crashing
 * the settings screen.
 */
private fun encodeIdentityQr(secretHex: String?, handle: String?): QrMatrix? {
    if (secretHex == null) return null
    return try {
        qrMatrix(identityQrEncode(secretHex, handle))
    } catch (e: FfiException) {
        null
    }
}

/**
 * Paint [matrix] dark-on-light with the mandatory quiet zone.
 *
 * Deliberately **not** theme-aware: a QR must stay dark-on-light to scan, so the light
 * background is painted explicitly rather than inherited from a (possibly dark) theme.
 *
 * `internal` (not `private`) so other Nostr-family QR renders — the NIP-46
 * bunker connect-string reveal (`NostrScreen.kt`) — reuse it rather than
 * re-implementing the module grid painter (priority #2).
 */
@Composable
internal fun QrCanvas(matrix: QrMatrix, modifier: Modifier = Modifier) {
    // The quiet zone is not part of the matrix — the renderer pads itself, or scanners
    // refuse the code. Never a hard-coded 4; that is what the shared export is for.
    val quiet = remember { qrQuietZoneModules().toInt() }
    val size = matrix.size.toInt()

    Canvas(modifier = modifier) {
        val modulesPerSide = size + 2 * quiet
        val scale = minOf(this.size.width, this.size.height) / modulesPerSide

        // Light background across the whole box — this IS the quiet zone at the edges.
        drawRect(color = Color.White, topLeft = Offset.Zero, size = this.size)

        for (y in 0 until size) {
            for (x in 0 until size) {
                if (!matrix.modules[y * size + x]) continue
                drawRect(
                    color = Color.Black,
                    topLeft = Offset((x + quiet) * scale, (y + quiet) * scale),
                    size = Size(scale, scale)
                )
            }
        }
    }
}
