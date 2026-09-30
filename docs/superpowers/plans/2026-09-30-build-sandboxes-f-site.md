# Build Sandboxes F — impresspress.org Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** impresspress.org tells a browser agent where to build a website: a "Build a website" docs page that lands in `llms.txt`, one sentence in the `llms.txt` intro, and `/build` → the default sandbox, `/templates` → the docs page.

**Architecture:** The marketing site is the `impresspress/site` GitHub repo (Vite + Preact + Tailwind; not in this workspace — cloned for this plan). Docs pages are `src/content/docs/<slug>.md` + a `DOCS_NAV` entry + `docs/<slug>/index.html` + `src/pages/docs-<slug>.jsx` + a Vite input; `scripts/ai-files.mjs` generates `llms.txt`, `llms-full.txt` and `/docs/<slug>.md` from them at build time. Redirects are a `public/_redirects` file, which Vite copies to `dist/` and Cloudflare honours.

**Tech Stack:** Node 22, Vite, Preact, `node --test`.

**Spec:** `docs/superpowers/specs/2026-09-30-build-sandboxes-design.md` §9, §12 (PR F), §14.

## Global Constraints

- Lands only after Plan E is deployed and `https://build-bootstrap.impresspress.org/` answers 200 — the link must never 404.
- The repo's rules: docs content has no H1 (the layout renders the title); no `solobase` anywhere; `npm test` (node:test) must pass; every `DOCS_NAV` entry has a content file with a frontmatter `title` and a `desc`.
- Redirect targets: `/build` → `https://build-bootstrap.impresspress.org` (302, the default may move); `/templates` → `/docs/build-a-website/` (301).
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; PR against `impresspress/site`'s default branch, never direct.

## Review Focus

1. The host does not honour `public/_redirects` — the post-deploy check in Task 4 catches it; the fallback is a Cloudflare zone redirect rule (spec §9), done by hand and recorded in the site README.
2. An agent that reads `/llms.txt` and not the docs page — the intro sentence names `/build` directly, so the page is not on the critical path. (Task 3, step 1 + its test.)
3. The page promising a WebMCP browser to a visitor who has none — the Requirements section says which browsers, in the same words as `docs/dev-sandbox.md`. (Task 2, step 1.)
4. The credentials on a public docs page — they are already on `demo.impresspress.org`'s llms line and on every sandbox landing page; the page repeats why they are safe. (Task 2, step 1.)
5. `docs/build-a-website.md` missing from `ai-files.test.mjs`'s expected list — the test fails until it is added, which is the point. (Task 2, step 3.)

---

### Task 1: A clone to work in

- [ ] **Step 1: Clone beside the worktrees**

```bash
git clone git@github.com:impresspress/site.git /home/joris/Programs/suppers-ai/workspace/impresspress-worktrees/site
cd /home/joris/Programs/suppers-ai/workspace/impresspress-worktrees/site
git checkout -b feat/build-a-website
npm ci
npm test
```
Expected: the existing tests pass (`ai-files`, `content`, `hero-sprite`, `how-it-works`, `ico`, `pixel`).

---

### Task 2: The docs page

**Files:**
- Create: `src/content/docs/build-a-website.md`
- Modify: `src/data/docs.js` (`DOCS_NAV`)
- Create: `docs/build-a-website/index.html`
- Create: `src/pages/docs-build-a-website.jsx`
- Modify: `vite.config.js` (input)
- Modify: `scripts/ai-files.test.mjs` (expected file list)

- [ ] **Step 1: The content**

`src/content/docs/build-a-website.md`:
```markdown
---
title: Build a website with an agent
---

A **build sandbox** is a copy of Impresspress that runs entirely in your browser, seeded with a website template and the tools an AI agent needs to build on it. Open one in a WebMCP-capable browser, hand the page to your agent, and it can write pages, compile Rust backend blocks, stock a shop, and export the result as a static bundle. Nothing leaves your browser.

## Start here

Open **[impresspress.org/build](/build)** — it takes you to the current default template. Sign in with the credentials shown on the landing page (`admin@example.com` / `admin123`; every visitor gets a private, throwaway instance, which is why they are public) and tell your agent to:

1. call `dev_status`,
2. call `dev_read_reference` and read its `site_markdown` before writing any file,
3. build the site with `dev_write_files` and `dev_write_file`, stock the shop with the `shop_*` tools, and `dev_export` when done.

A prompt that does all of that is on the workspace page under "Suggested prompt".

## Templates

| Template | Sandbox | What it ships |
|---|---|---|
| **bootstrap** (default) | [build-bootstrap.impresspress.org](https://build-bootstrap.impresspress.org) | Bootstrap 5, vendored; a Bootstrap-built welcome page; a guide to the framework, the shop pieces and the workspace |
| blank | [dev.impresspress.org](https://dev.impresspress.org) | A minimal welcome page and stylesheet; the same guide without a framework |

Each template is its own sandbox. Pick one by opening its address; there is no switching inside a workspace.

## Requirements

A Chromium-based browser with WebMCP support. The sandbox is cross-origin isolated (that is what lets it compile Rust in the browser), which Safari does not support; Firefox is untested.

## What happens to your work

Everything is stored in your browser's storage for that sandbox. Clearing the site's data throws it away — **export first** if you want to keep it. The exported zip is a static bundle you can serve from any file host; the workspace's own guide explains what it contains.
```

- [ ] **Step 2: Navigation, page, input**

`src/data/docs.js` — append to `DOCS_NAV`:
```js
  {
    slug: "build-a-website",
    label: "Build a website",
    href: "/docs/build-a-website/",
    desc: "Point a browser agent at a build sandbox and get a site, backend blocks and a shop — all in the browser.",
  },
```
`docs/build-a-website/index.html` — a copy of `docs/quickstart/index.html` with: `<title>Build a website with an agent — Impresspress docs</title>`, the `description` and `og:description` metas set to the `desc` above, `og:title` to the title, `og:url` to `https://impresspress.org/docs/build-a-website/`, and the script `src="/src/pages/docs-build-a-website.jsx"`.

`src/pages/docs-build-a-website.jsx`:
```jsx
import { render } from "preact";
import "../css/main.css";
import DocsLayout from "../components/DocsLayout";
import Markdown from "../components/Markdown";
import { parseFrontmatter } from "../lib/frontmatter";
import md from "../content/docs/build-a-website.md?raw";

const { data } = parseFrontmatter(md);

render(
  <DocsLayout active="build-a-website" title={data.title} markdown={md}>
    <Markdown src={md} />
  </DocsLayout>,
  document.getElementById("app")
);
```
`vite.config.js` — add to `rollupOptions.input`: `docsBuildAWebsite: resolve(root, "docs/build-a-website/index.html"),`.

- [ ] **Step 3: The generator test knows the page**

In `scripts/ai-files.test.mjs`, add `"docs/build-a-website.md",` to the `expected` list in `emits every expected file`, and add:
```js
test("the build-a-website page is in the llms.txt docs list and inlined in llms-full.txt", () => {
  assert.match(files["llms.txt"], /\]\(https:\/\/impresspress\.org\/docs\/build-a-website\.md\): /);
  assert.ok(files["llms-full.txt"].includes("\n# Build a website with an agent\n"));
});
```
Run: `npm test`
Expected: pass (`content.test.mjs` checks the new entry's file, title, desc and no-H1 rule).

- [ ] **Step 4: Build and look**

```bash
npm run build
ls dist/docs/build-a-website/index.html dist/docs/build-a-website.md
grep -c 'build-a-website' dist/llms.txt
```
Expected: both files exist; `1`.

- [ ] **Step 5: Commit**

```bash
git add src/content/docs/build-a-website.md src/data/docs.js docs/build-a-website src/pages/docs-build-a-website.jsx vite.config.js scripts/ai-files.test.mjs
git commit -m "docs: build a website with an agent — the build sandboxes

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: The `llms.txt` sentence and the redirects

**Files:**
- Modify: `scripts/ai-files.mjs` (`llmsTxt`)
- Modify: `scripts/ai-files.test.mjs`
- Create: `public/_redirects`

- [ ] **Step 1: The sentence, test first**

Add to `scripts/ai-files.test.mjs`:
```js
test("llms.txt tells an agent where to build a website", () => {
  assert.match(files["llms.txt"], /start at https:\/\/impresspress\.org\/build\b/);
});
```
Run `npm test` — expected: this one fails. Then in `scripts/ai-files.mjs`, in `llmsTxt`, after the paragraph beginning `Everything then runs on http://localhost:8090`, add a paragraph:
```
To build a website with a browser agent instead, start at ${LINKS.site}/build — it opens a browser-local sandbox seeded with a template (see "Build a website with an agent" below).
```
Run `npm test` — expected: pass.

- [ ] **Step 2: The redirects**

`public/_redirects`:
```
# The default build sandbox (302: the default template may move) and the
# templates listing (301: it is the docs page).
/build      https://build-bootstrap.impresspress.org   302
/templates  /docs/build-a-website/                     301
```
```bash
npm run build && cat dist/_redirects
```
Expected: the file is copied into `dist/` verbatim.

- [ ] **Step 3: Commit and PR**

```bash
git add scripts/ai-files.mjs scripts/ai-files.test.mjs public/_redirects
git commit -m "site: /build and /templates redirects; llms.txt points agents at the build sandbox

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push -u origin feat/build-a-website
gh pr create --repo impresspress/site --title "Build a website with an agent: docs page, llms.txt, /build redirect" --body "$(cat <<'EOF'
Plan F of the build-sandboxes design (impresspress/impresspress: docs/superpowers/specs/2026-09-30-build-sandboxes-design.md §9).

- New docs page "Build a website with an agent" (in `llms.txt` and at `/docs/build-a-website.md` via the generator).
- One sentence in the `llms.txt` intro pointing at `https://impresspress.org/build`.
- `public/_redirects`: `/build` → the default sandbox (302), `/templates` → the docs page (301).

Depends on `build-bootstrap.impresspress.org` being live (it is — see the deploy PR in impresspress/impresspress).

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

### Task 4: After the merge deploys

- [ ] **Step 1: Verify on the live site**

```bash
curl -sI https://impresspress.org/build | grep -i -E '^HTTP|^location'
curl -sI https://impresspress.org/templates | grep -i -E '^HTTP|^location'
curl -s https://impresspress.org/llms.txt | grep -n 'impresspress.org/build'
curl -s -o /dev/null -w '%{http_code}\n' https://impresspress.org/docs/build-a-website.md
```
Expected: `302` + `location: https://build-bootstrap.impresspress.org`; `301` + `location: /docs/build-a-website/`; one matching line; `200`.

- [ ] **Step 2: If the redirects did not take**

The host is not honouring `_redirects`. Add a Cloudflare zone redirect rule for the two paths by hand (the account that owns the `impresspress.org` zone), note it under a "Redirects" heading in the site README (so the next person knows they are not in the repo), and re-run step 1.
