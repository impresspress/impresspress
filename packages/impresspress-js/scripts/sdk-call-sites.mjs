/**
 * The SDK's HTTP call sites, read out of `src/services/*.ts`.
 *
 * This exists so the coverage number the type-freshness gate claims is
 * DERIVED rather than declared. A hand-kept list of call sites would go stale
 * the first time someone adds a method, and the gate would keep reporting the
 * old number.
 *
 * Two call shapes reach the wire, and they are the only two:
 *
 *   this.request<T>({ method: "GET", url: "/b/..." })
 *   this.call<T>("<block>", "<endpoint>", { method: "POST" })   // GET default
 *
 * `BaseService.request` is the single door (`RequestConfig.method` is
 * required, so a method is always written out), and `ExtensionsService.call`
 * builds `/b/${extension}/${endpoint}`.
 *
 * Interpolations are normalised to `{}` on both sides before matching, so
 * `/b/admin/api/iam/roles/${roleId}` matches the document's
 * `/b/admin/api/iam/roles/{id}` without this file having to know the
 * parameter's name. An interpolation that is a *call* rather than a value —
 * `${this.ownerProductPath(productId, scope)}`, which expands to several path
 * segments — cannot be resolved this way and is reported as such rather than
 * quietly counted as covered or as missing.
 *
 * The same is true of a call site whose path is not a literal at all: a
 * computed `url:`, or a `this.call(block, endpoint)` whose arguments are
 * variables. Those reach the census as `resolved: false` with the reason on
 * `raw`. They are never dropped — a dropped site leaves both the numerator
 * and the denominator, which is exactly the silent under-count this file
 * exists to prevent, and `test/api-coverage.test.ts` only tolerates
 * unresolvables matching the products-helper prefix.
 */
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVICES_DIR = join(HERE, "..", "src", "services");

/** A single path segment's worth of interpolation: a value, not a fragment. */
const SEGMENT_INTERPOLATION = /^(?:[A-Za-z0-9_$]+(?:\.[A-Za-z0-9_$]+)*|encode[A-Za-z0-9_$]*\([^()]*\))$/;

/** Replace `{a}` / `{a...}` path parameters with `{}`. */
export function normalizeTemplate(path) {
  return path.replace(/\{[^}]*\}/g, "{}");
}

/**
 * Turn a source-literal path into a comparable template, or return `null`
 * when an interpolation spans more than one segment.
 */
function normalizeLiteral(raw) {
  let out = "";
  let i = 0;
  while (i < raw.length) {
    if (raw.startsWith("${", i)) {
      const end = matchBrace(raw, i + 1);
      if (end === -1) return null;
      const expr = raw.slice(i + 2, end).trim();
      if (!SEGMENT_INTERPOLATION.test(expr)) return null;
      out += "{}";
      i = end + 1;
      continue;
    }
    out += raw[i];
    i += 1;
  }
  // A query string is not part of the path key.
  return out.split("?")[0];
}

/** Index of the `}` matching the `{` at `open`. */
function matchBrace(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i += 1) {
    if (text[i] === "{") depth += 1;
    else if (text[i] === "}") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/**
 * Index just past a balanced `(`...`)` starting at `open`, tracking string,
 * template and comment state so a paren inside a literal does not close it.
 */
function matchParen(text, open) {
  let depth = 0;
  let i = open;
  while (i < text.length) {
    const c = text[i];
    if (c === '"' || c === "'" || c === "`") {
      i = skipString(text, i);
      continue;
    }
    if (c === "(") depth += 1;
    else if (c === ")") {
      depth -= 1;
      if (depth === 0) return i;
    }
    i += 1;
  }
  return -1;
}

/** Index of the closing quote of the string starting at `start`. */
function skipString(text, start) {
  const quote = text[start];
  let i = start + 1;
  while (i < text.length) {
    if (text[i] === "\\") {
      i += 2;
      continue;
    }
    if (quote === "`" && text.startsWith("${", i)) {
      const end = matchBrace(text, i + 1);
      i = end === -1 ? text.length : end + 1;
      continue;
    }
    if (text[i] === quote) return i + 1;
    i += 1;
  }
  return text.length;
}

/** Skip a balanced `<`...`>` type-argument list starting at `open`. */
function matchAngle(text, open) {
  let depth = 0;
  let i = open;
  while (i < text.length) {
    const c = text[i];
    if (c === '"' || c === "'" || c === "`") {
      i = skipString(text, i);
      continue;
    }
    if (c === "<") depth += 1;
    else if (c === ">") {
      depth -= 1;
      if (depth === 0) return i;
    }
    i += 1;
  }
  return -1;
}

/** The argument-list text of every `marker(...)` in `source`. */
function callArguments(source, marker) {
  const found = [];
  let from = 0;
  for (;;) {
    const at = source.indexOf(marker, from);
    if (at === -1) return found;
    let cursor = at + marker.length;
    while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    if (source[cursor] === "<") {
      const angle = matchAngle(source, cursor);
      if (angle === -1) return found;
      cursor = angle + 1;
      while (/\s/.test(source[cursor] ?? "")) cursor += 1;
    }
    if (source[cursor] !== "(") {
      // A reference to the method, not a call of it (`base.service.ts`
      // defines `request`, `extensions.service.ts` defines `call`).
      from = at + marker.length;
      continue;
    }
    const close = matchParen(source, cursor);
    if (close === -1) return found;
    found.push({ text: source.slice(cursor + 1, close), index: at });
    from = close;
  }
}

/** Split an argument list at top-level commas. */
function splitArguments(text) {
  const parts = [];
  let depth = 0;
  let start = 0;
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === '"' || c === "'" || c === "`") {
      i = skipString(text, i);
      continue;
    }
    if ("([{".includes(c)) depth += 1;
    else if (")]}".includes(c)) depth -= 1;
    else if (c === "," && depth === 0) {
      parts.push(text.slice(start, i));
      start = i + 1;
    }
    i += 1;
  }
  parts.push(text.slice(start));
  return parts.map((p) => p.trim()).filter((p) => p.length > 0);
}

/** The raw body of a leading string/template literal, or `null`. */
function literalBody(text) {
  const trimmed = text.trim();
  const quote = trimmed[0];
  if (quote !== '"' && quote !== "'" && quote !== "`") return null;
  const end = skipString(trimmed, 0);
  if (end !== trimmed.length) return null;
  return trimmed.slice(1, end - 1);
}

/** `method: "POST"` inside an object literal, defaulting to `fallback`. */
function methodOf(text, fallback) {
  const match = text.match(/\bmethod\s*:\s*(?:options\?\.method\s*\|\|\s*)?["']([A-Z]+)["']/);
  return match ? match[1] : fallback;
}

/**
 * The `url:` property of a request-config object literal, as one of
 * `{ kind: "literal", body }`, `{ kind: "computed", text }` (a `url:` that is
 * some other expression) or `{ kind: "absent" }` (the object has no `url:`).
 * The two non-literal kinds are returned rather than folded into `null`, so
 * the caller can report *why* a site is unresolvable.
 */
function urlOf(text) {
  const at = text.search(/\burl\s*:\s*/);
  if (at === -1) return { kind: "absent" };
  const after = text.slice(at).replace(/^\burl\s*:\s*/, "");
  const quote = after[0];
  if (quote !== '"' && quote !== "'" && quote !== "`") {
    return { kind: "computed", text: after.split(/[,\n]/)[0].replace(/[\s,}]+$/, "") };
  }
  const end = skipString(after, 0);
  return { kind: "literal", body: after.slice(1, end - 1) };
}

/**
 * Every call site in `dir` (default `src/services`), as
 * `{ service, method, path, raw, resolved }`. `resolved: false` means the
 * path could not be reduced to a template — the reason is on `raw`. The
 * directory is a parameter so the census can be exercised against a fixture.
 */
export function callSites(dir = SERVICES_DIR) {
  const sites = [];
  const files = readdirSync(dir).filter((f) => f.endsWith(".ts")).sort();

  for (const file of files) {
    const source = readFileSync(join(dir, file), "utf8");

    for (const { text } of callArguments(source, "this.request")) {
      const url = urlOf(text);
      const method = methodOf(text, "GET");
      if (url.kind !== "literal") {
        // Not a path this reader can template. Reported, never skipped: a
        // skipped site is invisible to `api-coverage.test.ts`, which is the
        // one thing this census must not do.
        const raw =
          url.kind === "absent"
            ? "this.request({ ... }) with no `url:` property"
            : `non-literal url: ${url.text}`;
        sites.push({ service: file, method, path: raw, raw, resolved: false });
        continue;
      }
      // `ExtensionsService.call` reaches the wire through `this.request`
      // with a computed url; its own call sites are collected below.
      if (url.body.startsWith("/b/${extension}")) continue;
      const path = normalizeLiteral(url.body);
      sites.push({
        service: file,
        method,
        path: path ?? url.body,
        raw: url.body,
        resolved: path !== null,
      });
    }

    for (const { text } of callArguments(source, "this.call")) {
      const args = splitArguments(text);
      const method = methodOf(args[2] ?? "", "GET");
      const block = args.length > 0 ? literalBody(args[0]) : null;
      const endpoint = args.length > 1 ? literalBody(args[1]) : null;
      if (block === null || endpoint === null) {
        // Same rule as above: a `this.call` whose block or endpoint is a
        // variable is a call site whose path is unknown, not a non-call.
        const shown = args.slice(0, 2).join(", ");
        const raw = `this.call(${shown}) with a non-literal block or endpoint`;
        sites.push({ service: file, method, path: raw, raw, resolved: false });
        continue;
      }
      const raw = `/b/${block}/${endpoint}`;
      const path = normalizeLiteral(raw);
      sites.push({
        service: file,
        method,
        path: path ?? raw,
        raw,
        resolved: path !== null,
      });
    }
  }

  return sites;
}
