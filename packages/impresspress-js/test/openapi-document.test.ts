import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { buildDocument } from "../scripts/openapi-document.mjs";

/** The merged `components/schemas` table, by name. */
function schemas(document: ReturnType<typeof buildDocument>): Record<string, unknown> {
  return document.components.schemas as Record<string, unknown>;
}

/** Every `$ref` target anywhere under `value`. */
function refs(value: unknown, out: string[] = []): string[] {
  if (Array.isArray(value)) {
    value.forEach((v) => refs(v, out));
  } else if (value && typeof value === "object") {
    const ref = (value as Record<string, unknown>)["$ref"];
    if (typeof ref === "string") out.push(ref);
    Object.values(value).forEach((v) => refs(v, out));
  }
  return out;
}

/** Resolve a document-local JSON pointer (`#/a/b~1c`). */
function resolve(doc: unknown, ref: string): unknown {
  return ref
    .replace(/^#\//, "")
    .split("/")
    .map((part) => part.replace(/~1/g, "/").replace(/~0/g, "~"))
    .reduce<unknown>((node, key) => (node as Record<string, unknown> | undefined)?.[key], doc);
}

describe("the document assembled from the block snapshots", () => {
  /**
   * The snapshots carry the `components/schemas` their paths reach, and the
   * merge keeps them: products' offer schemas reach the recursive
   * `Condition`, and a document without it fails `openapi-typescript` with
   * "Can't resolve $ref".
   */
  it("resolves every $ref it contains", () => {
    const document = buildDocument();
    expect(schemas(document).Condition).toBeTypeOf("object");
    const targets = refs(document);
    expect(targets).toContain("#/components/schemas/Condition");
    for (const target of targets) {
      expect(resolve(document, target), `${target} resolves to nothing`).toBeDefined();
    }
  });

  it("refuses two blocks that give one schema name different definitions", () => {
    const dir = mkdtempSync(join(tmpdir(), "openapi-document-"));
    const fragment = (path: string, schema: object) =>
      JSON.stringify({ components: { schemas: { Shared: schema } }, paths: { [path]: {} } });
    try {
      writeFileSync(join(dir, "a.openapi.json"), fragment("/b/a", { type: "string" }));
      writeFileSync(join(dir, "b.openapi.json"), fragment("/b/b", { type: "integer" }));
      expect(() => buildDocument(dir)).toThrow(/components\/schemas\/Shared differs/);

      writeFileSync(join(dir, "b.openapi.json"), fragment("/b/b", { type: "string" }));
      expect(schemas(buildDocument(dir)).Shared).toEqual({ type: "string" });
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
