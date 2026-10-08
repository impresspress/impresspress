/**
 * Assemble one OpenAPI document from the committed per-block snapshots.
 *
 * Each snapshot is a block's FRAGMENT, not a document: `openapi_snapshot.rs`'s
 * `block_openapi` writes `{"components": {"schemas": …}, "paths": …}` — the
 * block's filtered, BTreeMap-sorted `paths`, plus the `components/schemas`
 * entries those paths reach through `$ref` (a recursive type such as products'
 * `Condition` is hoisted there) — with no `openapi` / `info` keys. They are the
 * right authority anyway — deterministic, regenerable offline with no booted
 * server, and already gated by a Rust test. The live `/openapi.json` route is
 * tier-filtered per caller, so what it serves depends on who asks.
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
 * collision throws. Two blocks may reach the same named schema; they must
 * then carry the same definition, since `$ref`s in both resolve to the one
 * entry the merged document keeps.
 */
export function buildDocument(dir = SNAPSHOT_DIR) {
  const paths = {};
  const owner = {};
  const schemas = {};
  const schemaOwner = {};

  for (const file of snapshotFiles(dir)) {
    const block = file.replace(/\.openapi\.json$/, "");
    const fragment = JSON.parse(readFileSync(join(dir, file), "utf8"));
    if (typeof fragment.paths !== "object" || typeof fragment.components?.schemas !== "object") {
      throw new Error(
        `${file} is not a block fragment (\`{"components": {"schemas": …}, "paths": …}\`); ` +
          `regenerate it with UPDATE_OPENAPI_SNAPSHOTS=1`,
      );
    }
    for (const [path, item] of Object.entries(fragment.paths)) {
      if (path in paths) {
        throw new Error(
          `${path} is published by both \`${owner[path]}\` and \`${block}\`; ` +
            `each block owns its own URL prefix, so this is a routing bug, not a merge`,
        );
      }
      paths[path] = item;
      owner[path] = block;
    }
    for (const [name, schema] of Object.entries(fragment.components.schemas)) {
      if (name in schemas && JSON.stringify(schemas[name]) !== JSON.stringify(schema)) {
        throw new Error(
          `components/schemas/${name} differs between \`${schemaOwner[name]}\` and \`${block}\`; ` +
            `one name cannot resolve to two definitions`,
        );
      }
      schemas[name] = schema;
      schemaOwner[name] ??= block;
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
      schemas,
      // Referenced by every `security: [{ bearerAuth: [] }]` operation.
      securitySchemes: {
        bearerAuth: { type: "http", scheme: "bearer", bearerFormat: "JWT" },
      },
    },
  };
}
