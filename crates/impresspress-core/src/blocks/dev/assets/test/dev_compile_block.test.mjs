// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_compile_block.test.mjs
//
// `dev_compile_block` — the half of it that needs no browser.
//
// The compile itself needs a `Worker`, a `SharedArrayBuffer` and 72 MiB of
// toolchain, and it is driven end to end by `dev-compile-tool.spec.ts`. What
// is here is everything AROUND that: reading a block out of the workspace,
// the digest that ties a stored build back to the exact sources it was made
// from, and the split between a failure that is an ANSWER about the block
// (`success: false`, no `isError`) and one that is a failure of the machinery
// (`isError`, and nothing to report about the block). Every one of those is a
// decision the page makes on its own, and none of them is worth a forty-second
// browser test.
//
// See `harness.mjs` for how the tail is loaded without adding a test hook to
// the shipped file.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { instantiate } from './harness.mjs';

/** The real manifest's shape, trimmed to the fields the page reads. */
const MANIFEST = {
  schema_version: 1,
  version: '807ace9e',
  entry: '/__impresspress_dev/compiler/807ace9e/worker.js',
  total_bytes: 75124831,
  target: 'wasm32-wasip1'
};

/** One macrotask, which is long enough for the tail's load-time work. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** The `sha256` a workspace entry reports, from its own content. */
const digest = (content) => createHash('sha256').update(content).digest('hex');

/** A `blocks/hello/` scaffold, as `dev_create_block` leaves it. */
const file = (path, content, extra = {}) => ({
  path,
  content,
  sha256: digest(content),
  ...extra
});

const CARGO_TOML = '[package]\nname = "hello"\n';
const LIB_RS = 'pub fn block() -> Block { Block::new("site/hello", "Says hello") }\n';

const HELLO = [
  file('blocks/hello/Cargo.toml', CARGO_TOML),
  file('blocks/hello/src/lib.rs', LIB_RS)
];

test('snapshotBlock reads the block crate-relative, and nothing about the guest out of it', async () => {
  const { handle } = instantiate({ workspace: HELLO });
  const snapshot = await handle.snapshotBlock('hello');

  // The worker's VFS is keyed on paths relative to the crate root: it writes
  // `Cargo.toml`, not `blocks/hello/Cargo.toml`. A snapshot that kept the
  // workspace prefix would produce a crate cargo cannot see.
  assert.deepEqual(Object.keys(snapshot.files).sort(), ['Cargo.toml', 'src/lib.rs']);
  assert.equal(snapshot.files['src/lib.rs'], LIB_RS);
  assert.deepEqual(snapshot.diagnostics, []);
  // The block depends on the guest crate by path and carries no copy of it,
  // so it is not legacy and there is no version to read out of it: the one
  // the page reports is the one the compiler session was built from (see the
  // staging tests).
  assert.equal(snapshot.legacy, false);
  assert.equal(snapshot.legacyGuestVersion, null);
});

test('snapshotBlock refuses a binary file under blocks/ with a binary-source diagnostic', async () => {
  const { handle } = instantiate({
    workspace: [
      ...HELLO,
      file('blocks/hello/src/logo.png', 'iVBORw0KGgo=', { encoding: 'base64' })
    ]
  });
  const snapshot = await handle.snapshotBlock('hello');

  // Not skipped: a crate that compiled without a file its source references
  // would produce a block whose sources are not the sources on disk.
  assert.equal(snapshot.diagnostics.length, 1);
  assert.equal(snapshot.diagnostics[0].severity, 'error');
  assert.equal(snapshot.diagnostics[0].code, 'binary-source');
  // Crate-relative, like everything else the compiler is told about.
  assert.equal(snapshot.diagnostics[0].file, 'src/logo.png');
  assert.match(snapshot.diagnostics[0].message, /base64/);
  assert.ok(!('src/logo.png' in snapshot.files));
});

test('snapshotBlock refuses a nested source directory with a nested-source diagnostic', async () => {
  const { handle, fetchCalls } = instantiate({
    workspace: [...HELLO, file('blocks/hello/src/routes/mod.rs', 'pub fn r() {}\n')]
  });
  const snapshot = await handle.snapshotBlock('hello');

  // Whether rubrc's write-file event creates intermediate directories has
  // never been verified, so a nested path is refused with the path in hand
  // rather than sent and left to fail inside rustc.
  assert.equal(snapshot.diagnostics.length, 1);
  assert.equal(snapshot.diagnostics[0].severity, 'error');
  assert.equal(snapshot.diagnostics[0].code, 'nested-source');
  assert.equal(snapshot.diagnostics[0].file, 'src/routes/mod.rs');
  assert.ok(!('src/routes/mod.rs' in snapshot.files));
  // Refused on the path alone: the contents cannot change the answer, so the
  // file is never read.
  assert.ok(
    !fetchCalls.some(
      (call) =>
        String(call[0]) === '/b/dev/api/files/read' &&
        JSON.parse(call[1].body).path === 'blocks/hello/src/routes/mod.rs'
    )
  );
});

test('a subdirectory that is not under src/ is refused by the same rule', async () => {
  const { handle, fetchCalls } = instantiate({
    workspace: [
      ...HELLO,
      file('blocks/hello/tests/smoke.rs', '#[test] fn t() {}\n'),
      file('blocks/hello/assets/logo.svg', '<svg/>\n')
    ]
  });
  const snapshot = await handle.snapshotBlock('hello');

  // The reason the rule exists is the VFS write, not the `src/` prefix: a
  // crate-relative path with a directory in it needs an intermediate
  // directory wherever it sits, and `tests/smoke.rs` used to sail past the
  // guard and fail inside the worker forty seconds later instead.
  // Sorted, because the order is the file listing's and not this rule's.
  assert.deepEqual(
    snapshot.diagnostics.map((d) => `${d.code} ${d.file}`).sort(),
    ['nested-source assets/logo.svg', 'nested-source tests/smoke.rs']
  );
  assert.ok(!('tests/smoke.rs' in snapshot.files));
  assert.ok(!('assets/logo.svg' in snapshot.files));
  // The flat crate itself is untouched by the widened rule.
  assert.deepEqual(Object.keys(snapshot.files).sort(), ['Cargo.toml', 'src/lib.rs']);
  assert.ok(
    !fetchCalls.some(
      (call) =>
        String(call[0]) === '/b/dev/api/files/read' &&
        JSON.parse(call[1].body).path === 'blocks/hello/tests/smoke.rs'
    )
  );
});

test('snapshotBlock refuses a Cargo.toml whose package is not the block', async () => {
  const { handle } = instantiate({
    workspace: [
      file('blocks/hello/Cargo.toml', '[package]\nname = "renamed"\n'),
      HELLO[1]
    ]
  });
  const snapshot = await handle.snapshotBlock('hello');

  // cargo names the artifact after the package and the worker reads it back
  // by the BLOCK's name, so a rename is a green build with nothing to
  // collect. Both names are in the message, because the fix is one of them.
  assert.equal(snapshot.diagnostics.length, 1);
  assert.equal(snapshot.diagnostics[0].code, 'package-name');
  assert.equal(snapshot.diagnostics[0].file, 'Cargo.toml');
  assert.match(snapshot.diagnostics[0].message, /renamed\.wasm/);
  assert.match(snapshot.diagnostics[0].message, /"hello"/);
});

test('a `[lib] name` that renames the artifact is refused too', async () => {
  const { handle } = instantiate({
    workspace: [
      file(
        'blocks/hello/Cargo.toml',
        '[package]\nname = "hello"\n\n[lib]\ncrate-type = ["cdylib"]\nname = "other"\n'
      ),
      HELLO[1]
    ]
  });
  // `[lib] name` is what cargo names a cdylib after, so it breaks the artifact
  // path exactly as a renamed package does — the check is on the file cargo
  // will write, not on one key.
  const snapshot = await handle.snapshotBlock('hello');
  assert.equal(snapshot.diagnostics.length, 1);
  assert.equal(snapshot.diagnostics[0].code, 'package-name');
  assert.match(snapshot.diagnostics[0].message, /other\.wasm/);
});

test('a name in a table that is neither [package] nor [lib] is not read as one', async () => {
  const { handle } = instantiate({
    workspace: [
      file(
        'blocks/hello/Cargo.toml',
        '[package]\nname = "hello"\n\n[lib]\ncrate-type = ["cdylib"]\n\n' +
          '[[bench]]\nname = "not-the-crate"\n'
      ),
      HELLO[1]
    ]
  });
  assert.deepEqual((await handle.snapshotBlock('hello')).diagnostics, []);
});

test('a hyphenated block matches the underscored artifact cargo writes', async () => {
  const { handle } = instantiate({
    workspace: [
      file('blocks/my-shop/Cargo.toml', '[package]\nname = "my-shop"\n'),
      file('blocks/my-shop/src/lib.rs', LIB_RS)
    ]
  });
  // cargo writes `my_shop.wasm` for a package called `my-shop`, and the worker
  // asks for exactly that — so this must not be a refusal.
  assert.deepEqual((await handle.snapshotBlock('my-shop')).diagnostics, []);
});

test('the source manifest digest is over sorted `path\\0sha256\\n` lines, crate-relative', async () => {
  const { handle } = instantiate({ workspace: HELLO });
  const snapshot = await handle.snapshotBlock('hello');

  // Spelled out here rather than recomputed by the same code under test: the
  // digest is the only link between a stored build and the exact bytes it was
  // made from, so this is the definition of it. NUL separates because a path
  // may contain anything else, and the paths are the ones the COMPILER saw.
  const expected = digest(
    [
      `Cargo.toml\0${digest(CARGO_TOML)}\n`,
      `src/lib.rs\0${digest(LIB_RS)}\n`
    ].join('')
  );
  assert.equal(snapshot.sourceSha, expected);
});

test('the source manifest digest changes when a source byte does', async () => {
  const { handle: before } = instantiate({ workspace: HELLO });
  const { handle: after } = instantiate({
    workspace: [HELLO[0], file('blocks/hello/src/lib.rs', `${LIB_RS}// a comment\n`)]
  });
  assert.notEqual(
    (await before.snapshotBlock('hello')).sourceSha,
    (await after.snapshotBlock('hello')).sourceSha
  );
});

test('snapshotBlock refuses a name with nothing under it rather than compiling an empty crate', async () => {
  const { handle } = instantiate({ workspace: HELLO });
  await assert.rejects(() => handle.snapshotBlock('nope'), /no block at blocks\/nope\//);
});

test('the Compile select is populated from the blocks/ prefixes in the file listing', async () => {
  const { elements } = instantiate({
    workspace: [
      ...HELLO,
      file('blocks/newsletter/Cargo.toml', '[package]\nname = "newsletter"\n'),
      file('site/index.html', '<!doctype html>\n')
    ]
  });
  await settle();

  // One option per block, and nothing for `site/`: a block IS a
  // `blocks/<name>/` prefix with files under it. In path order, because the
  // listing is.
  assert.deepEqual(
    elements.get('dev-compile-block').children.map((option) => option.value),
    ['hello', 'newsletter']
  );
});

test('with no compiler in the build, dev_compile_block is an isError and not a failed build', async () => {
  const { tools } = instantiate({
    hasModelContext: true,
    compilerManifest: null,
    workspace: HELLO
  });
  await settle();

  const result = await tools.get('dev_compile_block').execute({ name: 'hello' });
  // "This build has no toolchain" is not a verdict on the block: there is no
  // build to report diagnostics about, and an agent told `success: false`
  // would go looking for a mistake in its Rust.
  assert.equal(result.isError, true);
  assert.match(result.content[0].text, /No compiler in this build\./);
  assert.equal(result.structuredContent, undefined);
});

/**
 * A `BrowserRustCompiler` that answers without a worker.
 *
 * The real one is covered by `dev-compiler.spec.ts` against the protocol, and
 * by `dev-compile-tool.spec.ts` against real rustc. What the two tests below
 * need is only its CONTRACT: `compile` resolves with a `CompileResult` for a
 * crate that does not build, and rejects when the adapter itself cannot go on.
 */
function fakeCompiler(behaviour) {
  return class {
    constructor(manifest) {
      this.manifest = manifest;
    }

    async initialize() {
      return 'rustc 1.90.0-nightly (fake)';
    }

    async compile() {
      return behaviour();
    }
  };
}

const BUILT = {
  buildId: 'build-1',
  success: true,
  cancelled: false,
  artifact: Uint8Array.from([0, 1, 2, 3]),
  artifactSha256: 'unused',
  stdout: 'Finished `release` profile',
  stderr: '',
  diagnostics: [],
  elapsedMs: 38000,
  compilerVersion: 'rustc 1.90.0-nightly (fake)',
  guestVersion: 1
};

test('a crate that does not compile is a result, and the diagnostics keep rustc shape', async () => {
  const { tools } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: fakeCompiler(() => ({
      ...BUILT,
      success: false,
      artifact: null,
      stderr: 'error: expected `;`, found `value`\n',
      // The second one is what a rustc diagnostic without a number looks
      // like: `code` is optional in `compiler/src/protocol.ts` and required
      // in `validation::Diagnostic`.
      diagnostics: [
        { file: 'src/lib.rs', line: 12, column: 4, severity: 'error', message: 'expected `;`', code: 'E0425' },
        { file: 'src/lib.rs', line: 3, column: 1, severity: 'warning', message: 'unused import' }
      ]
    }))
  });
  await settle();

  const result = await tools.get('dev_compile_block').execute({ name: 'hello' });
  assert.equal(result.isError, undefined);
  assert.equal(result.structuredContent.success, false);
  // Nothing was staged, so there is no build row and no generation.
  assert.equal(result.structuredContent.build_id, null);
  assert.equal(result.structuredContent.generation, null);
  assert.equal(result.structuredContent.elapsed_ms, 38000);
  assert.deepEqual(result.structuredContent.diagnostics[0], {
    severity: 'error',
    code: 'E0425',
    message: 'expected `;`',
    file: 'src/lib.rs',
    line: 12,
    column: 4
  });
  // rustc numbered the first and not the second, and that is exactly what is
  // reported: `validation::Diagnostic.code` is an `Option<String>`, so the
  // page has no reason to invent a value for a diagnostic nobody coded.
  assert.equal(result.structuredContent.diagnostics[1].code, null);
  assert.equal(result.structuredContent.cancelled, false);
});

test('a compile the adapter gave up on is a timeout, not a crate that does not build', async () => {
  const { tools } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    // What `#abandonInFlight` resolves with when the 120 s budget expires: a
    // RESULT, with `cancelled` set and nothing at all to say about the crate.
    compiler: fakeCompiler(() => ({
      ...BUILT,
      success: false,
      cancelled: true,
      artifact: null,
      stdout: '',
      stderr: 'the compile did not finish within 120000 ms',
      diagnostics: []
    }))
  });
  await settle();

  const result = await tools.get('dev_compile_block').execute({ name: 'hello' });
  // Still a result — the page asked for the cancel, so there is nothing for
  // `isError` to report.
  assert.equal(result.isError, undefined);
  assert.equal(result.structuredContent.success, false);
  assert.equal(result.structuredContent.cancelled, true);
  // …and it does NOT come back as a failed build with an empty diagnostics
  // list, which is what an agent would answer by rewriting Rust that was
  // never the problem.
  assert.equal(result.structuredContent.diagnostics.length, 1);
  assert.equal(result.structuredContent.diagnostics[0].code, 'compile-timeout');
  assert.equal(result.structuredContent.diagnostics[0].severity, 'error');
  assert.equal(result.structuredContent.diagnostics[0].file, null);
  // The adapter's own account of it is carried into the message, so a budget
  // that moves is still visible on the page.
  assert.match(result.structuredContent.diagnostics[0].message, /did not finish within 120000 ms/);
});

test('a successful compile stages the artifact and merges what staging answered', async () => {
  const staged = [];
  // The sandbox as it stands, which the compile below changes: staging
  // activates a generation, so the status the page reads afterwards is the
  // one that generation produced. Modelling that is the whole point here —
  // the ladder assertion at the end is about what the page shows once the
  // activation is OVER and the journal has gone quiet again.
  let live = null;
  const { tools, elements } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: fakeCompiler(() => BUILT),
    status: () => ({
      active_generation: live,
      runtime_generation: live ? 1 : 0,
      blocks: [],
      activation: null,
      wafer_guest_version: 1
    }),
    stage(request) {
      staged.push(request);
      live = { id: 'gen_2', cause: 'block_compile', status: 'active' };
      return {
        build_id: 'bld_1',
        success: true,
        diagnostics: [],
        generation: { id: 'gen_2', cause: 'block_compile', status: 'active' },
        progress: [
          { phase: 'validating', ms: 4, detail: '' },
          { phase: 'building_runtime', ms: 91, detail: '' },
          { phase: 'publishing', ms: 2, detail: '' },
          { phase: 'active', ms: 0, detail: '' }
        ]
      };
    }
  });
  await settle();

  const result = await tools.get('dev_compile_block').execute({ name: 'hello' });
  assert.equal(result.isError, undefined);
  assert.equal(result.structuredContent.success, true);
  assert.equal(result.structuredContent.build_id, 'bld_1');
  assert.equal(result.structuredContent.generation.cause, 'block_compile');
  assert.equal(result.structuredContent.progress.length, 4);

  // The request the page built. The key SET is asserted, not just the fields
  // below: `StageBuildRequest` is `deny_unknown_fields`, so one extra key is a
  // 400 that no assertion about the fields it does carry would ever catch.
  assert.equal(staged.length, 1);
  assert.deepEqual(Object.keys(staged[0]).sort(), [
    'artifact_base64',
    'block_name',
    'compiler_version',
    'diagnostics',
    'source_manifest_sha256',
    'wafer_guest_version'
  ]);
  assert.equal(staged[0].block_name, 'hello');
  assert.equal(staged[0].artifact_base64, Buffer.from(BUILT.artifact).toString('base64'));
  assert.equal(staged[0].wafer_guest_version, 1);
  assert.equal(staged[0].compiler_version, 'rustc 1.90.0-nightly (fake)');
  assert.match(staged[0].source_manifest_sha256, /^[0-9a-f]{64}$/);

  // The ladder shows the activation that just finished. The journal rests at
  // `idle` the moment the swap is recorded, so the staging response is the
  // only account of those phases that survives the catch-up poll — a page
  // that drew the ladder from the status alone would blank it here.
  assert.equal(elements.get('dev-progress-steps').getAttribute('data-phase'), 'active');
  assert.deepEqual(
    elements.get('dev-progress-steps').children.map((step) => step.getAttribute('data-state')),
    ['done', 'done', 'done', 'done']
  );
});

test('the compiler is constructed with the guest the API hands out, and the staging request carries its version', async () => {
  const seen = { guest: null, staged: null };
  // `GuestResponse` as `GET /b/dev/api/guest` answers it — the API's own
  // spelling (`crate_name`), which the adapter, not the page, translates.
  const guest = {
    version: 2,
    files: {
      'Cargo.toml': '[package]\nname = "wafer_guest"\n',
      'src/lib.rs': 'pub const WAFER_GUEST_VERSION: u32 = 2;\n'
    },
    warmup: { crate_name: 'hello', files: { 'Cargo.toml': CARGO_TOML, 'src/lib.rs': LIB_RS } }
  };
  class Stub {
    constructor(manifest, options) {
      seen.guest = options.guest;
    }

    async initialize() {
      return 'rustc 1.90.0-nightly (fake)';
    }

    async compile() {
      return { ...BUILT, guestVersion: 2 };
    }
  }
  const { handle } = instantiate({
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: Stub,
    guest,
    status: { active_generation: null, runtime_generation: 0, blocks: [], activation: null },
    stage(request) {
      seen.staged = request;
      return {
        build_id: 'bld_1',
        success: true,
        diagnostics: [],
        generation: { id: 'gen_2', cause: 'block_compile', status: 'active' },
        progress: []
      };
    }
  });
  await settle();

  await handle.compileBlock('hello');
  assert.deepEqual(seen.guest, guest);
  // The version of the guest the session was BUILT from, as the compile
  // reported it — a `2` the harness's default guest (`1`) could not produce.
  assert.equal(seen.staged.wafer_guest_version, 2);
});

/**
 * Compile `workspace` as `hello` with a session whose guest is version 2, and
 * return the staging request the page sent.
 */
async function stagedFor(workspace) {
  let staged = null;
  const { handle } = instantiate({
    compilerManifest: MANIFEST,
    workspace,
    compiler: fakeCompiler(() => ({ ...BUILT, guestVersion: 2 })),
    status: { active_generation: null, runtime_generation: 0, blocks: [], activation: null },
    stage(request) {
      staged = request;
      return {
        build_id: 'bld_1',
        success: true,
        diagnostics: [],
        generation: { id: 'gen_2', cause: 'block_compile', status: 'active' },
        progress: []
      };
    }
  });
  await settle();
  await handle.compileBlock('hello');
  return staged;
}

test('a legacy block reports the version of the guest module it vendors, not the session\'s', async () => {
  // An old three-file block: its own copy of the SDK, compiled as part of the
  // block's crate. The session's guest crate (version 2) never enters that
  // build, so staging must be told the version the artifact was actually made
  // against — the gate that refuses a stale ABI is a claim about the artifact.
  const staged = await stagedFor([
    ...HELLO,
    file(
      'blocks/hello/src/wafer_guest.rs',
      '/// The ABI.\npub const WAFER_GUEST_VERSION: u32 = 1;\n'
    )
  ]);
  assert.equal(staged.wafer_guest_version, 1);
});

test('a legacy block whose vendored module states no version reports null, not the session\'s', async () => {
  const staged = await stagedFor([
    ...HELLO,
    file('blocks/hello/src/wafer_guest.rs', 'pub const WAFER_GUEST_VERSION: usize = 1;\n')
  ]);
  // `null` is "unknown", which staging records as `0` — an honest absence
  // rather than the session's version standing in for a module it did not build.
  assert.equal(staged.wafer_guest_version, null);
});

test('a two-file block reports the session\'s guest version', async () => {
  const staged = await stagedFor(HELLO);
  assert.equal(staged.wafer_guest_version, 2);
});

test('a guest crate the API would not serve is an isError, and the next compile fetches it again', async () => {
  let guestHits = 0;
  let constructed = 0;
  class Stub {
    constructor() {
      constructed += 1;
    }

    async initialize() {
      return 'rustc 1.90.0-nightly (fake)';
    }

    async compile() {
      return BUILT;
    }
  }
  const { tools } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: Stub,
    guest() {
      guestHits += 1;
      return guestHits === 1
        ? { status: 500, body: { error: 'internal', message: 'boom' } }
        : {
            body: {
              version: 1,
              files: {
                'Cargo.toml': '[package]\nname = "wafer_guest"\n',
                'src/lib.rs': 'pub const WAFER_GUEST_VERSION: u32 = 1;\n'
              },
              warmup: { crate_name: 'hello', files: { 'Cargo.toml': CARGO_TOML, 'src/lib.rs': LIB_RS } }
            }
          };
    },
    status: { active_generation: null, runtime_generation: 0, blocks: [], activation: null },
    stage: () => ({
      build_id: 'bld_1',
      success: true,
      diagnostics: [],
      generation: { id: 'gen_2', cause: 'block_compile', status: 'active' },
      progress: []
    })
  });
  await settle();

  const first = await tools.get('dev_compile_block').execute({ name: 'hello' });
  // Not a verdict on the block: the session could not be started at all, and
  // the message says which request failed rather than reading as the
  // build's own failure.
  assert.equal(first.isError, true);
  assert.equal(first.structuredContent, undefined);
  const prefix = 'dev_compile_block: ';
  assert.ok(first.content[0].text.startsWith(prefix), first.content[0].text);
  const message = first.content[0].text.slice(prefix.length);
  assert.ok(message.startsWith('fetching the guest crate: '), message);
  assert.match(message, /HTTP 500/);
  assert.equal(guestHits, 1);
  // No session was built from a guest the page does not have.
  assert.equal(constructed, 0);

  // A failed fetch is not remembered: the next compile asks again, and this
  // time the session starts.
  const second = await tools.get('dev_compile_block').execute({ name: 'hello' });
  assert.equal(second.isError, undefined);
  assert.equal(second.structuredContent.success, true);
  assert.equal(guestHits, 2);
  assert.equal(constructed, 1);
});

test('the status is not polled while the worker compiles, only while the sandbox activates', async () => {
  // A compiler that stops in the middle of `compile`, which is where the real
  // one spends ~40 seconds. Everything asserted below is asserted THERE.
  let release;
  const held = new Promise((resolve) => {
    release = resolve;
  });
  let statusReads = 0;
  const { handle, tools, elements } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: fakeCompiler(async () => {
      await held;
      return BUILT;
    }),
    status: () => {
      statusReads += 1;
      return { active_generation: null, runtime_generation: 0, blocks: [], activation: null };
    },
    stage: () => ({
      build_id: 'bld_1',
      success: true,
      diagnostics: [],
      generation: { id: 'gen_2', cause: 'block_compile', status: 'active' },
      progress: [{ phase: 'active', ms: 0, detail: '' }]
    })
  });
  await settle();
  const readsBeforeCompile = statusReads;

  const running = tools.get('dev_compile_block').execute({ name: 'hello' });
  await settle();

  // The whole point: nothing is polling. `/b/dev/api/status` cannot say
  // anything new while the work is in the worker, and on a D1-backed
  // deployment every one of those reads is real.
  assert.equal(handle.outstanding, 0);
  assert.equal(handle.isPolling, false);
  assert.equal(statusReads, readsBeforeCompile);
  // …and the button is out of reach for the duration, so a second click
  // cannot queue a second identical build behind this one.
  assert.equal(elements.get('dev-compile').disabled, true);
  assert.match(elements.get('dev-compile').title, /already running/);

  release();
  await running;

  // Back, because staging is over and there is a toolchain and a block again.
  assert.equal(elements.get('dev-compile').disabled, false);
  assert.equal(elements.get('dev-compile').title, '');
  // Staging DID open the window — the ladder the panel shows comes from the
  // catch-up that closing it runs.
  assert.ok(statusReads > readsBeforeCompile);
});

test('a second compile is refused while one is running, not queued behind it', async () => {
  // A compile that does not finish until this test says so, which is the only
  // way to have two of them in flight at once.
  let release;
  const running = new Promise((resolve) => {
    release = resolve;
  });
  let compiles = 0;
  const { tools, handle, elements } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: fakeCompiler(async () => {
      compiles += 1;
      await running;
      return { ...BUILT, success: false, artifact: null };
    })
  });
  await settle();

  const first = tools.get('dev_compile_block').execute({ name: 'hello' });
  await settle();
  assert.equal(elements.get('dev-compile').disabled, true);

  // The adapter would QUEUE this one — the worker is what refuses a
  // concurrent compile, and the page never lets it see one — so without the
  // guard this runs a second snapshot and a second build, and the first
  // one's `finally` re-enables the button while it is still going.
  const second = await tools.get('dev_compile_block').execute({ name: 'hello' });
  assert.equal(second.isError, true);
  assert.match(second.content[0].text, /a compile is already running/);
  // The first call may still be inside its snapshot fetches at this point —
  // the guard is set before any of them, which is the whole point, but how
  // far the first call has got by now is a scheduling detail (one macrotask
  // on a loaded CI runner is not always enough for it to reach the
  // compiler). What must hold HERE is that the refused call never started a
  // build of its own; how many builds ran in total is asserted once the
  // first call has finished.
  assert.ok(compiles <= 1, 'the refused call started a build');
  // Refused, so the button state still belongs to the compile that is running.
  assert.equal(elements.get('dev-compile').disabled, true);

  // And a refusal does not clear the first compile's flag on the way out:
  // the button comes back only when the build it belongs to ends.
  release();
  await first;
  assert.equal(compiles, 1, 'exactly one build ran: the second call never reached the compiler');
  assert.equal(elements.get('dev-compile').disabled, false);
  // Which is also when the next one is accepted.
  await assert.doesNotReject(handle.compileBlock('hello'));
});

test('a failed compile still releases the Compile button', async () => {
  const { tools, elements } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    // A throw, not a failed build: the machinery failed, and the `finally`
    // that re-enables the button is the only thing that can run.
    compiler: fakeCompiler(() => {
      throw new Error('the worker broke its protocol');
    })
  });
  await settle();

  const result = await tools.get('dev_compile_block').execute({ name: 'hello' });
  assert.equal(result.isError, true);
  assert.equal(elements.get('dev-compile').disabled, false);
});

test('the finished ladder belongs to one compile: the next one clears it before it starts', async () => {
  // The sandbox after a compile that landed, and a staging endpoint that
  // refuses the SECOND one — the shape that used to leave a fully green
  // ladder standing over a build that never went live.
  let live = null;
  let refuse = false;
  const { tools, elements } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: fakeCompiler(() => BUILT),
    status: () => ({
      active_generation: live,
      runtime_generation: live ? 1 : 0,
      blocks: [],
      activation: null,
      wafer_guest_version: 1
    }),
    stage() {
      if (refuse) {
        return {
          build_id: 'bld_2',
          success: false,
          diagnostics: [
            {
              severity: 'error',
              code: 'cap-collection',
              message: 'the block claimed a collection outside its namespace',
              file: null,
              line: null,
              column: null
            }
          ],
          generation: null,
          progress: []
        };
      }
      live = { id: 'gen_2', cause: 'block_compile', status: 'active' };
      return {
        build_id: 'bld_1',
        success: true,
        diagnostics: [],
        generation: live,
        progress: [
          { phase: 'validating', ms: 4, detail: '' },
          { phase: 'building_runtime', ms: 91, detail: '' },
          { phase: 'publishing', ms: 2, detail: '' },
          { phase: 'active', ms: 0, detail: '' }
        ]
      };
    }
  });
  await settle();

  await tools.get('dev_compile_block').execute({ name: 'hello' });
  assert.equal(elements.get('dev-progress-steps').getAttribute('data-phase'), 'active');

  // The second compile is refused by the validator, so nothing is activated
  // and `gen_2` is still live. Without the reset the ladder would still read
  // "Active, all four done" — a green account of a build that was thrown out.
  refuse = true;
  const second = await tools.get('dev_compile_block').execute({ name: 'hello' });
  assert.equal(second.isError, undefined);
  assert.equal(second.structuredContent.success, false);
  assert.equal(second.structuredContent.diagnostics[0].code, 'cap-collection');
  assert.equal(elements.get('dev-progress-steps').getAttribute('data-phase'), 'idle');
  assert.deepEqual(
    elements.get('dev-progress-steps').children.map((step) => step.getAttribute('data-state')),
    ['pending', 'pending', 'pending', 'pending']
  );
});

test('the ladder goes blank the moment a compile starts, not when its staging lands', async () => {
  // The previous test asserts what the ladder reads once a compile is OVER.
  // This one is about the eighty seconds in between: `drawLadder` runs only
  // from `observe`, and the status is not polled while the worker works, so a
  // reset that waited for the next poll would leave the last compile's four
  // green steps standing over this one for its whole duration.
  let live = null;
  let release;
  let held = null;
  const { tools, elements } = instantiate({
    hasModelContext: true,
    compilerManifest: MANIFEST,
    workspace: HELLO,
    compiler: fakeCompiler(async () => {
      if (held) await held;
      return BUILT;
    }),
    status: () => ({
      active_generation: live,
      runtime_generation: live ? 1 : 0,
      blocks: [],
      activation: null,
      wafer_guest_version: 1
    }),
    stage() {
      live = { id: 'gen_2', cause: 'block_compile', status: 'active' };
      return {
        build_id: 'bld_1',
        success: true,
        diagnostics: [],
        generation: live,
        progress: [
          { phase: 'validating', ms: 4, detail: '' },
          { phase: 'building_runtime', ms: 91, detail: '' },
          { phase: 'publishing', ms: 2, detail: '' },
          { phase: 'active', ms: 0, detail: '' }
        ]
      };
    }
  });
  await settle();

  // One compile that lands, so there is a green ladder to be wrong about.
  await tools.get('dev_compile_block').execute({ name: 'hello' });
  const steps = elements.get('dev-progress-steps');
  assert.equal(steps.getAttribute('data-phase'), 'active');
  assert.deepEqual(
    steps.children.map((step) => step.getAttribute('data-state')),
    ['done', 'done', 'done', 'done']
  );

  // A second compile, stopped inside the worker.
  held = new Promise((resolve) => {
    release = resolve;
  });
  const running = tools.get('dev_compile_block').execute({ name: 'hello' });
  await settle();

  assert.equal(steps.getAttribute('data-phase'), 'idle');
  assert.deepEqual(
    steps.children.map((step) => step.getAttribute('data-state')),
    ['pending', 'pending', 'pending', 'pending']
  );

  // …and it fills in again from the staging response, which is the only
  // account of those phases that survives the catch-up poll.
  release();
  await running;
  assert.equal(steps.getAttribute('data-phase'), 'active');
  assert.deepEqual(
    steps.children.map((step) => step.getAttribute('data-state')),
    ['done', 'done', 'done', 'done']
  );
});
