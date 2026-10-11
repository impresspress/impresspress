#!/usr/bin/env node
//
// The export runtime carries no workspace: checked on the built wasm.
//
//   node examples/dev-sandbox/check-runtime-markers.mjs <site.wasm> <dev.wasm>
//
// `<site.wasm>` is `impresspress-web` built with `browser-site-runtime` (the
// runtime the export shell ships); `<dev.wasm>` is the same crate built with
// `browser-devtools` (the sandbox's own). `build.sh` runs this after compiling
// both, and fails the build on a non-zero exit; the dev-sandbox e2e runs it
// again on the runtime binary a real export carried (`expectASiteRuntime` in
// `crates/impresspress-web/tests/e2e/fixtures/dev-sandbox.ts`).
//
// The site build compiles none of the dev block's workspace half — its
// routes, its tools, the compiler adapter — by construction (`#[cfg(feature =
// "block-dev")]`). This is the check on the artifact that construction is
// supposed to produce: Rust string literals survive `wasm-opt -Oz` verbatim in
// the data segment, so a workspace item that leaked into the site build
// leaves its literal behind.
//
// The markers are not a list kept here. They are every workspace endpoint the
// dev block declares (`crates/impresspress-core/tests/snapshots/dev.endpoints.json`,
// the path of each row — all but exactly `/b/dev`, which stays a reserved
// route prefix in the site runtime's validation), every tool the workspace
// serves (`dev.tools.json`), and the compiler's asset root. A new endpoint or
// tool becomes a marker through its snapshot.
//
// * The site wasm must contain NONE of them.
// * The dev wasm must contain ALL of them — the positive control: a marker
//   that no longer appears in the build that does have the workspace is a
//   marker that would pass vacuously, so it fails the check instead.
// * The site wasm must contain the site runtime's own markers (the ledger
//   table and the seed-error key), so `<site.wasm>` is the site runtime and
//   not a build with no sandbox at all.
//
// One subtraction first. The dev block's runtime-half migrations are embedded
// in the site runtime (`include_str!` in `blocks/dev/migrations/mod.rs`; the
// site runtime registers the block and runs them), and their `--` comments
// name workspace tools (`dev_status`, `dev_read_reference`). That prose is not
// workspace code, and the migration text cannot be edited (a shipped
// migration is immutable, comments included — `migration_helper.rs`). So in
// BOTH wasms each embedded `*.sqlite.sql` file's exact bytes must occur
// exactly once, and are blanked before any marker is matched:
//
// * exactly once, or the check fails — a file not found means the
//   subtraction stopped matching (and the site runtime lost its own
//   migrations); found twice means it is not one embedded string;
// * blanked in the dev wasm too, so a tool named only in migration prose
//   cannot satisfy the positive control;
// * blanked with NUL bytes rather than cut out, so the bytes on either side
//   are never joined into a match that is in neither.
//
// The files subtracted are exactly the ones `migrations/mod.rs` embeds: the
// directory's `*.sqlite.sql` and the `include_str!` list must name the same
// set, so a new migration cannot be missed and an unembedded file cannot be
// subtracted.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '../..');
const CORE = path.join(REPO, 'crates/impresspress-core');
const MIGRATIONS = path.join(CORE, 'src/blocks/dev/migrations');
const ENDPOINTS = path.join(CORE, 'tests/snapshots/dev.endpoints.json');
const TOOLS = path.join(CORE, 'tests/snapshots/dev.tools.json');

/// The route prefix the site runtime keeps reserved (`validation.rs`), and
/// so names: not a workspace marker.
const RESERVED_PREFIX = '/b/dev';
const COMPILER_ROOT = '/__impresspress_dev/compiler/';
/// The site runtime's own: the generation ledger's table and the key the seed
/// import records a failure under.
const RUNTIME_MARKERS = ['impresspress__dev__generations', 'IMPRESSPRESS__DEV__SEED_ERROR'];

function fail(lines) {
  for (const line of lines) console.error(`check-runtime-markers: ${line}`);
  process.exit(1);
}

const [sitePath, devPath, ...extra] = process.argv.slice(2);
if (!sitePath || !devPath || extra.length > 0) {
  fail(['usage: node check-runtime-markers.mjs <site.wasm> <dev.wasm>']);
}

/// The embedded migrations, as `[name, bytes]`, after checking that the
/// directory and `mod.rs`'s `include_str!` list agree.
function embeddedMigrations() {
  const onDisk = fs
    .readdirSync(MIGRATIONS)
    .filter((name) => name.endsWith('.sqlite.sql'))
    .sort();
  const modRs = fs.readFileSync(path.join(MIGRATIONS, 'mod.rs'), 'utf8');
  const embedded = [...modRs.matchAll(/include_str!\("([^"]+\.sqlite\.sql)"\)/g)]
    .map((m) => m[1])
    .sort();
  if (onDisk.length === 0) fail([`no *.sqlite.sql under ${MIGRATIONS}`]);
  if (JSON.stringify(onDisk) !== JSON.stringify(embedded)) {
    fail([
      `${MIGRATIONS}: the *.sqlite.sql files and mod.rs's include_str! list differ`,
      `  on disk:  ${onDisk.join(', ')}`,
      `  embedded: ${embedded.join(', ')}`,
    ]);
  }
  return onDisk.map((name) => [name, fs.readFileSync(path.join(MIGRATIONS, name))]);
}

/// Every offset of `needle` in `haystack`.
function offsets(haystack, needle) {
  const found = [];
  for (let at = haystack.indexOf(needle); at !== -1; at = haystack.indexOf(needle, at + 1)) {
    found.push(at);
  }
  return found;
}

/// `wasmPath`'s bytes with each migration blanked, or the reasons it could
/// not be: each must occur exactly once.
function withoutMigrations(wasmPath, migrations) {
  const bytes = Buffer.from(fs.readFileSync(wasmPath));
  const problems = [];
  for (const [name, text] of migrations) {
    const found = offsets(bytes, text);
    if (found.length !== 1) {
      problems.push(
        `${wasmPath}: ${name} occurs ${found.length} times, not exactly once — ` +
          'the embedded migration text is what is subtracted before matching',
      );
      continue;
    }
    bytes.fill(0, found[0], found[0] + text.length);
  }
  return { bytes, problems };
}

/// The workspace markers, from the snapshots.
function workspaceMarkers() {
  const rows = JSON.parse(fs.readFileSync(ENDPOINTS, 'utf8'));
  const paths = rows.map((row) => {
    const [, endpoint] = row.split(' ');
    if (!endpoint) fail([`${ENDPOINTS}: a row with no path: ${JSON.stringify(row)}`]);
    return endpoint;
  });
  const tools = JSON.parse(fs.readFileSync(TOOLS, 'utf8')).tools.map((tool) => tool.name);
  if (paths.length === 0 || tools.length === 0) fail(['a snapshot lists nothing']);
  const markers = [...paths.filter((p) => p !== RESERVED_PREFIX), ...tools, COMPILER_ROOT];
  return [...new Set(markers)];
}

const migrations = embeddedMigrations();
const site = withoutMigrations(sitePath, migrations);
const dev = withoutMigrations(devPath, migrations);
// Step 2 fails before any marker is matched: a marker scan over bytes the
// subtraction did not clean says nothing.
if (site.problems.length + dev.problems.length > 0) fail([...site.problems, ...dev.problems]);

const markers = workspaceMarkers();
const leaked = markers.filter((m) => site.bytes.includes(m));
const absent = markers.filter((m) => !dev.bytes.includes(m));
const runtimeMissing = RUNTIME_MARKERS.filter((m) => !site.bytes.includes(m));

const problems = [];
if (leaked.length > 0) {
  problems.push(`${sitePath} carries workspace code — ${leaked.length} marker(s): ${leaked.join(', ')}`);
}
if (absent.length > 0) {
  problems.push(
    `${devPath} lacks ${absent.length} workspace marker(s) the check relies on: ${absent.join(', ')}`,
  );
}
if (runtimeMissing.length > 0) {
  problems.push(
    `${sitePath} lacks the site runtime's own marker(s): ${runtimeMissing.join(', ')} — ` +
      'it is not a browser-site-runtime build',
  );
}
if (problems.length > 0) fail(problems);

console.log(
  `check-runtime-markers: ${markers.length} workspace markers absent from ${path.basename(sitePath)} ` +
    `and present in ${path.basename(devPath)}; ${migrations.length} migrations subtracted from each; ` +
    `site runtime markers present`,
);
