/**
 * Serve `src/probe.html` over `dist/` with the headers the sandbox uses.
 *
 * The compiler needs `SharedArrayBuffer`, so the page has to be
 * cross-origin isolated, and the value that matters is the one the sandbox
 * actually deploys: `Cross-Origin-Embedder-Policy: credentialless`, not the
 * `require-corp` rubrc's own build emits. Proving the worker starts under
 * `credentialless` is the point of running the probe at all — under
 * `require-corp` every same-origin asset would need a CORP header we do not
 * set, and the failure would only show up in production.
 *
 * Usage:
 *   node scripts/serve-probe.mjs [port]     # default 8099
 *
 * Routes:
 *   /                     src/probe.html
 *   /manifest.json, /<version>/**   dist/
 *   /guest.json           what `GET /b/dev/api/guest` hands the page: the guest
 *                         SDK (`crates/wafer-guest`) and the `hello` template
 *                         as its warm-up block
 *   /template/hello.json  the `hello` block template
 *   /template/table.json  the `table` block template (package `newsletter`)
 *   /template/legacy-hello.json
 *                         `hello` in the shape a block had before the guest was
 *                         a crate: self-contained, the SDK a module inside it —
 *                         what a seed archive exported before then contains
 *
 * Every route is read from the repo on each request (the templates under
 * `crates/impresspress-core/src/blocks/dev/templates/`, the guest from
 * `crates/wafer-guest`), so the probe compiles the real thing rather than a
 * copy that can drift.
 */

import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repo = path.resolve(here, "../../..");
const dist = path.join(here, "dist");
const templates = path.join(repo, "crates/impresspress-core/src/blocks/dev/templates");
const guest = path.join(repo, "crates/wafer-guest");
const port = Number(process.argv[2] ?? process.env.PROBE_PORT ?? 8099);

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".wasm": "application/wasm",
  ".br": "application/octet-stream",
};

const send = (response, status, body, type) => {
  response.writeHead(status, {
    "Content-Type": type,
    "Content-Length": Buffer.byteLength(body),
    // The two headers that make `crossOriginIsolated` true.
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Embedder-Policy": "credentialless",
    "Cache-Control": "no-store",
  });
  response.end(body);
};

/** A crate's two files, keyed crate-relative as the protocol wants them. */
const crateFiles = (root) => ({
  "Cargo.toml": fs.readFileSync(path.join(root, "Cargo.toml"), "utf8"),
  "src/lib.rs": fs.readFileSync(path.join(root, "src/lib.rs"), "utf8"),
});

/**
 * Replace exactly one occurrence of `marker`, or throw.
 *
 * The legacy crate is derived from the live sources by string surgery; a
 * marker that moved must stop the probe, not quietly produce a crate that
 * tests something else.
 */
const replaceOnce = (text, marker, replacement, where) => {
  const at = text.indexOf(marker);
  if (at === -1 || text.indexOf(marker, at + marker.length) !== -1) {
    throw new Error(`legacy-hello: expected exactly one ${JSON.stringify(marker)} in ${where}`);
  }
  return text.slice(0, at) + replacement + text.slice(at + marker.length);
};

/**
 * `hello` as a block looked before `wafer_guest` was a crate.
 *
 * The SDK is the crate's `lib.rs` verbatim, as a module: it is a valid module
 * file, and its `export!` macro is never expanded, so the `$crate::abi` paths
 * in it never have to resolve. The block writes the five exports out itself,
 * as the old template did, and has no dependencies.
 */
const legacyHello = () => {
  const hello = crateFiles(path.join(templates, "hello"));
  const dependencies = "[dependencies]\nwafer_guest = { path = \"../../wafer_guest\" }\n";
  const cargo = replaceOnce(hello["Cargo.toml"], dependencies, "[dependencies]\n", "hello/Cargo.toml");

  let lib = replaceOnce(
    hello["src/lib.rs"],
    "use wafer_guest::*;\n",
    '#[cfg(target_arch = "wasm32")]\nmod wafer_guest;\nuse crate::wafer_guest::*;\n',
    "hello/src/lib.rs",
  );
  lib = replaceOnce(
    lib,
    "wafer_guest::export!(block, init);\n",
    `#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn __wafer_alloc(size: i32) -> i32 {
    crate::wafer_guest::abi::alloc(size)
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn __wafer_host_codec() -> i32 {
    crate::wafer_guest::abi::host_codec()
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {
    crate::wafer_guest::abi::info(&block())
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn __wafer_handle(ptr: i32, len: i32) -> i64 {
    unsafe { crate::wafer_guest::abi::handle(&block(), ptr, len) }
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn __wafer_lifecycle(ptr: i32, len: i32) -> i64 {
    unsafe { crate::wafer_guest::abi::lifecycle(init, ptr, len) }
}
`,
    "hello/src/lib.rs",
  );

  return {
    crateName: "hello",
    files: {
      "Cargo.toml": cargo,
      "src/lib.rs": lib,
      "src/wafer_guest.rs": fs.readFileSync(path.join(guest, "src/lib.rs"), "utf8"),
    },
  };
};

/** `GET /b/dev/api/guest`'s shape. `version` is read off the source: this is a test tool. */
const guestJson = () => {
  const files = crateFiles(guest);
  const version = /WAFER_GUEST_VERSION: u32 = (\d+)/.exec(files["src/lib.rs"]);
  if (!version) throw new Error("guest.json: WAFER_GUEST_VERSION not found in crates/wafer-guest/src/lib.rs");
  return {
    version: Number(version[1]),
    files,
    warmup: { crateName: "hello", files: crateFiles(path.join(templates, "hello")) },
  };
};

const server = http.createServer((request, response) => {
  const url = new URL(request.url ?? "/", `http://localhost:${port}`);
  const pathname = decodeURIComponent(url.pathname);

  try {
    if (pathname === "/" || pathname === "/probe.html") {
      send(response, 200, fs.readFileSync(path.join(here, "src/probe.html")), TYPES[".html"]);
      return;
    }

    const json = {
      "/guest.json": guestJson,
      "/template/hello.json": () => ({ crateName: "hello", files: crateFiles(path.join(templates, "hello")) }),
      "/template/table.json": () => ({ crateName: "newsletter", files: crateFiles(path.join(templates, "table")) }),
      "/template/legacy-hello.json": legacyHello,
    }[pathname];
    if (json) {
      send(response, 200, JSON.stringify(json()), TYPES[".json"]);
      return;
    }

    // Everything else is a static asset out of dist/. `..` cannot escape,
    // because the resolved path is checked to still be under dist/.
    const file = path.resolve(dist, `.${pathname}`);
    if (!file.startsWith(dist) || !fs.existsSync(file) || !fs.statSync(file).isFile()) {
      send(response, 404, `no such file: ${pathname}\n`, "text/plain; charset=utf-8");
      return;
    }
    const extension = /\.br\.part-\d+$/.test(file) ? ".br" : path.extname(file);
    send(response, 200, fs.readFileSync(file), TYPES[extension] ?? "application/octet-stream");
  } catch (error) {
    send(response, 500, `${error?.stack ?? error}\n`, "text/plain; charset=utf-8");
  }
});

server.listen(port, () => {
  console.log(`probe: http://localhost:${port}/  (serving ${dist})`);
});
