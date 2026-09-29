// Shared harness for `dev.js` unit tests (`node --test
// crates/impresspress-core/src/blocks/dev/assets/test/*.test.mjs`).
//
// `dev.js` is authored as a TAIL with nothing on `window` by design (see the
// file's own header comment) — there is no export to import here the way
// `impresspress-browser/js/test` imports `bridge.js`'s real functions. So
// this harness does the same thing `blocks/dev/assets.rs::dev_js()` does —
// read the tail's source and wrap it in an IIFE — except this wrapper is
// test-only and closes over a returned handle instead of discarding the
// closure, which is how the tail's internal state and functions are reached
// without adding a single test hook to the shipped file.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const tail = fs.readFileSync(path.join(here, '..', 'dev.js'), 'utf8');

// Builds one fresh, isolated instance of the tail's closure. Each instance
// gets its own stub `document`/`window`/`fetch` so tests can't leak state
// (in particular `outstanding`/`polling`) into one another.
/**
 * @param {object} [options]
 * @param {boolean} [options.hasModelContext]  give the stub document a WebMCP
 *   registrar, as a browser that supports it would.
 * @param {object|null} [options.compilerManifest]  what
 *   `/__impresspress_dev/compiler/manifest.json` answers with: an object for a
 *   bundle that shipped the browser toolchain, `null` for one that did not
 *   (the host 404s, which is a normal build — see `discoverCompiler`).
 * @param {Array<{path: string, sha256: string, content: string,
 *                encoding?: string, size?: number, content_type?: string}>}
 *   [options.workspace]  the files `/b/dev/api/files` lists and
 *   `/b/dev/api/files/read` answers with. `encoding` defaults to `utf8`;
 *   `base64` is how the real endpoint reports a file that is not text, which
 *   is the case `snapshotBlock` has to refuse.
 * @param {Function} [options.compiler]  what the tail's
 *   `new BrowserRustCompiler(manifest)` builds. In the shipped page this
 *   binding comes from the module import `assets.rs` emits ahead of the IIFE
 *   (`DEV_JS_IMPORTS`); the harness reads the TAIL, which has no import, so it
 *   is supplied here instead.
 * @param {(request: object) => object} [options.stage]  what
 *   `POST /b/dev/api/builds/stage` answers with, given the decoded request
 *   body — a `StageBuildResponse` (`contracts.rs`).
 * @param {object|null} [options.exportManifest]  what
 *   `/b/dev/api/export/manifest` answers with — an `ExportManifest`
 *   (`contracts.rs`), or `null` for a 400 (nothing published yet).
 * @param {{status?: number, body?: string, bytes?: number}} [options.exportZip]
 *   what `/b/dev/api/export` answers with. The body is opaque to the page —
 *   it only ever calls `.blob()` on it — so a short string stands in for the
 *   archive.
 * @param {Promise<void>|null} [options.exportGate]  when set, the archive
 *   request parks on this promise, so a test can observe the page WHILE an
 *   export is in flight.
 * @param {object|(() => object)} [options.status]  what `/b/dev/api/status`
 *   answers with — a `StatusResponse`. A function, for a test whose subject
 *   is what the page does when the answer CHANGES: the page polls it, and
 *   every mutating call ends with one more read of it.
 * @param {Promise<void>|null} [options.statusGate]  when set, every status
 *   request parks on this promise, so a test can fire more poll ticks WHILE
 *   one is still outstanding — the only way to see the poll's in-flight
 *   guard, since nothing else in this harness yields.
 */
/** A Node timer that does not hold the process open. */
const unref = (timer) => {
  if (timer && typeof timer.unref === 'function') {
    timer.unref();
  }
  return timer;
};

export function instantiate({
  hasModelContext = false,
  compilerManifest = null,
  workspace = [],
  compiler = class {
    constructor() {
      throw new Error('this harness instance was not given a compiler');
    }
  },
  // A refusal rather than a throw or a success: an instance that was never
  // given a staging endpoint must not be able to look like one that staged
  // something, and the code says which instance it was.
  stage = () => ({
    build_id: null,
    success: false,
    diagnostics: [
      {
        severity: 'error',
        code: 'harness',
        message: 'this harness instance was not given a staging endpoint',
        file: null,
        line: null,
        column: null
      }
    ],
    generation: null,
    progress: []
  }),
  // An idle sandbox with nothing live, which is what every test that does not
  // care about the status wants: no `activation`, no `active_generation`.
  status = {},
  statusGate = null,
  exportManifest = null,
  exportZip = { status: 200, body: 'PK\u0003\u0004zip' },
  exportGate = null
} = {}) {
  // Everything `exportSite` handed the browser: one entry per download it
  // started, with the anchor's `download` name and the object URL it built.
  // The tail deliberately reaches `window`/`document` for the download rather
  // than returning bytes (there is nothing else a page CAN do with a file),
  // so this is the only way to assert that it did.
  const downloads = [];
  const revoked = [];
  const fetchCalls = [];
  // Every `setInterval` the tail has running, by id, so `fireInterval` can
  // deliver a tick and a cleared interval stops receiving them.
  const liveIntervals = new Map();
  // Every `window.addEventListener` the tail makes, so a test can fire the
  // real handler — `pagehide`'s `event.persisted` branch is a decision the
  // handler owns, and there is no other way to reach it.
  const windowListeners = [];
  // Enough of an element for the tail to build a list out of: the tail's only
  // DOM verbs are `innerHTML = ''` to empty a container and `appendChild` to
  // refill it, so `children` plus an `innerHTML` setter that clears it is a
  // faithful model of both — and it is what lets a test read the options the
  // Compile select was actually given, rather than trusting that a function
  // that appended into a black hole did the right thing.
  const fakeElement = () => {
    const element = {
      value: '',
      textContent: '',
      className: '',
      disabled: false,
      title: '',
      scrollTop: 0,
      scrollHeight: 0,
      children: [],
      attributes: {},
      setAttribute(name, value) {
        this.attributes[name] = value;
      },
      getAttribute(name) {
        return name in this.attributes ? this.attributes[name] : null;
      },
      appendChild(child) {
        this.children.push(child);
        return child;
      },
      addEventListener() {}
    };
    Object.defineProperty(element, 'innerHTML', {
      enumerable: true,
      configurable: true,
      get: () => '',
      set: (value) => {
        if (value === '') {
          element.children.length = 0;
        }
      }
    });
    return element;
  };

  // The attributes `blocks/dev/page.rs` actually ships on an element, for the
  // ids where the INITIAL state is load-bearing rather than incidental.
  // `#dev-compile` is `disabled` in the markup and only the compiler manifest
  // may clear it, so a stub that started it enabled would let a
  // `discoverCompiler` that did nothing at all pass.
  const MARKUP = {
    'dev-compile': { disabled: true },
    'dev-export': { disabled: true }
  };

  // Memoised by id, unlike `createElement`: the tail looks an element up once
  // and keeps it, so a stub that handed out a fresh object per call would let
  // a test read an element the code under test never touched. The map is
  // returned so a test can assert on what the tail did to the document.
  const elements = new Map();
  const elementById = (id) => {
    if (!elements.has(id)) {
      elements.set(id, Object.assign(fakeElement(), MARKUP[id]));
    }
    return elements.get(id);
  };

  // Everything the stub `document.modelContext` was handed, by name. The
  // shipped file registers `dev_compile_block` and `dev_export` itself
  // (`registerPageLocal`), and their `execute` is the only way to reach the
  // tool's own error handling — the split between a compiler failure, which
  // is a RESULT, and a machinery failure, which is an `isError`.
  const tools = new Map();

  /** One JSON body, as `api.get`/`api.post` read it (`response.text()`). */
  const answer = (body, status = 200) =>
    Promise.resolve({ ok: status < 400, status, text: async () => JSON.stringify(body) });

  const entry = (file) => ({
    path: file.path,
    sha256: file.sha256,
    size: file.size ?? file.content.length,
    content_type: file.content_type ?? 'text/plain; charset=utf-8'
  });

  const sandbox = {
    document: {
      getElementById: elementById,
      // An anchor the tail can set `href`/`download` on, append, click and
      // remove. `click()` records the download instead of starting one.
      createElement(tag) {
        const element = fakeElement();
        if (tag === 'a') {
          element.click = () => downloads.push({ href: element.href, name: element.download });
          element.remove = () => {};
        }
        return element;
      },
      body: { appendChild: (child) => child },
      addEventListener() {},
      ...(hasModelContext
        ? {
            modelContext: {
              registerTool(options) {
                tools.set(options.name, options);
              },
              unregisterTool(name) {
                tools.delete(name);
              }
            }
          }
        : {})
    },
    window: {
      addEventListener(type, listener) {
        windowListeners.push({ type, listener });
      }
    },
    fetch(...args) {
      fetchCalls.push(args);
      // The tail makes two kinds of request on load and they cannot share one
      // canned answer, so the stub routes by URL — the same split the real
      // page has: `api.*` calls go to the block's JSON API and read
      // `response.text()`, while the compiler manifest is a STATIC file read
      // with `response.json()` whose absence (404) is a normal build.
      const url = String(args[0]);
      if (url === '/__impresspress_dev/compiler/manifest.json') {
        if (compilerManifest === null) {
          return Promise.resolve({ ok: false, status: 404 });
        }
        return Promise.resolve({
          ok: true,
          status: 200,
          json: async () => compilerManifest
        });
      }
      // The two file endpoints, answered from `workspace` — enough of the
      // real `files.rs` for `loadFiles` and `snapshotBlock` to run against:
      // the list filters on `?prefix=`, and the read is by exact path and
      // reports the file's own encoding.
      if (url === '/b/dev/api/files' || url.startsWith('/b/dev/api/files?')) {
        // `URLSearchParams` percent-decodes, which is what the runtime does
        // too (`wafer-block`'s `http_codec` runs the raw query through
        // `form_urlencoded::parse` before a block ever sees it) — so the
        // `blocks%2Fhello%2F` the tail sends arrives here as `blocks/hello/`,
        // exactly as it does on the server.
        const prefix = url.includes('?')
          ? (new URLSearchParams(url.slice(url.indexOf('?') + 1)).get('prefix') ?? '')
          : '';
        return answer({ files: workspace.filter((f) => f.path.startsWith(prefix)).map(entry) });
      }
      if (url === '/b/dev/api/files/read') {
        const { path: wanted } = JSON.parse(args[1].body);
        const file = workspace.find((f) => f.path === wanted);
        if (!file) {
          return answer({ error: 'not_found', message: `no file at ${wanted}` }, 404);
        }
        // `FileReadResponse`'s own five fields — no `content_type`, which
        // only the listing carries.
        return answer({
          path: file.path,
          sha256: file.sha256,
          size: file.size ?? file.content.length,
          encoding: file.encoding ?? 'utf8',
          content: file.content
        });
      }
      if (url === '/b/dev/api/builds/stage') {
        return answer(stage(JSON.parse(args[1].body)));
      }
      if (url === '/b/dev/api/status') {
        const body = () => (typeof status === 'function' ? status() : status);
        // Read at RESOLUTION, not at call time, for the same reason
        // `exportGate` exists: a parked request that had already snapshotted
        // its answer could not show a test what changed while it was parked.
        return statusGate ? statusGate.then(() => answer(body())) : answer(body());
      }
      if (url === '/b/dev/api/export/manifest') {
        // `null` is the 400 the endpoint answers on an instance that has
        // published nothing — the one refusal `exportSite` can meet before it
        // has asked for a single byte of archive.
        return exportManifest === null
          ? answer({ error: 'failed_precondition', message: 'there is nothing to export yet' }, 400)
          : answer(exportManifest);
      }
      if (url === '/b/dev/api/export') {
        const { status: code = 200, body = '' } = exportZip;
        // `exportGate`, when given, holds the archive request open so a test
        // can observe what the page looks like WHILE an export is running —
        // the only way to see the in-flight guard, since nothing else in this
        // harness yields.
        if (exportGate) {
          return exportGate.then(() => ({
            ok: code < 400,
            status: code,
            text: async () => body,
            blob: async () => ({ size: body.length, type: 'application/zip' })
          }));
        }
        return Promise.resolve({
          ok: code < 400,
          status: code,
          text: async () => body,
          // The page only ever calls `.blob()` on this response, and only
          // ever hands the result to `URL.createObjectURL` — so an object
          // carrying the size is a faithful stand-in for a Blob here.
          blob: async () => ({ size: body.length, type: 'application/zip' })
        });
      }
      // `/b/dev/api/tools.json`. A body with no `tools` array is a manifest
      // with nothing to register, which is what every test that is not about
      // registration wants.
      return answer({ files: [] });
    },
    // The tail's own name for the class `assets.rs` imports into the module.
    BrowserRustCompiler: compiler,
    // The download half of `exportSite`. A counter rather than a real object
    // URL: the page's only contract with it is "what `createObjectURL`
    // returned is what `revokeObjectURL` is later given".
    URL: {
      createObjectURL: (blob) => `blob:fake/${blob.size}`,
      revokeObjectURL: (url) => revoked.push(url)
    },
    // Both timer functions are UNREF'd. Node keeps the process alive while a
    // timer is pending, and the tail schedules two long ones on purpose: the
    // ~300 ms status poll while a mutating call is in flight, and
    // `exportSite`'s 60 s object-URL revoke. A test that triggered either
    // would otherwise sit there until it fired. `unref` changes nothing about
    // when the callback runs — only whether Node waits for it.
    setTimeout: (fn, ms) => unref(globalThis.setTimeout(fn, ms)),
    clearTimeout: globalThis.clearTimeout,
    AbortController,
    console,
    Promise,
    // Registered as well as scheduled: the poll interval is 300 ms and a test
    // about what two ticks do to each other must not have to wait 600 ms for
    // them. `fireInterval` below runs the callbacks of every interval that is
    // still live, which is what a tick IS.
    setInterval: (fn, ms) => {
      const id = unref(globalThis.setInterval(fn, ms));
      liveIntervals.set(id, fn);
      return id;
    },
    clearInterval: (id) => {
      liveIntervals.delete(id);
      clearInterval(id);
    },
    Date,
    Array,
    Math,
    Error,
    JSON
  };

  // `vm` would be the conventional tool here, but the tail's top level
  // already runs long enough (`loadFiles().catch(...)`, event listener
  // registration) that a plain `Function` constructor closing over the
  // stub globals above is simpler than wiring a full `vm.Context`, and
  // nothing in the tail needs real module semantics.
  const factory = new Function(
    ...Object.keys(sandbox),
    `${tail}
return {
  withProgress,
  get outstanding() { return outstanding },
  get isPolling() { return polling !== null },
  abort,
  unregisterPageTools,
  get registered() { return registered.slice() },
  isFileConflict,
  refusalBody,
  refusalMessage,
  describeCompiler,
  snapshotBlock,
  compileBlock,
  exportSite,
  updateExportButton,
  get exportInFlight() { return exportInFlight },
  get statusInFlight() { return statusInFlight },
  get compilerManifest() { return compilerManifest }
};`
  );
  const handle = factory(...Object.values(sandbox));
  // Fires every `window` listener the tail registered for `type`, with
  // `event` as the argument — the shape a browser would deliver.
  const fireWindow = (type, event) => {
    windowListeners
      .filter((l) => l.type === type)
      .forEach((l) => l.listener(event));
  };
  // Deliver one tick to every interval the tail currently has running.
  const fireInterval = () => {
    for (const fn of [...liveIntervals.values()]) {
      fn();
    }
  };
  return { handle, fetchCalls, fireWindow, fireInterval, elements, tools, downloads, revoked };
}
