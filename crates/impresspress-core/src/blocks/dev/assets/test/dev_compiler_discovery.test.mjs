// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_compiler_discovery.test.mjs
//
// `discoverCompiler` — the branch that decides whether `/b/dev` can compile
// anything.
//
// Whether a bundle carries the browser Rust toolchain is a property of the
// BUNDLE, not of this crate: `examples/dev-sandbox/impresspress.toml` overlays
// `compiler/dist/` onto `/__impresspress_dev/compiler/`, and a build without
// that overlay is a legitimate build (CI's foundations job serves one). So the
// page ships a disabled Compile button and asks the host at load time, and
// BOTH answers are behaviour: a manifest enables the button and names the
// toolchain, and a 404 leaves it disabled with a reason on it.
//
// The e2e (`dev-workspace.spec.ts`) can only ever drive one of those, because
// `build.sh` refuses to produce a bundle without the compiler. This is where
// the other one is covered.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate } from './harness.mjs';

/**
 * One block in the workspace.
 *
 * Compile needs BOTH a toolchain and something to compile
 * (`updateCompileButton`), so a test about the manifest has to supply the
 * other half or it would be asserting the wrong reason for a disabled button.
 * The last case below is the other half on its own.
 */
const ONE_BLOCK = [
  { path: 'blocks/hello/Cargo.toml', sha256: 'a'.repeat(64), content: '[package]\n' }
];

/** The real manifest's shape, trimmed to the fields the page reads. */
const MANIFEST = {
  schema_version: 1,
  version: '807ace9e',
  entry: '/__impresspress_dev/compiler/807ace9e/worker.js',
  total_bytes: 75124831,
  target: 'wasm32-wasip1'
};

/**
 * One macrotask drains every microtask the harness's already-resolved
 * promises queued, so the tail's load-time work has finished by the time this
 * returns. No test hook in the shipped file, which is the point of the
 * harness.
 */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('a manifest enables the Compile button and names the toolchain', async () => {
  const { handle, elements, fetchCalls } = instantiate({
    compilerManifest: MANIFEST,
    workspace: ONE_BLOCK
  });
  await settle();

  // Fetched once, from the path the bundle overlays the compiler to, and
  // never from a cache — the manifest is the only file in that tree whose URL
  // does not carry the version.
  const [url, init] = fetchCalls.find(
    ([u]) => String(u) === '/__impresspress_dev/compiler/manifest.json'
  );
  assert.equal(url, '/__impresspress_dev/compiler/manifest.json');
  assert.deepEqual(init, { cache: 'no-store' });

  assert.equal(elements.get('dev-compile').disabled, false);
  // The whole manifest is kept, not just its version: `manifest.entry` is the
  // only path that names the pinned worker, and the button will hand this
  // object to `new BrowserRustCompiler(...)`.
  assert.deepEqual(handle.compilerManifest, MANIFEST);
  // 75124831 / 1048576 = 71.64… — MiB, matching every other figure published
  // about this toolchain.
  assert.equal(
    elements.get('dev-compiler-version').textContent,
    'Compiler v807ace9e · 71.6 MiB'
  );
});

test('a 404 leaves the button disabled with a reason on it', async () => {
  const { handle, elements } = instantiate({ compilerManifest: null, workspace: ONE_BLOCK });
  await settle();

  // Still disabled, exactly as `page.rs` shipped it.
  assert.equal(elements.get('dev-compile').disabled, true);
  assert.equal(elements.get('dev-compile').title, 'No compiler in this build');
  assert.equal(handle.compilerManifest, null);
  assert.equal(elements.get('dev-compiler-version').textContent, '');
});

test('the version line is the manifest, formatted — not a hardcoded string', () => {
  const { handle } = instantiate();
  // A second, differently-sized toolchain: a `describeCompiler` that ignored
  // its argument would pass the test above and fail here.
  assert.equal(
    handle.describeCompiler({ version: 'deadbeef', total_bytes: 1048576 }),
    'Compiler vdeadbeef · 1.0 MiB'
  );
});

test('a toolchain with nothing to compile leaves the button disabled, and says which half is missing', async () => {
  const { elements } = instantiate({ compilerManifest: MANIFEST, workspace: [] });
  await settle();

  // The manifest arrived, so the version line is filled in — but a workspace
  // with no `blocks/<name>/` prefix has nothing for Compile to act on, and a
  // button that offered the click anyway could only answer it with an alert.
  assert.equal(elements.get('dev-compiler-version').textContent, 'Compiler v807ace9e \u00b7 71.6 MiB');
  assert.equal(elements.get('dev-compile').disabled, true);
  assert.match(elements.get('dev-compile').title, /No block to compile/);
});

// ---- starting the toolchain ahead of the first compile --------------------
//
// `warmCompiler`: the page starts the toolchain as soon as it knows BOTH that
// this build carries one and that the workspace has a block, so a compile
// does not pay the start-up. Spec 2026-09-29 dev-block-compile-speed §2.2.

/** One workspace file, in the shape the harness's listing serves. */
const file = (path, content) => ({ path, sha256: 'b'.repeat(64), content });

const HELLO = () => file('blocks/hello/Cargo.toml', '[package]\nname = "hello"\n');

/** A `BrowserRustCompiler` stub that counts start-ups. */
function countingCompiler() {
  const calls = { initialize: 0 };
  class Stub {
    constructor(manifest) {
      this.manifest = manifest;
    }
    async initialize() {
      calls.initialize += 1;
      return 'rustc 1.90.0-nightly (fake)';
    }
    async compile() {
      throw new Error('not compiled in this test');
    }
  }
  return { Stub, calls };
}

test('the toolchain starts on load when the workspace already has a block', async () => {
  const { Stub, calls } = countingCompiler();
  const { elements } = instantiate({ compilerManifest: MANIFEST, workspace: [HELLO()], compiler: Stub });
  await settle();

  assert.equal(calls.initialize, 1);
  // The line the e2e times start-up by.
  assert.match(elements.get('dev-log').textContent, /compiler: ready \(807ace9e\)/);
});

test('a workspace with no block never starts the toolchain', async () => {
  const { Stub, calls } = countingCompiler();
  instantiate({
    compilerManifest: MANIFEST,
    workspace: [file('site/index.html', '<h1>hi</h1>')],
    compiler: Stub
  });
  await settle();

  assert.equal(calls.initialize, 0);
});

test('without a compiler in the build nothing starts, blocks or not', async () => {
  const { Stub, calls } = countingCompiler();
  instantiate({ compilerManifest: null, workspace: [HELLO()], compiler: Stub });
  await settle();

  assert.equal(calls.initialize, 0);
});

test('the toolchain starts when the first block is scaffolded, and only once', async () => {
  const { Stub, calls } = countingCompiler();
  const { handle } = instantiate({ compilerManifest: MANIFEST, workspace: [], compiler: Stub });
  await settle();
  assert.equal(calls.initialize, 0);

  // The listing refresh every mutating call performs, now with a block in it.
  handle.renderBlockChoices([HELLO()]);
  handle.renderBlockChoices([HELLO()]);
  await settle();

  assert.equal(calls.initialize, 1);
});

test('a failed start-up is not retried by the next listing refresh', async () => {
  let starts = 0;
  class Broken {
    async initialize() {
      starts += 1;
      throw new Error('worker would not start');
    }
  }
  const { handle, elements } = instantiate({
    compilerManifest: MANIFEST,
    workspace: [HELLO()],
    compiler: Broken
  });
  await settle();
  handle.renderBlockChoices([HELLO()]);
  await settle();

  // One attempt: the compile path keeps its own retry, and a warm-up re-armed
  // by every refresh would hammer a broken toolchain after every write.
  assert.equal(starts, 1);
  assert.match(
    elements.get('dev-log').textContent,
    /compiler: start-up failed \(worker would not start\); the first compile will retry/
  );
});
