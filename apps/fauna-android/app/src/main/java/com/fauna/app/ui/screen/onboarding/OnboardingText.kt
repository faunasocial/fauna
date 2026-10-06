package com.fauna.app.ui.screen.onboarding

import androidx.compose.runtime.Composable
import com.fauna.app.ui.util.localized
import uniffi.fauna_core.LocalizedText

/**
 * Onboarding-screen alias for the canonical [localized] resolver — maps a
 * shared-machine [LocalizedText] (i18n key + args) to the generated Android
 * string resource. Every onboarding screen (claim_code / handle_entry /
 * invite_request) routes its snapshot message through here. The resolution
 * logic lives once in [com.fauna.app.ui.util.localized]; this is just a
 * domain-named convenience so the call sites read clearly.
 */
@Composable
internal fun localizedOnboardingText(text: LocalizedText): String? = localized(text)
