import { test, expect, type BrowserContext, type Page } from '@playwright/test';

import { bootServiceWorker, loginAdmin, loginToWorkspace } from './fixtures/dev-sandbox';
import { execute, registeredTools, structured, toolNames, waitForTool } from './fixtures/webmcp-helpers';

/**
 * The checkpoint: a Rust block written, compiled, served and rolled back
 * without a server anywhere.
 *
 * Every other dev spec substitutes something. `dev-compiler.spec.ts` drives
 * the adapter against `fixtures/fake-compiler-worker.js` (and starts the real
 * worker once, to prove COEP lets it run at all); `dev-compile-tool.spec.ts`
 * runs the real `dev_compile_block` against a fake worker holding an artifact
 * `cargo` built on the HOST. This file substitutes nothing: rubrc's composed
 * toolchain — rustc, cargo and LLVM as wasm, 75 MiB of it — downloads into the
 * page, compiles the `table` template's newsletter block, and what it produces
 * is staged, validated, activated and served.
 *
 * That is why it is a job of its own (`e2e-dev-compile`) rather than another
 * file in `e2e-dev-sandbox`'s list: it needs `examples/dev-sandbox/compiler/
 * dist/`, which is ~55 minutes and 12.6 GB of RSS to build and is therefore
 * cached on `PIN.json` in CI (`compiler/README.md`).
 *
 * # What is asserted, and why each one is here
 *
 * 1. The reference an agent reads before writing Rust names the call the
 *    template makes, so "read the docs, then use them" is one story.
 * 2. The block compiles IN THE BROWSER and its table is created — not by a
 *    migration file, but by `db::ensure_table` inside the guest's `init`, on
 *    the collection the block claimed.
 * 3. Its curated tool reaches an anonymous visitor through the deployment
 *    manifest, and a call to it writes a row. This is the point of the whole
 *    sandbox: a block an agent wrote minutes ago is a tool another agent can
 *    use, with no deploy step in between.
 * 4. RECOMPILING it works — the case a browser agent hit and no test covered:
 *    a block that declares an agent tool used to collide with its own tool
 *    name on its second compile, and the only way out was to remove it.
 * 5. A broken edit is an ANSWER — a diagnostic with a line number — and the
 *    previously compiled block keeps serving. So is a crate that compiles and
 *    then fails to LINK, which this toolchain reports only as text.
 * 6. Rolling back to the generation before the compile removes the block, its
 *    route and its tool.
 *
 * # Why the visitor shares this browser context
 *
 * A Playwright context is an isolated storage partition: `browser.newContext()`
 * would get its own OPFS, boot its own sandbox from the seed, and never see a
 * block compiled in this one. So the visitor is a second PAGE in the same
 * context, and anonymity is arranged by clearing the context's cookies — which
 * also signs the admin page out, hence the second `loginAdmin` before the
 * rollback steps. See [`becomeVisitor`].
 */

/** The block this test builds. */
const BLOCK = 'newsletter';

/**
 * The routes the `table` template declares, verbatim
 * (`blocks/dev/templates/table/src/lib.rs`). The template is instantiated
 * under the block's own name, and this block is scaffolded as `newsletter`,
 * which is the name the template is written under — so these are unrewritten.
 */
const SUBSCRIBE = '/b/newsletter/subscribe';
const SUBSCRIBERS = '/b/newsletter/subscribers';

/** The one endpoint the template opts into `.agent_tool(..)`. */
const TOOL = 'subscribe_newsletter';

/**
 * The refusal a duplicate subscribe gets, verbatim from the `table` template,
 * and what the recompile in step 7 edits it to.
 *
 * A string literal is the smallest real source edit, and this one is the
 * smallest OBSERVABLE one: reading it back through the route proves the
 * artifact that just compiled is the one serving, and getting it at all proves
 * the block's table and rows outlived the recompile.
 */
const DUPLICATE = 'that address is already subscribed';
const DUPLICATE_EDITED = 'that address is already on the list';

/** Who subscribes, and through which door. */
const BY_ADMIN = 'admin@newsletter.test';
const BY_VISITOR = 'visitor@newsletter.test';

/**
 * The statement the broken edit removes a semicolon from, exactly as
 * `templates/table/src/lib.rs` writes it (one occurrence, in `subscribe`).
 *
 * A trailing edit (`content + '\nFAIL'`) would also fail to compile, but it
 * would prove less: the interesting claim is that rustc's position for an
 * error in the MIDDLE of a file survives the whole path — cargo's JSON, the
 * worker's transcript parse, the adapter, `stageDiagnostic`, the tool result —
 * so an agent is told where to look rather than that something, somewhere, is
 * wrong.
 */
const INTACT = 'let email = email.trim();';
const BROKEN = 'let email = email.trim()';

/**
 * Records what the page sent to `POST /b/dev/api/builds/stage`.
 *
 * The artifact's size has no other source: `dev_compile_block` reports
 * timings and diagnostics, and the staged module is content-addressed,
 * not measured. It is read off the request the page makes anyway; the wrapper
 * only observes and forwards.
 *
 * `try`/`catch` around the whole observation because a probe that threw would
 * take the page's own `fetch` down with it.
 */
const STAGE_PROBE = `
  (function () {
    var probe = { artifactBytes: null, stages: 0 };
    window.__devStageProbe = probe;
    var realFetch = window.fetch;
    window.fetch = function (input, init) {
      try {
        var url = typeof input === 'string' ? input : (input && input.url) || '';
        if (url.indexOf('/b/dev/api/builds/stage') !== -1 && init && typeof init.body === 'string') {
          var body = JSON.parse(init.body);
          if (typeof body.artifact_base64 === 'string') {
            probe.artifactBytes = atob(body.artifact_base64).length;
            probe.stages += 1;
          }
        }
      } catch (e) {
        // Observing must never be able to fail the thing observed.
      }
      return realFetch.apply(this, arguments);
    };
  })();
`;

type FileEntry = { path: string; sha256: string; size: number };
type FileRead = FileEntry & { encoding: string; content: string };
type Reference = { wafer_guest_version: number; markdown: string };
type CreateBlock = { name: string; files: FileEntry[] };
type Generation = { id: string; cause: string; status: string; blocks: number };
type Diagnostic = {
  severity: string;
  code: string | null;
  message: string;
  file: string | null;
  line: number | null;
  column: number | null;
};
type Compile = {
  success: boolean;
  cancelled: boolean;
  build_id: string | null;
  generation: Generation | null;
  diagnostics: Diagnostic[];
  stdout: string;
  stderr: string;
  elapsed_ms: number;
  timings: { sources_ms: number; toolchain_ms: number; compile_ms: number; stage_ms: number };
  compiler_version: string | null;
  progress: Array<{ phase: string; ms: number }>;
};

/**
 * `POST /b/newsletter/subscribe`, from inside whichever document is passed.
 *
 * The body comes back as TEXT, here and in [`listSubscribers`], because both
 * are also called when the route is gone: a 404 from the router is not the
 * block's JSON, and `response.json()` would throw before the caller could
 * assert on the status it was actually checking for.
 */
async function subscribe(page: Page, email: string) {
  return page.evaluate(
    async ([url, address]) => {
      const response = await fetch(url, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ email: address }),
      });
      return { status: response.status, body: await response.text() };
    },
    [SUBSCRIBE, email] as const,
  );
}

/** `GET /b/newsletter/subscribers` — admin-only, so the status is part of the answer. */
async function listSubscribers(page: Page) {
  return page.evaluate(async (url) => {
    const response = await fetch(url);
    return { status: response.status, body: await response.text() };
  }, SUBSCRIBERS);
}

/** The subscriber e-mails, newest first, from an admin document. */
async function subscriberEmails(page: Page): Promise<string[]> {
  const listed = await listSubscribers(page);
  expect(listed.status, listed.body).toBe(200);
  return (JSON.parse(listed.body) as { subscribers: Array<{ email: string }> }).subscribers.map(
    (s) => s.email,
  );
}

test('an agent scaffolds, compiles and uses a Rust block end to end', async ({ page, context }) => {
  // The bill: a cold sandbox boot (wasm compile, OPFS create, migrations, seed
  // import), 75 MiB of toolchain into the page, THREE release builds of the
  // block (the compile, the recompile, and the one that fails), and the
  // activations that each rebuild the wasmi runtime. Measured at 1.3 minutes
  // for two builds on a 24-core box; twelve is the ceiling CI is given,
  // because a runner is slower and a cold `wasm-pack` cache is not this
  // test's to control.
  test.setTimeout(12 * 60 * 1000);

  // Nothing here drives the buttons that alert, but an unhandled dialog blocks
  // the page rather than failing it — a hang with no message is the worst
  // possible way for this to break.
  const dialogs: string[] = [];
  page.on('dialog', (dialog) => {
    dialogs.push(dialog.message());
    return dialog.dismiss();
  });

  await page.addInitScript(STAGE_PROBE);
  await loginToWorkspace(page);
  await waitForTool(page, 'dev_compile_block');

  // --- 1. The reference an agent reads first ------------------------------
  //
  // `reference_markdown` splices the templates into the guide at render time,
  // so this is not a doc file that may have drifted: the call named here is
  // the call the block below makes.
  const reference = structured<Reference>(await execute(page, 'dev_read_reference', {}));
  expect(reference.markdown).toContain('db::ensure_table');
  expect(reference.wafer_guest_version).toBeGreaterThan(0);

  // --- 2. Scaffold ---------------------------------------------------------
  //
  // The first block in the workspace is what starts the toolchain
  // (`warmCompiler` in `dev.js`), and it starts DURING this call: the listing
  // refresh that finds the block is awaited inside the tool before it
  // returns. So start-up is timed from before the call, not after it.
  const warmStarted = Date.now();
  const created = structured<CreateBlock>(
    await execute(page, 'dev_create_block', { name: BLOCK, template: 'table' }),
  );
  expect(created.files.map((f) => f.path)).toEqual([
    `blocks/${BLOCK}/Cargo.toml`,
    `blocks/${BLOCK}/src/lib.rs`,
  ]);
  // Source is not a deployment: nothing serves until it is compiled.
  expect((await listSubscribers(page)).status).toBe(404);

  // --- 3. Compile, in the browser -----------------------------------------
  //
  // Only once the toolchain has said it is up, so `first_compile_ms` below is
  // a compile against a started toolchain — what a visitor who scaffolds and
  // then edits for a few seconds actually waits for — and `ready_ms` is not
  // mixed into it. The budget is the download plus the start on a slow
  // runner.
  await expect(page.locator('#dev-log')).toContainText('compiler: ready', { timeout: 6 * 60 * 1000 });
  const readyMs = Date.now() - warmStarted;
  const compileStarted = Date.now();
  const compiled = structured<Compile>(await execute(page, 'dev_compile_block', { name: BLOCK }));
  const firstCompileMs = Date.now() - compileStarted;
  expect(compiled.success, JSON.stringify(compiled.diagnostics)).toBe(true);
  expect(compiled.cancelled).toBe(false);
  // `elapsed_ms` is the call's whole wall clock, so it cannot be less than
  // the build plus the activation it also waited for — the shape of the
  // `elapsed_ms: 813` an agent was once given for a fifty-second call.
  expect(compiled.timings.compile_ms).toBeGreaterThan(0);
  expect(compiled.timings.stage_ms).toBeGreaterThan(0);
  // Each figure is rounded to the millisecond on its own, so the comparisons
  // allow for that rounding and nothing more.
  expect(compiled.elapsed_ms).toBeGreaterThanOrEqual(
    compiled.timings.compile_ms + compiled.timings.stage_ms - 2,
  );
  expect(compiled.elapsed_ms).toBeLessThanOrEqual(firstCompileMs + 1);
  // The output is the build, not the toolchain's chatter: rubrc's cargo
  // prints two `DEBUG:` lines on every run and its rustc the whole wasm-ld
  // command line on every link, and neither reaches a build that worked.
  expect(compiled.stdout).not.toContain('DEBUG:');
  expect(compiled.stderr).not.toContain('DEBUG:');
  expect(compiled.stderr).not.toContain('Linking using');
  expect(compiled.stderr).toContain('Finished `release` profile');
  // No ERROR-severity diagnostic, rather than none at all: rustc at a future
  // pin may warn on the `table` template — a lint that turned on, an import
  // that stopped being needed — and a warning is a yellow checkpoint, not a
  // red one. The build succeeded; that is what this line is about.
  expect(
    compiled.diagnostics.filter((d) => d.severity === 'error'),
    JSON.stringify(compiled.diagnostics),
  ).toEqual([]);
  expect(compiled.build_id).toBeTruthy();
  expect(compiled.generation?.cause).toBe('block_compile');
  expect(compiled.generation?.status).toBe('active');
  // `rustc --version` as run inside the worker's own VFS. A page cannot
  // synthesize it, so this is the toolchain identifying itself.
  expect(compiled.compiler_version).toContain('rustc');
  // The block set changed, so the runtime was rebuilt — the step a site-only
  // write skips.
  expect(compiled.progress.map((p) => p.phase)).toEqual([
    'validating',
    'building_runtime',
    'publishing',
    'active',
  ]);

  // The numbers CI greps into its job summary.
  //
  // `ready_ms` is start-up — the toolchain (download included on a cold
  // cache), the `GET /b/dev/api/guest` it is started with and the one-time
  // build of that guest crate — measured from just before the scaffold call
  // that starts it to the page's `compiler: ready` log line. It includes the
  // part of the scaffold that runs before the warm-up begins, and the log line
  // is polled, so it can read late by both, never early. `compile_ms` is
  // `dev_compile_block`'s `timings.compile_ms`: the build in the worker, as
  // the page waited for it (cargo plus the shell round trips around it, the
  // message hops and the artifact's digest); `artifact_bytes` is
  // the module the page staged. `first_compile_ms` is the whole first
  // `dev_compile_block` call against the already started toolchain, timed
  // from here — `elapsed_ms` is the tool's own figure for the same wait.
  const probe = await page.evaluate(
    () =>
      (window as unknown as { __devStageProbe: { artifactBytes: number; stages: number } })
        .__devStageProbe,
  );
  expect(probe.stages).toBe(1);
  console.log(
    `dev-compile: ready_ms=${readyMs} compile_ms=${compiled.timings.compile_ms} ` +
      `artifact_bytes=${probe.artifactBytes} first_compile_ms=${firstCompileMs} ` +
      `elapsed_ms=${compiled.elapsed_ms}`,
  );

  // --- 4. The table exists, because the guest's `init` created it ----------
  //
  // No migration file, no DDL: the block claimed `site__newsletter__*`, which
  // is what turned on the `schema` capability, and `db::ensure_table` ran on
  // activation. A row going in and coming back out is the proof.
  const first = await subscribe(page, BY_ADMIN);
  expect(first.status, first.body).toBe(200);
  expect(JSON.parse(first.body)).toEqual({ ok: true });
  expect(await subscriberEmails(page)).toEqual([BY_ADMIN]);

  // --- 5. An anonymous visitor finds the tool and uses it ------------------
  const visitor = await becomeVisitor(context);

  // Once `webmcp.js` has registered anything, it has registered everything
  // this session's manifest carries.
  const visitorTools = (await registeredTools(visitor, 1)).map((t) => t.name);
  expect(visitorTools).toContain(TOOL);

  // …and only the public one. The template's two reads are `Auth::Admin`, so
  // the manifest this session was served must not offer them — and the
  // endpoint refuses anyway, which is what the next line checks. The manifest
  // filter is an affordance; the endpoint is the gate.
  expect((await listSubscribers(visitor)).status).not.toBe(200);

  const viaTool = await execute(visitor, TOOL, { email: BY_VISITOR });
  expect(viaTool.isError, viaTool.content[0]?.text).toBeFalsy();
  expect(viaTool.structuredContent).toEqual({ ok: true });

  // --- 6. Back as the admin ------------------------------------------------
  //
  // Signing in again re-cookies the whole context, the visitor page included.
  // That is fine for what is left of its job: the last thing asked of it is
  // that the tool is GONE, and a tool the manifest no longer carries is absent
  // at every auth level.
  await loginAdmin(page);
  await page.goto('/b/dev', { waitUntil: 'commit' });
  await expect(page.locator('#dev-progress-steps li').first()).toBeAttached({ timeout: 60_000 });
  await waitForTool(page, 'dev_compile_block');

  // The visitor's tool call really wrote a row, through the block, into the
  // table the guest created.
  expect(await subscriberEmails(page)).toEqual([BY_VISITOR, BY_ADMIN]);

  // --- 7. Recompiling keeps the block live, with its data and its tool ----
  //
  // The scenario an agent hit driving the live sandbox. Everything above is
  // this block's FIRST compile; this is its second. Staging seeded the "agent
  // tool name already claimed" set from the runtime's registered blocks —
  // which, after the rebuild in step 3, INCLUDE the live `site/newsletter` —
  // so a block that declares a tool collided with ITSELF on recompile and was
  // refused `tool-name-duplicate` naming `subscribe_newsletter`, a tool
  // nothing else has ever declared. The agent's only way forward was
  // `dev_remove_block`, which takes the block, its route and its tool offline
  // for the length of a full compile.
  const live = structured<FileRead>(
    await execute(page, 'dev_read_file', { path: `blocks/${BLOCK}/src/lib.rs` }),
  );
  const edited = live.content.replace(DUPLICATE, DUPLICATE_EDITED);
  // Same guard as the broken edit below: the template moving out from under
  // this test is worth its own failure, or the recompile would pass on bytes
  // it never changed.
  expect(edited, `"${DUPLICATE}" is no longer in the table template`).not.toBe(live.content);
  structured(
    await execute(page, 'dev_write_file', {
      path: `blocks/${BLOCK}/src/lib.rs`,
      content: edited,
      expected_sha256: live.sha256,
    }),
  );

  const recompiled = structured<Compile>(await execute(page, 'dev_compile_block', { name: BLOCK }));
  expect(recompiled.success, JSON.stringify(recompiled.diagnostics)).toBe(true);
  expect(
    recompiled.diagnostics.filter((d) => d.severity === 'error'),
    JSON.stringify(recompiled.diagnostics),
  ).toEqual([]);
  // A NEW generation, active — a recompile REPLACES the block rather than
  // joining the live set beside itself.
  expect(recompiled.generation?.cause).toBe('block_compile');
  expect(recompiled.generation?.status).toBe('active');
  expect(recompiled.generation?.id).not.toBe(compiled.generation?.id);
  expect(recompiled.generation?.blocks).toBe(1);

  // The route answers out of the artifact that just compiled: the address
  // step 4 subscribed is still a duplicate — so the table and its rows
  // outlived the recompile — and the refusal is the edited one.
  const duplicate = await subscribe(page, BY_ADMIN);
  expect(duplicate.status, duplicate.body).toBe(409);
  expect(duplicate.body).toContain(DUPLICATE_EDITED);
  expect(await subscriberEmails(page)).toEqual([BY_VISITOR, BY_ADMIN]);

  // --- 8. A broken edit is a diagnostic, not an outage --------------------
  const source = structured<FileRead>(
    await execute(page, 'dev_read_file', { path: `blocks/${BLOCK}/src/lib.rs` }),
  );
  const damaged = source.content.replace(INTACT, BROKEN);
  // The template moved out from under this test rather than the edit failing
  // to break anything — worth its own failure, because the compile below would
  // otherwise succeed and the assertions would blame the compiler.
  expect(damaged, `${INTACT} is no longer in the table template`).not.toBe(source.content);
  structured(
    await execute(page, 'dev_write_file', {
      path: `blocks/${BLOCK}/src/lib.rs`,
      content: damaged,
      expected_sha256: source.sha256,
    }),
  );

  const brokenBuild = structured<Compile>(
    await execute(page, 'dev_compile_block', { name: BLOCK }),
  );
  expect(brokenBuild.success).toBe(false);
  // Not a timeout and not a machinery failure: rustc read the file and had an
  // opinion about it.
  expect(brokenBuild.cancelled).toBe(false);
  expect(brokenBuild.build_id).toBeNull();
  expect(brokenBuild.generation).toBeNull();
  // The first ERROR rather than `diagnostics[0]`: cargo emits in its own
  // order, and a warning arriving ahead of the error would make an index a
  // coin flip. There is exactly one thing wrong with the file, so this is the
  // diagnostic about it.
  const error = brokenBuild.diagnostics.find((d) => d.severity === 'error');
  expect(error, JSON.stringify(brokenBuild.diagnostics)).toBeTruthy();
  expect(error?.file).toBe('src/lib.rs');
  expect(error?.line ?? 0).toBeGreaterThan(0);
  expect(error?.column ?? 0).toBeGreaterThan(0);
  // rustc's rendering of that error leads `stderr`, and cargo's `Finished` —
  // which the real cargo prints after rustc has failed — does not follow it.
  expect(brokenBuild.stderr).toMatch(/^error: /);
  expect(brokenBuild.stderr).toContain(`--> src/lib.rs:${error?.line}:${error?.column}`);
  expect(brokenBuild.stderr).not.toContain('Finished');
  expect(brokenBuild.stderr).not.toContain('DEBUG:');
  expect(brokenBuild.timings.stage_ms).toBe(0);
  console.log(
    `dev-compile: broken_compile_ms=${brokenBuild.timings.compile_ms} ` +
      `diagnostic=${error?.file}:${error?.line}:${error?.column} ${JSON.stringify(error?.message)}`,
  );

  // Nothing was staged, so the generation that compiled is still the live one
  // — routes, table and rows intact.
  expect(await subscriberEmails(page)).toEqual([BY_VISITOR, BY_ADMIN]);

  // --- 8b. A link failure is a diagnostic too ------------------------------
  //
  // rustc compiles this and wasm-ld then cannot find the library. In this
  // toolchain the linker's complaint is a plain line after rustc's
  // `Linking using …` command, not a JSON diagnostic, so it reaches the agent
  // as a `link-error` only because the adapter reads it out of the
  // transcript — and the command line, dropped from every build that linked,
  // is kept for the one that did not.
  const linkSource = structured<FileRead>(
    await execute(page, 'dev_read_file', { path: `blocks/${BLOCK}/src/lib.rs` }),
  );
  structured(
    await execute(page, 'dev_write_file', {
      path: `blocks/${BLOCK}/src/lib.rs`,
      content:
        linkSource.content.replace(BROKEN, INTACT) +
        '\n#[link(name = "doesnotexist")]\nextern "C" {\n    fn from_missing_lib() -> i32;\n}\n\n' +
        '#[no_mangle]\npub extern "C" fn use_missing_lib() -> i32 {\n    unsafe { from_missing_lib() }\n}\n',
      expected_sha256: linkSource.sha256,
    }),
  );
  const unlinked = structured<Compile>(await execute(page, 'dev_compile_block', { name: BLOCK }));
  expect(unlinked.success, JSON.stringify(unlinked.diagnostics)).toBe(false);
  expect(unlinked.diagnostics[0], JSON.stringify(unlinked.diagnostics)).toEqual({
    severity: 'error',
    code: 'link-error',
    message: 'wasm-ld: unable to find library -ldoesnotexist',
    file: null,
    line: null,
    column: null,
  });
  // …and only it: the worker's `artifact-missing` ("failed without a
  // diagnostic") would contradict it, so the adapter drops that one.
  expect(unlinked.diagnostics.map((d) => d.code)).toEqual(['link-error']);
  expect(unlinked.stderr).toContain('Linking using');
  expect(unlinked.stderr).toContain('wasm-ld: error: unable to find library -ldoesnotexist');
  expect(unlinked.stderr).not.toContain('Finished');
  expect(unlinked.stderr).not.toContain('DEBUG:');
  expect(unlinked.build_id).toBeNull();
  expect(await subscriberEmails(page)).toEqual([BY_VISITOR, BY_ADMIN]);

  // --- 9. Rollback removes the block, its route and its tool ---------------
  //
  // Newest first, so the first generation with no blocks is the sandbox as it
  // was immediately before the compile.
  const ledger = structured<{ generations: Generation[] }>(
    await execute(page, 'dev_list_generations', {}),
  );
  const beforeBlock = ledger.generations.find((g) => g.blocks === 0);
  expect(beforeBlock, JSON.stringify(ledger.generations)).toBeTruthy();
  structured(await execute(page, 'dev_rollback', { id: beforeBlock!.id }));

  expect((await listSubscribers(page)).status).toBe(404);
  expect((await subscribe(page, 'nobody@newsletter.test')).status).toBe(404);

  // The deployment manifest is generated from the live block set, so the tool
  // goes with the route. `refresh()` is what a page does instead of reloading
  // when what it can offer has changed; the reload before it is there because
  // this page has been sitting on a document whose tools were registered two
  // generations ago.
  await visitor.reload({ waitUntil: 'commit' });
  await visitor.waitForFunction(
    () => (window as unknown as { __impresspressWebmcp?: { generation(): number } })
      .__impresspressWebmcp !== undefined,
    null,
    { timeout: 60_000 },
  );
  await visitor.evaluate(() =>
    (window as unknown as { __impresspressWebmcp: { refresh(): Promise<void> } })
      .__impresspressWebmcp.refresh(),
  );
  const afterRollback = await toolNames(visitor);
  expect(afterRollback).not.toContain(TOOL);

  expect(dialogs).toEqual([]);
});

/**
 * A second page in the SAME browser context, signed out.
 *
 * `browser.newContext()` is the obvious way to get an anonymous visitor and it
 * is the wrong one here: contexts are isolated storage partitions, so a fresh
 * one gets its own OPFS, boots its own sandbox from the seed, and has never
 * heard of a block compiled in this one. The visitor has to share this
 * context, which means anonymity has to come from the cookie jar — and that
 * jar is the context's, so clearing it signs the admin page out too. The
 * caller signs back in when it needs the control plane again.
 *
 * Nothing is installed on it first: the browser's own WebMCP is there from
 * the first navigation, so `webmcp.js` registers into the real
 * `navigator.modelContext` as the page loads.
 */
async function becomeVisitor(context: BrowserContext): Promise<Page> {
  await context.clearCookies();
  const visitor = await context.newPage();
  visitor.on('dialog', (dialog) => dialog.dismiss());
  // Same origin, same service worker, same OPFS — but a document of its own,
  // so `webmcp.js` fetches the manifest for THIS session, which now has no
  // cookie.
  await bootServiceWorker(visitor);
  return visitor;
}
