//! Which generations the ledger keeps, and deleting the rest.
//!
//! The ledger is append-only while a generation is reachable; it is not
//! infinite. Design §7.3 keeps the last 20 generations, and this module is
//! what makes that sentence true — the rows outside the retained set are
//! *deleted*, not relabelled, and [`super::gc`] then reclaims the content
//! only those rows named.
//!
//! # What "retained" means
//!
//! Three things, and each is load-bearing:
//!
//! * the [`RETAINED_GENERATIONS`] most recent rows — the set the ledger view
//!   shows and a rollback can target;
//! * the generation that is **serving**, however far down the ledger it has
//!   fallen. Twenty refused activations after a good one do not make the good
//!   one collectable: it is what the site currently *is*;
//! * every **in-flight** row (staged, validating, activating). The journal may
//!   name one and boot convergence re-runs it (design §7.3), so deleting one
//!   turns an interrupted activation into an unrecoverable one.
//!
//! The second and third are [`GenerationStatus::survives_retention`], which
//! owns that half of the definition so it cannot be restated differently here
//! and in the query that deletes.
//!
//! An in-flight row is kept because an activation might still finish it — not
//! because it is immortal. `activation::converge_on_boot` retires the ones no
//! journal names, which is what stops a crash loop from accumulating rows that
//! pin blobs against the workspace quota (design amendment 13) and what keeps
//! [`retained`]'s in-flight listing bounded.
//!
//! # Superseded is not an age
//!
//! `Superseded` is the status of a generation a later one replaced, written by
//! the activation that replaced it. Nothing rewrites a row's status because it
//! got old, so a `Failed` generation still reads as `Failed` for as long as it
//! is kept, and "how the generation ended" and "how old it is" stay two
//! separate questions.
//!
//! # Why deleting, rather than labelling
//!
//! A label leaves the row — and everything it names — reachable forever, which
//! is exactly what the collector cannot work with: the blob store's quota
//! (design §6.6) bounds what is *stored*, and nothing stored can be reclaimed
//! while some row still names it. Deleting is also what makes the retention
//! window a real bound: the ledger holds 20 rows plus whatever is live, rather
//! than every generation the sandbox has ever published with 20 of them
//! unmarked.

use wafer_run::{context::Context, WaferError};

use super::repo::generations::{self, GenerationRow};

/// How many generations the ledger keeps (design §7.3).
///
/// Also the default page size of `GET /b/dev/api/generations`, so the default
/// listing is exactly the set that can still be rolled back to.
pub const RETAINED_GENERATIONS: usize = 20;

/// The rows retention keeps.
///
/// The newest [`RETAINED_GENERATIONS`], then the generation that is serving,
/// then whatever is still in flight — deduplicated by id, so a caller can
/// treat the result as a set, which is what the collector needs it to be.
///
/// # Why three queries and not one
///
/// A single `status IN (…)` listing would be capped by a page, and the two
/// kinds of row behind that cap are not equally safe to lose. The serving
/// generation is what the site *is*: losing it would let the collector delete
/// the blobs the site is being served from, so it is fetched on its own
/// ([`generations::find_active`], one row, no cap between it and the answer).
/// The in-flight rows are recovery targets and their listing IS capped — which
/// is safe only because the set is *bounded*: `activation::converge_on_boot`
/// retires every in-flight row the journal does not name, so what remains is
/// the one being converged on plus the at-most-one an activation is running.
/// Without that retirement, a crash loop would accumulate staged rows that
/// pinned blobs against the workspace quota forever and, past the cap, would
/// start pushing the serving generation out of this set.
pub async fn retained(ctx: &dyn Context) -> Result<Vec<GenerationRow>, WaferError> {
    let mut rows = generations::list_recent(ctx, RETAINED_GENERATIONS as u32).await?;
    if rows.len() < RETAINED_GENERATIONS {
        // The whole ledger is inside the window: the other two queries could
        // only return rows this one already holds.
        return Ok(rows);
    }

    let older = generations::find_active(ctx)
        .await?
        .into_iter()
        .chain(generations::list_in_flight(ctx).await?);
    for row in older {
        if !rows.iter().any(|kept| kept.id == row.id) {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Delete every generation outside the retained set, returning the rows that
/// went, newest first.
///
/// Runs at the end of every successful activation — the only moment a
/// generation stops being the newest of anything. Idempotent: a second pass
/// over a ledger already inside the window deletes nothing and returns an
/// empty list.
///
/// The boundary is the oldest row the window keeps, and the pass deletes
/// strictly below it. Paged rather than listed whole: every row a page holds
/// is deleted — in one write, [`generations::delete_many`] — before the next
/// page is asked for, so an empty page means done and a ledger far past the
/// window is collected in full rather than down to one page's worth of rows.
pub async fn prune(ctx: &dyn Context) -> Result<Vec<GenerationRow>, WaferError> {
    let newest = generations::list_recent(ctx, RETAINED_GENERATIONS as u32).await?;
    let Some(boundary) = newest.get(RETAINED_GENERATIONS - 1) else {
        // Fewer rows than the window holds: nothing is outside it.
        return Ok(Vec::new());
    };

    let mut pruned = Vec::new();
    loop {
        let page = generations::list_prunable(ctx, boundary).await?;
        if page.is_empty() {
            return Ok(pruned);
        }
        let ids: Vec<String> = page.iter().map(|row| row.id.clone()).collect();
        generations::delete_many(ctx, &ids).await?;
        pruned.extend(page);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::dev::{
            contracts::SiteManifest,
            generation::{self, GenerationManifest},
            repo,
            repo::generations::{GenerationCause, GenerationStatus, NewGeneration},
            test_support::FakeControl,
        },
        test_support::TestContext,
    };

    /// Append one row in `status`, with an empty manifest.
    async fn row(ctx: &TestContext, status: GenerationStatus) -> GenerationRow {
        let id = repo::new_id();
        let mut manifest = GenerationManifest::staged(SiteManifest::default(), Vec::new());
        manifest.identify(id.clone(), None);
        let row = generations::insert(
            ctx,
            &NewGeneration {
                id,
                parent_id: None,
                cause: GenerationCause::SiteWrite,
                site_manifest_json: generation::canonical_text(&manifest.site).expect("canonical"),
                block_manifest_json: generation::canonical_text(&manifest.blocks)
                    .expect("canonical"),
                manifest_sha256: generation::manifest_sha256(&manifest).expect("hash"),
            },
        )
        .await
        .expect("insert");
        if status != GenerationStatus::Staged {
            generations::set_status(ctx, &row.id, status, None, None)
                .await
                .expect("set status");
        }
        GenerationRow { status, ..row }
    }

    /// The ids the ledger still holds, newest first.
    async fn ledger(ctx: &TestContext) -> Vec<String> {
        generations::list_recent(ctx, 200)
            .await
            .expect("list")
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    /// **A page is one write.** The rows a pass prunes go in one `batch`
    /// of deletes rather than a `delete` each — in the browser, one OPFS
    /// save instead of one per generation.
    #[tokio::test]
    async fn a_pass_deletes_its_page_in_one_batch() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        for _ in 0..RETAINED_GENERATIONS + 5 {
            row(&ctx, GenerationStatus::Superseded).await;
        }
        let (ctx, log) = ctx.record_writes();

        assert_eq!(prune(&ctx).await.expect("prune").len(), 5);

        {
            let log = log.lock().expect("log");
            assert_eq!(log.deletes, 0, "no generation goes through a single delete");
            assert_eq!(log.batch_ops, [5]);
        }
        assert_eq!(ledger(&ctx).await.len(), RETAINED_GENERATIONS);
    }

    #[tokio::test]
    async fn a_ledger_inside_the_window_is_left_alone() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        for _ in 0..RETAINED_GENERATIONS {
            row(&ctx, GenerationStatus::Superseded).await;
        }
        assert!(prune(&ctx).await.expect("prune").is_empty());
        assert_eq!(ledger(&ctx).await.len(), RETAINED_GENERATIONS);
        assert_eq!(
            retained(&ctx).await.expect("retained").len(),
            RETAINED_GENERATIONS
        );
    }

    #[tokio::test]
    async fn rows_past_the_window_are_deleted_newest_first_and_the_pass_is_idempotent() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let mut ids = Vec::new();
        for _ in 0..(RETAINED_GENERATIONS + 3) {
            ids.push(row(&ctx, GenerationStatus::Superseded).await.id);
        }

        let pruned = prune(&ctx).await.expect("prune");
        assert_eq!(
            pruned.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            vec![ids[2].clone(), ids[1].clone(), ids[0].clone()],
            "the three oldest went, newest of them first",
        );
        assert_eq!(ledger(&ctx).await.len(), RETAINED_GENERATIONS);
        assert!(prune(&ctx).await.expect("prune again").is_empty());
    }

    /// The pass is not bounded by one listing page: a ledger far past the
    /// window is collected all the way down.
    ///
    /// 250 rows against a 200-row page cap: 230 are outside the window, so
    /// the second page is what finishes the job.
    #[tokio::test]
    async fn a_ledger_deeper_than_one_page_is_pruned_in_full() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        for _ in 0..250 {
            row(&ctx, GenerationStatus::Superseded).await;
        }
        assert_eq!(prune(&ctx).await.expect("prune").len(), 230);
        assert_eq!(ledger(&ctx).await.len(), RETAINED_GENERATIONS);
    }

    /// The active generation is what the site *is*. Age cannot collect it, and
    /// neither the pass nor the retained set may lose sight of it.
    #[tokio::test]
    async fn the_serving_generation_survives_however_far_down_it_has_fallen() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let active = row(&ctx, GenerationStatus::Active).await;
        let staged = row(&ctx, GenerationStatus::Staged).await;
        for _ in 0..(RETAINED_GENERATIONS + 5) {
            row(&ctx, GenerationStatus::Failed).await;
        }

        let pruned = prune(&ctx).await.expect("prune");
        assert_eq!(pruned.len(), 5, "only the failed rows past the window");
        assert!(pruned.iter().all(|r| r.status == GenerationStatus::Failed));

        let ledger = ledger(&ctx).await;
        assert!(
            ledger.contains(&active.id),
            "the serving generation is kept"
        );
        assert!(ledger.contains(&staged.id), "so is the in-flight one");

        // And the collector's view of "retained" holds them too, even though
        // the window alone would not.
        let retained_ids: Vec<String> = retained(&ctx)
            .await
            .expect("retained")
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert!(retained_ids.contains(&active.id));
        assert!(retained_ids.contains(&staged.id));
        assert_eq!(retained_ids.len(), RETAINED_GENERATIONS + 2);
    }

    /// The serving generation is fetched on its own, so no listing page can
    /// come between it and the retained set.
    ///
    /// The rows piled on top of it are **in flight**, and that is the whole
    /// design of the fixture. A page cap can only hide a row from a listing it
    /// would otherwise have been in, and the only rows sharing a listing with
    /// the serving generation are the ones a single
    /// `status IN (active, staged, validating, activating)` query returns —
    /// superseded and failed rows were never in it, so piling those on proves
    /// nothing. So: 210 staged rows newer than the active one, against a
    /// 200-row page. Read that way the serving generation is the 211th and
    /// falls off the end, and the collector then deletes the blobs the site is
    /// being served from. Read as it is now — one targeted lookup for the row
    /// that is serving, a capped listing for the rest — it cannot.
    #[tokio::test]
    async fn the_serving_generation_survives_more_in_flight_rows_than_a_page_holds() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let active = row(&ctx, GenerationStatus::Active).await;
        // Ordinary history between them, so the pass below has work to do.
        for _ in 0..20 {
            row(&ctx, GenerationStatus::Superseded).await;
        }
        for _ in 0..210 {
            row(&ctx, GenerationStatus::Staged).await;
        }

        let retained_ids: Vec<String> = retained(&ctx)
            .await
            .expect("retained")
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert!(
            retained_ids.contains(&active.id),
            "the serving generation is retained past any page boundary",
        );
        // The window's 20 (all staged), plus the 200 the in-flight page holds
        // — 20 of which the window already had — plus the serving row.
        assert_eq!(retained_ids.len(), RETAINED_GENERATIONS + 180 + 1);

        // And the pass leaves it alone: only the superseded rows past the
        // window are retention's to delete.
        let pruned = prune(&ctx).await.expect("prune");
        assert_eq!(pruned.len(), 20);
        assert!(pruned
            .iter()
            .all(|row| row.status == GenerationStatus::Superseded));
        assert_eq!(
            generations::get(&ctx, &active.id)
                .await
                .expect("get")
                .status,
            GenerationStatus::Active,
        );
    }

    /// Retention deletes by age; it never rewrites how a generation ended.
    #[tokio::test]
    async fn a_failed_row_inside_the_window_keeps_its_status() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let failed = row(&ctx, GenerationStatus::Failed).await;
        for _ in 0..(RETAINED_GENERATIONS - 1) {
            row(&ctx, GenerationStatus::Superseded).await;
        }
        prune(&ctx).await.expect("prune");
        assert_eq!(
            generations::get(&ctx, &failed.id)
                .await
                .expect("get")
                .status,
            GenerationStatus::Failed,
        );
    }
}
