# Build Sandboxes E — Deployment and Docs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One Cloudflare Worker per seed — `impresspress-build-bootstrap` at `build-bootstrap.impresspress.org` beside the existing blank one — deployable by hand and by the workflow matrix, and documented for visitors and maintainers.

**Architecture:** `wrangler.toml` gains an `[env.bootstrap]` environment with its own `name` and `routes`; `[assets]` stays `./dist`, so a deploy is always "build this seed into `dist/`, then deploy this environment". `deploy-dev-sandbox.yml` becomes a matrix over seeds. `docs/dev-sandbox.md` gains a "Templates" section and the example README documents seeds, `--seed`, `--out` and the per-environment deploy.

**Tech Stack:** wrangler v4, GitHub Actions, Markdown.

**Spec:** `docs/superpowers/specs/2026-09-30-build-sandboxes-design.md` §8, §12 (PR E), §14.

## Global Constraints

- Branch from `main` after Plans B and C have merged (the docs describe what they shipped).
- Worker names and domains: blank → `impresspress-dev-sandbox` / `dev.impresspress.org` (unchanged); bootstrap → `impresspress-build-bootstrap` / `build-bootstrap.impresspress.org`.
- Creating the Worker and its custom domain on Cloudflare is a new cloud resource: it is done by the user, or with an explicit go-ahead at that step (spec §8.3, §14). Nothing in this plan creates one silently.
- The workflow's missing secrets and the manual dev-test deploy practice are unchanged (spec §3).
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; PR, never direct to `main`.

## Review Focus

1. `wrangler deploy --env bootstrap` picking up a `dist/` built from the blank seed — nothing in wrangler can tell; the README's deploy recipe puts the build and the deploy on adjacent lines with the same seed name, and the workflow uses `${{ matrix.seed }}` for both. (Task 1 step 4, Task 2 step 1.)
2. `[assets]` not inherited by the environment — the dry run in Task 1 step 2 decides whether the block is repeated; either outcome is written into the comment.
3. Two matrix jobs racing to upload the same compiler-dist cache — they fetch a published release asset, not a cache (`fetch-dist.sh`), so there is nothing to race on. (Task 2, step 1 comment.)
4. A visitor on `dev.impresspress.org` reading the new docs and expecting Bootstrap — the Templates section names which sandbox ships what, and the workspace page names the template (Plan B). (Task 3, step 2.)
5. A maintainer bumping Bootstrap and forgetting the manifest — `build.sh --check` and CI fail on the pin/manifest mismatch; the README's bump recipe lists the three commands in order. (Task 3, step 1.)

---

### Task 1: The wrangler environment

**Files:**
- Modify: `examples/dev-sandbox/wrangler.toml`

- [ ] **Step 1: Add the environment**

Append to `examples/dev-sandbox/wrangler.toml`:
```toml

# The bootstrap-seeded sandbox (`build.sh --seed bootstrap`), its own Worker
# on its own domain. An environment shares `main` and `compatibility_date`
# with the top level; `name` and `routes` are per environment. `[assets]`
# still points at `./dist`, so a deploy is always "build THIS seed into
# dist/, then deploy THIS environment" — the two lines the README keeps
# together, and the matrix in deploy-dev-sandbox.yml keys on one variable.
[env.bootstrap]
name = "impresspress-build-bootstrap"
routes = [{ pattern = "build-bootstrap.impresspress.org", custom_domain = true }]
```

- [ ] **Step 2: Decide whether `[assets]` must be repeated**

```bash
mkdir -p examples/dev-sandbox/dist && touch examples/dev-sandbox/dist/index.html
cd examples/dev-sandbox && npx wrangler@4 deploy --dry-run --env bootstrap 2>&1 | tail -20; cd -
```
Read the output. If it reports the assets directory (a line like `Total Upload` / `Uploading … assets` or the `[assets]` binding `ASSETS`), inheritance works and nothing more is needed. If it warns that `env.bootstrap` has no `assets` or that `ASSETS` is unbound, add under `[env.bootstrap]`:
```toml
[env.bootstrap.assets]
directory = "./dist"
binding = "ASSETS"
not_found_handling = "single-page-application"
```
and change the comment's `[assets]` sentence to "`[assets]` is repeated because wrangler does not inherit it into an environment (verified with `--dry-run`)". Remove the placeholder `dist/index.html` if you created it (`rm -r examples/dev-sandbox/dist` only if `dist/` did not exist before this step).

- [ ] **Step 3: Both environments dry-run clean**

```bash
cd examples/dev-sandbox && npx wrangler@4 deploy --dry-run 2>&1 | grep -E 'impresspress-dev-sandbox|dev.impresspress.org' ; npx wrangler@4 deploy --dry-run --env bootstrap 2>&1 | grep -E 'impresspress-build-bootstrap|build-bootstrap.impresspress.org'; cd -
```
Expected: the top-level run names the blank Worker and domain; the env run names the bootstrap ones.

- [ ] **Step 4: Commit**

```bash
git add examples/dev-sandbox/wrangler.toml
git commit -m "dev-sandbox: wrangler environment for the bootstrap sandbox

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: The workflow matrix

**Files:**
- Modify: `.github/workflows/deploy-dev-sandbox.yml`

- [ ] **Step 1: One job per seed**

Replace the `jobs:` block's header and the two build/deploy steps so the job reads:
```yaml
jobs:
  deploy:
    runs-on: ubuntu-latest
    strategy:
      # Every seed is its own Worker; one failing must not cancel the other.
      fail-fast: false
      matrix:
        seed: [blank, bootstrap]
    steps:
      … (checkout, toolchain, wasm-pack, pre-build, install CLI, node, fetch-dist — unchanged) …

      # `compiler_is_current` matches `compiler/PIN.json` against the tree the
      # step above put in place, so this builds the wasm and assembles the
      # bundle for THIS seed without touching the compiler. Each matrix job
      # fetches the same published release asset — no cache, nothing to race.
      - name: Build examples/dev-sandbox (${{ matrix.seed }} seed)
        run: examples/dev-sandbox/build.sh --seed ${{ matrix.seed }}

      - name: Deploy to Cloudflare Workers (${{ matrix.seed }})
        # Pinned to v3's tagged commit, not the floating major tag — this
        # job holds CLOUDFLARE_API_TOKEN, the same reasoning as the
        # wasm-pack-action pin above ("live push-to-main infra").
        uses: cloudflare/wrangler-action@9acf94ace14e7dc412b076f2c5c20b8ce93c79cd  # v3
        with:
          workingDirectory: examples/dev-sandbox
          # The blank seed is the top-level wrangler config; every other
          # seed is the environment of the same name (wrangler.toml).
          command: ${{ matrix.seed == 'blank' && 'deploy' || format('deploy --env {0}', matrix.seed) }}
          apiToken: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          accountId: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
```
Leave `concurrency` as it is: the matrix jobs of one run share the group without cancelling each other; only a newer push cancels an older run.

- [ ] **Step 2: Lint the workflow**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/deploy-dev-sandbox.yml')); print('yaml ok')"
command -v actionlint >/dev/null && actionlint .github/workflows/deploy-dev-sandbox.yml || echo "actionlint not installed — skipped"
```
Expected: `yaml ok` (and no actionlint findings if it is installed).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/deploy-dev-sandbox.yml
git commit -m "ci: deploy one sandbox Worker per seed

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Docs — the README, the visitor guide

**Files:**
- Modify: `examples/dev-sandbox/README.md`
- Modify: `docs/dev-sandbox.md`

- [ ] **Step 1: README**

Add a `## Seeds` section after the intro paragraphs:

```markdown
## Seeds

A seed is what a fresh origin boots with (generation 0). Each lives under
`seeds/<name>/`:

| Seed        | Sandbox                              | Ships                                                                 |
|-------------|--------------------------------------|-----------------------------------------------------------------------|
| `blank`     | https://dev.impresspress.org         | A minimal welcome page and stylesheet; the site guide without a framework |
| `bootstrap` | https://build-bootstrap.impresspress.org | Bootstrap 5.3.8 vendored under `site/vendor/bootstrap/`; a Bootstrap-built welcome page; the site guide for the framework and the shop pieces |

- `site/**` — the site files. `manifest.json` is **generated** from them by
  `seeds/write-manifest.py <name>`; run it after every edit and commit both.
- `sandbox.json` + `guide.md` — the template name, the prompt the workspace
  page suggests, and the site-authoring guide `dev_read_reference` serves as
  `site_markdown`. The generator puts them in the manifest's `sandbox` block.
- `vendor.json` (bootstrap) — upstream URLs and sha256 pins of the vendored
  files. `seeds/vendor.py bootstrap` downloads and verifies them; the
  vendored bytes are identical to upstream.
- `seeds/check-seeds.py` — what `build.sh --check` runs: every manifest
  equals what the generator would write, and every vendored file matches its
  pin.

**Bumping Bootstrap:** edit `version` and the three URLs in
`seeds/bootstrap/vendor.json`, then

```sh
seeds/vendor.py bootstrap --refresh      # downloads, rewrites the sha256 pins
seeds/write-manifest.py bootstrap        # regenerates the manifest
build.sh --check                         # proves the three agree
```

and commit `vendor.json`, `manifest.json` and the vendored files together.
Update the version the guide and the welcome page name (`guide.md`,
`site/index.html`) — `dev-bootstrap.spec.ts` asserts the guide names the
vendored version.
```

In `## Build`, after the build command block, add:
```markdown
`build.sh --seed bootstrap` builds the bootstrap seed into `dist/` instead;
`--out DIR` moves the finished bundle to `DIR` so `dist/` stays free for the
next seed (CI builds bootstrap, moves it aside, then builds blank).
```

In `## Deploying`, replace the one-Worker description with:
```markdown
Each seed is its own static-assets Worker, from the same `wrangler.toml`:
the blank seed is the top-level config (`impresspress-dev-sandbox`,
`dev.impresspress.org`); every other seed is the wrangler *environment* of
the same name (`[env.bootstrap]` → `impresspress-build-bootstrap`,
`build-bootstrap.impresspress.org`). A deploy is always the build and the
deploy of ONE seed, back to back:

```sh
examples/dev-sandbox/build.sh --seed bootstrap
cd examples/dev-sandbox && wrangler deploy --env bootstrap
```

(`build.sh` and `wrangler deploy` for the blank one.) `deploy-dev-sandbox`
runs that pair once per seed as a matrix.
```
and add to the **One-time setup** list:
```markdown
4. A custom domain `build-bootstrap.impresspress.org` attached to the
   `impresspress-build-bootstrap` worker, the same way as step 2. The first
   deploy of a new Worker must be `wrangler deploy` (not `versions upload`).
```
Update the "Live URL" line to list both.

- [ ] **Step 2: The visitor guide**

In `docs/dev-sandbox.md`, after "## Opening it with an agent", add:

```markdown
## Templates

A sandbox is seeded from a **template**: what generation 0 holds, and the
site-authoring guide `dev_read_reference` serves as `site_markdown`. Each
template is its own sandbox; pick one by opening its address.

| Template | Sandbox | What it ships |
|---|---|---|
| **bootstrap** (default) | https://build-bootstrap.impresspress.org | Bootstrap 5 vendored under `site/vendor/bootstrap/`, a Bootstrap-built welcome page, a guide to the framework and the shop pieces |
| blank | https://dev.impresspress.org | A minimal welcome page and stylesheet, the same guide without a framework |

https://impresspress.org/build always opens the default. `dev_status`
reports the template's name, and the workspace page names it beside its
suggested prompt. There is no switching templates inside a workspace: a
template is a starting point, and everything after it is yours.
```
Change the intro sentence "The page includes a "Suggested prompt" you can copy and paste, which walks an agent through building a small shop end to end" to "The page includes a "Suggested prompt" you can copy and paste — each template's own — which walks an agent through building a small shop end to end". In "Known limits", add "- No switching templates inside a workspace; each template is its own sandbox."

- [ ] **Step 3: Commit**

```bash
git add examples/dev-sandbox/README.md docs/dev-sandbox.md
git commit -m "docs: seeds, per-seed deploys, and the templates a visitor can pick

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: The PR, and the deploy that needs a go-ahead

- [ ] **Step 1: PR**

```bash
git push -u origin HEAD
gh pr create --title "dev-sandbox: one Worker per seed, and the docs for templates" --body "$(cat <<'EOF'
Plan E of the build-sandboxes design (docs/superpowers/specs/2026-09-30-build-sandboxes-design.md §8, §12).

- `wrangler.toml`: `[env.bootstrap]` → `impresspress-build-bootstrap` at `build-bootstrap.impresspress.org`.
- `deploy-dev-sandbox.yml`: a matrix over `seed: [blank, bootstrap]`, each job building and deploying its own seed.
- README: seeds, the bump recipe, `--seed`/`--out`, per-environment deploys and the one-time domain setup. `docs/dev-sandbox.md`: a Templates section.

Not done here: creating the `impresspress-build-bootstrap` Worker and its custom domain on Cloudflare (spec §8.3) — a new cloud resource, for the user to create or approve.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 2: The first deploy — stop and ask**

This is the one step that creates a cloud resource. Report to the user: the PR is open; the first deploy needs `wrangler deploy --env bootstrap` from a bootstrap build on an account logged in to the `impresspress.org` zone, then the custom domain attached (README one-time setup step 4). Do not run it without an explicit yes. Once it answers, verify:
```bash
curl -sI https://build-bootstrap.impresspress.org/ | head -1
curl -s https://build-bootstrap.impresspress.org/seed/manifest.json | python3 -c "import json,sys; m=json.load(sys.stdin); print(m['sandbox']['template'], len(m['site']))"
curl -sI https://build-bootstrap.impresspress.org/vendor/bootstrap/bootstrap.min.css | grep -i content-type
```
Expected: `HTTP/2 200`, `bootstrap 4`, and — note — the static host's content type for the vendored file at the `/vendor/…` URL is the host's, not the runtime's; only the service-worker-served site serves the manifest's type. The e2e in Plan C is where that is proved.
