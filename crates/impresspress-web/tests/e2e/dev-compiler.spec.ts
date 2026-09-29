import { test, expect } from '@playwright/test';
import { createHash } from 'node:crypto';
import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { loginToWorkspace } from './fixtures/dev-sandbox';

/**
 * `BrowserRustCompiler` — the page half of the compiler protocol.
 *
 * The class under test is
 * `crates/impresspress-core/src/blocks/dev/assets/compiler-adapter.js`, served
 * at `/b/dev/static/compiler-adapter.js`. It only exists inside a document: it
 * creates a `Worker`, moves an `ArrayBuffer` across a `postMessage` boundary
 * and digests it with `crypto.subtle`, none of which a host test can reach. So
 * this file is the only place it runs.
 *
 * # Why the worker is a fake
 *
 * The real one (`examples/dev-sandbox/compiler/`) is a 365 MiB composed
 * toolchain: seven seconds to `ready`, thirty-eight to compile the `hello`
 * template. Everything asserted here — the queue, the cancel, the four
 * protocol violations — behaves the same against real rustc, and a spec
 * paying forty seconds a case could not afford to cover them.
 * `fixtures/fake-compiler-worker.js` speaks `compiler/src/protocol.ts`
 * exactly, including the two refusals that make the adapter's queue mean
 * something (a second `compile` while one is in flight, and anything after a
 * `cancel`). Plan 3 Task 6 drives the real compiler end to end.
 *
 * The one thing a fake cannot answer is whether the REAL worker is allowed to
 * start at all — which is a property of headers, not of the protocol — so the
 * last test here starts the packaged worker and waits for its `ready`. See
 * that test for what it is really proving.
 *
 * # How the fake gets served
 *
 * The manifest the page reads names the worker's URL, so pointing the page at
 * the fake is a matter of writing a second manifest beside the real one:
 * `/__impresspress_dev/compiler/test/manifest.json`, whose `entry` is
 * `/__impresspress_dev/compiler/test/worker.js`. Both files are written into
 * the *served* `dist/` below rather than committed — `examples/dev-sandbox/
 * dist/` is build output, and a fixture inside the deployed compiler tree
 * would ship to dev.impresspress.org. The real `manifest.json` is never
 * touched: the deployed tree stays exactly what `build.sh` produced, plus one
 * directory this file creates and removes again.
 */

/**
 * The bundle being served on `TEST_PORT`.
 *
 * CI exports `DEV_DIST` from `build.sh`'s last line; a local run defaults to
 * the same path that script writes to.
 */
const DEV_DIST =
  process.env.DEV_DIST ??
  fileURLToPath(new URL('../../../../examples/dev-sandbox/dist', import.meta.url));

/** Where the test-only manifests and worker go, and the URLs they get. */
const TEST_COMPILER_DIR = path.join(DEV_DIST, '__impresspress_dev', 'compiler', 'test');
const TEST_MANIFEST_URL = '/__impresspress_dev/compiler/test/manifest.json';
const TEST_WORKER_URL = '/__impresspress_dev/compiler/test/worker.js';

/**
 * Three manifests, one worker.
 *
 * An `init` message carries its id and the guest crate the API served, neither
 * of which is a switch, so a fake that has to behave differently DURING
 * start-up can only be told which way through the URL it was started from —
 * and the manifest's `entry` is where the page gets that URL. Hence a
 * manifest per init behaviour rather than a worker per behaviour.
 */
const SILENT_MANIFEST_URL = '/__impresspress_dev/compiler/test/manifest-silent.json';
const DRIP_MANIFEST_URL = '/__impresspress_dev/compiler/test/manifest-drip.json';

/**
 * The start-up silence window this file injects, and the drip interval that
 * has to survive it.
 *
 * The shipped window is six minutes (`INIT_SILENCE_TIMEOUT_MS`), which is a
 * backstop for the worker's own 300 s step guards and not something a test can
 * wait for. `new BrowserRustCompiler(manifest, { initSilenceMs })` exists for
 * exactly this. Six drip ticks of 400 ms is 2.4 s of start-up — comfortably
 * longer than the 1.5 s window, with no gap in it longer than 400 ms.
 */
const TEST_SILENCE_MS = 1500;
const DRIP_TICK_MS = 400;

const FAKE_WORKER = fileURLToPath(new URL('./fixtures/fake-compiler-worker.js', import.meta.url));

/** The manifest `build.sh` overlaid — the real toolchain's, not the fixture's. */
const REAL_MANIFEST_URL = '/__impresspress_dev/compiler/manifest.json';
const REAL_MANIFEST_FILE = path.join(DEV_DIST, '__impresspress_dev', 'compiler', 'manifest.json');

/** The sixteen bytes `fake-compiler-worker.js` hands back, and their digest. */
const ARTIFACT = Buffer.from(Array.from({ length: 16 }, (_, i) => i));
const ARTIFACT_SHA256 = createHash('sha256').update(ARTIFACT).digest('hex');

/** `validation::MAX_ARTIFACT_BYTES` — the ceiling both halves enforce. */
const MAX_ARTIFACT_BYTES = 4194304;

/** What `fake-compiler-worker.js` answers `rustc --version` with. */
const FAKE_RUSTC_VERSION = 'rustc 1.90.0-nightly (fake worker)';

/**
 * The same shape `scripts/write-manifest.mjs` writes, minus the asset
 * inventory nothing on the page reads: the adapter takes `entry`, `version`
 * and `target`, and refuses a manifest missing any of them.
 */
function writeManifest(name: string, entry: string) {
  writeFileSync(
    path.join(TEST_COMPILER_DIR, name),
    `${JSON.stringify(
      {
        schema_version: 1,
        version: 'test',
        entry,
        total_bytes: 0,
        assets: [],
        license: 'MIT OR Apache-2.0',
        target: 'wasm32-wasip1',
      },
      null,
      2,
    )}\n`,
  );
}

test.beforeAll(() => {
  // Removed before it is created, not just after it is used: a run killed
  // between the two hooks (a `Ctrl-C`, a timed-out CI job) leaves the fixture
  // inside `dist/`, and the next run would build on top of whatever version of
  // the worker was there — or ship it, if a deploy came first.
  rmSync(TEST_COMPILER_DIR, { recursive: true, force: true });
  mkdirSync(TEST_COMPILER_DIR, { recursive: true });
  copyFileSync(FAKE_WORKER, path.join(TEST_COMPILER_DIR, 'worker.js'));
  writeManifest('manifest.json', TEST_WORKER_URL);
  writeManifest('manifest-silent.json', `${TEST_WORKER_URL}?silent-init=1`);
  writeManifest('manifest-drip.json', `${TEST_WORKER_URL}?drip-init=${DRIP_TICK_MS}`);
});

test.afterAll(() => {
  // The static server serves whatever is on disk, so the fixture must not
  // outlive the run that needed it — least of all in a tree a deploy uploads.
  rmSync(TEST_COMPILER_DIR, { recursive: true, force: true });
});

test('the compiler adapter reports progress, results and diagnostics', async ({ page }) => {
  // A cold boot of the sandbox (wasm compile, OPFS create, migrations, seed
  // import) is minutes on a cold cache; the adapter's own work is milliseconds.
  test.setTimeout(300_000);
  await loginToWorkspace(page);

  const result = await page.evaluate(async (manifestUrl) => {
    // Dynamic, and from the page — the module is the unit under test, so
    // loading it by the URL `dev.js` itself imports is the point. A
    // non-literal specifier keeps the transpiler from trying to resolve a
    // path that only exists at runtime.
    const modulePath = '/b/dev/static/compiler-adapter.js';
    const { BrowserRustCompiler } = await import(modulePath);
    const manifest = await (await fetch(manifestUrl)).json();
    const compiler = new BrowserRustCompiler(manifest);

    const stages: string[] = [];
    const version = compiler.version;
    await compiler.initialize((progress: any) => stages.push(progress.stage));
    const ok = await compiler.compile({
      crateName: 'hello',
      files: { 'src/lib.rs': '// ok' },
      onProgress: (progress: any) => stages.push(progress.stage),
    });
    const bad = await compiler.compile({
      crateName: 'hello',
      files: { 'src/lib.rs': '// FAIL' },
    });
    await compiler.dispose();
    // A `compile` after `dispose` is a caller error, not a compiler error, so
    // it must reject rather than resolve with a failed build.
    const afterDispose = await compiler
      .compile({ crateName: 'hello', files: { 'src/lib.rs': '// ok' } })
      .then(
        () => null,
        (error: Error) => error.message,
      );

    return {
      version,
      stages,
      ok: {
        success: ok.success,
        cancelled: ok.cancelled,
        bytes: Array.from(ok.artifact as Uint8Array),
        sha: ok.artifactSha256,
        stdout: ok.stdout,
        compilerVersion: ok.compilerVersion,
        buildId: ok.buildId,
      },
      bad: {
        success: bad.success,
        artifact: bad.artifact,
        sha: bad.artifactSha256,
        diagnostics: bad.diagnostics,
      },
      afterDispose,
    };
  }, TEST_MANIFEST_URL);

  // The manifest's version is the pinned bundle's, straight through.
  expect(result.version).toBe('test');
  // `initialize` reports the toolchain coming up and `compile` reports the
  // build, each to the callback its own caller passed, in order.
  expect(result.stages).toEqual(['download', 'initializing', 'compiling']);

  expect(result.ok.success).toBe(true);
  expect(result.ok.cancelled).toBe(false);
  // The artifact is the transferred buffer, viewed — not a copy, and not the
  // bytes of some earlier build.
  expect(result.ok.bytes).toEqual(Array.from(ARTIFACT));
  expect(result.ok.sha).toMatch(/^[0-9a-f]{64}$/);
  // …and the digest is over those bytes, which a `sha` that merely looked
  // like hex would not prove.
  expect(result.ok.sha).toBe(ARTIFACT_SHA256);
  expect(result.ok.stdout).toContain('hello');
  // What `POST /b/dev/api/builds/stage` will record as `compiler_version`:
  // the string the worker reported at `ready`.
  expect(result.ok.compilerVersion).toBe(FAKE_RUSTC_VERSION);
  expect(result.ok.buildId).toMatch(/^build-\d+$/);

  // A crate that does not compile is an ANSWER. `compile` resolves with it.
  expect(result.bad.success).toBe(false);
  expect(result.bad.artifact).toBeNull();
  expect(result.bad.sha).toBeNull();
  expect(result.bad.diagnostics[0]).toMatchObject({
    file: 'src/lib.rs',
    line: 1,
    column: 1,
    severity: 'error',
    code: 'E0425',
  });

  expect(result.afterDispose).toContain('disposed');
});

/**
 * The guest crate `dev.js` hands the adapter reaches the worker in the
 * protocol's shape.
 *
 * `dev.js` constructs `new BrowserRustCompiler(manifest, { guest })` with
 * `GET /b/dev/api/guest`'s answer unchanged, so this does the same. The
 * adapter renames `warmup.crate_name` to `warmup.crateName` on the way into
 * `postMessage`; a worker handed the API's spelling would build no warm-up
 * crate at all, and every compile of the session would rebuild the SDK. The
 * fake keeps the `init` it received on `self.receivedInit`, and Playwright's
 * handle on the worker reads it back — the one place that message is visible.
 */
test('the compiler adapter starts its worker with the guest crate the API serves', async ({
  page,
}) => {
  test.setTimeout(300_000);
  await loginToWorkspace(page);

  const started = await page.evaluate(async (manifestUrl) => {
    const modulePath = '/b/dev/static/compiler-adapter.js';
    const { BrowserRustCompiler } = await import(modulePath);
    const manifest = await (await fetch(manifestUrl)).json();
    const guest = await (await fetch('/b/dev/api/guest')).json();
    const compiler = new BrowserRustCompiler(manifest, { guest });
    const progress: Array<{ stage: string; detail?: string }> = [];
    await compiler.initialize((p: any) => progress.push({ stage: p.stage, detail: p.detail }));
    // Kept alive for the worker read below: `dispose` terminates the worker,
    // and with it the global that holds the `init`.
    (window as any).__guestCompiler = compiler;
    return { guest, progress };
  }, TEST_MANIFEST_URL);

  const worker = page.workers().find((w) => new URL(w.url()).pathname === TEST_WORKER_URL);
  expect(worker, page.workers().map((w) => w.url()).join(', ')).toBeTruthy();
  const received = await worker!.evaluate(() => (self as any).receivedInit);

  // What the API serves: the two files of the SDK crate and a `hello` block
  // that depends on it, which the worker builds once before `ready`.
  expect(Object.keys(started.guest.files).sort()).toEqual(['Cargo.toml', 'src/lib.rs']);
  expect(started.guest.warmup.crate_name).toBe('hello');

  // …and what the worker received: the same files, byte for byte, in the
  // protocol's shape — `crateName`, not `crate_name`, and no `version` (the
  // adapter keeps that for the `CompileResult`, not the worker).
  expect(received.type).toBe('init');
  expect(received.guest).toEqual({
    files: started.guest.files,
    warmup: { crateName: 'hello', files: started.guest.warmup.files },
  });

  // The warm-up is reported as its own start-up step, so a page showing
  // progress can say what the first thirty seconds are for.
  expect(started.progress).toContainEqual({
    stage: 'initializing',
    detail: 'building wafer_guest once for this session',
  });

  // Every build of the session records the guest it was linked against.
  const built = await page.evaluate(async () => {
    const compiler = (window as any).__guestCompiler;
    const result = await compiler.compile({ crateName: 'hello', files: { 'src/lib.rs': '// ok' } });
    await compiler.dispose();
    return { success: result.success, guestVersion: result.guestVersion };
  });
  expect(built).toEqual({ success: true, guestVersion: started.guest.version });
});

test('the compiler adapter queues compiles, cancels one, and recovers from a broken worker', async ({
  page,
}) => {
  test.setTimeout(300_000);
  await loginToWorkspace(page);

  const result = await page.evaluate(async (manifestUrl) => {
    const modulePath = '/b/dev/static/compiler-adapter.js';
    const { BrowserRustCompiler } = await import(modulePath);
    const manifest = await (await fetch(manifestUrl)).json();
    const compiler = new BrowserRustCompiler(manifest);

    const settle = (promise: Promise<any>): Promise<any> =>
      promise.then(
        (value: any) => ({ ok: true, value }),
        (error: Error) => ({ ok: false, message: error.message }),
      );

    await compiler.initialize();

    // --- the queue --------------------------------------------------------
    // Both calls are made before either resolves. The worker refuses a second
    // `compile` while one is in flight (`protocol.ts`: "a queue would only
    // hide a bug in the page"), so if the adapter did not serialise them, the
    // second would come back failed with that refusal as its stderr instead
    // of a build.
    const first = compiler.compile({ crateName: 'queued-a', files: { 'src/lib.rs': '// ok' } });
    const second = compiler.compile({ crateName: 'queued-b', files: { 'src/lib.rs': '// ok' } });
    const queued = await Promise.all([first, second]);

    // --- cancel -----------------------------------------------------------
    // `slow` keeps the fake worker busy for five seconds, so there is
    // something in flight to cancel.
    const slow = compiler.compile({ crateName: 'slow', files: { 'src/lib.rs': '// ok' } });
    await new Promise((resolve) => setTimeout(resolve, 200));
    await compiler.cancel();
    const cancelled = await slow;
    // The worker is spent, so the adapter must have terminated it and be
    // willing to build another. Nothing here calls `initialize` again: the
    // point is that `compile` alone is enough.
    const afterCancel = await compiler.compile({
      crateName: 'after-cancel',
      files: { 'src/lib.rs': '// ok' },
    });

    // --- cancel with a compile already queued behind it -------------------
    // The case the queue makes possible and nothing above covers: the cancel
    // lands on the build in flight, and the one waiting its turn has to end up
    // on a worker that does not exist yet. If `#destroy` left the instance in
    // a state the queue could not start from, this is where it would hang.
    const doomed = compiler.compile({ crateName: 'slow', files: { 'src/lib.rs': '// ok' } });
    const queuedBehind = compiler.compile({
      crateName: 'behind-cancel',
      files: { 'src/lib.rs': '// ok' },
    });
    await new Promise((resolve) => setTimeout(resolve, 200));
    await compiler.cancel();
    const doomedResult = await doomed;
    const behindResult = await queuedBehind;

    // --- the protocol violations ------------------------------------------
    // Each must REJECT — the compiler said nothing about the crate, so there
    // is no result to resolve with — and leave the instance usable.
    const violations: Record<string, any> = {};
    for (const crateName of [
      'protocol-wrong-id',
      'protocol-ready-for-build',
      'protocol-not-a-buffer',
      'protocol-oversized',
      'protocol-malformed-result',
      'protocol-malformed-diagnostic',
      'protocol-error',
    ]) {
      const broke = await settle(compiler.compile({ crateName, files: { 'src/lib.rs': '// ok' } }));
      const next = await settle(
        compiler.compile({ crateName: 'recovered', files: { 'src/lib.rs': '// ok' } }),
      );
      violations[crateName] = {
        rejected: broke.ok === false,
        message: broke.message ?? null,
        recovered: next.ok === true && next.value.success === true,
      };
    }

    await compiler.dispose();
    return {
      queued: queued.map((r: any) => ({ success: r.success, stdout: r.stdout })),
      cancelled: {
        success: cancelled.success,
        cancelled: cancelled.cancelled,
        artifact: cancelled.artifact,
        stderr: cancelled.stderr,
        elapsedMs: cancelled.elapsedMs,
        compilerVersion: cancelled.compilerVersion,
      },
      afterCancel: { success: afterCancel.success, stdout: afterCancel.stdout },
      doomed: { cancelled: doomedResult.cancelled, stderr: doomedResult.stderr },
      behind: { success: behindResult.success, stdout: behindResult.stdout },
      violations,
    };
  }, TEST_MANIFEST_URL);

  // Both compiles ran, in the order they were requested — the fake worker
  // numbers its builds, so the sequence is its own account of what happened.
  expect(result.queued.map((r) => r.success)).toEqual([true, true]);
  expect(result.queued[0].stdout).toBe('fake build #1: queued-a');
  expect(result.queued[1].stdout).toBe('fake build #2: queued-b');

  // The cancelled compile RESOLVES: the page asked for this.
  expect(result.cancelled.success).toBe(false);
  expect(result.cancelled.cancelled).toBe(true);
  expect(result.cancelled.artifact).toBeNull();
  // The WORKER's own account of the cancelled build, not one the adapter
  // made up: `cancel` names the compile's id, so the answer to it *is* that
  // compile's one terminal message. If the adapter had invented an id, the
  // worker would have answered `error: nothing in flight` instead and the
  // page would be reading a synthesized result here.
  expect(result.cancelled.stderr).toBe('cancelled');
  expect(result.cancelled.compilerVersion).toBe(FAKE_RUSTC_VERSION);
  // A fresh worker was built on demand: this is build #1 of the new one.
  expect(result.afterCancel.success).toBe(true);
  expect(result.afterCancel.stdout).toBe('fake build #1: after-cancel');

  // The cancel took the build it named…
  expect(result.doomed.cancelled).toBe(true);
  expect(result.doomed.stderr).toBe('cancelled');
  // …and the one queued behind it ran on a worker built for it afterwards.
  // Build #1 again: the fake numbers builds per worker, so anything else here
  // would mean the queued compile went to the worker that was torn down.
  expect(result.behind.success).toBe(true);
  expect(result.behind.stdout).toBe('fake build #1: behind-cancel');

  const violations = result.violations as Record<
    string,
    { rejected: boolean; message: string | null; recovered: boolean }
  >;
  expect(violations['protocol-wrong-id'].message).toContain(
    'answered result for a request this page did not make',
  );
  expect(violations['protocol-not-a-buffer'].message).toContain('not an ArrayBuffer');
  expect(violations['protocol-oversized'].message).toContain(
    `over the sandbox limit of ${MAX_ARTIFACT_BYTES}`,
  );
  expect(violations['protocol-error'].message).toContain('the compiler image is corrupt');
  // The right id, the wrong KIND of message: an `init`'s answer settling a
  // compile would hand the caller an object with no `diagnostics` at all.
  expect(violations['protocol-ready-for-build'].message).toContain(
    'answered ready for build-',
  );
  expect(violations['protocol-ready-for-build'].message).toContain(
    'which was a request for result',
  );
  // Fields checked before any of them is handed on.
  expect(violations['protocol-malformed-result'].message).toContain(
    '`stdout` is not a string',
  );
  expect(violations['protocol-malformed-diagnostic'].message).toContain(
    'diagnostics[0] has no `line` number',
  );
  // Every one of them rejected, and every one left a compiler that still works.
  for (const crateName of Object.keys(violations)) {
    expect(violations[crateName].rejected, crateName).toBe(true);
    expect(violations[crateName].recovered, crateName).toBe(true);
  }
});

/**
 * The start-up watchdog: a worker that stops speaking is let go of, and one
 * that is merely slow is not.
 *
 * `initialize` has no deadline, on purpose — the variable part of it is a
 * 75 MiB download, and refusing slow connections would be the wrong failure.
 * That left one hole: a worker that neither answers nor fires its `error`
 * event (a fetch stalled with no failure, a subordinate worker that died
 * quietly) parked `initialize` for the life of the page, with every queued
 * `compile` behind it and `dispose()` the only way out. The watchdog closes it
 * without reintroducing a ceiling, and BOTH halves of that are asserted here:
 * the silent worker is abandoned, and the one whose start-up runs longer than
 * the window while still reporting is not.
 *
 * The shipped window is six minutes — a backstop for the worker's own 300 s
 * per-step guards, which report a wedged step far better than this side can —
 * so the test injects a short one through the constructor option that exists
 * for it. Nothing else about the behaviour is test-specific.
 */
test('a start-up that goes silent is abandoned; one that is merely slow is not', async ({
  page,
}) => {
  test.setTimeout(300_000);
  await loginToWorkspace(page);

  const result = await page.evaluate(
    async (input: { silentUrl: string; dripUrl: string; silenceMs: number }) => {
      const modulePath = '/b/dev/static/compiler-adapter.js';
      const { BrowserRustCompiler } = await import(modulePath);

      /** Run something and report how long it took, settled either way. */
      const timed = async (run: () => Promise<any>): Promise<any> => {
        const started = Date.now();
        try {
          const value = await run();
          return { ok: true, ms: Date.now() - started, value };
        } catch (error: any) {
          return { ok: false, ms: Date.now() - started, message: error.message };
        }
      };

      // --- the worker that stops speaking ---------------------------------
      const silentManifest = await (await fetch(input.silentUrl)).json();
      const silent = new BrowserRustCompiler(silentManifest, {
        initSilenceMs: input.silenceMs,
      });
      const first = await timed(() => silent.initialize());
      // A second call has to build a NEW worker rather than hand back the
      // rejection the first one left behind. Only the clock can tell those
      // apart: a sticky `#ready` would reject at once.
      const second = await timed(() => silent.initialize());
      // And a compile has to fail rather than queue behind a start-up that is
      // never going to finish.
      const compileAfter = await timed(() =>
        silent.compile({ crateName: 'hello', files: { 'src/lib.rs': '// ok' } }),
      );
      await silent.dispose();

      // --- the worker that is merely slow ---------------------------------
      const dripManifest = await (await fetch(input.dripUrl)).json();
      const drip = new BrowserRustCompiler(dripManifest, { initSilenceMs: input.silenceMs });
      const slowStart = await timed(() => drip.initialize());
      const built = slowStart.ok
        ? await timed(() =>
            drip.compile({ crateName: 'after-slow-start', files: { 'src/lib.rs': '// ok' } }),
          )
        : null;
      await drip.dispose();

      return { first, second, compileAfter, slowStart, built };
    },
    { silentUrl: SILENT_MANIFEST_URL, dripUrl: DRIP_MANIFEST_URL, silenceMs: TEST_SILENCE_MS },
  );

  // Abandoned, and for the stated reason.
  expect(result.first.ok).toBe(false);
  expect(result.first.message).toContain(
    `stopped reporting progress for ${TEST_SILENCE_MS} ms`,
  );
  // It waited the window — it did not give up early — and it did not wait the
  // six-minute default, which is the whole point of the injected option.
  expect(result.first.ms).toBeGreaterThanOrEqual(TEST_SILENCE_MS - 200);
  expect(result.first.ms).toBeLessThan(30_000);

  // The second attempt paid the window again, so it started a worker of its
  // own: the failure was not cached.
  expect(result.second.ok).toBe(false);
  expect(result.second.ms).toBeGreaterThanOrEqual(TEST_SILENCE_MS - 200);

  // …and so did the compile, instead of waiting for ever.
  expect(result.compileAfter.ok).toBe(false);
  expect(result.compileAfter.message).toContain('stopped reporting progress');

  // The other half: a start-up LONGER than the window, never silent for as
  // long as it, finishes. A ceiling would have killed this one.
  expect(result.slowStart.ok, result.slowStart.message).toBe(true);
  expect(result.slowStart.ms).toBeGreaterThan(TEST_SILENCE_MS);
  expect(result.built.ok).toBe(true);
  expect(result.built.value.success).toBe(true);
});

/**
 * The packaged worker starts — which is a statement about HEADERS, not about
 * the protocol.
 *
 * A document with a `Cross-Origin-Embedder-Policy` inherits that policy to
 * every dedicated worker it starts, and the browser refuses one whose script
 * response does not carry a compatible COEP. `/b/dev` is `credentialless`
 * (`blocks/dev/page.rs`), and the toolchain lives under
 * `/__impresspress_dev/compiler/` — a prefix on the service worker's bypass
 * list, so the wasm runtime never sees those responses and cannot put a header
 * on them. The static host is what answers them, and the host here is
 * `python3 -m http.server`, which sends no such header: before
 * `sw.js.tmpl`'s passthrough existed this failed with
 * `net::ERR_BLOCKED_BY_RESPONSE` and a `Worker` `error` event carrying an empty
 * message — all the page can ever be told.
 *
 * So the header on the response below can only have come from the service
 * worker, and the worker reaching `ready` can only have happened because it
 * did. That is the whole point of the test, and it is why it uses a bare
 * `Worker` rather than `BrowserRustCompiler`: the adapter is covered above,
 * and a failure here must be unambiguous about which layer broke.
 *
 * It is also the only test in this file that pays for the real toolchain —
 * ~75 MiB over loopback and a wasm instantiation, measured at 7–11 s cold
 * (`compiler/README.md`). It does not compile anything; Plan 3 Task 6 does.
 */
test('the packaged compiler worker starts under the isolation headers the service worker adds', async ({
  page,
}) => {
  test.setTimeout(600_000);

  // What `build.sh` overlaid into the bundle being served. Read from disk so
  // the assertions below are against the tree under test, not against
  // whatever the page happened to fetch.
  const onDisk = JSON.parse(readFileSync(REAL_MANIFEST_FILE, 'utf8'));

  await loginToWorkspace(page);

  // `/b/dev` itself: the capability, not just the header. `SharedArrayBuffer`
  // is what the toolchain's threads need and what isolation buys.
  expect(await page.evaluate(() => crossOriginIsolated)).toBe(true);

  const headers = await page.evaluate(async (url: string) => {
    const response = await fetch(url, { cache: 'no-store' });
    return {
      status: response.status,
      coep: response.headers.get('cross-origin-embedder-policy'),
      coop: response.headers.get('cross-origin-opener-policy'),
      manifest: await response.json(),
    };
  }, REAL_MANIFEST_URL);
  expect(headers.status).toBe(200);
  // Nothing on the static host sends these. The service worker did.
  expect(headers.coep).toBe('credentialless');
  expect(headers.coop).toBe('same-origin');
  expect(headers.manifest.entry).toBe(onDisk.entry);

  const started = await page.evaluate(async (entry: string) => {
    const worker = new Worker(entry, { type: 'module' });
    try {
      return await new Promise<{ rustcVersion: string }>((resolve, reject) => {
        const timer = setTimeout(
          () => reject(new Error('the compiler worker never answered `ready`')),
          420_000,
        );
        worker.addEventListener('error', (event) => {
          event.preventDefault();
          clearTimeout(timer);
          // An empty message is the signature of a COEP refusal: the browser
          // will not say more about a worker it declined to start.
          reject(
            new Error(
              'the compiler worker failed to start: ' +
                (event.message || '(no message — this is what a COEP refusal looks like)'),
            ),
          );
        });
        worker.addEventListener('message', (event) => {
          const message = event.data;
          if (message.type === 'ready' && message.id === 'coep-probe') {
            clearTimeout(timer);
            resolve({ rustcVersion: message.rustcVersion });
          } else if (message.type === 'error' && message.id === 'coep-probe') {
            clearTimeout(timer);
            reject(new Error('the compiler worker refused init: ' + message.message));
          }
        });
        // `protocol.ts`'s `InitMessage`. `ready` is its one success answer.
        worker.postMessage({ type: 'init', id: 'coep-probe' });
      });
    } finally {
      // 75 MiB of toolchain has no business outliving this test.
      worker.terminate();
    }
  }, onDisk.entry);

  // The toolchain really came up: this string is `rustc --version` as run
  // inside the worker's VFS, not something the page could have synthesized.
  expect(started.rustcVersion).toContain('rustc');
});
