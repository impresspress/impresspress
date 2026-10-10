// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/compile_output.test.mjs
//
// `shapeCompileOutput` — what `compiler-adapter.js` makes of the worker's raw
// transcript before a `CompileResult` leaves it.
//
// The inputs are not made up. `fixtures/compile-outputs.json` holds `result`
// messages the REAL packaged compiler sent (dist 807ace9e.2) for five crates:
// the `hello` template, the same with a dead-code warning, with a missing
// `;`, with a library wasm-ld cannot find, and with a Cargo.toml cargo cannot
// parse. Each test states what the raw message contained, so a pin bump that
// changes the toolchain's chatter fails here in the precondition rather than
// passing on a filter with nothing left to filter.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const OUTPUTS = JSON.parse(fs.readFileSync(path.join(here, 'fixtures', 'compile-outputs.json'), 'utf8'));

// The adapter is a browser ES module served as `.js` with no package.json
// beside it, so it is loaded from its source text as a `data:` module — the
// one form every node this suite runs on reads as ESM. It is the shipped
// file, byte for byte; nothing is added to it for the test.
const source = fs.readFileSync(path.join(here, '..', 'compiler-adapter.js'), 'utf8');
const { shapeCompileOutput } = await import(
  'data:text/javascript;charset=utf-8,' + encodeURIComponent(source)
);

const DEBUG = /^DEBUG: /m;
const FINISHED = /^\s*Finished `release` profile/m;
const LINKING = /^Linking using /m;

test('a successful build loses the debug lines and the linker command, and keeps Finished', () => {
  const raw = OUTPUTS.success;
  // What the toolchain really printed.
  assert.match(raw.stdout, DEBUG);
  assert.match(raw.stderr, DEBUG);
  assert.match(raw.stderr, LINKING);
  assert.ok(raw.stderr.length > 1800, 'the raw stderr carries the ~1.8 KB link command');

  const shaped = shapeCompileOutput(raw);
  assert.doesNotMatch(shaped.stdout, DEBUG);
  assert.doesNotMatch(shaped.stderr, DEBUG);
  assert.doesNotMatch(shaped.stderr, LINKING);
  // The build did finish, so cargo saying so is true and stays.
  assert.equal(
    shaped.stderr,
    '   Compiling hello v0.1.0 (/blocks/hello)\n' +
      '    Finished `release` profile [optimized] target(s) in 1.80s'
  );
  // The worker's housekeeping is still there, minus cargo's debug lines.
  assert.equal(
    shaped.stdout,
    '     Removed 10 files, 230.4KiB total\n' +
      'Downloading /target/wasm32-wasip1/release/hello.wasm: [=========================] 100.0% (112.6 KB/112.6 KB)\n' +
      'Download successful.'
  );
  assert.deepEqual(shaped.diagnostics, []);
});

test('a warning survives in rendered and structured form on a build that succeeds', () => {
  const raw = OUTPUTS.warning;
  const shaped = shapeCompileOutput(raw);
  assert.ok(shaped.stderr.startsWith('warning: function `unused_helper` is never used\n  --> src/lib.rs:45:4'));
  assert.doesNotMatch(shaped.stderr, DEBUG);
  assert.doesNotMatch(shaped.stderr, LINKING);
  assert.match(shaped.stderr, FINISHED);
  assert.deepEqual(shaped.diagnostics, raw.diagnostics);
});

test('a syntax error keeps rustc\'s rendering and drops the Finished that contradicts it', () => {
  const raw = OUTPUTS.syntax_error;
  assert.equal(raw.success, false);
  // The lie as captured: rustc failed, and cargo said it finished anyway.
  assert.match(raw.stderr, FINISHED);
  assert.match(raw.stderr, DEBUG);

  const shaped = shapeCompileOutput(raw);
  assert.doesNotMatch(shaped.stderr, FINISHED);
  assert.doesNotMatch(shaped.stderr, DEBUG);
  assert.doesNotMatch(shaped.stdout, DEBUG);
  // rustc's own words come first and are untouched, then cargo's status.
  assert.ok(
    shaped.stderr.startsWith(
      'error: expected `;`, found `Ok`\n  --> src/lib.rs:34:14\n   |\n34 |     let x = 1\n'
    ),
    shaped.stderr
  );
  assert.ok(shaped.stderr.endsWith('\n\n   Compiling hello v0.1.0 (/blocks/hello)'), shaped.stderr);
  // The structured diagnostics are rustc's, unchanged — nothing linker-shaped
  // is invented for a build that never reached the link.
  assert.deepEqual(shaped.diagnostics, raw.diagnostics);
});

test('a link failure keeps the linker command, drops Finished, and becomes a link-error diagnostic', () => {
  const raw = OUTPUTS.link_error;
  assert.equal(raw.success, false);
  assert.match(raw.stderr, /^wasm-ld: error: unable to find library -ldoesnotexist$/m);
  // All the worker could say about it: the module is missing, see stderr.
  assert.deepEqual(
    raw.diagnostics.map((d) => d.code),
    ['artifact-missing']
  );

  const shaped = shapeCompileOutput(raw);
  // The one build where the command line is evidence: which libraries wasm-ld
  // was handed, and in what order.
  assert.match(shaped.stderr, LINKING);
  assert.match(shaped.stderr, /"-l" "doesnotexist"/);
  assert.match(shaped.stderr, /^wasm-ld: error: unable to find library -ldoesnotexist$/m);
  assert.doesNotMatch(shaped.stderr, FINISHED);
  assert.doesNotMatch(shaped.stderr, DEBUG);
  assert.doesNotMatch(shaped.stdout, DEBUG);
  // The reason, as a diagnostic an agent can read without scanning stderr —
  // in place of the worker's `artifact-missing`, whose "failed without a
  // diagnostic" it would otherwise contradict.
  assert.deepEqual(shaped.diagnostics, [
    {
      file: '',
      line: 0,
      column: 0,
      severity: 'error',
      code: 'link-error',
      message: 'wasm-ld: unable to find library -ldoesnotexist'
    }
  ]);
});

test('a manifest cargo cannot parse keeps cargo\'s error in both streams, minus the debug lines', () => {
  const raw = OUTPUTS.manifest_error;
  const shaped = shapeCompileOutput(raw);
  const rendered =
    'error: key with no value, expected `=`\n  --> blocks/hello/Cargo.toml:35:3\n';
  assert.ok(shaped.stderr.startsWith(rendered), shaped.stderr);
  assert.ok(shaped.stdout.startsWith(rendered), shaped.stdout);
  assert.doesNotMatch(shaped.stderr, DEBUG);
  assert.doesNotMatch(shaped.stdout, DEBUG);
  assert.deepEqual(shaped.diagnostics, raw.diagnostics);
});

test('artifact-missing stays when nothing explains it, and when the linker only warned', () => {
  const missing = {
    file: '',
    line: 0,
    column: 0,
    severity: 'error',
    code: 'artifact-missing',
    message: 'the build produced no /target/wasm32-wasip1/release/hello.wasm'
  };
  const withoutLinker = shapeCompileOutput({
    success: false,
    stdout: '',
    stderr: '   Compiling hello v0.1.0 (/blocks/hello)',
    diagnostics: [missing]
  });
  assert.deepEqual(withoutLinker.diagnostics, [missing]);
  const warnedOnly = shapeCompileOutput({
    success: false,
    stdout: '',
    stderr: 'wasm-ld: warning: something odd',
    diagnostics: [missing]
  });
  assert.deepEqual(
    warnedOnly.diagnostics.map((d) => d.code),
    ['link-warning', 'artifact-missing']
  );
});

test('a line is dropped only when it IS a debug line, not when it mentions one', () => {
  const shaped = shapeCompileOutput({
    success: true,
    stdout: '',
    stderr: 'note: DEBUG: main started\n  DEBUG: logger setup done\nDEBUG: main started',
    diagnostics: []
  });
  assert.equal(shaped.stderr, 'note: DEBUG: main started\n  DEBUG: logger setup done');
});

test('a linker warning on a build that succeeded is a link-warning, and the command still goes', () => {
  const shaped = shapeCompileOutput({
    success: true,
    stdout: '',
    stderr:
      '   Compiling hello v0.1.0 (/blocks/hello)\n' +
      'Linking using "wasm-ld" "-flavor" "wasm"\n' +
      'wasm-ld: warning: function signature mismatch: f\n' +
      '    Finished `release` profile [optimized] target(s) in 1.00s',
    diagnostics: []
  });
  assert.doesNotMatch(shaped.stderr, LINKING);
  assert.match(shaped.stderr, /^wasm-ld: warning: function signature mismatch: f$/m);
  assert.deepEqual(shaped.diagnostics, [
    {
      file: '',
      line: 0,
      column: 0,
      severity: 'warning',
      code: 'link-warning',
      message: 'wasm-ld: function signature mismatch: f'
    }
  ]);
});
