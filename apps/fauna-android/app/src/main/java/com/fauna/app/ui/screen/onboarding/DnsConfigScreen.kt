package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.UrlOpener
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.util.localized
import com.fauna.ffi.onboarding.CredentialForm
import com.fauna.ffi.onboarding.FieldMetaPlain
import com.fauna.ffi.onboarding.FieldTypePlain
import com.fauna.ffi.onboarding.HostedAuthState
import com.fauna.ffi.onboarding.ProviderStatus
import com.fauna.ffi.onboarding.formatPrice
import com.fauna.ffi.onboarding.handleTld
import com.fauna.ffi.provisioning.ContactInfo
import com.fauna.ffi.provisioning.RegistrarAvailability
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import social.fauna.generated.Capability
import social.fauna.generated.PROVIDERS
import social.fauna.generated.ProviderMeta
import uniffi.fauna_core.LocalizedText
import javax.inject.Inject
import social.fauna.generated.Ids

@HiltViewModel
class DnsConfigVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun toggleBuy(on: Boolean) = host.machine.toggleBuyDomain(on)
    fun toggleSame(on: Boolean) = host.machine.toggleSameProviderForVps(on)
    fun selectProvider(id: String) = host.machine.selectDnsProvider(id)
    fun setCred(field: String, value: String) = host.machine.setDnsCred(field, value)
    fun verify() = viewModelScope.launch { host.machine.verifyDns() }
    fun setUpLater() = host.machine.dnsSetUpLater()
    fun continueAction() {
        host.machine.continueFromDns()
    }
    fun confirmPrice() = host.machine.confirmPrice()
    fun setContact(contact: ContactInfo) = host.machine.setContact(contact)
}

private fun emptyContact() = ContactInfo(
    firstName = "",
    lastName = "",
    email = "",
    phone = "",
    address1 = "",
    city = "",
    state = "",
    postalCode = "",
    country = "",
)

@Composable
fun DnsConfigScreen(
    navController: NavController,
    vm: DnsConfigVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val state = remember(tick) { vm.host.machine.dnsConfig() }
    val isLoading = remember(tick) { vm.host.machine.isLoading() }
    val canContinue = remember(tick) { vm.host.machine.canContinueDns() }
    val canVerify = remember(tick) { vm.host.machine.canVerifyDns() }
    val statusKey = remember(tick) { vm.host.machine.dnsStatusTextKey() }
    val visibleFields = remember(tick) { vm.host.machine.visibleDnsFields() }
    val providerStatus = remember(tick) { vm.host.machine.providerStatus() }
    // Contact-form / registrar-notes / no-provider-message visibility are derived
    // in the shared machine (`should_show_contact_form` /
    // `should_show_registrar_notes` / `should_show_no_provider_message`),
    // mirroring linux + apple; clients just render the result.
    val showContactForm = remember(tick) { vm.host.machine.shouldShowContactForm() }
    val showRegistrarNotes = remember(tick) { vm.host.machine.shouldShowRegistrarNotes() }
    // The "no registrar carries .{tld}" message gates on the handle-check outcome
    // (`buy_domain && DomainAvailable { buyable_via_provider: false }`), NOT the
    // LATE, provider-specific `provider_status()` — the message is about the TLD,
    // so the shared handle-check getter is canonical (onboarding.md § 4 dns_config).
    val showNoProviderMessage = remember(tick) { vm.host.machine.shouldShowNoProviderMessage() }
    // TLD for the no-provider `.{tld}` message is the shared `handle_tld`
    // derivation (domain-first, after the `@` then the domain's last dot — a
    // local-part dot no longer leaks), not a hand-rolled substring (uniform
    // with macOS/iOS; onboarding.md § dns_config).
    val tld = remember(tick) {
        handleTld(handle = vm.host.machine.currentHandle()) ?: ""
    }
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    // A `hosted-auth` field's button label/enabled — the CredentialsForm
    // composable itself stays FFI-free (onboarding.md § 4); this is where the
    // per-tick machine reads happen, same as every other state val above.
    val hostedAuthDnsFields = remember(tick, visibleFields) {
        visibleFields.filter { it.fieldType == FieldTypePlain.HOSTED_AUTH }
    }
    val hostedAuthLabels = remember(tick, hostedAuthDnsFields) {
        hostedAuthDnsFields.associate {
            it.id to hostedAuthButtonLabel(ctx, vm.host.machine.hostedAuthState(CredentialForm.DNS, it.id))
        }
    }
    val hostedAuthEnabled = remember(tick, hostedAuthDnsFields) {
        hostedAuthDnsFields.associate { it.id to vm.host.machine.hostedAuthCanBegin(CredentialForm.DNS, it.id) }
    }

    val dnsProviders = remember { PROVIDERS.filter { it.capabilities.contains(Capability.DNS) } }
    // Per-row eligibility (buy_domain ⇒ Registrar, same_provider_for_vps ⇒ Vps)
    // is the shared `dns_provider_eligible` predicate — re-evaluated on tick so
    // toggling the buy/VPS checkboxes greys ineligible providers (linux parity).
    val providerEligible = remember(tick) {
        dnsProviders.associate { it.id to vm.host.machine.dnsProviderEligible(it.id) }
    }
    // The other half of the same pair: why a greyed row is dead
    // (`ui/README.md` § Copy comprehensibility rule 5). `None` exactly when
    // `dnsProviderEligible` is true, so verdict and explanation can't
    // disagree (onboarding.md § 4).
    val providerIneligibleReason = remember(tick) {
        dnsProviders.associate { it.id to vm.host.machine.dnsProviderIneligibleReason(it.id) }
    }
    val selectedProvider = dnsProviders.find { it.id == state.selectedProviderId }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_dns_config_title),
            style = MaterialTheme.typography.headlineSmall,
        )

        Spacer(Modifier.height(16.dp))

        Row(verticalAlignment = Alignment.CenterVertically) {
            Checkbox(
                checked = state.buyDomain,
                onCheckedChange = { vm.toggleBuy(it) },
                modifier = Modifier.testTag(Ids.DNS_BUY_DOMAIN_CHECKBOX),
            )
            Text(stringResource(R.string.onboarding_dns_config_buy_domain_checkbox))
        }

        Row(verticalAlignment = Alignment.CenterVertically) {
            Checkbox(
                checked = state.sameProviderForVps,
                onCheckedChange = { vm.toggleSame(it) },
                modifier = Modifier.testTag(Ids.DNS_SAME_PROVIDER_CHECKBOX),
            )
            Text(stringResource(R.string.onboarding_dns_config_same_provider_checkbox))
        }

        Spacer(Modifier.height(16.dp))

        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            dnsProviders.forEachIndexed { idx, provider ->
                DnsProviderButton(
                    label = provider.id.replaceFirstChar { it.uppercase() },
                    selected = provider.id == state.selectedProviderId,
                    enabled = providerEligible[provider.id] ?: true,
                    reason = localized(providerIneligibleReason[provider.id]),
                    tag = "dns-provider-row[$idx]",
                    onClick = { vm.selectProvider(provider.id) },
                )
            }
        }

        if (selectedProvider != null) {
            Spacer(Modifier.height(16.dp))
            ProviderHelpRow(selectedProvider)

            Spacer(Modifier.height(8.dp))
            CredentialsForm(
                fields = visibleFields,
                creds = state.creds,
                tag = "dns-credentials-form",
                enabled = !isLoading,
                onChange = { id, v -> vm.setCred(id, v) },
                hostedAuthLabels = hostedAuthLabels,
                hostedAuthEnabled = hostedAuthEnabled,
                onHostedAuthClick = { fieldId ->
                    scope.launch {
                        try {
                            val prompt = vm.host.machine.hostedAuthBegin(CredentialForm.DNS, fieldId)
                            UrlOpener.open(ctx, prompt.verificationUrl)
                            vm.host.machine.hostedAuthWait(CredentialForm.DNS, fieldId)
                        } catch (e: Exception) {
                            // Already in HostedAuthState.Failed (painted as the
                            // button label on the next tick) — nothing to
                            // re-derive here.
                        }
                    }
                },
            )

            Spacer(Modifier.height(8.dp))
            Button(
                onClick = { vm.verify() },
                enabled = canVerify && !isLoading,
                modifier = Modifier.testTag(Ids.DNS_VERIFY_BUTTON),
            ) {
                if (isLoading) {
                    CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                } else Text(stringResource(R.string.common_verify))
            }

            // dns-status-text — i18n key from snapshot.dns_status_text_key().
            // Resolve through the canonical by-name resolver (folds what was a
            // hand-rolled positional duplicate that misordered multi-arg keys).
            localized(statusKey)?.let { statusText ->
                Spacer(Modifier.height(8.dp))
                Text(
                    statusText,
                    modifier = Modifier.testTag(Ids.DNS_STATUS_TEXT),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }

            if (state.buyDomain) {
                val availability = state.currentAvailability
                if (availability is RegistrarAvailability.Buyable) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        formatPrice(availability.priceCents, availability.currency ?: "USD"),
                        modifier = Modifier.testTag(Ids.DNS_TLD_PRICE_DISPLAY),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }

                if (providerStatus is ProviderStatus.UnregisteredBuyable) {
                    Spacer(Modifier.height(8.dp))
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(
                            checked = state.priceAgreed,
                            onCheckedChange = { if (it && !state.priceAgreed) vm.confirmPrice() },
                            enabled = !state.priceAgreed,
                            modifier = Modifier.testTag(Ids.DNS_PRICE_CONFIRM_CHECKBOX),
                        )
                        Text(stringResource(R.string.registrar_price_confirm))
                    }
                }

                if (showNoProviderMessage) {
                    Spacer(Modifier.height(8.dp))
                    Text(
                        ctx.getStringFmt(R.string.onboarding_dns_config_no_provider_carries_tld, tld),
                        modifier = Modifier.testTag(Ids.DNS_NO_PROVIDER_MESSAGE),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }

                // Visibility is the shared `should_show_registrar_notes`; the
                // notes TEXT stays platform-side (the generated provider
                // registry's i18n key → Android string resource).
                if (showRegistrarNotes) {
                    val notesKey = selectedProvider?.registrarNotesKey
                    if (notesKey != null) {
                        val notesResName = notesKey.replace('.', '_')
                        val notesResId =
                            ctx.resources.getIdentifier(notesResName, "string", ctx.packageName)
                        if (notesResId != 0) {
                            Spacer(Modifier.height(8.dp))
                            Text(
                                ctx.getString(notesResId),
                                modifier = Modifier.testTag(Ids.DNS_REGISTRAR_NOTES_TEXT),
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                    }
                }

                if (showContactForm) {
                    Spacer(Modifier.height(8.dp))
                    ContactForm(
                        contact = state.contact ?: emptyContact(),
                        enabled = !isLoading,
                        onChange = { vm.setContact(it) },
                    )
                }
            }
        }

        Spacer(Modifier.height(8.dp))
        TextButton(
            onClick = { vm.setUpLater() },
            modifier = Modifier.testTag(Ids.DNS_SET_UP_LATER_BUTTON),
        ) { Text(stringResource(R.string.onboarding_dns_config_set_up_later)) }

        Spacer(Modifier.weight(1f))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = { navController.popBackStack() },
                modifier = Modifier.testTag(Ids.DNS_CONFIG_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }
            Button(
                onClick = {
                    vm.continueAction()
                    navController.navigate(routeForStep(vm.host.machine.step()))
                },
                enabled = canContinue && !isLoading,
                modifier = Modifier.testTag(Ids.DNS_CONFIG_CONTINUE_BUTTON),
            ) { Text(stringResource(R.string.common_continue)) }
        }
    }
}

/**
 * One `dns-provider-row[idx]` button + its rule-5 reason (`ui/README.md` §
 * Copy comprehensibility rule 5), split out stateless so it is directly
 * testable without an `OnboardingHost`/FFI machine — mirrors how
 * `MailAliasesContent` stays FFI-free by taking already-resolved strings.
 * [reason] is the already-`localized()`-resolved ineligibility text (or
 * `null`/empty when the provider is eligible); [DisabledControlReasonText]
 * renders nothing in that case.
 */
@Composable
internal fun DnsProviderButton(
    label: String,
    selected: Boolean,
    enabled: Boolean,
    reason: String?,
    tag: String,
    onClick: () -> Unit,
) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        FilledTonalButton(
            onClick = onClick,
            enabled = enabled,
            modifier = Modifier.testTag(tag),
            colors = if (selected) {
                ButtonDefaults.filledTonalButtonColors(
                    containerColor = MaterialTheme.colorScheme.primaryContainer,
                )
            } else ButtonDefaults.filledTonalButtonColors(),
        ) { Text(label) }
        DisabledControlReasonText(reason)
    }
}

@Composable
internal fun ProviderHelpRow(provider: ProviderMeta) {
    val ctx = LocalContext.current
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            provider.id,
            modifier = Modifier
                .testTag(Ids.DNS_PROVIDER_LINK)
                .testTag(Ids.VPS_PROVIDER_LINK),
            style = MaterialTheme.typography.bodySmall,
        )
        TextButton(
            onClick = {
                UrlOpener.open(ctx, provider.signupUrl)
            },
            modifier = Modifier
                .testTag(Ids.DNS_PROVIDER_OPEN_BROWSER_BUTTON)
                .testTag(Ids.VPS_PROVIDER_OPEN_BROWSER_BUTTON),
        ) { Text(stringResource(R.string.onboarding_dns_config_open_in_browser)) }
    }
    Text(
        localized(LocalizedText(key = provider.helpKey, args = emptyMap())) ?: provider.helpKey,
        modifier = Modifier
            .testTag(Ids.DNS_PROVIDER_HELP_TEXT)
            .testTag(Ids.VPS_PROVIDER_HELP_TEXT),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/**
 * A `hosted-auth` field's button label, from the machine's own
 * [HostedAuthState] — the Kotlin twin of tui's `hosted_auth_button` match
 * arm and web's `hostedAuthButtonLabel`. Nothing here is re-derived beyond
 * string lookup. Non-composable (takes [ctx] explicitly) so callers can
 * compute it inside a `remember` block, same shape as [resolveLocalized].
 */
internal fun hostedAuthButtonLabel(ctx: android.content.Context, state: HostedAuthState): String =
    when (state) {
        is HostedAuthState.Idle -> ctx.getString(R.string.provisioning_hosted_auth_connect)
        is HostedAuthState.Pending -> ctx.getStringFmt(R.string.provisioning_hosted_auth_pending, state.userCode)
        is HostedAuthState.Connected -> ctx.getString(R.string.provisioning_hosted_auth_connected)
        is HostedAuthState.Failed -> ctx.getStringFmt(R.string.provisioning_hosted_auth_failed, state.message)
    }

@Composable
internal fun CredentialsForm(
    fields: List<FieldMetaPlain>,
    creds: Map<String, String>,
    tag: String,
    enabled: Boolean,
    onChange: (String, String) -> Unit,
    hostedAuthLabels: Map<String, String> = emptyMap(),
    hostedAuthEnabled: Map<String, Boolean> = emptyMap(),
    onHostedAuthClick: (String) -> Unit = {},
) {
    Column(modifier = Modifier.fillMaxWidth().testTag(tag)) {
        fields.forEach { field ->
            if (field.fieldType == FieldTypePlain.HOSTED_AUTH) {
                // A `hosted-auth` field is a button, not an input — the
                // bundled provider's hosted sign-in (onboarding.md § 4), tui's
                // `hosted_auth_button` shape one-to-one: same derived id, same
                // begin → open → wait sequence (caller-owned, this composable
                // stays FFI-free — plain label/enabled data in, a callback
                // out).
                Button(
                    onClick = { onHostedAuthClick(field.id) },
                    enabled = enabled && (hostedAuthEnabled[field.id] ?: false),
                    modifier = Modifier.fillMaxWidth().testTag("$tag-${field.id}"),
                ) { Text(hostedAuthLabels[field.id] ?: "") }
            } else {
                OutlinedTextField(
                    value = creds[field.id] ?: "",
                    onValueChange = { onChange(field.id, it) },
                    label = { Text(field.id) },
                    modifier = Modifier.fillMaxWidth().testTag("$tag-${field.id}"),
                    enabled = enabled,
                    singleLine = true,
                    keyboardOptions = if (field.fieldType == FieldTypePlain.SECRET) {
                        KeyboardOptions(keyboardType = KeyboardType.Password)
                    } else KeyboardOptions.Default,
                    visualTransformation = if (field.fieldType == FieldTypePlain.SECRET) {
                        PasswordVisualTransformation()
                    } else androidx.compose.ui.text.input.VisualTransformation.None,
                )
            }
            Spacer(Modifier.height(4.dp))
        }
    }
}

@Composable
private fun ContactForm(
    contact: ContactInfo,
    enabled: Boolean,
    onChange: (ContactInfo) -> Unit,
) {
    Column(modifier = Modifier.fillMaxWidth().testTag(Ids.DNS_CONTACT_FORM)) {
        Text(
            stringResource(R.string.onboarding_dns_config_contact_form_heading),
            style = MaterialTheme.typography.titleSmall,
        )
        Spacer(Modifier.height(4.dp))
        ContactField("first-name", contact.firstName, enabled) {
            onChange(contact.copy(firstName = it))
        }
        ContactField("last-name", contact.lastName, enabled) {
            onChange(contact.copy(lastName = it))
        }
        ContactField("email", contact.email, enabled) {
            onChange(contact.copy(email = it))
        }
        ContactField("phone", contact.phone, enabled) {
            onChange(contact.copy(phone = it))
        }
        ContactField("address1", contact.address1, enabled) {
            onChange(contact.copy(address1 = it))
        }
        ContactField("city", contact.city, enabled) {
            onChange(contact.copy(city = it))
        }
        ContactField("state", contact.state, enabled) {
            onChange(contact.copy(state = it))
        }
        ContactField("postal-code", contact.postalCode, enabled) {
            onChange(contact.copy(postalCode = it))
        }
        ContactField("country", contact.country, enabled) {
            onChange(contact.copy(country = it))
        }
    }
}

@Composable
private fun ContactField(
    slug: String,
    value: String,
    enabled: Boolean,
    onChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(slug) },
        modifier = Modifier.fillMaxWidth().testTag("dns-contact-$slug-input"),
        enabled = enabled,
        singleLine = true,
    )
    Spacer(Modifier.height(4.dp))
}
