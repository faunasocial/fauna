package com.fauna.app.ui.screen.feed

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.fauna.app.R
import com.fauna.app.payments.SalePriceSaveResult
import com.fauna.app.payments.SalePriceTier
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.viewmodel.FeedVM
import kotlinx.coroutines.launch
import social.fauna.generated.Ids

/**
 * The sold post's *Change price...* dialog (`feed-post-sale-price-dialog`;
 * `monetization.md` § The asking price -> *Re-pricing a sold post*; reference
 * painter tui's `sale_price_verbs`). The money plane's: no FFI symbol is named
 * here, everything rides [FeedVM]'s `src/payments` / `src/noPayments` glue, and
 * the store-safe twin never reaches this composable.
 *
 * Opens once the author's own tiers have answered, because the two price fields
 * pre-fill from the selling tier and the save compares against exactly that
 * read. A failed read shows `sale_price.error_load` on `error-message` and
 * closes. A refused asking price (the composer's own rule) shows its catalog
 * string on `error-message` and leaves the dialog open with the typed text.
 * An advertised post ([advertised]) also paints `sale_price.advertised_note`
 * (`feed-post-sale-price-advertised-note`) so the author is told what the edit
 * does to the Nostr advertisement before saving.
 */
@Composable
internal fun SalePriceDialog(
    vm: FeedVM,
    tierName: String,
    advertised: Boolean,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val appMessages = LocalAppMessages.current
    val scope = rememberCoroutineScope()
    var tier by remember { mutableStateOf<SalePriceTier?>(null) }
    var hint by remember { mutableStateOf("") }
    var asking by remember { mutableStateOf("") }
    var saving by remember { mutableStateOf(false) }

    LaunchedEffect(tierName) {
        try {
            val loaded = vm.loadSalePriceTier(tierName)
            hint = loaded.hint
            asking = loaded.asking
            tier = loaded
        } catch (e: Exception) {
            appMessages.showError(
                context.getStringFmt(R.string.sale_price_error_load, e.message ?: e.toString()),
            )
            onDismiss()
        }
    }

    val loaded = tier ?: return
    AlertDialog(
        onDismissRequest = onDismiss,
        modifier = Modifier.testTag(Ids.FEED_POST_SALE_PRICE_DIALOG),
        title = { Text(stringResource(R.string.sale_price_title)) },
        text = {
            Column {
                OutlinedTextField(
                    value = hint,
                    onValueChange = { hint = it },
                    placeholder = { Text(stringResource(R.string.feed_post_sell_price_placeholder)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_POST_SALE_PRICE_HINT),
                    singleLine = true,
                )
                OutlinedTextField(
                    value = asking,
                    onValueChange = { asking = it },
                    placeholder = { Text(stringResource(R.string.feed_post_sell_asking_price_placeholder)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_POST_SALE_PRICE_ASKING),
                    singleLine = true,
                )
                if (advertised) {
                    Text(
                        text = stringResource(R.string.sale_price_advertised_note),
                        modifier = Modifier.fillMaxWidth().testTag(Ids.FEED_POST_SALE_PRICE_ADVERTISED_NOTE),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    saving = true
                    scope.launch {
                        try {
                            when (val r = vm.saveSalePrice(loaded, hint, asking)) {
                                is SalePriceSaveResult.Closed -> onDismiss()
                                is SalePriceSaveResult.Refused ->
                                    appMessages.showError(catalogText(context, r.i18nKey))
                            }
                        } catch (e: Exception) {
                            appMessages.showError(
                                context.getStringFmt(R.string.sale_price_error_save, e.message ?: e.toString()),
                            )
                        } finally {
                            saving = false
                        }
                    }
                },
                enabled = !saving,
                modifier = Modifier.testTag(Ids.FEED_POST_SALE_PRICE_SAVE_BUTTON),
            ) { Text(stringResource(R.string.common_save)) }
        },
        dismissButton = {
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.testTag(Ids.FEED_POST_SALE_PRICE_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.common_cancel)) }
        },
    )
}

/** The catalog string a shared refusal names by key (`feed.post.sell_asking_price_invalid`
 *  is the generated resource `feed_post_sell_asking_price_invalid`); the key
 *  itself when this build lacks the resource, so a refusal is never blank. */
private fun catalogText(context: android.content.Context, key: String): String {
    @Suppress("DiscouragedApi")
    val id = context.resources.getIdentifier(key.replace('.', '_'), "string", context.packageName)
    return if (id == 0) key else context.getString(id)
}
