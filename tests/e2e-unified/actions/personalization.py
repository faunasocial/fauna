"""Trained-topics facet actions — the Personalization home's
``personalization-trained-factor-*`` family (topic-factors.md § Authoring
surface & picker; IDs user-approved 2026-07-09).

One ``name-input``, two flows (the ratified shape): the input +
``create-button`` create a factor inline; a row's ``rename-button`` retargets
the same input at that row (prefilled) and the next commit renames.
"""
import time

from drivers.base import PlatformDriver
from helpers import budgets
from helpers.waiting import wait_until


class PersonalizationActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # ── navigation ───────────────────────────────────────────────────────

    def navigate(self) -> None:
        """Navigate to the Personalization home Settings sub-page (the same
        nav shape as LabelerCatalogActions.navigate_home)."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "personalization"}]},
        })

    # ── trained-topics facet ─────────────────────────────────────────────

    def create_topic(self, name: str) -> None:
        """Create a trained topic via the inline add row."""
        self.driver.wait_for("personalization-trained-factor-name-input")
        self.driver.clear_and_type("personalization-trained-factor-name-input", name)
        self.driver.click("personalization-trained-factor-create-button")

    def topic_count(self) -> int:
        return self.driver.count("personalization-trained-factor-item")

    def wait_for_topic_count(self, expected: int, timeout: float = 15.0) -> int:
        """Gate on the async-rendered rows, never the synchronously-built
        list container (the settings-sub-page e2e trap — tips § linux)."""
        deadline = time.monotonic() + timeout
        count = self.topic_count()
        while count != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.topic_count()
        return count

    def topic_name(self, index: int = 0) -> str:
        return self.driver.get_text("personalization-trained-factor-name", index=index) or ""

    def topic_names(self) -> list:
        return [self.topic_name(i) for i in range(self.topic_count())]

    def create_sole_topic(self, name: str) -> None:
        """Create ``name`` and leave it the page's ONLY trained topic.

        The session actor outlives every module, so a topic an earlier test
        created and never deleted — it failed before reaching its own cleanup —
        is still in the registry. A count gate (``wait_for_topic_count(1)``) is
        then satisfied by that leftover before this test's own row renders, and
        every ``index=0`` read after it addresses someone else's topic. The
        2026-09-11 linux sweep lost a whole module this way: one engagement-cues
        failure left a ``Cats-…`` topic behind, and all five trained-topic tests
        downstream failed on it, each finding one more leftover than the last.

        So this waits for THIS topic's own row, never a count. The rows are
        rebuilt from one registry snapshot, so a render that shows ours shows
        every leftover too — a not-yet-loaded list can never pass for a swept
        one. Then it deletes every other row, one confirmed delete at a time.
        """
        self.create_topic(name)

        def diagnose():
            try:
                error = self.driver.get_text("error-message")
            except Exception as exc:  # the page may not be mounted at all
                error = f"<unreadable: {exc}>"
            return f"rows={self.topic_names()!r}; error={error!r}"

        wait_until(
            lambda: any(name in shown for shown in self.topic_names()),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"this test's own topic {name!r} never rendered; {diagnose()}",
        )
        while True:
            names = self.topic_names()
            foreign = [i for i, shown in enumerate(names) if name not in shown]
            if not foreign:
                break
            before = len(names)
            self.delete_topic(foreign[0])
            wait_until(
                lambda: self.topic_count() < before,
                budgets.RPC_ROUNDTRIP_S,
                diagnose=lambda: (
                    f"deleting the leftover topic {names[foreign[0]]!r} never "
                    f"removed its row; {diagnose()}"
                ),
            )
        names = self.topic_names()
        assert len(names) == 1 and name in names[0], (
            f"expected {name!r} to be the only trained topic; {diagnose()}"
        )

    def topic_example_count_text(self, index: int = 0) -> str:
        """The row's example-count display text (e.g. ``"1 examples"``)."""
        return (
            self.driver.get_text("personalization-trained-factor-example-count", index=index)
            or ""
        )

    def topic_factor_key(self) -> str:
        """The FIRST row's minted stable composition key (``topic:<hex>``).

        Read from the row's ``factor`` test-attr (hex only — a CSS class can't
        carry ``:``): the registry is sealed under the user's BackupKey, so no
        wire read can answer, and ``feed-factor-select``'s ``select()`` needs
        the stable key. Indexed reads can ride a scoped ``get_attr`` when a
        multi-topic test needs them.
        """
        hexpart = self.driver.get_attr("personalization-trained-factor-item", "factor")
        assert hexpart, "trained-factor row exposes no `factor` test-attr"
        return f"topic:{hexpart}"

    def rename_topic(self, index: int, new_name: str) -> None:
        """Retarget the shared name-input at row ``index`` and commit."""
        self.driver.click("personalization-trained-factor-rename-button", index=index)
        self.driver.clear_and_type("personalization-trained-factor-name-input", new_name)
        self.driver.click("personalization-trained-factor-create-button")

    def delete_topic(self, index: int = 0) -> None:
        self.driver.click("personalization-trained-factor-delete-button", index=index)

    # ── engagement (Layer A) affordances ─────────────────────────────────

    def engagement_toggle_on(self, index: int = 0) -> bool:
        """The row's ``learn_from_engagement`` toggle state — a live Switch
        read via the uniform ``get_attr(id, "state")`` idiom, scoped to the
        row (``get_attr`` takes no index; scoped queries are the convention)."""
        return (
            self.driver.get_attr(
                "personalization-trained-factor-engagement-toggle",
                "state",
                scope=f"personalization-trained-factor-item[{index}]",
            )
            == "true"
        )

    def set_engagement_toggle(self, index: int, on: bool, timeout: float = 10.0) -> None:
        """Flip the row's "Learn from my activity" toggle to ``on`` and gate on
        the persisted state coming back (the commit round-trips the sealed
        registry, then the facet re-renders its rows)."""
        if self.engagement_toggle_on(index) == on:
            return
        self.driver.click(
            "personalization-trained-factor-engagement-toggle", index=index
        )
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.engagement_toggle_on(index) == on:
                return
            time.sleep(0.2)
        # The page may paint no `error-message` at all, and a `get_text` on an
        # absent element raises: a failure message that itself raises hides the
        # failure it was written to report (convention 6).
        error = (
            self.driver.get_text("error-message")
            if self.driver.is_visible("error-message")
            else ""
        )
        raise AssertionError(
            f"engagement toggle[{index}] never reached {on} "
            f"(error: {error or 'none shown'})"
        )

    def clear_engagement_data(self) -> None:
        """"Clear activity data" — deletes the sealed cues:v1 rollup from the
        user's own nest and resets the live engine."""
        self.driver.click("personalization-clear-engagement-data-button")

    # ── publish review-prune sheet (topic-factors.md § Publishing) ───────
    # The sheet is single-instance and pre-targeted at the row whose
    # `publish-button` opened it (the admin-dns-rename-sheet shape). Its
    # exemplar rows arrive asynchronously — the corpus scoring reads the
    # FeedManager's loaded window — so every count read gates rather than
    # snapshots.

    def open_publish_sheet(self, index: int = 0) -> None:
        """Open the review-prune sheet for row ``index``, pre-targeted at it."""
        self.driver.click("personalization-trained-factor-publish-button", index=index)
        self.driver.wait_for("personalization-trained-factor-publish-sheet")

    def publish_sheet_visible(self) -> bool:
        return not self.driver.is_absent("personalization-trained-factor-publish-sheet")

    def publish_limitation_text(self) -> str:
        """The mandated copy: a List scores only what the publisher saw, and
        publishing is anonymous (topic-factors.md § Publishing — both are
        ratified disclosures the sheet owes the user)."""
        return (
            self.driver.get_text("personalization-trained-factor-publish-limitation-note")
            or ""
        )

    def publish_exemplar_count(self) -> int:
        return self.driver.count("personalization-trained-factor-publish-exemplar-item")

    def wait_for_publish_exemplars(self, minimum: int = 1, timeout: float = 15.0) -> int:
        """Gate on the async-scored exemplar rows (the corpus read round-trips
        the sealed model before the rows can render)."""
        deadline = time.monotonic() + timeout
        count = self.publish_exemplar_count()
        while count < minimum and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.publish_exemplar_count()
        return count

    def publish_exemplar_text(self, index: int) -> str:
        return (
            self.driver.get_text(
                "personalization-trained-factor-publish-exemplar-text", index=index
            )
            or ""
        )

    def publish_exemplar_score(self, index: int) -> str:
        return (
            self.driver.get_text(
                "personalization-trained-factor-publish-exemplar-score", index=index
            )
            or ""
        )

    def publish_exemplar_included(self, index: int) -> bool:
        """Whether exemplar ``index`` is checked (= included). Read via the
        uniform ``get_attr(id, "state")`` idiom, scoped to the row — the linux
        agent answers a bare ``gtk::CheckButton``'s state natively."""
        return (
            self.driver.get_attr(
                "personalization-trained-factor-publish-exemplar-checkbox",
                "state",
                scope=f"personalization-trained-factor-publish-exemplar-item[{index}]",
            )
            == "true"
        )

    def set_publish_exemplar_included(self, index: int, included: bool) -> None:
        """Check/uncheck exemplar ``index`` (CHECKED = include — the
        restore-kind-checkbox prune semantics)."""
        if self.publish_exemplar_included(index) == included:
            return
        self.driver.click(
            "personalization-trained-factor-publish-exemplar-checkbox", index=index
        )

    # ── the Model kind (topic-factors.md § Publishing a trained factor, v2) ──
    # The kind picker is a RAW-VALUE select: it round-trips the `artifact_kind`
    # discriminator, never a translated label, so these two constants are the
    # same on all 7 apps and no rewording of the copy can break the driver.
    PUBLISH_KIND_LIST = "list"
    PUBLISH_KIND_MODEL = "text-model"

    def publish_kind(self) -> str:
        return (
            self.driver.get_text("personalization-trained-factor-publish-kind-select")
            or ""
        )

    def set_publish_kind(self, kind: str) -> None:
        """Swap which artifact kind the open sheet reviews. Discards the prune
        and re-reads — the two kinds review different objects."""
        self.driver.select("personalization-trained-factor-publish-kind-select", kind)

    def publish_ngram_count(self) -> int:
        return self.driver.count("personalization-trained-factor-publish-ngram-item")

    def wait_for_publish_ngrams(self, minimum: int = 1, timeout: float = 20.0) -> int:
        """Gate on the async-scrubbed vocabulary. The scrub is heavier than the
        List's corpus read — it fetches every marked post before it can prune —
        so the budget is generous; the assertion is on the COUNT, never on the
        wait (convention 14)."""
        deadline = time.monotonic() + timeout
        count = self.publish_ngram_count()
        while count < minimum and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.publish_ngram_count()
        return count

    def publish_ngram_text(self, index: int) -> str:
        return (
            self.driver.get_text(
                "personalization-trained-factor-publish-ngram-text", index=index
            )
            or ""
        )

    def publish_ngram_direction(self, index: int) -> str:
        """The class direction — part of the ratified disclosure, not
        decoration: a pattern learned from *less like this* examples must say
        so."""
        return (
            self.driver.get_text(
                "personalization-trained-factor-publish-ngram-direction", index=index
            )
            or ""
        )

    def publish_ngram_count_text(self, index: int) -> str:
        """The class-blind distinct-post count (more + less) — the quantity the
        3-post privacy floor bounds."""
        return (
            self.driver.get_text(
                "personalization-trained-factor-publish-ngram-count", index=index
            )
            or ""
        )

    def publish_ngram_included(self, index: int) -> bool:
        return (
            self.driver.get_attr(
                "personalization-trained-factor-publish-ngram-checkbox",
                "state",
                scope=f"personalization-trained-factor-publish-ngram-item[{index}]",
            )
            == "true"
        )

    def set_publish_ngram_included(self, index: int, included: bool) -> None:
        if self.publish_ngram_included(index) == included:
            return
        self.driver.click(
            "personalization-trained-factor-publish-ngram-checkbox", index=index
        )

    def publish_ngram_refusal_visible(self) -> bool:
        """The refusal state: nothing survived the 3-post floor, so there is
        nothing that can be shared without quoting a single post."""
        return not self.driver.is_absent(
            "personalization-trained-factor-publish-ngram-empty"
        )

    def type_publish_name(self, name: str) -> None:
        self.driver.clear_and_type(
            "personalization-trained-factor-publish-name-input", name
        )

    def publish_submit_enabled(self) -> bool:
        """Whether Publish is armed. Disarmed with nothing kept — publishing an
        empty List means nothing (the `restore-confirm-button` precedent)."""
        return self.driver.is_enabled(
            "personalization-trained-factor-publish-submit-button"
        )

    def submit_publish(self) -> None:
        self.driver.click("personalization-trained-factor-publish-submit-button")

    def cancel_publish(self) -> None:
        self.driver.click("personalization-trained-factor-publish-cancel-button")

    def wait_for_publish_sheet_closed(self, timeout: float = 15.0) -> bool:
        """A successful publish closes the sheet; a refusal leaves it open with
        the reason on the page ``error-message``."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.publish_sheet_visible():
                return True
            time.sleep(0.2)
        return False

    # ── Layer-B signal sharing (engagement-cues.md § Layer B) ────────────

    def signal_sharing_on(self) -> bool:
        """The signal-sharing opt-in toggle state — read via the uniform
        ``get_attr(id, "state")`` idiom. Non-optimistic: the linux widget sets
        the attr only from the nest-confirmed ``signal_share.status`` reply
        (``"on"``/``"off"``, the report-share toggle convention — NOT the bare
        Switch ``"true"``/``"false"``), so this doubles as the round-trip proof."""
        return (
            self.driver.get_attr("personalization-share-signals-toggle", "state") == "on"
        )

    def set_signal_sharing(self, on: bool, timeout: float = 10.0) -> None:
        """Flip the signal-sharing opt-in to ``on`` and gate on the persisted
        state coming back (the flip round-trips ``signal_share.set`` →
        ``signal_share.status``, then the pane re-renders the toggle)."""
        if self.signal_sharing_on() == on:
            return
        self.driver.click("personalization-share-signals-toggle")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.signal_sharing_on() == on:
                return
            time.sleep(0.2)
        raise AssertionError(
            f"signal-sharing toggle never reached {on} "
            f"(error: {self.driver.get_text('error-message') or 'none shown'})"
        )

    # ── signal-share-published-list transparency pane readers ────────────
    # Mirror mail_spam's report-share published-list readers; the pane renders
    # the nest-wide ≥k export view (signal:* + report:* aggregates).

    def signal_published_count(self) -> int:
        """How many `signal-share-published-list-item` rows are rendered."""
        return self.driver.count("signal-share-published-list-item-hash")

    def signal_published_hash(self, index: int) -> str:
        return self.driver.get_text("signal-share-published-list-item-hash", index=index)

    def signal_published_factor(self, index: int) -> str:
        return self.driver.get_text("signal-share-published-list-item-factor", index=index)

    def signal_published_contributor_count(self, index: int) -> str:
        return self.driver.get_text("signal-share-published-list-item-count", index=index)
