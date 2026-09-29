/**
 * Regenerate `src/generated/api.ts` from the committed per-block OpenAPI
 * snapshots.
 *
 * Run it with `npm run generate:types`. CI runs the same command and then
 * `git diff --exit-code` over the output: a Rust change that reshapes a
 * described response body regenerates a different file, and the diff is the
 * failure. That gate is only worth what the snapshots describe, which is why
 * `endpoints_the_sdk_calls_publish_a_response_schema`
 * (`crates/impresspress-core/tests/openapi_snapshot.rs`) requires a response
 * schema on every endpoint this package calls.
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

import openapiTS, { astToString } from "openapi-typescript";

import { buildDocument, OUTPUT_FILE, snapshotFiles } from "./openapi-document.mjs";

const HEADER = `/**
 * GENERATED FILE - do not edit.
 *
 * Produced by \`npm run generate:types\` from the committed per-block OpenAPI
 * snapshots in \`crates/impresspress-core/tests/snapshots\`, which are
 * themselves generated from each block's \`EndpointRoute\` table. Edit the
 * Rust contract, regenerate the snapshot, then regenerate this file.
 *
 * An endpoint appears here only if it declares a schema. The Rust test
 * \`endpoints_the_sdk_calls_publish_a_response_schema\` is what keeps that set
 * from silently shrinking to "whatever already had one".
 */
`;

const document = buildDocument();
const ast = await openapiTS(document, { alphabetize: true });
const source = HEADER + astToString(ast);

mkdirSync(dirname(OUTPUT_FILE), { recursive: true });
writeFileSync(OUTPUT_FILE, source);

const paths = Object.keys(document.paths).length;
console.log(
  `wrote ${OUTPUT_FILE} - ${paths} paths from ${snapshotFiles().length} block snapshots`,
);
