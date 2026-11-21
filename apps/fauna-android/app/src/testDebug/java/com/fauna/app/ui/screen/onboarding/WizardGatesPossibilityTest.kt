package com.fauna.app.ui.screen.onboarding

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.navigation.compose.rememberNavController
import com.fauna.app.core.OnboardingHost
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.OnboardingObserver
import com.fauna.ffi.provisioning.ContactInfo
import kotlinx.coroutines.flow.MutableStateFlow
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when`
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import com.fauna.app.ui.util.isPossible

/**
 * The onboarding wizard's Continues paint from the machine's answers
 * (`docs/goal/ui/README.md` § Offer only what is possible, rule 6) — over a
 * REAL `OnboardingMachine`: the providers Continue and the DNS Continue are
 * disabled while their answer is blocked and live once the machine's state
 * makes it possible, the button following the answer and never an app-side
 * read of its own.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class WizardGatesPossibilityTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val tick = MutableStateFlow(0L)

    private fun machine(): OnboardingMachine = OnboardingMachine(object : OnboardingObserver {
        override fun `onChanged`() {
            tick.value = tick.value + 1
        }
    })

    private fun host(machine: OnboardingMachine): OnboardingHost {
        val host = mock(OnboardingHost::class.java)
        `when`(host.machine).thenReturn(machine)
        `when`(host.tick).thenReturn(tick)
        return host
    }

    private fun waitEnabled(tag: String) = composeTestRule.waitUntil(timeoutMillis = 10_000) {
        runCatching { composeTestRule.onNodeWithTag(tag).assertIsEnabled() }.isSuccess
    }

    @Test
    fun theProvidersContinuePaintsFromTheMachinesAnswer() {
        val m = machine()
        m.beginCreateIdentity()
        m.setCurrentHandle("alice@example.com")
        assertFalse("precondition: no provider, so Continue is blocked", m.providersContinuePossibility().isPossible)
        composeTestRule.setContent {
            ProviderChoiceScreen(navController = rememberNavController(), vm = ProviderChoiceVM(host(m)))
        }
        composeTestRule.onNodeWithTag(Ids.PROVIDER_CHOICE_CONTINUE_BUTTON).assertIsNotEnabled()

        // A provider whose checks found the handle's zone and listed a server
        // covers every goal (fixture setup, as the e2e suite seeds it).
        composeTestRule.runOnIdle {
            m.callMachineMethod(
                "set_provider_connection_for_test",
                """["hetzner", {"creds": {"api-token": "t"}, "connected": true, "held": false,
                  "zones": [{"id": "zone-1", "name": "example.com"}], "server_checked": true,
                  "locations": [{"id": "fsn1", "name": "Falkenstein", "city": "Falkenstein", "country": "DE"}],
                  "server_types": [{"id": "cx23", "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
                    "price_monthly_cents": 500, "currency": "EUR"}],
                  "listed_for_location": "fsn1"}]""",
            )
        }
        assertTrue("the machine now answers possible", m.providersContinuePossibility().isPossible)
        waitEnabled(Ids.PROVIDER_CHOICE_CONTINUE_BUTTON)
    }

    @Test
    fun theDnsContinuePaintsFromTheMachinesAnswer() {
        val m = machine()
        m.setCurrentHandle("alice@example.com")
        m.selectDnsProvider("porkbun")
        assertFalse("precondition: not verified, so Continue is blocked", m.dnsContinuePossibility().isPossible)
        composeTestRule.setContent {
            DnsConfigScreen(navController = rememberNavController(), vm = DnsConfigVM(host(m)))
        }
        composeTestRule.onNodeWithTag(Ids.DNS_CONFIG_CONTINUE_BUTTON).assertIsNotEnabled()

        // The registrar quoted the domain as buyable; the price is agreed and
        // the registration contact given.
        composeTestRule.runOnIdle {
            m.callMachineMethod(
                "set_dns_availability_for_test",
                """{"provider_id": "porkbun", "buy_domain": true, "price_cents": 1200, "currency": "EUR"}""",
            )
            m.confirmPrice()
            m.setContact(
                ContactInfo(
                    firstName = "Alice",
                    lastName = "Example",
                    email = "alice@example.com",
                    phone = "+1.5555550100",
                    address1 = "1 Example Street",
                    city = "Example",
                    state = "EX",
                    postalCode = "00000",
                    country = "US",
                ),
            )
        }
        assertTrue("the machine now answers possible", m.dnsContinuePossibility().isPossible)
        waitEnabled(Ids.DNS_CONFIG_CONTINUE_BUTTON)
    }
}
