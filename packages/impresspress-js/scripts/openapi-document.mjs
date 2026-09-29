/**
 * Assemble one OpenAPI document from the committed per-block snapshots.
 *
 * The snapshots are `paths` FRAGMENTS, not documents: `openapi_snapshot.rs`'s
 * `block_openapi` writes only the filtered, BTreeMap-sorted `paths` map, with
 * no `openapi` / `info` / `components` keys. They are the right authority
 * anyway — deterministic, regenerable offline with no booted server, and
 * already gated by a Rust test. The live `/openapi.json` route is tier-
 * filtered per caller, so what it serves depends on who asks.
 *
 * Every `*.openapi.json` in the directory is included, discovered by glob
 * rather than listed here: a block that gains a snapshot must not need a
 * second edit in this package to enter the generated types.
 */
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));

/** `crates/impresspress-core/tests/snapshots`, relative to this package. */
export const SNAPSHOT_DIR = join(HERE, "..", "..", "..", "crates", "impresspress-core", "tests", "snapshots");

/** The generated types, relative to this package. */
export const OUTPUT_FILE = join(HERE, "..", "src", "generated", "api.ts");

/** Snapshot file names, sorted, so the merge order is stable. */
export function snapshotFiles(dir = SNAPSHOT_DIR) {
  return readdirSync(dir)
    .filter((name) => name.endsWith(".openapi.json"))
    .sort();
}

/**
 * The merged document.
 *
 * A path served by two blocks would mean one of them is claiming the other's
 * prefix, which is a routing bug rather than something to merge quietly, so a
 * collision throws.
 */
export function buildDocument(dir = SNAPSHOT_DIR) {
  const paths = {};
  const owner = {};

  for (const file of snapshotFiles(dir)) {
    const block = file.replace(/\.openapi\.json$/, "");
    const fragment = JSON.parse(readFileSync(join(dir, file), "utf8"));
    for (const [path, item] of Object.entries(fragment)) {
      if (path in paths) {
        throw new Error(
          `${path} is published by both \`${owner[path]}\` and \`${block}\`; ` +
            `each block owns its own URL prefix, so this is a routing bug, not a merge`,
        );
      }
      paths[path] = item;
      owner[path] = block;
    }
  }

  if (Object.keys(paths).length === 0) {
    throw new Error(`no paths found under ${dir} - the snapshots are missing or empty`);
  }

  return {
    openapi: "3.1.0",
    // This document is an assembly artifact, not a released API version: the
    // per-block snapshots carry no version and the generated types do not
    // reproduce `info`, so a value here would be noise that has to be
    // maintained.
    info: { title: "Impresspress", version: "0.0.0" },
    paths,
    components: {
      // Referenced by every `security: [{ bearerAuth: [] }]` operation.
      securitySchemes: {
        bearerAuth: { type: "http", scheme: "bearer", bearerFormat: "JWT" },
      },
    },
  };
}
