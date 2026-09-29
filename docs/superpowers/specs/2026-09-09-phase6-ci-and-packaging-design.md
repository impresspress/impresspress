# Phase 6 — Continuous integration, packaging and the client library

**Date:** 2026-09-09
**Status:** Decided 2026-09-09. The eight rulings below are taken; they override the inventory that
follows wherever they differ, and they override `docs/CODE_REVIEW_2026-09-05.md` wherever they
contradict it — the review is four days and six phases stale, and the reconnaissance re-derived
every claim in this document from the tree. Pull requests execute in the order of ruling 6.7.
Sections 0–9 below are the reconnaissance and stay as the record; they are not re-litigated. Note
that the rulings are numbered 6.1–6.8 after the phase and the inventory's own §6 and §7 have
subsections of similar numbers, so ruling headings carry the word "Ruling" and a bare `§6.n` or
`§7.n` always means the inventory.
**Repos:** `impresspress` only, pull requests on the `Jsuppers` fork. No producer change — the
`wafer-run` pin does not move in this phase.
**Origin:** Phase 6 of `docs/CODE_REVIEW_2026-09-05.md` (§7, the phase-6 line); theme T9; bugs B17,
B27 and B28; and the review's "one `HttpClient`", "one service worker", "regenerate the SDK types
with a CI freshness check" and "single Rust test convention" lines.
**Base verified against:** `7540296d` ("Merge pull request #53 from Jsuppers/phase5/delete-gen1"),
which contains phases 0–5, against a clean tree. **Every line number in the review is stale**; each
number below was re-read from that tree. Run-history evidence comes from the fork
`Jsuppers/impresspress` (origin) and the frozen org repo `impresspress/impresspress` (upstream),
both queried on 2026-09-09.

**Method note.** Every line number and count in `docs/CODE_REVIEW_2026-09-05.md` was treated as
stale and re-derived. Where a claim no longer reproduces it is marked **CLOSED** with the evidence
that closes it. Six of the review's phase-6 items turn out to be already fixed, one of them (B17) by
the review's own phase-0 PR #10; two more are wrong about the repository rather than stale. A
finding that was fixed by other work is a different thing from a finding that was wrong, and only
the row tells you which — so nothing here is deleted, it is closed in place.

**Corrections since the reconnaissance.** The first pull request of ruling 6.7 re-verified the
reconnaissance against the tree and against the runs interface before landing this document. Where a
number moved, the claim carries an inline blockquote reading **Correction (PR 1 of ruling 6.7,
2026-09-09)** immediately below it rather than being silently patched, so a reader can see which
parts of the document were checked after the fact and which were not. Everything else stands as
written.

## Decisions taken

These are the rulings of 2026-09-09. They bind every phase 6 pull request and are not to be
re-litigated.

### Ruling 6.1 — Six items are CLOSED as already fixed, with evidence

Each is recorded here as closed **with the evidence that closed it**, rather than deleted. Every
citation below was re-checked against `7540296d` while landing this document.

- **B17, the release workflow.** Closed by the review's own phase-0 pull request #10
  (`fix/release-workflow-builds-wasm`, merged as `6c43e1b6`, commit `61a4823e`).
  `.github/workflows/release.yml:17-51` is now a `build-wasm` job: `jetli/wasm-pack-action` pinned
  to `0d096b08b4e5a7de8c28de67e11e945404e9eefa` at `v0.15.0` (`:36-41`), the `wasm-pack build` at
  `:45`, and `actions/upload-artifact` publishing `impresspress-web-pkg` at `:47-51`. The matrix
  `needs: build-wasm` (`:55`) and downloads that artifact into `crates/impresspress-web/pkg`
  (`:112-117`), and `:123` is `cargo build --locked …`. Only two parts of B17 still reproduce: the
  workflow **has never run** (0 runs on the fork, 0 upstream; the only tag in either repo is
  `compiler-807ace9e`, and no `v*` tag exists), and `RELEASE.md:235` still describes it as something
  that "will automatically" work. Those two residues are ruling 6.7's fifth pull request.
- **B27, committed browser-automation artifacts.** Closed before this run by upstream pull request
  #93 (`chore/baselines-out-of-mcp-scratch`, merged `f19d7ca0`, 2026-09-03), which deleted the
  tracked `.playwright-mcp/` dumps. `git ls-files .playwright-mcp` returns **0** tracked files; the
  directory is ignored at `.gitignore:24` under a comment at `:19-23` recording the history. The
  wholesale `git add` is gone: `regen-visual-baselines.yml:175-178` declares a **two-entry**
  `BASELINES` pathspec array, `:184-189` narrows it to the paths that actually drifted, and `:197`
  is the only staging line.
- **B28, the pre-commit hook.** Closed by phase-0 pull request #11
  (`fix/repo-hygiene-hooks-gitignore`, merged `19e4868f`). `.githooks/pre-commit:21` runs
  **`cargo +nightly fmt --all`**, not stable `cargo fmt`, and `:7-10` records the exact bug B28
  describes as already fixed; `rustfmt.toml`'s two options are nightly-only, which is why. The
  prettier lookup at `:35` points at `packages/impresspress-js/node_modules/.bin/prettier`, falls
  back to `command -v prettier` at `:37` and skips silently at `:39`. The package the review says
  the hook looks for, `packages/cloud-dashboard`, appears **nowhere in the repository but the review
  itself** (`git grep -l -a cloud-dashboard` → `docs/CODE_REVIEW_2026-09-05.md`).
- **The unignored worktree directory.** Ignored, at `.gitignore:63` (`.claude/worktrees/`), under a
  comment at `:61-62` explaining the size. `git ls-files .claude` returns 0 tracked files.
- **The 89-megabyte leftover package.** Never existed in this repository. `packages/solobase-site`
  has 0 tracked files, no ignore rule matches it, and the directory is absent; `packages/` holds 49
  tracked files, all under `impresspress-js/` and `impresspress-web/`. The 89 MB was local disk in
  the reviewer's own checkout.
- **Half of the client-types finding.** The roles type already mirrors its server view in the
  correct case convention: `packages/impresspress-js/src/types/generated/database.ts:185-197`
  declares `IAMRole` in **snake_case** against `AdminRoleView`, with a doc comment at `:180-184`
  naming the shape it replaced, and `src/services/iam.service.ts:9-14` + `:39-45` declare and unwrap
  the real `{records, total_count, page, page_size}` envelope. The other half of the finding — the
  33 remaining interfaces in that file — reproduces, and is ruling 6.7's sixth pull request.

**Two further claims are wrong by large margins rather than stale.**

- Shared-context adoption is **17 of 46**, not 2 of 23. `crates/impresspress-core/tests/` holds 46
  tracked `.rs` files; 17 name `TestContext` and 17 name `MigrationTestCtx` (one file,
  `tests/auth/common.rs`, is in both lists because it defines the second and mentions the first, so
  the split is 17 users of the shared context against 16 users of the holdout plus its definition).
- There are **331** inline test modules, not 168 — `#[cfg(test)]` immediately followed by a `mod`
  declaration, across 256 files; 372 if every `#[cfg(test)]` attribute is counted. Either way it is
  roughly twice the review's figure.

> **Correction (PR 1 of ruling 6.7, 2026-09-09).** The reconnaissance's §7 gives 337 inline modules
> across 254 files. Re-derived here at `7540296d`, the strict count — `#[cfg(test)]` on its own line
> immediately followed by `mod ` — is **331 across 256 files**, and the count of `#[cfg(test)]`
> attributes at any indent is **372**. The difference is a counting method, not a change in the
> tree, and it does not touch the ruling: both figures are about twice the review's 168.

### Ruling 6.2 — The fault injection phase 6 was meant to add ALREADY EXISTS

The shared test context already carries failing-operation contexts, read and write and list
breakers, a parked-read helper and a failing-put helper, used across dozens of files. Several were
added by this run. §7.2 lists each with its line and its seam. **Do not add them again.**

### Ruling 6.3 — The single test convention is OUT OF SCOPE

The reconnaissance found seven fixtures where folding into the shared context would delete the
assertion the fixture exists to make (§7.5): two mark every other method `unreachable!()`, two *are*
the assertion, and one cannot be replaced because the shared context hands the **inner** context to
nested blocks.

The fault injection half is done. The rest is a phase of its own, not a pull request at the end of
this one. It is recorded as a candidate in §9's "Deliberately not in this split", with that
reasoning.

### Ruling 6.4 — Ordering is load-bearing: parity BEFORE extraction

This is the most important ruling in the phase.

The main-branch workflow is missing five jobs and seven steps that the pull-request gate has (§1.2),
**including the capability-grant security audit** (`scripts/audit-wrap-grants.sh`), which runs on
pull requests only.

If the reusable-workflow extraction happens first, the natural parameterisation is the intersection
of the two, and those five jobs and seven steps quietly leave the pull-request gate as well. That is
silent coverage loss, and it lands on a security audit.

So: **make the two bodies equal first, then extract.** Not the other way round.

### Ruling 6.5 — Three pull requests can silently reduce coverage, and each must prove it did not

A workflow refactor that removes a check fails nothing. Name the evidence in each body:

- **The extraction** must diff the flattened job-and-step list, taken from the runs interface before
  and after, to empty.
- **The type freshness check** must not certify only the endpoints that are already described. A
  check over the described subset certifies exactly the endpoints that could not drift invisibly,
  which is precisely how a phase 2 pull request reshaped seven response bodies past both snapshot
  gates.
- **Dropping the dead dependency clones** must show both workflows green and an identical dependency
  resolution before and after.

### Ruling 6.6 — The dead clones are worse than the review says, and that changes their priority

All **24** of them are dead, not the two the review names — every one of those crates is pinned by
revision, and the file that would consume a clone is ignored.

One is not merely wasteful: the client-library job executes an install and build from an **unpinned
clone of a third-party default branch, on every merge**, for a dependency that no longer exists.
That is arbitrary third-party code running in a job with repository credentials. Treat its removal
as the priority within its pull request and say so plainly.

### Ruling 6.7 — Seven pull requests, in this order

Consolidating the reconnaissance's ten (§9), since several are small and share a subject. Where this
ruling and §9 differ, this ruling wins.

1. **Close the fixed findings and sweep the periphery.** Land this document with ruling 6.1's
   evidence, and remove the tracked files nothing references.
2. **Continuous integration hygiene.** Drop all 24 dead clones, and fix the two path filters that
   miss their own inputs. Ruling 6.5's evidence applies.
3. **Make the main-branch workflow match the pull-request gate.** Alone, because it restores a
   security audit and ruling 6.4 depends on it landing first.
4. **Extract the reusable workflows.** Alone, and the riskiest in the phase.
5. **The release workflow's residues, and delete the unused package.** Fix the documentation that
   describes an unrun workflow as working, and say plainly that it has never run rather than
   implying it works.
6. **One request client, and honest types.** Three clients today; uploads and downloads have no
   timeout and cannot be cancelled. Forty-five drifted or phantom type declarations.
7. **The type freshness check.** Ruling 6.5's second bullet is the whole point of this pull request;
   a check that certifies the already-described subset is worse than none, because it looks like
   coverage.

### Ruling 6.8 — The deploy workflow's consecutive failures are NOT in scope to fix

`.github/workflows/deploy-dev-sandbox.yml` fails at the final deployment step because **this fork
holds no `CLOUDFLARE_API_TOKEN`**, which is expected and not a defect. **Do not attempt to make it
pass, and do not delete it.** It is recorded here so a future reader does not mistake a wall of red
runs for a wall of broken builds.

The evidence, re-taken from the runs interface on 2026-09-09 while landing this document: the fork's
`deploy-dev-sandbox.yml` has **41 completed runs and not one success**, and the most recent **15**
are consecutive failures, one per merge to `main` from PR #38 through PR #53. In the latest of them
(run `34296258976`, the merge of PR #53) **every step succeeds** — checkout, toolchain, wasm-pack,
the wasm pre-build, the CLI install, the compiler-dist fetch, the sandbox build — and the single
failing step is `Deploy to Cloudflare Workers`. Each run costs about 14 minutes of runner time
(`00:44:09Z → 00:58:11Z`).

Adding the fork's Cloudflare secrets, or guarding the deploy step with
`if: github.repository == 'impresspress/impresspress'`, is a **user decision about the fork**, not a
code fix. It is flagged in §9 and left undecided.

> **Correction (PR 1 of ruling 6.7, 2026-09-09).** The reconnaissance's §1.6 says "12 of the last
> 12". The streak is **15** as of the merge of PR #53, and the lifetime record is 41 failures with 0
> successes. The direction of the finding is unchanged; only the counter moved, and it moves again
> with every merge.

## Coordination notes

Things the first pull request in this phase does that the reviewer of a later one has to expect.

- **Three tracked files were deleted by pull request 1 and none of them is a fixture.** They are
  `payment-link-state.jpeg` (48 325 bytes, a dropped screenshot added 2026-07-22 in `13f9e6bb`),
  `.intentionally-empty-file.o` (0 bytes, added in the initial commit `99c4e07c` — the name reads
  like a fixture and it is not one), and the root `package.json` stub. The proof for each is
  recorded in §8 items 4 and 5 and in that pull request's body: a binary-safe `git grep` over every
  tracked file for each name hits only `docs/CODE_REVIEW_2026-09-05.md:123`, **and** a
  `git log --all -S` over the entire history shows that the only commit which ever introduced either
  literal is `7936ffa3`, the commit that added that review. Nothing referenced them at any point,
  not merely today.
- **No continuous-integration path filter matches any of the three files, so pull request 1 runs no
  workflow.** That is the pre-existing filter behaviour, not something the pull request changed:
  `ci.yml`'s and `ci-main.yml`'s `paths:` lists cover `crates/**`, `examples/**`, `packages/**`,
  `Cargo.toml`, `Cargo.lock`, `scripts/**`, the workflow file itself and the experiment guest, and
  `docs/**` and repository-root files other than the two manifests appear in no filter. A reviewer
  should expect a checks list that is empty rather than green, and should not read it as a skipped
  gate.
- **The old review documents under `docs/` were deliberately left in place.** §9's second
  reconnaissance pull request proposed deleting `docs/CODE_REVIEW_2026-06-05_findings.json` and
  `docs/documentation_review_report.md` and adding superseded banners to two more. Ruling 6.7 item 1
  scopes to "the tracked files nothing references", and the findings file **is** referenced —
  `docs/CODE_REVIEW_2026-06-05_HANDOFF.md:10` names it as its own structured artifact. Deleting a
  record that another record cites is a different act from removing residue. Those items stay open;
  see §8 item 8.

---

## 0. Verdict summary

| Review item | Status now |
|---|---|
| Reusable `workflow_call` workflows | **Open.** 684 lines byte-identical across `ci.yml`/`ci-main.yml`, plus three hand copies of `build-wasm` and a fourth of the cross-compile matrix. |
| Fix `release.yml` (B17) | **Mostly CLOSED.** wasm-pack step and `--locked` both present. Two residues: the wasm itself is built without `--locked`, and nothing ties the tag to `Cargo.toml`'s version. Still never run; `RELEASE.md` still documents it as working. |
| Delete the wafer-client-js steps | **Open, and bigger than described.** The SDK's `wafer-client-js` dependency is gone, but *all 24* `git clone` steps across six workflows are dead — nothing in CI reads `../wafer-run`. The SDK job additionally *executes* code from that unpinned clone. |
| One service worker | **Open, and cheaper than described.** Two implementations; the npm one has zero consumers, is untested, and does not build. |
| Regenerate SDK types with a CI freshness check | see §5 |
| One `HttpClient` | see §6 |
| Single Rust test convention | see §7 |
| B27 `.playwright-mcp/` `git add` | **CLOSED** — fixed by PR #93 (2026-09-03), before this run started. |
| B28 pre-commit hook | **CLOSED** — hook runs `cargo +nightly fmt --all`; the prettier path is correct. |
| T9 `.claude/worktrees/` unignored | **CLOSED** — `.gitignore:63`. |
| T9 `packages/solobase-site/` | **CLOSED** — does not exist in the repo (it was local disk in the reviewer's checkout). |
| T9 root `package.json` stub, `payment-link-state.jpeg`, `.intentionally-empty-file.o` | **Reproduces.** |
| T9 `NICE_TO_HAVE.md` | **Reproduces, worse:** 5 of 7 bullets stale, not 4. |

> **Correction (PR 1 of ruling 6.7, 2026-09-09).** The `package.json` stub, `payment-link-state.jpeg`
> and `.intentionally-empty-file.o` rows are now **CLOSED — deleted**. All three were removed by the
> first pull request of ruling 6.7, each with the proof recorded in §8 items 4 and 5: a binary-safe
> `git grep` over every tracked file, plus a `git log --all -S` over the whole history, which shows
> the only commit that ever introduced either filename literal is `7936ffa3` — the commit that added
> `docs/CODE_REVIEW_2026-09-05.md` itself. The `NICE_TO_HAVE.md` row is untouched and still open.

---

## 1. The workflows as they are now

Six workflow files, 3 090 lines.

| File | Lines | Trigger | Jobs |
|---|---|---|---|
| `.github/workflows/ci.yml` | 1 447 | `pull_request` → `main`, path-filtered | 17 |
| `.github/workflows/ci-main.yml` | 1 091 | `push` → `main`, path-filtered | 14 |
| `.github/workflows/release.yml` | 167 | `push` tag `v*` | 3 (`build-wasm`, `build` ×5 matrix, `release`) |
| `.github/workflows/deploy-demo.yml` | 63 | `push` → `main` (3 paths) + dispatch | 1 |
| `.github/workflows/deploy-dev-sandbox.yml` | 109 | `push` → `main` (8 paths) + dispatch | 1 |
| `.github/workflows/regen-visual-baselines.yml` | 213 | `workflow_dispatch` only | 1 |

No workflow uses `workflow_call` today; there are no composite actions.

### 1.1 What each does

- **`ci.yml`** is the PR gate: build the web wasm once and share it as an
  artifact, then format/lint, host tests, a postgres-feature test lane, a
  wasm32 compile lane, two wasm-target test lanes, an e2e build that produces
  the CLI binary + bundled site as artifacts, three browser e2e lanes, the
  visual-baseline lane, three products lanes, `cargo audit`, and the SDK.
- **`ci-main.yml`** is the post-merge gate: the same shape minus five jobs,
  plus a 5-target cross-compile matrix and an `llvm-cov` coverage upload.
- **`release.yml`** builds the wasm, cross-compiles five targets, packages
  tar.gz/zip and creates a GitHub Release with `--generate-notes`.
- **`deploy-demo.yml`** builds the CLI, runs `impresspress build --target web
  --release`, and pushes `crates/impresspress-web/pkg/` to
  `impresspress/impresspress-demo`'s `gh-pages` (demo.impresspress.org).
- **`deploy-dev-sandbox.yml`** builds the CLI, fetches the pre-published
  compiler dist, runs `examples/dev-sandbox/build.sh`, and `wrangler deploy`s
  to Cloudflare (dev.impresspress.org).
- **`regen-visual-baselines.yml`** checks out a named branch, boots the native
  server, re-runs the visual suites with `--update-snapshots`, and pushes the
  changed PNGs back to that branch.

### 1.2 Job-level asymmetry between the two CI workflows

`ci.yml` job ids: `build-wasm, check, test, test-postgres, cloudflare,
cloudflare-wasm-test, browser-wasm-test, e2e-build, e2e-smoke, e2e-dev-sandbox,
e2e-dev-compile, e2e-visual, products-browser, product-examples,
products-postgres, audit, sdk`.

`ci-main.yml` job ids: `build-wasm, check, test, test-postgres, wasm,
e2e-dev-sandbox, e2e-dev-compile, products-browser, product-examples,
products-postgres, audit, sdk, cross-compile, coverage`.
(`ci-main`'s `wasm` is `ci`'s `cloudflare` under a different id — same
`name: Cloudflare wasm32 check`.)

**Only in `ci.yml` (never runs on a merge to `main`):**
`cloudflare-wasm-test`, `browser-wasm-test`, `e2e-build`, `e2e-smoke`,
`e2e-visual`.

**Only in `ci-main.yml` (never runs on a PR):** `cross-compile`, `coverage`.

**Steps present in `ci.yml`'s shared jobs and missing from `ci-main.yml`'s:**

| Step | `ci.yml` | What it gates |
|---|---|---|
| `Lint — no full-page html! outside ui/` | `:79-80` (`scripts/grep-guard-html.sh`) | UI ownership rule |
| `Lint — WRAP-grant coverage on cross-block db::* callsites` | `:82-83` (`scripts/audit-wrap-grants.sh`) | **security**: the gate whose parser bug PR #19 fixed |
| `Check impresspress + impresspress-core (no default features)` | `:120-123` | the `--no-default-features --features sqlite` build |
| `Test the vector block without the llm backend` | `:133-143` region | `blocks::vector` without `llm` |
| `Run webmcp.js unit tests (node --test)` | `:224-225` | `webmcp.js` composition |
| `Check impresspress-cloudflare tests (wasm32)` | `:305-306` | wasm test-code compile |
| `Check impresspress-cloudflare optional blocks (wasm32)` | `:322-325` | `block-llm` / `block-vector` on wasm32 |

That is seven steps and five jobs of coverage that a merge to `main` does not
have. The WRAP-grant audit is the one that matters most: it is a security gate,
and it is PR-only.

### 1.3 Byte-level duplication (measured)

Extracting each job block and diffing:

| Job | `ci.yml` lines | `ci-main.yml` lines | differing lines |
|---|---|---|---|
| `build-wasm` | 41 | 41 | **0** |
| `test-postgres` | 22 | 22 | **0** |
| `e2e-dev-sandbox` | 207 | 207 | **0** |
| `e2e-dev-compile` | 253 | 253 | **0** |
| `products-browser` | 39 | 39 | **0** |
| `product-examples` | 41 | 41 | **0** |
| `products-postgres` | 57 | 57 | **0** |
| `audit` | 24 | 24 | **0** |
| `check` | 73 | 46 | 27 |
| `test` | 101 | 78 | 23 |
| `cloudflare` / `wasm` | 118 | 115 | 121 (comments + 2 steps) |
| `sdk` | 52 | 35 | 19 |

**684 lines are byte-identical across the two files.** A further 344/274 lines
are the same jobs with the divergences in §1.2.

Cross-file duplication beyond the two CI files:

- `build-wasm` exists **three times**: `ci.yml:31-71`, `ci-main.yml:28-68`
  (identical) and `release.yml:17-51` (same substance, 35 lines, different
  comments).
- The 5-target cross-compile matrix exists **twice**: `ci-main.yml:986-1053`
  and `release.yml:53-152` (release adds packaging + upload).
- `wasm-pack build --target web --release --out-dir pkg` appears **six times**
  (`ci.yml:63`, `ci-main.yml:60`, `release.yml:45`, `deploy-demo.yml:38`,
  `deploy-dev-sandbox.yml:60`, `regen-visual-baselines.yml:92`), none with
  `--locked`.
- The `jetli/wasm-pack-action` pin plus its 8-line explanatory comment appears
  in five files.
- The native-server boot block is duplicated: `ci.yml:1170-1198` vs
  `regen-visual-baselines.yml:107-128` — same env, same port 8093, same DB
  path, same 60 s curl loop; only the comments differ.
- The `Pre-build wasm → cargo install --path crates/impresspress → impresspress
  build --target web --release` triple appears in `deploy-demo.yml:36-48`,
  `deploy-dev-sandbox.yml:58-63`, `regen-visual-baselines.yml:90-99` and (as an
  artifact-download variant) `ci.yml`'s `e2e-build`.
- `git clone --depth 1 https://github.com/wafer-run/wafer-run.git` appears **24
  times** across all six files (see §3).

### 1.4 What a `workflow_call` refactor would actually collapse

Realistically collapsible:

1. **`build-wasm` → one reusable workflow with an `upload-artifact` output.**
   Removes 2 of 3 copies (~76 lines) and makes the release path use the same
   builder CI proved.
2. **The eight byte-identical jobs → one reusable `ci-shared.yml` called by
   both `ci.yml` and `ci-main.yml`** with inputs for the PR-only skip gates.
   Removes ~684 lines from `ci-main.yml`.
3. **The cross-compile matrix → one reusable workflow** called by `ci-main.yml`
   (build only) and `release.yml` (build + package). Removes ~68 lines and
   makes `release.yml`'s never-executed matrix share a code path that runs on
   every merge — which is the single highest-value change in this phase,
   because it converts "never tested" into "tested on every merge".
4. **The native-server boot block → a composite action** used by `e2e-visual`
   and `regen-visual-baselines`. ~25 lines, but more importantly it stops the
   two from drifting on env or port.
5. **The wasm-pack install pin → a composite action.** ~45 lines across five
   files, and one place to bump the pin.

Not collapsible without changing behaviour:

- The `sdk` / `browser-wasm-test` / `cloudflare-wasm-test` skip gates use
  `git diff … ${{ github.event.pull_request.base.sha }}..HEAD`, which does not
  exist on a `push` event. Any reusable job must take "should I skip?" as an
  **input** computed by the caller, or run unconditionally on the push side.
- `deploy-demo` / `deploy-dev-sandbox` / `regen` share a *prefix* with
  `e2e-build` but diverge immediately after (gh-pages push, wrangler deploy,
  `--update-snapshots`). Only the prefix is worth extracting.

**⚠ Coverage risk — the failure mode that matters here.** The divergences in
§1.2 are *not* all accidental. A refactor that parameterises the shared jobs
down to their intersection silently deletes seven steps and five jobs from the
PR gate, including the WRAP-grant security audit. The refactor must go the other
way: define the reusable job as `ci.yml`'s (larger) body and let `ci-main.yml`
gain the missing steps. Evidence that it did: the job-name and step-name list
produced by `gh api …/jobs` for a PR run before and after must be a superset,
never a subset, and `ci-main`'s must grow by exactly the seven steps and the
jobs listed in §1.2.

### 1.5 Path filters that miss their own dependencies

`ci.yml` paths: `crates/**`, `examples/**`, `packages/**`, `Cargo.toml`,
`Cargo.lock`, `scripts/**`, `.github/workflows/ci.yml`,
`experiments/browser-service-worker-blocks/guest/**`.

`ci-main.yml` paths: the same **minus `scripts/**`**.

- **`ci-main.yml` runs `bash scripts/check-audit-expiry.sh` at `:937` but does
  not list `scripts/**`.** A change confined to that script (or to
  `audit-wrap-grants.sh`, which `ci-main` does not run at all) skips `ci-main`
  entirely.
- **Neither filter lists `.cargo/**`.** `.cargo/audit.toml` holds the audit
  ignore list with owner+expiry entries, and `check-audit-expiry.sh` reads it.
  Editing an ignore's expiry does not run the audit that consumes it.
- **Neither filter lists `rustfmt.toml`** (2 nightly-only options) or the
  `justfile`. A change to formatting rules runs no formatter.
- **`deploy-demo.yml` is the worst offender.** Its filter is
  `crates/impresspress-web/**`, `crates/impresspress-core/**`, and its own file
  — but the job runs `cargo install --path crates/impresspress --locked`
  (`:41`) and `impresspress build --target web --release` (`:48`), so it builds
  from `crates/impresspress/**`, `crates/impresspress-bundle/**`,
  `crates/impresspress-browser/**`, `Cargo.toml` and `Cargo.lock`. None are in
  the filter. `deploy-dev-sandbox.yml:9-25` lists exactly these and carries a
  comment explaining why ("A path missing here is not a skipped run — it is
  dev.impresspress.org silently left on stale code"). `deploy-demo.yml` never
  got the same treatment, so demo.impresspress.org can silently serve a stale
  bundle after a CLI or bundler change.

### 1.6 Jobs and specs that never run, or cannot pass

- **`release.yml` has never run.** 0 runs on the fork (workflow id 350845661)
  and `total_count: 0` on upstream (id 312223969). The only tag in either repo
  is `compiler-807ace9e`; no `v*` tag exists. Upstream has one Release
  (`compiler-807ace9e`, 2026-09-02, 1 asset) created outside this workflow.
- **`deploy-dev-sandbox.yml` fails on every push to the fork's `main`.** 12 of
  the last 12 runs are `failure`, each ~13 minutes. Every step succeeds except
  the final `Deploy to Cloudflare Workers` — the fork holds no
  `CLOUDFLARE_API_TOKEN`. This is ~13 minutes of runner time burned per merge
  to produce a red X that means nothing. Either add the secrets to the fork or
  guard the deploy step with `if: github.repository == 'impresspress/impresspress'`.

  > **Correction (PR 1 of ruling 6.7, 2026-09-09).** Re-taken from the runs interface: the streak is
  > **15**, not 12 — one failure per merge to `main` from PR #38 through PR #53 — and across the
  > workflow's whole life the fork has **41 completed runs and 0 successes**. In the newest of them
  > (run `34296258976`) every step succeeds and only `Deploy to Cloudflare Workers` fails, at about
  > 14 minutes of runner time per run. **Ruling 6.8 forbids fixing or deleting this workflow**; it is
  > recorded so a reader does not mistake the red runs for broken builds.
- **`crates/impresspress-web/tests/e2e/sw-update.spec.ts` cannot pass.** It
  `execSync`s `` `touch crates/impresspress-web/src/lib.rs && cd
  crates/impresspress-web && make build` `` at `:29` and `` `cd
  crates/impresspress-web && make build` `` at `:55`. **There is no `Makefile`
  anywhere in the repository** (`find -iname Makefile` → nothing;
  `crates/impresspress-web/` holds only `Cargo.toml`, `impresspress.toml`,
  `package.json`, `js/`, `src/`, `tests/`). No workflow runs this spec, so
  nothing has ever noticed. `crates/impresspress-web/package.json`'s documented
  `npm run e2e` script runs the whole `tests/e2e` directory and therefore fails
  on it. The equivalent recipe today is `wasm-pack build --target web --release
  --out-dir pkg && impresspress build --target web --release`.
- **`crates/impresspress-web/tests/e2e/vector.spec.ts` is run by no workflow.**
- **Four `examples/tests/*.spec.ts` are run by no workflow**: `blog.spec.ts`,
  `blog-web.spec.ts`, `dropship.spec.ts`, `saas.spec.ts`. CI's
  `product-examples` job only runs `npm run test:products`
  (`products.playwright.config.ts` → `products-examples.spec.ts`); the
  `npm test` → `./run-tests.sh` path that runs the other four needs a real
  server and is never invoked.

### 1.7 Other current-state facts worth recording

- **`impresspress-cloudflare` is clippy'd on no target.** Workspace clippy
  excludes it (`ci.yml:113`, `ci-main.yml:104`). The only wasm32 clippy is
  `cargo clippy -p impresspress-web --target wasm32-unknown-unknown --features
  browser-devtools --no-deps -- -D warnings` (`ci.yml:723`, `ci-main.yml:377`).
  So the carry-forward note "still no CI job runs `clippy --target
  wasm32-unknown-unknown`" is now **half wrong** — `impresspress-web` is
  covered; `impresspress-cloudflare` and `impresspress-browser` are not.
- **Node versions are unpinned in aggregate**: 16 `node-version` declarations
  split 20/22 with no central value (`ci.yml` ×8, `ci-main.yml` ×6,
  `deploy-dev-sandbox.yml` 22, `regen` 20). GitHub now annotates every run with
  *"Node.js 20 is deprecated … being forced to run on Node.js 24"* for
  `actions/checkout@v4`, `actions/setup-node@v4`, `wrangler-action` and
  `wasm-pack-action`.
- **`regen-visual-baselines.yml:104` uses `npm install` where `ci.yml:1165`
  uses `npm ci`** — the review's claim, still reproduces. The regen workflow is
  the one whose output is committed, so it is the one that most needs the
  lockfile.
- **`just build` and CI build different wasm.** `justfile:7` sets
  `RUSTFLAGS="-C target-feature=+simd128"`; no CI or deploy job sets it, and
  `Cargo.toml:102-110` records the deliberate decision *not* to thread it
  through CI. So the documented local build produces a different artefact from
  every published one. Either drop it from the `justfile` or thread it, but do
  not leave the two disagreeing silently.
- **Cargo invocations without `--locked`**: `cargo clippy -p impresspress-web
  --target wasm32-unknown-unknown` (`ci.yml:723`, `ci-main.yml:377`), `cargo
  build --release --target wasm32-wasip1 --manifest-path
  experiments/.../guest/Cargo.toml` (`ci.yml:734`, `ci-main.yml:388`), `cargo
  install cargo-audit` (`ci.yml:1388`, `ci-main.yml:943`), and all six
  `wasm-pack build` calls.

---

## 2. `release.yml` specifically

The review's B17 says: no wasm-pack step (`crates/impresspress/build.rs`
exits 1 without it), no `--locked`, never run, documented as working.

**Re-derived, claim by claim:**

| B17 claim | Status |
|---|---|
| "No wasm-pack step" | **CLOSED.** `release.yml:17-51` is a `build-wasm` job: pinned `jetli/wasm-pack-action@…v0.15.0` (`:36-41`), `wasm-pack build --target web --release --out-dir pkg` (`:45`), `upload-artifact` as `impresspress-web-pkg` (`:47-51`). The build matrix `needs: build-wasm` (`:55`) and downloads it into `crates/impresspress-web/pkg` (`:112-117`). The header comment at `:12-16` names `build.rs` and the exit-1 behaviour explicitly. |
| "no `--locked`" | **CLOSED for the binary.** `release.yml:123` is `cargo build --locked -p impresspress --release --target ${{ matrix.target }}`, with a comment at `:120-121` giving the reason. |
| "never run" | **REPRODUCES.** 0 runs on the fork; `total_count: 0` upstream. No `v*` tag has ever existed in either repo. |
| "`RELEASE.md` documents it as working" | **REPRODUCES.** `RELEASE.md:235-237` — "The Release workflow will automatically: 1. Build binaries for all 5 platforms … 2. Create a GitHub Release". `:221-233` gives the `git tag v0.2.0 && git push origin v0.2.0` recipe. |

This is a phase-0 closure: the review's own PR table lists `#10 | B17 release
workflow builds the wasm first and uses --locked | fix/release-workflow-builds-wasm`,
and that PR merged. **B17 should be closed with this evidence, not carried.**

**Residues that are genuinely open:**

1. **The release wasm is not built from the locked dependency set.**
   `release.yml:45`'s `wasm-pack build` passes no `-- --locked`. The binary
   resolves the lockfile; the wasm blob baked into it does not. Same defect in
   all six `wasm-pack build` sites.
2. **Nothing checks the tag against `Cargo.toml`'s version.** Workspace
   `version = "0.1.0"` (`Cargo.toml:32`) while `RELEASE.md:229` demonstrates
   `v0.2.0`. `RELEASE.md:209` makes updating the version a manual checklist
   item. A tag/version mismatch produces a Release whose binaries report a
   different version, with no gate.
3. **The matrix has never executed even though an equivalent one runs every
   merge.** `ci-main.yml:986-1053` (`cross-compile`) builds the same five
   targets with the same `download-artifact` + `cargo build --locked -p
   impresspress --release --target …` shape and passes on every merge. The
   untested part of `release.yml` is therefore only the *packaging* steps
   (`:125-141` tar/zip) and the `gh release create` step (`:154-167`). That is
   the argument for §1.4's item 3: make `release.yml`'s build job call the same
   reusable workflow `ci-main` calls, so the only untested surface left is
   packaging.
4. **`release.yml` clones wafer-run twice** (`:24`, `:89`) for nothing (§3).
5. **`gh release create … --generate-notes`** has no `--verify-tag` and no
   draft/verify step; a mis-typed tag publishes immediately.

**How to prove a fix works without pushing a real tag:** add
`workflow_dispatch` with a `dry_run` input that runs everything up to but not
including `gh release create`, and run it once. That is the only evidence short
of an actual release, and it is worth having permanently.

---

## 3. The steps that clone a dependency the client library no longer needs

**The dependency really is gone.** `packages/impresspress-js/package.json` has
no `dependencies` block at all — only `devDependencies` (`@types/node`,
`typescript-eslint`, `eslint`, `jsdom`, `prettier`, `tsup`, `typescript`,
`vitest`). `packages/impresspress-js/src/http-client.ts:3-8` records the
removal: *"This SDK previously depended on `wafer-client-js` via a local … path"*.
`packages/impresspress-js/scripts/smoke-pack-install.sh:4-6` exists precisely to
prove no such `file:` dependency comes back.

**The steps that build it are therefore dead:**

- `ci.yml:1421-1428` — `Checkout wafer-run (for local dependency)` and
  `Build wafer-client-js (file: dep needs dist for tsc)` running `npm ci &&
  npm run build` in `../wafer-run/packages/wafer-client-js`.
- `ci-main.yml:962-969` — the same two steps, without the PR skip gate.

**But the finding is larger than the review states: every `git clone` of
wafer-run in every workflow is dead.**

- Root `Cargo.toml:38-55` declares all 18 wafer crates as
  `{ git = "https://github.com/wafer-run/wafer-run", rev = "c55c9320…" }`.
  There is no `path` dependency on `../wafer-run` in any manifest in the
  workspace, and `experiments/browser-service-worker-blocks/verify/Cargo.toml:9`
  is likewise a `git`+`rev` dependency.
- The only mechanism that could redirect those to the clone is a `[patch]` in
  `.cargo/config.toml`, which is **gitignored** (`.gitignore:3`); only
  `.cargo/config.toml.example` is tracked, and no workflow copies it into place
  (`grep -n "config.toml" .github/workflows/*.yml` finds only release.yml's
  two `~/.cargo/config.toml` linker appends).
- So the cloned tree is never read by cargo. **24 clone sites**: `ci.yml` ×9
  (`:38, 86, 153, 253, 276, 542, 689, 915, 1422`), `ci-main.yml` ×10
  (`:35, 77, 123, 200, 223, 343, 569, 963, 1016, 1062`), `release.yml` ×2
  (`:24, 89`), `deploy-demo.yml:19`, `deploy-dev-sandbox.yml:39`,
  `regen-visual-baselines.yml:66`.

**What they cost.** Measured on real runs:

- Wall clock is small: 1–3 s per clone. On PR run `34294496117` the eight
  executed clones cost **13 s total**; on ci-main run `34282921589` the fifteen
  clones plus the `wafer-client-js` build cost **17 s total** (4 s of that being
  `npm ci && npm run build` of wafer-client-js).
- **The real cost is supply chain, not time.** Every clone is
  `--depth 1` off wafer-run's *default branch*, unpinned — while the workspace
  deliberately pins `rev = c55c9320…`. In 22 of the 24 sites the tree is only
  written to disk and never executed. In the SDK job it is **executed**: `npm
  ci && npm run build` in `../wafer-run/packages/wafer-client-js` runs that
  repository's install and build scripts at whatever its default branch happens
  to be, inside a runner that also holds the checkout. On `ci-main.yml` that
  job has no skip gate, so it runs on **every merge to `main`** — to build a
  package nothing consumes.

**Closing it touches:** 24 step deletions across six files, plus the two
`wafer-client-js` build steps. No other change. Evidence it did not reduce
coverage: a full CI run green with the steps removed, and `cargo tree -p
impresspress | grep wafer` unchanged (still resolving the pinned rev, since
that is where it always resolved).

---

## 4. Service workers

**Two implementations exist**, plus one 9-line fixture, one 3-line test stub
and one gitignored rendered copy.

### 4.1 `crates/impresspress-bundle/assets/sw.js.tmpl` — 302 lines. Production.

- Rendered by `crates/impresspress-bundle/src/bundle/mod.rs:239`
  (`render_if_exists(pkg_dir, "sw.js.tmpl", "sw.js", &vars)`); vars built at
  `mod.rs:330-418`. Seven placeholders (`__BUILD_ID__`, `__WASM_JS__`,
  `__WASM_JS_PREFIX__`, `__APP_NAME__`, `__DEV_ENABLED__`, `__EXTRA_BYPASS__`,
  `__EXTRA_BYPASS_EXACT__`). Embedded via `crates/impresspress-bundle/src/assets.rs:30-31`.
- Registered by `crates/impresspress-bundle/assets/loader.js.tmpl:124`.
- **Deny-list intercept** (`:145-240`): passes cross-origin through, bypasses a
  fixed set of asset paths plus per-app `--extra-bypass-prefix`, and
  **intercepts `/` and `/index.html`**. Everything else goes to wasm.
- `skipWaiting()` unconditionally in `install` (`:86`); `clients.claim()` (`:91`).
- Real recovery: poison flag, `registration.unregister()`,
  `{type:'sw-self-destruct'}` to every client, `client.navigate()`, plain-`fetch`
  fallback (`:23-58`, `:276-302`); `loader.js.tmpl:114-118` sets a
  `sessionStorage` breaker and the next boot wipes caches/SW/OPFS.
- Message bridge for `load-asset-response`, `llm-*`, `embed-*-response`,
  `image-*` (`:98-139`), and under `DEV_ENABLED` a `passthrough()` that
  re-serves bypassed responses with `COEP: credentialless` / `COOP: same-origin`
  (`:234-274`) — the cross-origin isolation the dev sandbox depends on.
- **Heavily gated.** ~30 textual assertions in
  `crates/impresspress-bundle/tests/bundle_integration.rs`;
  `crates/impresspress-core/tests/dev_export.rs:317-618`; live e2e in
  `crates/impresspress-web/tests/e2e/smoke.spec.ts:9-33` (run at `ci.yml:651`).
  Its rendered text is **rewritten by exact string match** at
  `crates/impresspress-core/src/blocks/dev/export.rs:437-480`, which requires
  exactly one occurrence of `const DEV_ENABLED = true;`.

### 4.2 `packages/impresspress-web/src/worker.ts` — 104 lines. The npm package.

- Registered from `packages/impresspress-web/src/index.ts:25-28` and
  `src/update.ts:18-22`. **Nothing in this repository calls either**, except the
  unused fixture `packages/impresspress-web/test-fixtures/vite-app/src/main.ts:3`.
- **Allow-list intercept** (`:98-103`, `:58-67`): default routes `['/b/',
  '/health', '/openapi.json', '/.well-known/agent.json']` (`:8`), replaceable at
  runtime by an `{type:'impresspress:config', routes}` postMessage. `/` passes
  through.
- **No `skipWaiting` in install** (`:76-82`, with a comment naming the other
  SW); only on an explicit `{type:'skip-waiting'}` message (`:88-92`).
  `clients.claim()` on activate (`:85`).
- **No recovery**: a failed init returns `503 "Impresspress not initialized"`
  (`:43-45`); it only clears `initPromise` so the next request retries.
- **Zero CI references.** No `test` script despite `vitest ^4.1.4` in
  devDependencies and a 103-line `src/update.test.ts`. `dist/` is gitignored
  (`.gitignore:7`), so the published package is unreproducible.
- **It does not build.** `worker.ts:4` imports `./wasm/impresspress_web.js`, but
  `build:wasm` emits into `dist/wasm` while `tsconfig.json` sets
  `rootDir: "src"` / `include: ["src"]` — `tsc` cannot resolve it (TS2307).
  And `src/update.test.ts` is inside `include`, so a successful build would
  publish `dist/update.test.js` under `files: ["dist/"]`.
- **It cannot enable the dev sandbox.** It calls `wasmInitialize()` with no
  argument; `crates/impresspress-web/src/lib.rs:52` takes a required `JsValue`,
  so `Reflect::get` errors and `dev` silently resolves `false`.

### 4.3 Are they genuinely different?

They are two configurations of the same job — run the same
`impresspress_web` wasm in a service worker — shaped for opposite deployments:
#1 owns the whole origin (self-hosted bundle, demo, dev sandbox); #2 is a
library meant to claim only API routes on someone else's site. The behavioural
differences are real and enumerated above: deny-list vs allow-list, build-time
vs runtime route config, unconditional vs message-gated `skipWaiting`,
self-destruct recovery vs bare 503, hashed vs fixed wasm module URL, bridges and
COEP passthrough present vs absent, build-id-driven update vs none.

### 4.4 Is consolidating safe?

**Yes — because #2 has no consumers.** Deleting `packages/impresspress-web`
breaks only its own fixture. The one non-code consideration is that
`impresspress-web@0.2.0` is a published npm name, so removal is a deprecation
decision.

Merging *into* #1 is the direction that would break things, and it would touch:
`export.rs:61-480`'s exact-string rewriter and `SW_PATH`, ~30 assertions in
`bundle_integration.rs` (including `sw.contains("if (DEV_ENABLED && url.pathname
!== '/sw.js') {")` at `:315` and `:359`), `dev_export.rs:338-345`,
`blocks/dev/test_support.rs:359-382`'s `FAKE_SW_JS`, the seven placeholder names
and `build_template_vars`, and `loader.js.tmpl`'s registration +
`sw-self-destruct` contract.

**Recommendation:** delete `packages/impresspress-web` (or reduce it to a thin
registration helper around the bundler-rendered `sw.js`) and keep `sw.js.tmpl`
as the single implementation. Making #1 serve #2's use case would additionally
need an allow-list mode and a non-hashed module path, neither of which exists.

**⚠ Coverage note:** deleting the package removes `src/update.test.ts`, which
runs in no CI job today, so nothing measurable is lost — but say so explicitly
in the PR rather than letting it vanish.

---

## 5. The client library's types

The client library is `packages/impresspress-js` (npm `@impresspress/sdk` v1.0.0),
17 source files, 2 616 TS lines, **zero runtime dependencies**.

### 5.1 How the types are produced today

**There is no generator.** `src/types/generated/database.ts:1-3` says so in its
own header: *"Hand-maintained database row types (the solobase-era Go generator
no longer exists). Keep in sync with the Rust schema sources: each block's
migrations/\*.sql and, for products, contracts.rs."* A repo-wide search for
`openapi-typescript`, `quicktype`, `ts-rs`, `typeshare` or `codegen` across
`crates/`, `scripts/`, `justfile` and `.github/` returns nothing. The directory
name `generated/` is a lie, and `README.md:50` still says the types are
"generated from the backend models", contradicting the file's own first line.

**There is, however, a real schema authority — two of them, both already
committed:**

- **Live route:** `crates/impresspress-core/src/pipeline.rs:216-241` serves
  `/openapi.json` (and `/.well-known/agent.json`) from
  `wafer_core::discovery::generate_openapi(&visible_infos, …)`, where
  `visible_infos` is tier-filtered per caller (`pipeline.rs:104-133`) — an
  anonymous caller sees only the Public subset.
- **Committed snapshots:** `crates/impresspress-core/tests/snapshots/` holds
  24 files — ten `*.openapi.json` (admin, auth_ui, dev, files, legalpages, llm,
  messages, products, tickets, vector) produced by
  `crates/impresspress-core/tests/openapi_snapshot.rs` (block list at `:28-38`,
  writer at `:84-131`, regen with `UPDATE_OPENAPI_SNAPSHOTS=1`), and fourteen
  `*.endpoints.json` produced by `crates/impresspress-core/tests/endpoint_surface.rs`.
  `endpoint_surface.rs:1-15` explains the split: the OpenAPI snapshot only
  carries endpoints where `BlockEndpoint::has_schema()` is true; the endpoints
  file carries every declared row.

Schemas themselves come from `schemars` over each block's `contracts.rs`, wired
in via `.query_params(request_schema_of::<T>)` / `.output(response_schema_of::<T>)`
on `EndpointRoute` (e.g. `blocks/admin/mod.rs:139-145`).

### 5.2 How far the types have drifted

**45 drifted or phantom type declarations.**

*33 of the 34 interfaces in `src/types/generated/database.ts`* (590 lines) are
wrong. The file carries 229 camelCase fields against a server that serialises
snake_case everywhere; only `IAMRole` (`:185-197`) has been corrected. Worked
examples:

- `AuthUser` (`:47-62`) declares `username, confirmed, firstName, lastName,
  displayName, phone, location, metadata, lastLogin`. The table
  (`blocks/auth/migrations/001_auth_schema.sqlite.sql:8-17` +
  `006_user_extended_fields.sqlite.sql:8-13`, row struct
  `blocks/auth/repo/users.rs:29-51`) has `email, display_name, avatar_url, role,
  email_verified, name, disabled, deleted_at, last_login_at, auth_version`.
  **Only `id` and `email` survive.** The review's claim reproduces verbatim.
- Six `CloudStorage*` interfaces (`:74, 88, 99, 111, 126, 134`) against **two**
  real cloud tables (`files/repo/quota.rs:20`, `files/repo/shares.rs:17`).
- `IAMIAMPolicy` (`:168-183`) is a Casbin `ptype, v0..v5` table; this repo's
  authz is WRAP grants (`platform_state/wrap_grants.rs:23`).
- 18 `Product*` interfaces (`:212 … :523`) describe solobase's dynamic
  custom-table engine. Only `ProductProduct` (`:426`) and `ProductGroup`
  (`:379`) map to real tables, both camelCase; `ProductGroup` carries 25
  `filterNumeric/Text/Boolean/Enum/Location` fields against a 9-column table.
- `TableNames = {}` (`:587-589`) is an empty const, so `TableName` resolves to
  `never`.

*12 more hand-written types outside `generated/`*, including
`types/models.ts:21` whose comment says *"Matches Go's auth.UserResponse
struct"*; `types/auth.ts:10-13`'s `{data: …}` envelope the server never emits
(and which `base.service.ts:28-32` documents as absent); `types/auth.ts:15-21`'s
`SignupRequest` sending `username/firstName/lastName` where
`auth_ui/contracts.rs:87-94` accepts `{email, password, name?}`;
`types/auth.ts:63-68` for a 2FA surface that does not exist; and
`types/storage.ts:5-43`, five exported helpers keyed on fields no endpoint
returns.

**What is NOT drifted, and this matters for scoping:** the parts written against
`contracts.rs` are current. `storage.service.ts:27-37` matches
`files.openapi.json`; `iam.service.ts:9-14` matches `AdminRoleListResponse`
(`blocks/admin/contracts.rs:227-236`); `auth.service.ts:18-25` matches
`contracts::AuthenticatedUser` + `MeUser`; and **all 22 products interfaces in
`extensions.service.ts` machine-compare clean against `products.openapi.json`**.
The drift is concentrated in the row-type file and the unused `types/*.ts`
files — not in the code paths the SDK actually calls.

**Review claim (a), item by item:**

| Claim | Verdict |
|---|---|
| `database.ts:1-2` "the solobase-era Go generator no longer exists" | reproduces (`:1-3`) |
| `AuthUser` field mismatch | reproduces |
| `IAMService.getRoles(): IAMRole[]` vs server's `RecordList` | **CLOSED** — `iam.service.ts:9-14` + `:45` unwrap the real envelope; `IAMRole` mirrors `AdminRoleView` in snake_case with a doc comment (`database.ts:180-184`) naming the old shape |
| `Extension` fields vs `admin/mod.rs:160-166` | reproduces, but at **`admin/mod.rs:672-687`** — the handler emits `{name, version, interface, summary, enabled}`; `extensions.service.ts:3-15` declares non-optional `description`/`author` that are never sent and omits `interface`/`summary` |
| README says "generated from the backend models" | reproduces (`README.md:50`) |
| `types-compatibility.test.ts` pins stale against stale | **partly** — 3 of 4 cases still do (`:20-33` AuthUser, `:41-55` StorageObject, `:61-67` Bucket); the `IAMRole` case (`:72-83`) was corrected |

### 5.3 What a CI freshness check would need

**Authority:** the committed `*.openapi.json` snapshots, not the live route.
They are deterministic (BTreeMap-sorted, pretty-printed, `openapi_snapshot.rs:70-82`),
regenerable offline with no booted server, and already gated. The live route is
tier-filtered per caller and needs a runtime.

**Generator:** an `openapi.json → .d.ts` step (`openapi-typescript` is the
obvious fit) over the merged snapshot set — plus a wrapper, because the
snapshots are `paths` **fragments**, not documents: `block_openapi`
(`openapi_snapshot.rs:70-82`) emits only the filtered `paths` map, with no
`openapi`/`info`/`components` keys.

**The check:** regenerate, `git diff --exit-code`. Identical in shape to the
existing Rust snapshot gates, and it would live beside them.

**Why this is harder than it looks — three obstacles, all load-bearing:**

1. **Most of what the SDK calls is not described.** Described: login, logout,
   me (GET+PATCH), signup, refresh; the four IAM role routes; 2 of 9 storage
   routes; nearly all products routes. **Not described:** `forgot-password`,
   `reset-password`, `change-password`, `verify`, `resend-verification`,
   `oauth/login`; `GET /b/admin/api/extensions`; six storage routes including
   bucket CRUD, object upload/delete, `search` and `recent`; and **the entire
   `/b/cloudstorage/*` surface** (`files.openapi.json` contains zero
   cloudstorage paths). A generator run today would cover roughly a third of the
   SDK's calls and silently leave the rest hand-written.
2. **The two worst drifts sit on undescribed endpoints, so no gate can ever see
   them.** `Extension` is declared with only `.summary(…)` at
   `admin/mod.rs:330-335`, so `has_schema()` is false and it never enters
   `/openapi.json`.
3. **`generated/database.ts` describes database rows, which the server never
   publishes.** `auth_ui/contracts.rs:11-20` and `admin/contracts.rs` both state
   the projection rule; `files/contracts.rs` has `RecordView`/`RecordListView`
   for exactly this. No OpenAPI generator can produce row types. Their real
   authority is `migrations/*.sql` plus each repo module's row struct.
   Realistically they should be **deleted**, not generated — nothing under
   `src/services/` imports them except `IAMRole` (`iam.service.ts:2`), and
   `models.ts:2-11` re-exports five purely to alias.

Consequential corollary: **`types/{auth,iam,storage}.ts` are hand-written and
imported by nothing under `src/services/`**. They reach consumers only via
`index.ts:12`'s `export * from './types'`. A freshness check has nothing to
compare them to; the honest fix is deletion.

Also: `test/types-compatibility.test.ts:20-33, 41-55, 61-67` compiles **only
because both sides are stale**. Regenerating the types breaks it by
construction, so that test must move in the same PR.

### 5.4 Has drift bitten before?

Yes, and the record is in the tree — but the brief's framing needs one
correction. **No phase-2 PR was sent back.** All 53 fork PRs merged. What
happened is that PR #22 (`phase2/files-typed-rows`, merge `5e41ac6e`,
2026-09-06) reshaped seven files handlers from `RecordList`
(`{records:[{id,data}],total_count,page,page_size}`) to `Page<T>`
(`{rows,total}`) in commits `bcab0cbe`, `c4e8046a`, `8fc2fd17` — and then
reverted the wire change **inside the same PR**, in commit `1ccbb452`
*"fix(files): publish the RecordList envelope the JS SDK reads"*, whose message
reads: *"Serializing `Page<T>` straight out of the seven JSON handlers reshaped
six response bodies that `packages/impresspress-js` reads — and both snapshot
gates passed anyway, because none of those endpoints declares an output
schema."* The same reasoning is preserved at
`docs/superpowers/plans/2026-09-06-ownership-4-files-typed-rows.md:18, :25`.

So the danger is real and demonstrated, and the mechanism is precisely obstacle
(2) above: **the snapshot gates could not see it.** Lockstep changes have
happened twice, both outside phase 2 — `d660c9e5` (PR #15, comment-only) and
`b37b5197` (PR #36, phase 3, a real contract change dropping
`amount_cents`/`user_id` from order schemas with `products.openapi.json` and
`extensions.service.ts` in the same commit).

**Staleness:** the SDK's last commit is `b37b5197`, 2026-09-07. Phases 4 and 5
(PRs #37–#53) landed with **zero** commits to `packages/impresspress-js/`.

**⚠ Coverage risk.** A freshness check that only compares generated types against
the snapshots will report green on exactly the endpoints where drift is
possible-but-invisible. Declaring `.output(response_schema_of::<T>)` on the
undescribed endpoints is therefore not an optional extra — it is the part that
makes the check mean anything. The PR that adds the check must state how many
of the SDK's call sites it covers, and the number must go up, not be left
implicit.

---

## 6. One request client

**Three distinct HTTP request implementations exist**, all in
`packages/impresspress-js`:

| # | Path | Lines | Timeout | Abort | Error codes | Auth | Query | Body |
|---|---|---|---|---|---|---|---|---|
| 1 | `src/http-client.ts:89-173` `HttpClient.request` | 85 | yes — per-request + config, default 30 s (`:96`) | yes — composes caller signal with an internal controller (`:110-120`) | yes — `error→code`, `message`, `code→detailCode` (`:159-169`), plus `"aborted"`/`"timeout"`/`"network_error"` (`:136-141`) | yes (`:104-106`) | yes — `buildQueryString` `:43-52` | JSON |
| 2 | `src/services/base.service.ts:56-91` `requestFormData` | 36 | **no** | **no** | `error`/`message`/`status`/`data`, **no `detailCode`** (`:83-88`) | yes (`:63-65`) | no | multipart, **hardcodes POST** (`:69`) |
| 3 | `src/services/base.service.ts:94-124` `requestBlob` | 31 | **no** | **no** | `error`/`message`/`status`, **no `detailCode`, no `data`** (`:121`) | yes (`:97-99`) | no | none, **hardcodes GET** (`:101`) |

**They differ meaningfully, not cosmetically.** An upload
(`storage.service.ts:178`) or a download (`:145`) hangs forever on a stalled
connection and cannot be cancelled, while every JSON call times out at 30 s and
honours a caller's `AbortSignal`. #2 also calls `res.json()` unguarded
(`base.service.ts:79`) where #1 wraps parsing in try/catch
(`http-client.ts:151-155`), so a malformed JSON error body throws a raw
`SyntaxError` instead of an `ImpresspressError`.

Review claim (b) reproduces in full:
- `requestFormData`/`requestBlob` lack timeout/abort/`detailCode` — yes.
- **Each service constructs its own `HttpClient`** (`base.service.ts:19-24`);
  six subclasses, six instances, each holding its own copy of the API key.
  `BaseService.setApiKey` (`:144-147`) therefore stores the secret in two
  places (`this.config.apiKey` for #2/#3, `this.http` for #1).
- **`client.ts` fans `setApiKey` over a hand-listed array twice** — identical
  six-element literals at `client.ts:49-56` and `client.ts:65-72`.
- **`buildQueryString` x2** — `http-client.ts:43-52` (returns a leading `?`)
  and `base.service.ts:126-138` (returns bare, callers prepend `?` by hand at
  `storage.service.ts:136-139, 212` and `extensions.service.ts:41-44`).

Review claim (c) reproduces with slightly larger counts: **10** primary bare
`Error` throws, not 9 (`popup-auth-session.ts:80, 135, 151, 161, 169`;
`base.service.ts:48`; `auth.service.ts:235, 315, 327`; `storage.service.ts:174`),
plus 2 wrapper sites. `popup-auth-session.ts` still has no machine-readable
cancel/timeout/blocked codes — although the SDK already owns the vocabulary
(`"aborted"`/`"timeout"` at `http-client.ts:136-138`). Hand-written `any`s: **9**
outside `generated/` (`types/storage.ts:14`, `types/iam.ts:17`,
`base.service.ts:8, 10, 126`, `extensions.service.ts:9, 32, 37, 38`), plus 21
inside `generated/database.ts`, which `.eslintrc.json:19` excludes from lint
entirely (and `:22` downgrades `no-explicit-any` to a warning).

**Unifying touches:** `base.service.ts` (delete all three duplicated helpers),
`http-client.ts` (accept a `FormData`/raw body, a `responseType: 'blob'`, and a
method for the form path), `client.ts:49-72` (replace both fan-outs with one
shared client or a config held by reference), and the two call sites at
`storage.service.ts:145, 178`.

**⚠ What could break:**
1. `requestFormData` deliberately does **not** set `Content-Type`
   (`base.service.ts:66`) so `fetch` can inject the multipart boundary;
   `HttpClient` always sets `application/json` (`http-client.ts:100`).
2. `requestBlob` must skip JSON parsing on success.
3. `test/services.test.ts:197-214` stubs `globalThis.fetch` directly for the
   blob/multipart paths, so those fakes move.
4. **Adding the default 30 s timeout to uploads is a behaviour change** for
   large files. The unified path needs a longer or opt-out timeout on the
   blob/form methods, stated explicitly rather than inherited.

---

## 7. Test conventions

Every number in the review's paragraph is stale, most of them by a wide margin.
The direction of the diagnosis survives; the shape of the problem has moved.

| Review claim | Status now |
|---|---|
| products in-tree tests "12 601 lines" | **stale, ~64 % low** — 18 files, **20 651 lines**, 327 test fns |
| products tests "on `TestContext`" | reproduces, and is now the point: `products/tests/harness.rs:14` imports `crate::test_support::TestContext`; the old 657-line `mock_context.rs` is gone (`harness.rs:5-8` records the deletion) |
| `auth/common.rs:18-66` hand-rolls `MigrationTestCtx` | type still exists, lines wrong: `crates/impresspress-core/tests/auth/common.rs:35` (struct), `:180` (`impl Context`), plus a second context `AsAuthUi` at `:138`/`:141`; the file is 222 lines |
| "`Cargo.toml:34-36` says `test-support` exists" | feature is at `crates/impresspress-core/Cargo.toml:47`, doc block `:41-46`, self dev-dep `:258` |
| "2 of 23 files use `TestContext`" | **wrong by a wide margin** — `crates/impresspress-core/tests/` now holds **46** `.rs` files (18 900 lines): **17 use `TestContext`**, 16 use `MigrationTestCtx`, 1 hand-rolls its own, 1 uses only `real_block_infos`, **11 need no context at all** |
| "168 inline `#[cfg(test)]` modules" | **stale, ~2x low** — **337** declarations across **254** files, 2 199 inline test fns |
| "seven with their own `Context` fakes" | roughly holds, different set: 18 `impl Context` total, of which 2 are production (`impresspress-web/src/dev_runtime.rs:149, 594`) and 2 are the shared harness (`test_support.rs:1307, 1543`), leaving **14 hand-rolled**, 12 of them in `src/` |
| "`test_support.rs` is 1 176 lines" | **stale, 2.4x low** — **2 834 lines** |

> **Correction (PR 1 of ruling 6.7, 2026-09-09).** Two counts in this table were re-derived at
> `7540296d` and moved slightly. The strict inline-test-module count — `#[cfg(test)]` on its own line
> immediately followed by `mod ` — is **331 across 256 files**, not 337 across 254; counting every
> `#[cfg(test)]` attribute at any indent gives **372**. And `MigrationTestCtx` is named by **17**
> files under `crates/impresspress-core/tests/`, one of which (`tests/auth/common.rs`) is its own
> definition, so the 16-user figure below is right and the file count is 17. Neither delta touches
> ruling 6.1: adoption is 17 of 46 against the review's 2 of 23, and the module count is about twice
> the review's 168. `test_support.rs` is 2 834 lines, confirmed.

### 7.1 How many conventions there really are — four, not three

1. **`TestContext` (dominant).** `crates/impresspress-core/src/test_support.rs`,
   2 834 lines, gated at `lib.rs:43-44` behind `feature = "test-support"`
   (`Cargo.toml:47`). Adoption: **113 of 221** `#[cfg(test)]`-bearing files in
   `impresspress-core/src` use it (123 use something from `test_support`), 16 of
   18 products test files, 17 of 46 core integration files. Real SQLite, real
   `DatabaseBlock`, real config/crypto blocks, real WRAP.
2. **`MigrationTestCtx` — the one true holdout.** 16 files, all under
   `crates/impresspress-core/tests/auth/`, ~4 200 lines of tests behind a
   222-line fixture.
3. **Source-scan / golden gates.** 11 files in `core/tests/` (`repo_door.rs`
   46 KB, `error_door.rs`, `endpoint_surface.rs`, `manifest_block_parity.rs`,
   `autoreg_smoke.rs`, `block_enabled_defaults.rs`, `wafer_guest_parity.rs`,
   `wafer_guest_golden.rs`, `wafer_pin_surface.rs`, `lockfile_e2e.rs`,
   `env_config_source.rs`) plus `products/tests/repo_door_test.rs`. These use no
   context **by design** and must not be folded into anything.
4. **`wasm-bindgen-test` on wasm32.** **165** `#[wasm_bindgen_test]` fns across
   21 files — `impresspress-browser/src` (9 files, 76 tests) and
   `impresspress-cloudflare/src` (12 files, 89 tests). Cannot use `TestContext`:
   `impresspress-cloudflare` cannot compile for a native target at all
   (`Cargo.toml:184-189` — `JsFuture` holds `Rc<RefCell<_>>`, so its
   `StorageService` fails the `Send` bound), and `TestContext` depends on
   `wafer-block-sqlite`.

Only **one crate enables `test-support`**: `impresspress-core` itself
(`Cargo.toml:258`). `crates/impresspress`'s dev-dep on core takes
`features = ["block-dev"]` only; `impresspress-cloudflare`, `-browser`,
`-bundle`, `-native` have no dev-dep on core at all. **That manifest fact is the
root cause of the duplicated `MessageCapture`**, see §7.3.

### 7.2 Fault injection is already folded in

The phase-6 fix line reads "fault injection folded into `TestContext`". **That
work is largely done during this run.** Present in
`crates/impresspress-core/src/test_support.rs` today:

| Helper | Line | Seam |
|---|---|---|
| `break_writes()` | `:766` → `FailingWritesDb:1094` | every mutating `DatabaseService` method fails |
| `break_reads()` | `:787` → `FailingReadsDb:837` | every read fails |
| `break_list_reads()` | `:801` | list reads fail |
| `FailingDbOpContext` | `:1468` (`new():1497`, `failing_with():1509`, `after_passing():1526`, `impl Context:1543`) | fails a specific `(wire op, collection)` pair; everything else passes through |
| `hold_next_storage_get()` | `:619` → `InMemoryStorageService::hold_next_get():2151`, `HeldGet:1998` (`release():2024`, `was_reached():2032`, `budget_expired():2050`) | parks one named storage `get` with a bounded poll budget |
| `fail_next_storage_put()` | `:629` → `:2161` | one-shot storage write failure |
| `storage_ops()` / `ops()` | `:639` / `:2166` | storage call log |
| `MessageCapture` | `:2368` | `tracing::Subscriber` capturing emitted log lines |

Adoption: `FailingDbOpContext` in **25** files outside its definition;
`break_writes`/`break_reads`/`break_list_reads` in **21**; the storage seams in
`tests/dev_files.rs` (4 sites), `tests/dev_activation.rs` and `tests/dev_gc.rs`
(4 sites). **The remaining phase-6 work is convergence onto what already exists,
not building it.**

### 7.3 What "one convention" would actually mean

- **Retire `MigrationTestCtx`** (16 files). Requires adding to `TestContext`:
  (a) a `mint_access_token(sub, extra_claims, ttl)` routed through the
  registered `CryptoBlock` under an auth-ui caller identity — `access_token_for:1818`
  hardcodes a 1 h TTL, only `roles` as extra claims, and a *different* secret
  (`TEST_JWT_SECRET = "test-jwt-secret"` vs `MigrationTestCtx`'s
  `"test-jwt-secret-padded-to-min-32-bytes-aaaa"`, which
  `TestContext::with_auth_and_crypto():1597` already uses); (b) an expired-token
  minter (move `sign_access_token_expired`, `tests/auth/common.rs:114`); (c) a
  way to set a config *value* without registering a real config block —
  `set_config():137` currently couples the two.
- **Delete `FailingGetContext`** (`src/blocks/admin/iam.rs:471`) — a duplicate of
  `FailingDbOpContext`.
- **Delete `ConfigCtx`** (`src/blocks/email.rs:626`) — `set_config` covers it;
  its doc at `:625` still references a `MockContext` that no longer exists.
- **De-duplicate `SequencedStripeNetwork`** (`products/tests/stripe_tests.rs:72`
  vs `products/tests/provider_tests.rs:20`).
- **De-duplicate `MessageCapture`** — but only after the shared copy
  (`test_support.rs:2368`) gains a full-field visitor; see the warning below.
  Unblocking it means adding `"test-support"` to `crates/impresspress/Cargo.toml`'s
  dev-dep on core.

### 7.4 Must-keep — hand-rolled for a real reason

| Fixture | Why |
|---|---|
| `MessagesWriteFails` (`llm/routes/test_support.rs:158`) | Documented at `:148-157` and confirmed at `test_support.rs:1388-1396`: `TestContext::call_block` hands the **inner** context to a registered block, so a wrapper is invisible to a nested block's DB calls |
| `RaceTheRestoreWrite` (`products/tests/handler_tests.rs:1645`), `DeleteBetweenProductReads` (`:4941`) | Deterministic mid-request interleavings, already built **on** `Arc<TestContext>` — this is the intended extension pattern, not a violation |
| `PanicCtx` (`llm/routes/test_support.rs:24`), `NopCtx` (`system.rs:93`) | The panic *is* the assertion |
| `AlwaysErrorsOnList` (`platform_state/block_settings.rs:817`), `ErroringDb` (`platform_state/wrap_grants.rs:471`) | Every non-target method is `unreachable!()` — see §7.5 |
| Everything in `impresspress-cloudflare/src` and `impresspress-browser/src` | Cannot use `TestContext` (native compile impossible / SQLite dependency) |
| The 11 source-scan and golden gates | They assert about source text and manifests |
| `dev/test_support.rs`, `vector/test_support.rs`, `llm/routes/test_support.rs` stub blocks | These fake *seams and blocks*, the complement of what `TestContext` fakes. `dev/test_support.rs` is already exported under the same feature and used by `TestContext::with_dev()` |

### 7.5 ⚠ Where folding into `TestContext` would REDUCE coverage

This is the highest-risk item in the whole phase, because every one of these
looks like a mechanical substitution and is not.

1. **`AlwaysErrorsOnList` and `ErroringDb`** fail one method and mark **every
   other method `unreachable!()`**. The assertion is not "the read failed" but
   "`load_and_seed` short-circuited at the first `list()` and never touched
   anything else". `break_list_reads()` and `FailingDbOpContext` delegate
   non-failing calls to real SQLite, so the short-circuit claim would silently
   stop being tested.
2. **`PanicCtx` / `NopCtx`.** "`call_block` must not be invoked on a parse-error
   path" is enforced by the panic. On a `TestContext` the call would succeed or
   return `NotFound`, and the test would pass either way.
3. **`MessagesWriteFails`.** Replacing it with `break_writes()` collapses "the
   user turn failed" and "the assistant turn failed" into one scenario — which
   is the entire property under test.
4. **`FailingGetContext`** looks like a safe deletion, but the substitute must
   name the *right* table. `FailingDbOpContext` scopes by `(op, collection)`
   precisely because many repo calls share one wire op
   (`test_support.rs:1454-1458`), while `FailingGetContext` fails **all**
   `database.get`. Per-call-site check, not a mechanical swap.
5. **`RecordingContext` (`tests/extra_routes_test.rs:36`) and `RecordingCtx`
   (`llm/routes/test_support.rs:87`)** assert on the *dispatch record* — which
   block was called, with what meta, and that the body was empty
   (`llm/pages.rs:1150-1153`: "a list is a GET with no body, not a query
   smuggled into one"). `TestContext` has no call log. Folding without first
   adding one deletes those assertions.
6. **`MigrationTestCtx` deliberately leaves `wafer-run/config` unregistered**
   (`tests/auth/common.rs:10-11`) so `config::get_default(.., "sqlite")` falls
   through to the declared default. `TestContext::set_config` registers a real
   `ConfigBlock` (`test_support.rs:137-160`), so a naive port could change which
   backend the migration path selects — silently, in the tests that exist to pin
   migrations.
7. **`crates/impresspress/tests/webmcp_refusal_boot_logging.rs`'s local
   `MessageCapture` (`:75`)** records **every** field of every event (`:81-91`),
   which is how it asserts on the `scope=outputSchema` structured field. The
   shared copy (`test_support.rs:2368`) records only `message` and counts
   occurrences. Swapping as-is drops the structured-field assertion.

---

## 8. Periphery (T9, B27, B28)

Verified at HEAD `7540296d` against a clean tree.

| # | Item | Verdict |
|---|---|---|
| 1 | `.playwright-mcp/` baselines + B27's wholesale `git add` | **CLOSED** |
| 2 | `.claude/worktrees/` unignored | **CLOSED** |
| 3 | `packages/solobase-site/` | **CLOSED — never a repo fact** |
| 4 | Root `package.json` stub | reproduces |
| 5 | `payment-link-state.jpeg`, `.intentionally-empty-file.o` | reproduce |
| 6 | `.githooks/pre-commit` (B28) | **CLOSED** |
| 7 | `NICE_TO_HAVE.md` | reproduces, **worse than stated** |
| 8 | Committed doc artifacts | reproduce |

**1. `.playwright-mcp/` and B27 — closed, with evidence.**
`git ls-files .playwright-mcp` returns **0 tracked files** (not 72 PNGs, not 97
dumps). The directory does not exist in this worktree and is ignored at
`.gitignore:24`, with a comment at `:19-23` recording the history. Baselines now
live in Playwright's default location: **92 tracked snapshot PNGs** across
`crates/impresspress-web/tests/e2e/visual-baseline.spec.ts-snapshots/` (64),
`…/products-lifecycle.spec.ts-snapshots/` (8) and
`examples/tests/products-examples.spec.ts-snapshots/` (20).
`crates/impresspress-web/tests/playwright.config.ts:10-14` writes the rule down.
**The wholesale add is gone:** `regen-visual-baselines.yml` is now 213 lines and
its only staging line is `:197`, filtered (`:184-189`) from a two-entry
`BASELINES` array (`:175-178`). Branch `chore/baselines-out-of-mcp-scratch`
(tip `258fe344`) is an ancestor of HEAD via `f19d7ca0` (PR #93, 2026-09-03).
The workflow was reindexed again on 2026-09-08 by `8c71b7bf`. **Nothing to do.**

**2. `.claude/worktrees/` — closed.** `.gitignore:63`; `git ls-files .claude`
returns 0 tracked files. *Nuance:* `.claude/` as a whole is **not** ignored —
`.claude/settings.json` and `.claude/agents/*.md` remain committable. If the
intent is "Claude Code scratch never enters the repo", the rule is narrower than
that.

**3. `packages/solobase-site/` — closed.** The directory does not exist,
`git ls-files packages/solobase-site` is empty, and no ignore rule matches it.
`packages/` holds **49 tracked files**, all under `impresspress-js/` and
`impresspress-web/`. The 89 MB was local disk in the reviewer's checkout.

**4. Root `package.json` — reproduces (inert).** 79 bytes, 4 lines,
`{name, version, private}`. No `scripts`, no `workspaces`, no deps; no root
`package-lock.json`. Every `npm ci`/`npm run` in every workflow carries an
explicit `working-directory`. Nothing in workflows, `justfile` or `scripts/`
reads it. Deleting it breaks nothing; the only arguments to keep are that
`"private": true` blocks an accidental root `npm publish`, and that npm/eslint
config resolution walks up (no config lives there today).

> **Correction (PR 1 of ruling 6.7, 2026-09-09) — CLOSED, deleted.** Re-verified and removed. Its
> history is the whole argument: the initial commit `99c4e07c` gave it a `workspaces` array naming
> `packages/solobase-js` and `packages/solobase-site`, `b48c6837` deleted that array (and the root
> `package-lock.json` with it), and `4893cc6b` only renamed the package during the
> solobase → impresspress rename. What is left has been inert since April. Nothing reads it: the
> only tracked file containing the string `impresspress-monorepo` is the file itself, no tracked file
> anywhere refers to a parent `package.json` (`git grep -E '\.\./+([a-zA-Z0-9_-]+/)*package\.json'`
> is empty), every `npm ci` / `npm run` step in all six workflows carries an explicit
> `working-directory`, and the four `actions/setup-node` steps that pass `cache: npm` each pin an
> explicit `cache-dependency-path` (`crates/impresspress-web/package-lock.json` or
> `examples/package-lock.json`), so none of them resolves a lockfile by walking up to the root. The
> `npm publish` argument dies with the file: there is no root `package-lock.json` and no root
> `scripts`, so nothing publishes from the root to guard.

**5. Tracked, unreferenced files — both reproduce.**
`payment-link-state.jpeg` (repo root, **48 325 bytes**, JPEG 1319x693, added
2026-07-22 in `13f9e6bb` — a dropped screenshot) and `.intentionally-empty-file.o`
(repo root, **0 bytes**, added in the initial commit `99c4e07c`). Both were
checked hard, since `.intentionally-empty-file.o` reads like a fixture: a
repo-wide grep for either filename (excluding `.git`, `node_modules`, `target`)
hits **only** `docs/CODE_REVIEW_2026-09-05.md:123`; there is no `*.o` pattern or
negation in `.gitignore`; root `Cargo.toml` has no `include`/`exclude` keys; the
`justfile`, `scripts/` and `.github/` reference neither. Genuinely residue.

> **Correction (PR 1 of ruling 6.7, 2026-09-09) — CLOSED, deleted.** Both files were removed, and the
> proof was extended past the current tree, because this run has already had one dead-code claim turn
> out false when the author searched only the tree and not its history. Three searches, all at
> `7540296d`:
>
> 1. **Every tracked file, binary included.** `git grep -l -a -F payment-link-state` and
>    `git grep -l -a -F intentionally-empty` each return exactly one path,
>    `docs/CODE_REVIEW_2026-09-05.md` — the review naming them as residue. `-a` matters: the ordinary
>    text-only grep would have skipped the repository's own binary assets.
> 2. **Every commit that ever touched either literal.** `git log --all -S'payment-link-state'` and
>    `git log --all -S'intentionally-empty'` each return exactly one commit, `7936ffa3`
>    ("docs: commit the 2026-09-05 review report…"). So neither name has ever appeared in a script, a
>    test, a workflow or a manifest, in any commit on any branch — not merely today.
> 3. **Each file's own history.** `payment-link-state.jpeg` was added once, in `13f9e6bb`
>    ("feat(products): harden commerce lifecycle and admin UX"), with no accompanying reference — a
>    dropped screenshot. `.intentionally-empty-file.o` was added once, in the initial commit
>    `99c4e07c`, and despite the name is not a fixture: no `*.o` pattern or negation exists in
>    `.gitignore`, and root `Cargo.toml` has no `include`/`exclude` keys that would need a file to
>    keep a directory alive.

**6. `.githooks/pre-commit` (B28) — closed.** The hook is 46 lines (the review's
`:8` and `:23` both moved). `:18-22` runs **`cargo +nightly fmt --all`**, not
stable `cargo fmt`; `:7-10` carries a note recording the exact bug B28
describes, as fixed. `rustfmt.toml` is 2 lines (`imports_granularity = "Crate"`,
`group_imports = "StdExternalCrate"`), both nightly-only, which is why. CI
matches: `ci.yml:102` and `ci-main.yml:93` both run
`cargo +nightly fmt --all -- --check`, with `rustup toolchain install nightly
--component rustfmt` at `ci.yml:94` / `ci-main.yml:85`. Prettier at `:35` points
at `packages/impresspress-js/node_modules/.bin/prettier`, falls back to
`command -v prettier` (`:37`) and silently skips if neither exists (`:39`);
**`packages/cloud-dashboard` appears nowhere in the repo** except the review
doc. Clippy at `:25-27` mirrors CI's exclusions.

*Separate, non-repo finding worth telling the user:* in this environment
`core.hooksPath` is set to
`/home/joris/Programs/suppers-ai/workspace/solobase/.git/hooks` — an
**unrelated repository's** hooks directory. So no impresspress hook runs on
commit here, and solobase's would. `README.md:25-29` documents the correct
setting. One config command; no repo change.

**7. `NICE_TO_HAVE.md` — reproduces, worse.** 27 lines, **seven** bullets (the
review said four items), of which **five** are stale:

| Line | Item | Status |
|---|---|---|
| `:7` | KV caching for project resolution in the dispatch worker | Stale and mis-targeted. KV caching exists (`impresspress-cloudflare/src/kv_cached_db.rs:1-10`) but for the per-block config-var path; there is no dispatch worker in this repo. Its cited spec `docs/superpowers/specs/2026-05-22-kv-cached-d1-config-source-design.md` **no longer exists** |
| `:11` | Configurable Argon2 params / `ARGON2_MEMORY_COST` | Not this repo's code. Argon2 lives upstream (`impresspress-native/src/crypto.rs:16`); `grep -rn ARGON2 crates/` returns no hits. Only *asserted* here (`impresspress-cloudflare/src/crypto_service.rs:151`). Acting on it means a wafer-run change |
| `:15` | Coverage via `cargo-tarpaulin` | Done, differently — `ci-main.yml:1054` `coverage:` job uses `cargo-llvm-cov` (`:1084`) |
| `:17` | Multi-browser Playwright matrix | **Genuinely open.** Both configs run one project (`crates/impresspress-web/tests/playwright.config.ts:53-55`, `examples/playwright.config.ts:32-37`), `desktop-chrome` only |
| `:19` | Component-level frontend tests (Vitest) | Done — `packages/impresspress-js/package.json:11-12, 29`, run at `ci.yml:1428-1437` |
| `:23` | Load testing (k6/Artillery) | **Genuinely open.** No k6/artillery anywhere |
| `:27` | Release asset key inventory | Self-marked resolved in the file; `crates/impresspress-core/src/release_inventory.rs` exists |

**⚠** Acting on the `:17` browser-matrix item would invalidate all 92 committed
baselines (chromium `-linux.png` renders) and change what CI verifies. That is a
separate decision, not a hygiene edit.

**8. Other committed artifacts.** Largest tracked files:
`crates/impresspress-bundle/assets/vendor/sql-wasm.wasm` (744 K, legitimate),
`crates/impresspress-core/tests/snapshots/products.openapi.json` (612 K, **a CI
gate — do not touch**), 20 example baselines 216–520 K (**the regen workflow
writes here — do not touch**), and:

- **`docs/CODE_REVIEW_2026-06-05_findings.json` — 468 667 bytes, still
  tracked.** Referenced only by `docs/CODE_REVIEW_2026-06-05_HANDOFF.md:10` and
  the 09-05 review.
- `docs/CODE_REVIEW_2026-06-05_HANDOFF.md` (11 145 B) — `:8-10` says its
  artifacts are "untracked" when both are tracked; `:15-19` records two racing
  Claude PIDs and a stale cwd.
- `docs/documentation_review_report.md` (7 933 B) — **8 broken
  `file:///home/joris/Programs/impresspress/workspace/...` links** at `:32, 35,
  47, 49, 54, 55, 66, 69`, pointing at a spec and an entire
  `docs/superpowers/handoffs/` directory that do not exist here.
- `docs/CODE_REVIEW_2026-07-16_FINDINGS.md` (36 115 B) — pinned to revision
  `ed94a3f6` with no resolved/superseded banner.
- The review's "untracked 72 KB 07-24 file in `git status`" **does not
  reproduce**; the tree is clean.

**No workflow reads anything under `docs/`** (`grep -rn "docs/"
.github/workflows/` returns zero hits), so deleting any of these has zero CI
impact.

> **Correction (PR 1 of ruling 6.7, 2026-09-09) — deliberately NOT done.** Ruling 6.7 item 1 scopes
> its sweep to "the tracked files nothing references", and these are referenced.
> `docs/CODE_REVIEW_2026-06-05_findings.json` is named by `docs/CODE_REVIEW_2026-06-05_HANDOFF.md:10`
> as that handoff's own structured artifact; deleting a record another record cites is a different
> act from removing residue, and it would leave the handoff pointing at nothing.
> `docs/documentation_review_report.md` is unreferenced except by the 09-05 review naming it as a
> defect, but its defect is eight broken links inside it, not its existence, and a review record with
> broken links is still a record. The superseded banners for
> `docs/CODE_REVIEW_2026-06-05_HANDOFF.md` and `docs/CODE_REVIEW_2026-07-16_FINDINGS.md` were not
> added either. All four items stay **open** and want a decision from whoever owns the review
> archive, not a drive-by deletion by a hygiene pull request.

---

## 9. Proposed split into pull requests

Ten PRs. The ordering argument is: **close what is already fixed first** (so the
phase's ledger is honest and later PRs are not re-litigating dead findings),
then **do the coverage-*increasing* CI work before the coverage-*collapsing*
refactor** (so the reusable workflow is extracted from a body that is already
the superset), then the independent package work, then the test convention last
because it is the largest and the most likely to reduce coverage silently.

> **Correction (PR 1 of ruling 6.7, 2026-09-09).** This ten-way split is **superseded by ruling
> 6.7**, which consolidates it into seven pull requests. The mapping: reconnaissance PRs 1 and 2
> become ruling 6.7's first (this document, plus the file removals — the documentation deletions and
> the `NICE_TO_HAVE.md` trim are *not* carried, see §8 item 8); PRs 3 and 4 become its second; PR 5
> its third; PR 6 its fourth; PRs 7 and 8 its fifth; PR 9 its sixth; PR 10 its seventh. The ordering
> argument below is unchanged and is what ruling 6.4 rests on. The per-PR detail in the subsections
> that follow stays as the record of what each one is for.

---

### PR 1 — `chore/phase6-close-fixed-findings` *(docs only)*

Amend `docs/CODE_REVIEW_2026-09-05.md` in place with a status line on B17, B27,
B28, and the T9 items that no longer reproduce (`.claude/worktrees/`,
`packages/solobase-site/`, the `IAMService.getRoles` half of the SDK finding),
each with the commit or file:line that closes it.

*Why first:* six of this phase's items are already done. Anyone reading the
review — including a later reviewer of PRs 2–10 — will otherwise spend budget
re-deriving them, exactly as happened with ruling 5.4 during phase 4.
*Coverage risk:* none. *Evidence:* the citations in §2 and §8 above.

---

### PR 2 — `chore/repo-periphery-sweep` *(deletions)*

Delete `payment-link-state.jpeg`, `.intentionally-empty-file.o`,
`docs/CODE_REVIEW_2026-06-05_findings.json`, `docs/documentation_review_report.md`;
add a superseded banner to `docs/CODE_REVIEW_2026-06-05_HANDOFF.md` and
`docs/CODE_REVIEW_2026-07-16_FINDINGS.md`; trim `NICE_TO_HAVE.md` to the two
live items (`:17`, `:23`) and re-file the Argon2 item against wafer-run or drop
it; decide the root `package.json` (keep with a one-line comment saying why, or
delete). Optionally widen `.gitignore` for `.claude/` beyond `worktrees/`.

*Why second:* trivially reviewable, unblocks nothing, blocks nothing, and gets
the noise out of the way before the real work.
*Coverage risk:* **none, verified** — no workflow references `docs/`, and neither
deleted file is named anywhere outside the review.
*Evidence:* the `docs/` grep over `.github/workflows/` is empty; the filename
greps in §8 item 5.

---

### PR 3 — `fix/ci-drop-dead-wafer-run-clones`

Delete all **24** wafer-run clone steps and the two `Build wafer-client-js`
steps across `ci.yml`, `ci-main.yml`, `release.yml`, `deploy-demo.yml`,
`deploy-dev-sandbox.yml`, `regen-visual-baselines.yml`.

*Why third:* it is a pure deletion, it removes ~24 steps from the files PRs 6–7
will restructure (so those diffs get smaller and clearer), and it closes a
supply-chain hole: the SDK job **executes** `npm ci && npm run build` from an
unpinned shallow clone of a third-party default branch, unconditionally on every
merge to `main`, for a dependency that no longer exists.
*Coverage risk:* **low but must be proven.** The theory is that `Cargo.toml`
pins every wafer crate by `rev` and no `.cargo/config.toml` `[patch]` exists in
CI, so the clone is never read.
*Evidence required:* a full green `ci.yml` **and** `ci-main.yml` run with the
steps gone, plus `cargo tree -p impresspress | grep wafer` before and after
showing the same `rev = c55c9320…` resolution. If any job goes red, that job is
the counter-example and the finding is wrong for it.

---

### PR 4 — `fix/ci-path-filters-cover-their-inputs`

Add `scripts/**` and `.cargo/**` to `ci-main.yml`'s filter; add `.cargo/**`,
`rustfmt.toml` and `justfile` to `ci.yml`'s; rewrite `deploy-demo.yml`'s filter
to list what it actually builds (`crates/impresspress/**`,
`crates/impresspress-bundle/**`, `crates/impresspress-browser/**`, `Cargo.toml`,
`Cargo.lock`), modelled on `deploy-dev-sandbox.yml:9-25` and carrying the same
explanatory comment.

*Why here:* it strictly **increases** what triggers, so it cannot hide a
regression, and it must land before the reusable-workflow PRs so those are
measured against a correct trigger set.
*Coverage risk:* **none — it can only add runs.** The cost is more CI minutes.
*Evidence:* a commit touching only `rustfmt.toml` and a commit touching only
`scripts/check-audit-expiry.sh` each trigger the workflow they should.

---

### PR 5 — `fix/ci-main-matches-the-pr-gate`

Add to `ci-main.yml` the seven steps and five jobs §1.2 lists as PR-only —
critically the WRAP-grant security audit (`scripts/audit-wrap-grants.sh`) and
the HTML guard, plus `cloudflare-wasm-test`, `browser-wasm-test`, `e2e-build`,
`e2e-smoke`, `e2e-visual`. The three PR-only skip gates
(`sdk`, `browser-wasm-test`, `cloudflare-wasm-test`) key on
`github.event.pull_request.base.sha`, which does not exist on `push`; on the
push side they simply run unconditionally.

*Why before the refactor, not after:* this is the whole coverage argument. If
the reusable workflow is extracted while `ci-main` is the smaller body, the
natural parameterisation is "the intersection", and seven steps and five jobs
quietly leave the PR gate too. Making the two bodies equal **first** turns PR 6
into a mechanical extraction with nothing to decide.
*Coverage risk:* **none — strictly additive.** Cost: `ci-main` gets longer
(a recent merge run was ~21 min; the added jobs run in parallel and are 1–8 min
each, so the critical path stays on `e2e-dev-compile` at ~9 min).
*Evidence:* the jobs listing for a merge run before and after — the job-name list
must gain exactly `cloudflare-wasm-test`, `browser-wasm-test`, `e2e-build`,
`e2e-smoke`, `e2e-visual`, and the `check`/`test`/`wasm` step lists must gain
exactly the seven named steps. Nothing may disappear.

---

### PR 6 — `refactor/ci-reusable-workflows`

Extract three reusable workflows and two composite actions:
`.github/workflows/_build-wasm.yml` (called by `ci.yml`, `ci-main.yml`,
`release.yml`), `.github/workflows/_shared-jobs.yml` (the eight byte-identical
jobs plus the four now-identical ones, with skip-gate booleans as inputs),
`.github/workflows/_cross-compile.yml` (called by `ci-main.yml` for build-only
and `release.yml` for build+package), a composite action for the `wasm-pack`
install pin, and a composite action for the native-server boot block
(`ci.yml:1170-1198` = `regen-visual-baselines.yml:107-128`).

*Why after PR 5:* by then the two bodies are equal and the extraction has no
judgement calls in it.
*Coverage risk:* **HIGHEST IN THE PHASE.** A workflow refactor can silently
delete a step, and nothing goes red.
*Evidence required:* dump the jobs listing for a PR run and a merge run
**before** the PR and again after, and diff the flattened `(job name, step name)`
lists. The two sets must be identical, not merely similar. Attach both dumps to
the PR body. Additionally: no `--locked` may be dropped, and the `wasm-pack` pin
must remain `v0.15.0` in exactly one place.

---

### PR 7 — `fix/release-workflow-residues`

Take `release.yml` onto PR 6's `_build-wasm` and `_cross-compile` workflows; add
`-- --locked` to the wasm-pack build (and to the other five sites); add a
tag-vs-`Cargo.toml`-version guard; add `--verify-tag` to `gh release create`;
add a `workflow_dispatch` `dry_run` input that runs everything except
`gh release create`. Update `RELEASE.md`: state that the workflow has never
produced a release, and document the dry-run as the pre-flight step.

*Why here:* it depends on PR 6's reusable workflows, and it is the only way to
get evidence about a workflow that has never run.
*Coverage risk:* moderate in the opposite direction — this is a workflow with
**zero** prior runs, so there is no baseline to compare against.
*Evidence required:* one successful `workflow_dispatch` dry run, with its job
log attached. Without that, this PR is a hypothesis. Adding `-- --locked` to
`wasm-pack` is a real risk if the lockfile ever disagrees with the manifest — a
green `ci.yml` proves it does not today.

---

### PR 8 — `chore/delete-packages-impresspress-web` *(one service worker)*

Delete `packages/impresspress-web` (worker, index, update, the vite-app fixture)
and leave `crates/impresspress-bundle/assets/sw.js.tmpl` as the single service
worker. State in the PR body that the package has zero in-repo consumers, does
not currently build (`tsconfig.json` `rootDir: "src"` vs
`build:wasm` emitting to `dist/wasm`, TS2307), calls `wasmInitialize()` with no
argument against a signature that requires one, and runs in no CI job — and
raise the npm deprecation decision for `impresspress-web@0.2.0` as a question
for the user.

*Why independent:* it touches nothing any other PR touches.
*Coverage risk:* **low, but it must be said out loud** that deleting
`src/update.test.ts` removes a test — one that no CI job runs today, so nothing
measurable is lost. Do not let it vanish silently.
*Evidence:* a repo-wide search for `packages/impresspress-web` outside the
package itself returns nothing but the fixture.
*Do not* attempt to merge the two workers. `sw.js.tmpl` is load-bearing for
`blocks/dev/export.rs:437-480`'s exact-string rewrite, ~30 assertions in
`bundle_integration.rs`, `dev_export.rs:317-618` and `smoke.spec.ts`.

---

### PR 9 — `refactor/sdk-one-http-client-and-honest-types`

Two halves that could be split again if the diff gets large:

**(a) One request client.** Fold `requestFormData` and `requestBlob` into
`HttpClient` (raw/`FormData` body, `responseType: 'blob'`, explicit method);
delete the second `buildQueryString`; give the six services one shared
`HttpClient` so `client.ts:49-56` and `:65-72`'s twin fan-outs collapse to one
assignment; throw `ImpresspressError` with machine-readable codes at the 10 bare
`Error` sites, reusing the existing `"aborted"`/`"timeout"` vocabulary.

**(b) Honest types.** Delete `src/types/generated/database.ts` and the unused
`src/types/{auth,iam,storage}.ts`, keeping only what `src/services/` imports
(`IAMRole` moves next to `iam.service.ts`); fix `Extension`
(`extensions.service.ts:3-15`) against `admin/mod.rs:672-687`; fix
`README.md:50`; rewrite the three stale cases in `types-compatibility.test.ts`.

*Why here:* independent of all CI work, and it must land before PR 10 so the
freshness check has a small, honest surface to check.
*Coverage risk:* real but bounded. Unifying the clients changes upload/download
behaviour (a 30 s default timeout where there was none) — the blob/form path
needs an explicit longer or opt-out timeout, stated in the PR. Deleting types is
a **breaking change** for any external consumer of `@impresspress/sdk`.
*Evidence:* `npm run build && npm run lint && npm test && npm run
test:pack-install` green; a before/after list of the deleted exports in the PR
body; and an explicit note that `.eslintrc.json:19`'s `generated/` exclusion can
go with the directory.

---

### PR 10 — `feat/ci-sdk-type-freshness` *(the check, and what it needs first)*

Add `.output(response_schema_of::<T>)` to the undescribed endpoints the SDK
calls — the auth password/verify routes, `GET /b/admin/api/extensions`, the six
storage routes, and the `/b/cloudstorage/*` surface — regenerating the affected
`*.openapi.json` snapshots. Then add a generator step (`openapi-typescript` over
a merged document assembled from the ten snapshot `paths` fragments) and a CI
step that regenerates and diffs with `--exit-code`.

*Why last:* it depends on PR 9's smaller surface and on the endpoints declaring
schemas, and it is the only PR in the phase that adds a new gate.
*Coverage risk:* **subtle and important.** A freshness check over only the
already-described endpoints reports green on exactly the endpoints where drift
is possible-but-invisible — which is the precise mechanism by which PR #22's
seven-response-body reshape passed both snapshot gates (`1ccbb452`'s commit
message says so). Adding the check without adding the schemas would create a
gate that certifies the wrong thing.
*Evidence required:* the PR body must state **how many of the SDK's call sites
the check covers**, before and after, and the number must go up. A deliberate
red test — reshape one described response body, watch the check fail, revert —
is the only proof the gate works.

---

### Ordering summary

```
1 close-fixed-findings ─┐
2 periphery-sweep ──────┤ (independent, any order)
                        │
3 drop-wafer-clones ────┤
4 path-filters ─────────┤ (pure deletion / strictly additive)
                        ▼
5 ci-main-matches-pr-gate   ← makes the two CI bodies equal
                        ▼
6 reusable-workflows        ← HIGHEST coverage risk; mechanical only after 5
                        ▼
7 release-residues          ← needs 6's reusable workflows

8 delete-packages-impresspress-web   (independent of 1-7)

9 sdk-one-client-and-honest-types    (independent of 1-8)
                        ▼
10 sdk-type-freshness-check          ← needs 9
```

### The PRs that could reduce coverage if done carelessly

| PR | Failure mode | The evidence that rules it out |
|---|---|---|
| **6 reusable-workflows** | A step vanishes into a parameterisation and nothing goes red | Flattened `(job, step)` list from the run-jobs API, PR run and merge run, before and after, diffed to empty. Attach both. |
| **10 sdk-freshness-check** | The gate certifies only the endpoints that already had schemas — the ones that could not drift invisibly anyway | Covered-call-site count before/after, plus a deliberate red test on a described response body |
| **3 drop-wafer-clones** | A job somewhere really does read `../wafer-run` and now resolves differently | Green `ci.yml` and `ci-main.yml`, plus identical `cargo tree` wafer resolution before and after |
| *(later, in the test-convention work)* | Folding a fault-injection fixture into `TestContext` deletes the assertion it existed to make — §7.5 lists seven | Per-fixture: quote the assertion the fixture carries and show the replacement still fails against unfixed code |

### Deliberately **not** in this split

- **The single Rust test convention (§7).** It is the largest item in the phase
  by far — 16 `MigrationTestCtx` files, 14 hand-rolled contexts, 337 inline test
  modules — and §7.5 shows at least seven places where the obvious move deletes
  an assertion. The fault injection the review asked for is **already folded
  in**, so the remaining work is convergence, not construction, and it does not
  belong in a CI-and-packaging phase. It wants its own phase, its own spec, and
  a per-fixture argument. If it must land here, it is at minimum four PRs:
  `TestContext` gains token-minting and a call log; retire `MigrationTestCtx`;
  delete the two true duplicates (`FailingGetContext`, `ConfigCtx`) and the two
  copied fakes (`SequencedStripeNetwork`, `MessageCapture`); document the
  must-keeps at their definitions so the next sweep does not try again.
- **`deploy-dev-sandbox.yml`'s guaranteed failure on the fork.** Adding
  `if: github.repository == 'impresspress/impresspress'` to the deploy step, or
  adding the fork's Cloudflare secrets, is a **user decision about the fork**,
  not a code fix. It costs ~13 minutes of runner time per merge today and
  produces a red X that means nothing. Flagged, not decided.
- **`sw-update.spec.ts`'s `make build`.** The spec cannot pass and runs nowhere.
  Fixing the recipe (to `wasm-pack build … && impresspress build --target web
  --release`) and wiring it into CI is a genuine coverage *increase*, but it is a
  new test lane, not a refactor — and the same is true of `vector.spec.ts` and
  the four `examples/tests/*.spec.ts` that CI never runs. Each needs its own
  proof that it passes before it is gated on. Worth its own small PR after PR 5.
- **The `justfile` / CI `RUSTFLAGS` divergence** (`+simd128` locally, absent
  everywhere else). One line either way, but it changes what artefact developers
  build; it wants a deliberate answer, not a drive-by.
- **Node 20 → 22 consolidation** and the `actions/checkout@v4` Node-20
  deprecation annotations. Mechanical, but it belongs with PR 6's composite
  actions rather than as a scattered 16-site edit.
