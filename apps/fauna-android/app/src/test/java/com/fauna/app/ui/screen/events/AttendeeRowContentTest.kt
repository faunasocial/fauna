package com.fauna.app.ui.screen.events

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.data.api.Attendee
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.AttendeeDisplay

/**
 * Compose-level coverage for the stateless [AttendeeRow] (the event-detail
 * `attendee-item`, events.md § Attendee list presentation, ratified 2026-06-23):
 * the generated monogram avatar, the display name + email-beneath (omitted when
 * the name IS the email), and the colored RSVP-status label.
 *
 * The text *derivation* (CN→email fallback, monogram initial, email-beneath
 * visibility) now lives in shared Rust (`fauna_core::ical::attendee_display`,
 * unit-tested there) and reaches the row via UniFFI `attendeeDisplay`; the
 * RSVP-status **label** is likewise single-sourced in shared Rust
 * (`fauna_core::ical::rsvp_status_label`, unit-tested there) and reaches the row
 * via the injected `statusLabel` (the trailing color stays a per-platform map). So
 * this test seeds the projected [AttendeeDisplay] **and** the `statusLabel`
 * **directly** — no native call — and asserts the row wires each field to the
 * right UI slot (android Robolectric never calls real FFI; see
 * `ValueFormatResolverTest`).
 * `attendee-id`/`attendee-status`: the row keeps the canonical `attendee-item`
 * id on the outer `ListItem`; `attendee-id` now tags the headline (display
 * name) and `attendee-status` the trailing status text — the same shape
 * windows/tui/web already ship, so android stays id-identical to them
 * (priority #1). The other content (monogram/email) stays asserted by text.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AttendeeRowContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun monogramAndEmailBeneathName() {
        composeTestRule.setContent {
            AttendeeRow(
                Attendee(email = "ada@example.com", name = "Ada Lovelace", rsvp = "going"),
                view = AttendeeDisplay(
                    displayName = "Ada Lovelace",
                    monogram = "A",
                    secondaryEmail = "ada@example.com",
                ),
                statusLabel = "Going",
            )
        }
        // Monogram = uppercased initial of the display name (exact "A", distinct
        // from the "Ada Lovelace" headline).
        composeTestRule.onNodeWithText("A").assertExists()
        composeTestRule.onNodeWithText("Ada Lovelace").assertExists()
        // Email renders beneath the name (the projection carried a secondary email).
        composeTestRule.onNodeWithText("ada@example.com").assertExists()
    }

    @Test
    fun emailOmittedWhenNameIsTheEmail() {
        composeTestRule.setContent {
            AttendeeRow(
                Attendee(email = "bob@example.com", name = "", rsvp = "invited"),
                // No CN → the email is the display name and there is no secondary line.
                view = AttendeeDisplay(
                    displayName = "bob@example.com",
                    monogram = "B",
                    secondaryEmail = null,
                ),
                statusLabel = "Invited",
            )
        }
        composeTestRule.onNodeWithText("B").assertExists()
        // The email renders exactly once (the headline) — no duplicate supporting line.
        assertEquals(
            1,
            composeTestRule.onAllNodesWithText("bob@example.com").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun statusLabelGoing() {
        composeTestRule.setContent {
            AttendeeRow(
                Attendee(email = "g@x.test", name = "Grace", rsvp = "going"),
                view = AttendeeDisplay("Grace", "G", null),
                statusLabel = "Going",
            )
        }
        composeTestRule.onNodeWithText("Going").assertExists()
    }

    @Test
    fun statusLabelInterested() {
        composeTestRule.setContent {
            AttendeeRow(
                Attendee(email = "i@x.test", name = "Iris", rsvp = "interested"),
                view = AttendeeDisplay("Iris", "I", null),
                statusLabel = "Interested",
            )
        }
        composeTestRule.onNodeWithText("Interested").assertExists()
    }

    @Test
    fun attendeeIdAndStatusTagDistinctNodesInsideAttendeeItem() {
        // Before this row's fix, neither child carried a tag, so both
        // onNodeWithTag lookups below would fail before assertTextEquals
        // ever ran (Compose's onNodeWithTag throws when nothing matches).
        composeTestRule.setContent {
            AttendeeRow(
                Attendee(email = "guest@example.com", name = "", rsvp = "invited"),
                view = AttendeeDisplay(
                    displayName = "guest@example.com",
                    monogram = "G",
                    secondaryEmail = null,
                ),
                statusLabel = "Invited",
            )
        }
        composeTestRule.onNodeWithTag(Ids.ATTENDEE_ITEM).assertExists()
        // ListItem's headline/trailing slots merge into the row's own semantics
        // node by default, so the two child tags are invisible to the MERGED
        // tree even though they render distinctly — useUnmergedTree = true is
        // what the assertion failure itself points at.
        composeTestRule.onNode(hasTestTag(Ids.ATTENDEE_ID), useUnmergedTree = true)
            .assertTextEquals("guest@example.com")
        composeTestRule.onNode(hasTestTag(Ids.ATTENDEE_STATUS), useUnmergedTree = true)
            .assertTextEquals("Invited")
    }

    @Test
    fun statusLabelRendersInjectedLabel() {
        // The label text now comes from the shared `rsvp_status_label` map (the
        // `tentative` → "Tentative" capitalize, formerly an android `else` arm, lives
        // in shared Rust and is unit-tested there); the row just wires the resolved
        // label into the trailing slot.
        composeTestRule.setContent {
            AttendeeRow(
                Attendee(email = "t@x.test", name = "Tim", rsvp = "tentative"),
                view = AttendeeDisplay("Tim", "T", null),
                statusLabel = "Tentative",
            )
        }
        composeTestRule.onNodeWithText("Tentative").assertExists()
    }
}
