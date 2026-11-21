package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.onboarding.FieldMetaPlain
import com.fauna.ffi.onboarding.FieldTypePlain
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

/**
 * A credential form's `hosted-auth` sign-in button is painted from the shared
 * answer it is handed (`hosted_auth_possibility` /
 * `provider_hosted_auth_possibility`; `docs/goal/ui/README.md` § Offer only
 * what is possible, rules 2 and 6): blocked → shown, disabled, its reason
 * beneath; possible → live with no reason; absent → not composed.
 * [CredentialsForm] is FFI-free, so the answers are built here directly.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class CredentialsFormHostedAuthTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val field = FieldMetaPlain(
        id = "sign-in",
        fieldType = FieldTypePlain.HOSTED_AUTH,
        labelKey = "provisioning.hosted_auth_connect",
        required = true,
        kinds = emptyList(),
    )

    private fun form(answer: Possibility) {
        composeTestRule.setContent {
            CredentialsForm(
                fields = listOf(field),
                creds = emptyMap(),
                tag = "dns-credentials-form",
                enabled = true,
                onChange = { _, _ -> },
                hostedAuthLabels = mapOf(field.id to "Sign in"),
                hostedAuth = mapOf(field.id to answer),
            )
        }
    }

    @Test
    fun aBlockedSignInIsDisabledWithItsReasonBeneath() {
        form(
            Possibility.Blocked(
                LocalizedText(key = "provisioning.checking_credentials", args = emptyMap()),
            ),
        )
        val reason = ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(R.string.provisioning_checking_credentials)
        composeTestRule.onNodeWithTag("dns-credentials-form-sign-in").assertIsNotEnabled()
        composeTestRule.onNodeWithText(reason).assertExists()
    }

    @Test
    fun aPossibleSignInIsLiveWithNoReason() {
        form(Possibility.Possible)
        val reason = ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(R.string.provisioning_checking_credentials)
        composeTestRule.onNodeWithTag("dns-credentials-form-sign-in").assertIsEnabled()
        composeTestRule.onNodeWithText(reason).assertDoesNotExist()
    }

    @Test
    fun anAbsentSignInIsNotOffered() {
        form(Possibility.Absent)
        composeTestRule.onNodeWithTag("dns-credentials-form-sign-in").assertDoesNotExist()
    }
}
