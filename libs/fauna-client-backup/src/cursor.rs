//! The termination guard shared by every cursor-paginated destination walk in
//! this crate ([`crate::generations::read_one`], [`crate::audit::read_full_custody`]) —
//! extracted because both hand-copied the identical refusal: a destination
//! that claims more rows while serving an empty page, or repeats the cursor it
//! was just given, would loop forever. That is a contract violation to
//! surface as `Err`, never a partial page to launder into a clean (short)
//! completion — a short walk would understate the destination's true content
//! and could hide the very rows a storm produced.
//!
//! The one-step check above answers only *"did this page move us?"* — a
//! destination alternating two (or more) distinct cursors satisfies it on
//! every page while never draining. [`MAX_PAGES_PER_DESTINATION_WALK`] is the second,
//! independent bound that answers *"will this walk end?"* — the distinction
//! `account-sync-plane.md` § Feeds and cursors draws as *"the walk is
//! bounded, and the spin refusal is not the bound"* (ruled 2026-09-02).

/// Upper bound on pages one destination pagination walk will fetch before it
/// refuses rather than keep following the counterpart's cursor.
///
/// Same shape as the precedent `account-sync-plane.md` § Feeds and cursors
/// names for this exact class of problem — a counterpart-driven paginated
/// walk that must terminate against an untrusted counterpart —
/// `fauna_sync_engine::page_walk::MAX_PAGES_PER_WALK`: a hard-coded
/// termination guarantee, not a performance knob, sized so no honest walk
/// reaches it. Sized differently, though: that walk is checkpointed per page
/// and resumable, so its ceiling only has to bound *pages*. Both walks here
/// accumulate every row into one `Vec` and return it only once draining
/// completes (`generations::read_one`, `audit::read_full_custody`), so this
/// ceiling also bounds the worst-case single-destination memory a hostile
/// destination can force before the refusal lands — the growth the finding
/// measured (up to one frame per page, unbounded).
///
/// Derived from the nest's own paging bounds, both in
/// `bins/fauna-nest/src/backup_handlers.rs`: `BACKUP_LIST_FETCH_CAP` (8192)
/// is the per-request row bound, and that constant's own doc comment notes a
/// realistic encoded row is ≥ ~250 bytes — which is what makes 8192 rows
/// also roughly the byte budget's (`SERVE_PAGE_BUDGET_BYTES`, ~1.94 MiB —
/// `bins/fauna-nest/src/segments/mod.rs`) worst case, so an honest page
/// realistically carries close to that many rows. At this ceiling that is up
/// to ~524,288 rows honestly covered per destination — far past any one
/// account's retained-generation or live-custody count — while the
/// worst-case accumulated wire bytes before refusal stays ~124 MiB (64 × the
/// ~1.94 MiB frame budget), bounded rather than the unbounded growth a
/// cycling or ever-fresh cursor forces today.
pub(crate) const MAX_PAGES_PER_DESTINATION_WALK: usize = 64;

/// What to do after one fetched page: stop, or fetch again with the given
/// cursor.
#[derive(Debug)]
pub(crate) enum CursorStep {
    Done,
    Next(String),
}

/// Advance past one page. `what` names the RPC kind for the error text (e.g.
/// `"generation.list"`, `"custody.list"`); `cursor` is the cursor that was
/// just sent; `page_empty`/`next_cursor` come from the page just received;
/// `pages_fetched` is the walk's own running count, including the page just
/// received — the caller increments it once per fetch, before calling this.
pub(crate) fn advance_cursor(
    what: &str,
    cursor: &Option<String>,
    page_empty: bool,
    next_cursor: Option<String>,
    pages_fetched: usize,
) -> Result<CursorStep, String> {
    match next_cursor {
        None => Ok(CursorStep::Done),
        Some(next) => {
            if page_empty || cursor.as_ref() == Some(&next) {
                return Err(format!("{what} page advanced no cursor"));
            }
            if pages_fetched >= MAX_PAGES_PER_DESTINATION_WALK {
                return Err(format!(
                    "{what} exceeded {MAX_PAGES_PER_DESTINATION_WALK} pages without draining"
                ));
            }
            Ok(CursorStep::Next(next))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draining_page_below_the_ceiling_advances() {
        assert!(matches!(
            advance_cursor("k", &None, false, Some("n".into()), 1),
            Ok(CursorStep::Next(n)) if n == "n"
        ));
    }

    #[test]
    fn an_absent_cursor_completes_regardless_of_pages_fetched() {
        assert!(matches!(
            advance_cursor(
                "k",
                &Some("c".into()),
                false,
                None,
                MAX_PAGES_PER_DESTINATION_WALK
            ),
            Ok(CursorStep::Done)
        ));
    }

    #[test]
    fn the_no_progress_checks_still_fire_at_the_ceiling() {
        // The ceiling is a second, independent bound — it must not mask the
        // one-step-back checks that already exist.
        let err = advance_cursor(
            "k",
            &Some("same".into()),
            false,
            Some("same".into()),
            MAX_PAGES_PER_DESTINATION_WALK,
        )
        .unwrap_err();
        assert!(err.contains("advanced no cursor"), "got: {err}");
    }

    #[test]
    fn a_draining_page_at_the_ceiling_is_refused() {
        let err = advance_cursor(
            "k",
            &Some("a".into()),
            false,
            Some("b".into()),
            MAX_PAGES_PER_DESTINATION_WALK,
        )
        .unwrap_err();
        assert!(
            err.contains(&format!("exceeded {MAX_PAGES_PER_DESTINATION_WALK} pages")),
            "got: {err}"
        );
    }
}
