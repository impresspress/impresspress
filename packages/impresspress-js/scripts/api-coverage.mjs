/**
 * How much of the SDK's call surface the type-freshness gate actually covers.
 *
 * The gate regenerates `src/generated/api.ts` from the committed OpenAPI
 * snapshots and diffs it, so it can only see a reshaped response body on an
 * endpoint whose response is described. A gate that covers only the endpoints
 * that already described theirs reports green on exactly the ones where drift
 * is possible but invisible — which is how PR #22's seven-response-body
 * reshape passed both existing snapshot gates (`1ccbb452`).
 *
 * So the number is measured, not asserted. Run:
 *
 *     node scripts/api-coverage.mjs [snapshot-dir]
 *
 * `test/api-coverage.test.ts` asserts on the same function.
 */
import { buildDocument, SNAPSHOT_DIR } from "./openapi-document.mjs";
import { callSites, normalizeTemplate } from "./sdk-call-sites.mjs";

/**
 * Call sites whose success response is deliberately not JSON. They can never
 * be covered by a JSON schema, so they are excluded from the denominator
 * rather than counted as a miss. The Rust test
 * `endpoints_the_sdk_calls_publish_a_response_schema` pins the same two from
 * the other side: each must still be a declared route, and neither may grow
 * an `application/json` response schema.
 */
export const NOT_JSON = [
  {
    method: "GET",
    path: "/b/storage/api/buckets/{}/objects/{}",
    why: "raw object bytes with the stored Content-Type",
  },
  {
    method: "POST",
    path: "/b/auth/api/verify",
    why: "an SSR HTML page - `api::verify::handle` answers `html_respond(...)` on every branch",
  },
];

/** `{ covered, uncovered, notJson, unresolved, total }` for one snapshot set. */
export function coverage(dir = SNAPSHOT_DIR) {
  const document = buildDocument(dir);
  const described = new Set();
  for (const [path, item] of Object.entries(document.paths)) {
    for (const [method, operation] of Object.entries(item)) {
      if (operation?.responses?.["200"]?.content?.["application/json"]?.schema) {
        described.add(`${method.toUpperCase()} ${normalizeTemplate(path)}`);
      }
    }
  }

  const covered = [];
  const uncovered = [];
  const notJson = [];
  const unresolved = [];

  for (const site of callSites()) {
    if (!site.resolved) {
      unresolved.push(site);
      continue;
    }
    const key = `${site.method} ${site.path}`;
    if (NOT_JSON.some((e) => `${e.method} ${e.path}` === key)) notJson.push(site);
    else if (described.has(key)) covered.push(site);
    else uncovered.push(site);
  }

  return { covered, uncovered, notJson, unresolved, total: covered.length + uncovered.length };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const dir = process.argv[2] ?? SNAPSHOT_DIR;
  const { covered, uncovered, notJson, unresolved, total } = coverage(dir);
  console.log(`snapshots: ${dir}`);
  console.log(`covered:   ${covered.length}/${total} resolvable JSON call sites`);
  console.log(`not JSON:  ${notJson.length} (excluded: ${NOT_JSON.map((e) => e.why).join("; ")})`);
  console.log(`unresolved: ${unresolved.length} (path assembled by a helper call)`);
  for (const site of uncovered.sort((a, b) => a.path.localeCompare(b.path))) {
    console.log(`  UNCOVERED ${site.method} ${site.path}   (${site.service})`);
  }
}
