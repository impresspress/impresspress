//! Process-wide "config tables were written" counter.
//!
//! Exists because a runtime build both READS config and WRITES it, in that
//! order, within one pass. `builder::boot` initializes the admin block
//! first, under every `InitPolicy`; admin's `Init` runs its migrations and
//! then `settings::seed_defaults`, and the Cloudflare boot hook follows with
//! `platform_state::variables::seed_auto_generated`. Every one of those inserts rows into the
//! variables table AFTER some other block has already resolved its config —
//! the database service block, for one, is lazily initialized by admin's own
//! seeding query and declares a config key of its own.
//!
//! A config reader that caches what it read therefore cannot cache it for its
//! own lifetime: a value seeded halfway through the pass would stay invisible
//! to every block initialized after the cache was filled. For a key that is
//! required and has no default that is not a stale read, it is a permanent
//! `InitError` cached for the block slot's lifetime — the impresspress #209
//! regression class the `BootHooks` ordering exists to prevent.
//!
//! So writers bump this counter and readers compare it against the value they
//! captured. A build that seeds nothing (the overwhelmingly common case on an
//! established database) never bumps it and never re-reads.
//!
//! ## Why it is process-wide, not thread-local
//!
//! Every cache this counter guards is process-wide. `blocks::config`'s
//! `VariablesConfigBlock` is registered once and shared by every request it
//! serves, and it tags its memoized `variables` snapshot with the generation it
//! read at. Native serves requests on tokio's multi-threaded runtime
//! (`impresspress`'s `#[tokio::main]` over tokio "full", a task per connection,
//! work-stealing across workers), so the thread that performs an admin write is
//! routinely not the thread that next reads config.
//!
//! A per-thread counter therefore could not do this job: an admin's
//! `PATCH /b/admin/api/settings/{key}` bumped only the worker that handled it,
//! and every other worker compared its own untouched counter against the
//! generation stamped on the shared snapshot, found no change, and served the
//! pre-write value for the life of the process. The waste was the same defect
//! read the other way round: the tag on the snapshot is whichever worker
//! refilled it last, so once two workers disagreed about the count they
//! discarded and refetched each other's snapshot for as long as reads kept
//! alternating between them. Counters from different threads were compared as
//! if they were one.
//!
//! [`std::sync::atomic::AtomicU64`], never a `RefCell`: Cloudflare can
//! hard-stop a request without running destructors, and a stranded borrow flag
//! wedges the isolate for the rest of its life (see
//! `impresspress_core::isolate_cell`). An atomic has no borrow flag and no
//! guard to strand, so it satisfies that constraint exactly as `Cell` did. On
//! wasm32 an isolate is single-threaded, so the atomic is never contended and
//! the shared counter is simply the same counter every reader in that isolate
//! already compared against.

use std::sync::atomic::{AtomicU64, Ordering};

/// Bumped on every write to a table whose contents a runtime bakes in at
/// build/init time. Monotonic (wrapping) for the life of the process; the
/// absolute value is meaningless, only changes are.
static CONFIG_WRITE_GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    /// How many writes [`note_config_write`] recorded on THIS thread.
    ///
    /// Test-only, and not what any reader compares: it exists so a test can
    /// attribute writes to the code it just ran. The generation above is
    /// process-wide, so under `cargo test`'s parallel threads it moves for
    /// reasons a test asserting "this path wrote nothing" has no control over.
    static WRITES_NOTED_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Record that a config table was just written.
///
/// Deliberately NOT gated on whether the KV config-version stamp was also
/// bumped: the deploy-init funnel suppresses that stamp (one explicit bump
/// after ~19 sequential same-key puts) and the seeding it performs is exactly
/// the case this counter has to catch.
pub fn note_config_write() {
    // Belt and braces. What makes a reader's next query correct is the order
    // of the calls — every caller bumps AFTER its write has committed to the
    // database, and the reader's own query goes back to that database — not
    // anything this counter publishes. `Release`/`Acquire` costs nothing here
    // and means the day some caller does hang data off the generation, it
    // already sees the writer's stores rather than acquiring the ordering bug
    // as well.
    CONFIG_WRITE_GENERATION.fetch_add(1, Ordering::Release);
    #[cfg(test)]
    WRITES_NOTED_HERE.with(|n| n.set(n.get().wrapping_add(1)));
}

/// The current generation. A reader that cached config alongside a previous
/// value must re-read when this differs.
pub fn config_write_generation() -> u64 {
    CONFIG_WRITE_GENERATION.load(Ordering::Acquire)
}

/// How many writes [`note_config_write`] has recorded on the calling thread.
///
/// The assertion tool for "this code path wrote nothing" (or "wrote exactly
/// once"). A CURRENT-THREAD `#[tokio::test]` — the default flavour, and what
/// every caller of this uses — runs its whole body on one thread, so a delta
/// of zero here is a statement about the path under test rather than about
/// what every other test in the binary happened to be doing. A test that opts
/// into `flavor = "multi_thread"` spreads its work over workers and must
/// assert on what it observes instead, the way
/// `blocks::config::tests::an_admin_write_on_another_worker_reaches_a_warm_snapshot`
/// does.
#[cfg(test)]
pub(crate) fn writes_noted_on_this_thread() -> u64 {
    WRITES_NOTED_HERE.with(std::cell::Cell::get)
}

/// Whether a write to `table` can change what the config snapshot holds.
///
/// Narrower than [`crate::cache_key::bumps_config_version`] on purpose. That
/// predicate covers every table a runtime bakes in — variables,
/// block_settings and wrap_grants — because all three must move the KV
/// version stamp. The snapshot is built from the VARIABLES table alone, so
/// bumping it for the other two would discard a perfectly good snapshot:
/// every block writes its migration state to `block_settings` during its own
/// `Init` (`migration_helper::write_state`), so on a first boot after a code
/// change that is one discarded snapshot and one re-query per block — the
/// per-block read amplification this whole mechanism exists to remove,
/// reintroduced in the pass that can least afford it.
pub fn writes_invalidate_config_snapshot(table: &str) -> bool {
    table == crate::platform_state::variables::TABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_changes_the_generation_a_read_captured() {
        let noted = writes_noted_on_this_thread();
        let before = config_write_generation();
        assert_eq!(
            writes_noted_on_this_thread(),
            noted,
            "reading the generation must not itself record a config write"
        );

        note_config_write();
        assert_eq!(
            writes_noted_on_this_thread(),
            noted + 1,
            "a config write must be recorded once"
        );
        assert_ne!(
            config_write_generation(),
            before,
            "a config write must invalidate what a reader captured earlier"
        );

        let after_one = config_write_generation();
        note_config_write();
        assert_ne!(config_write_generation(), after_one);
    }

    /// A write on one thread has to be visible to a reader on another.
    ///
    /// The counter guards a process-wide snapshot: `blocks::config`'s block is
    /// registered once and read from every tokio worker, so a generation only
    /// the writing thread can see leaves every other worker convinced its cache
    /// is current. Two plain OS threads are enough to state that here; the
    /// end-to-end version — an admin write on one tokio worker, a config read
    /// on another — is
    /// `blocks::config::tests::an_admin_write_on_another_worker_reaches_a_warm_snapshot`.
    #[test]
    fn a_write_on_another_thread_is_visible_here() {
        let before = config_write_generation();
        std::thread::spawn(note_config_write)
            .join()
            .expect("the writing thread finishes");
        assert_ne!(
            config_write_generation(),
            before,
            "a write on another thread must move the generation this thread reads, \
             or a cache warmed here never learns the config store moved"
        );
    }

    /// Only the table the snapshot is BUILT from may invalidate it.
    ///
    /// `block_settings` is the one that matters: every block writes its
    /// migration state there during `Init`, so treating those writes as
    /// snapshot-invalidating costs one full re-query per block on a first
    /// boot — exactly the amplification the snapshot removes.
    #[test]
    fn only_variables_writes_invalidate_the_snapshot() {
        assert!(writes_invalidate_config_snapshot(
            crate::platform_state::variables::TABLE
        ));
        assert!(!writes_invalidate_config_snapshot(
            crate::platform_state::block_settings::TABLE
        ));
        assert!(!writes_invalidate_config_snapshot("unrelated_table"));

        // Every table this returns true for must also bump the KV version
        // stamp, or a runtime could rebuild from config no isolate is told
        // to re-read.
        assert!(crate::cache_key::bumps_config_version(
            crate::platform_state::variables::TABLE
        ));
    }
}
