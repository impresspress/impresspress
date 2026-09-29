// The page half of the dev sandbox's compiler protocol.
//
// `examples/dev-sandbox/compiler/` packages Rubrc — rustc, cargo and LLVM
// built to wasm and composed into one component — as a module worker that
// speaks the message protocol in `compiler/src/protocol.ts`. This file is the
// other end of that protocol: the class `/b/dev` uses to turn "compile this
// crate" into a worker session, and a worker session back into a
// `CompileResult`.
//
// # Why this is a module and not part of `dev.js`
//
// `dev.js` is a TAIL composed into one IIFE with `webmcp-core.js`
// (`blocks/dev/assets.rs`), and everything in it is deliberately unreachable
// from outside that closure. The compiler is the one piece of the workspace
// that has a surface worth testing on its own — a protocol with a queue, a
// timeout and four failure modes — and a class sealed inside an IIFE cannot
// be driven by a test. So it ships as an ES module with a named export, and
// `dev.js` becomes a module script that imports it. The class is never hung
// off the page's global object: a global would put the compiler on the same
// footing as the page's own tools, which anything running in this document —
// including the site the preview iframe renders — could then reach.
//
// It imports nothing. `webmcp-core.js`'s `buildRequest`/`toolOptions` are
// about calling HTTP tools over `fetch`; a worker session has no HTTP in it —
// which is also why the guest crate is handed in by the caller rather than
// fetched here.
//
// # The shape of a session (compiler/README.md is the long version)
//
//     new BrowserRustCompiler(manifest, { guest })   // manifest.json + GET /b/dev/api/guest
//     await c.initialize(onProgress)      // 'download' … 'initializing' … ready
//                                         // (~7 s, plus ~30 s building the guest crate once)
//     await c.compile({ crateName, files, onProgress })   // 'compiling' … result
//     await c.cancel()                    // the in-flight compile resolves cancelled
//     await c.dispose()
//
// Two properties of the worker drive most of what is below:
//
//  * **It runs one compile at a time, and refuses a second rather than
//    queueing it.** The queue therefore lives here — see `compile`.
//  * **`cancel` spends the worker.** Rubrc's shell runs a command on a
//    session thread that nothing outside it can unwind, so a cancelled worker
//    answers the compile with `{ cancelled: true }` and marks itself broken.
//    The adapter must `terminate()` it and `init` a fresh one, which costs a
//    re-instantiation (~7 s), not a re-download. That is why `cancel` and the
//    compile timeout share one code path: both end in a destroyed worker.
//    Note that a `cancel` carries the COMPILE's id, not one of its own, and a
//    cancel naming anything else is answered `error` and changes nothing —
//    which is the worker refusing to be bricked by a stray click, and the
//    reason the adapter must never invent an id for it.
//
// The 120 s budget is the adapter's policy, not the worker's: the worker's own
// ten-minute ceiling is a backstop for a wedged shell. A worker that answers
// slowly is not misbehaving, it is compiling — so the timeout does not treat
// it as a protocol violation, it cancels it.

/**
 * The largest artifact this page will carry.
 *
 * `impresspress-core`'s `blocks::dev::validation::MAX_ARTIFACT_BYTES` — the
 * limit `POST /b/dev/api/builds/stage` enforces on the other side. Written as
 * the byte count rather than `4 * 1024 * 1024` so a test in `dev_page.rs` can
 * assert the two halves still agree.
 */
const MAX_ARTIFACT_BYTES = 4194304;

/**
 * How long a single `compile` may run before the adapter gives up on it.
 *
 * The probe's own figure for the `hello` template is ~38 s
 * (`compiler/README.md`), and the worker has its own ten-minute ceiling per
 * shell command. This is the *page's* patience, not the compiler's: past two
 * minutes the human has no way to tell a slow build from a wedged one, and a
 * wedged worker cannot be unwedged — only replaced. So the timeout does what
 * `cancel` does.
 */
const COMPILE_TIMEOUT_MS = 120000;

/**
 * How long `cancel` waits for the worker to answer before killing it.
 *
 * The answer is the cancelled compile's own terminal message — one `result`
 * per request, cancel included — so this is a real wait, not a courtesy: it is
 * what lets the page report the compile as the WORKER saw it rather than as
 * the adapter guessed. Bounded, because a worker wedged badly enough not to
 * answer is exactly what a cancel is for. The worker's message loop is the
 * WASI farm, not the blocked session thread, so it answers immediately or not
 * at all; two seconds is generous.
 */
const CANCEL_GRACE_MS = 2000;

/**
 * How long `initialize` tolerates a worker that has stopped speaking.
 *
 * NOT a ceiling on start-up. The timer is re-armed by every `progress`
 * message, so a slow link finishes however long it takes — `vfs-runner.ts`
 * posts `download` progress per chunk, and the farm posts one for each step
 * after that. What it catches is the case nothing else can: a worker that
 * neither answers nor errors — a fetch stalled with no failure, a subordinate
 * worker that died silently — which would otherwise leave `#ready` pending for
 * the life of the page, with every queued `compile` behind it and `dispose()`
 * the only way out.
 *
 * Six minutes because it must never pre-empt the worker's OWN guards. The
 * worker gives each start-up step a `SYSROOT_TIMEOUT_MS` of 300 s
 * (`worker-entry.ts`) and reports a step that overruns as an `error` — with a
 * message naming the step, which is strictly better than anything this side
 * can say. A window under that would race those guards and could kill a
 * healthy init on a slow machine, trading a hang for a wrong answer. This is
 * the backstop for the worker's backstops, and nothing finer.
 *
 * `new BrowserRustCompiler(manifest, { initSilenceMs })` overrides it; the
 * end-to-end test injects a short one so the behaviour can be driven in
 * seconds rather than minutes.
 */
const INIT_SILENCE_TIMEOUT_MS = 360000;

/**
 * A diagnostic, exactly as `protocol.ts` defines it.
 *
 * @typedef {{ file: string, line: number, column: number,
 *             severity: 'error' | 'warning', message: string, code?: string }} Diagnostic
 */

/**
 * What one `compile` call answers with.
 *
 * `success: false` is an ordinary answer — a crate that does not compile is
 * the normal case in a sandbox — so `compile` resolves with it rather than
 * rejecting. It rejects only when the adapter itself cannot go on: a protocol
 * violation, a worker that failed to start, a disposed compiler.
 *
 * @typedef {{
 *   buildId: string,
 *   success: boolean,
 *   cancelled: boolean,
 *   artifact: Uint8Array | null,
 *   artifactSha256: string | null,
 *   stdout: string,
 *   stderr: string,
 *   diagnostics: Diagnostic[],
 *   elapsedMs: number,
 *   compilerVersion: string | null,
 *   guestVersion: number | null
 * }} CompileResult
 */

/**
 * Why this `result` cannot be handed to the page, or `null` if it can.
 *
 * Every field `ResultMessage` declares, checked before any of it is passed
 * on. The adapter is the only thing between the worker and code that will
 * `.map()` over `diagnostics` and put `line` into a template, so a `result`
 * that is the right SHAPE is part of the contract, not just the right type
 * tag: a `diagnostics` entry with a string `line` would otherwise surface as
 * a rendering bug three layers away from the worker that caused it.
 *
 * `severity` is restricted to the two values `protocol.ts` names. Safe
 * against the real compiler: both of `worker-entry.ts`'s parsers drop every
 * other rustc level before building a `Diagnostic` (the JSON path skips
 * anything that is not `error`/`warning`, and the text fallback's regex only
 * matches those two).
 */
function malformedResult(message) {
  if (typeof message.success !== 'boolean') {
    return '`success` is not a boolean';
  }
  if (typeof message.stdout !== 'string') {
    return '`stdout` is not a string';
  }
  if (typeof message.stderr !== 'string') {
    return '`stderr` is not a string';
  }
  if (typeof message.elapsedMs !== 'number' || !Number.isFinite(message.elapsedMs)) {
    return '`elapsedMs` is not a finite number';
  }
  if (message.cancelled !== undefined && typeof message.cancelled !== 'boolean') {
    return '`cancelled` is present but not a boolean';
  }
  if (!Array.isArray(message.diagnostics)) {
    return '`diagnostics` is not an array';
  }
  for (let index = 0; index < message.diagnostics.length; index += 1) {
    const wrong = malformedDiagnostic(message.diagnostics[index]);
    if (wrong) {
      return 'diagnostics[' + index + '] ' + wrong;
    }
  }
  return null;
}

/** The same, for one entry of `diagnostics`. */
function malformedDiagnostic(diagnostic) {
  if (!diagnostic || typeof diagnostic !== 'object') {
    return 'is not an object';
  }
  if (typeof diagnostic.file !== 'string') {
    return 'has no `file` string';
  }
  if (typeof diagnostic.line !== 'number' || !Number.isFinite(diagnostic.line)) {
    return 'has no `line` number';
  }
  if (typeof diagnostic.column !== 'number' || !Number.isFinite(diagnostic.column)) {
    return 'has no `column` number';
  }
  if (diagnostic.severity !== 'error' && diagnostic.severity !== 'warning') {
    return 'has a `severity` that is neither error nor warning';
  }
  if (typeof diagnostic.message !== 'string') {
    return 'has no `message` string';
  }
  if (diagnostic.code !== undefined && typeof diagnostic.code !== 'string') {
    return 'has a `code` that is not a string';
  }
  return null;
}

/** Whether `files` is a crate's files as the worker writes them: path → text. */
function isFileMap(files) {
  return (
    !!files &&
    typeof files === 'object' &&
    !Array.isArray(files) &&
    Object.values(files).every((contents) => typeof contents === 'string')
  );
}

/** Lowercase hex SHA-256 of a buffer, the form `builds/stage` records. */
async function sha256Hex(buffer) {
  const digest = await crypto.subtle.digest('SHA-256', buffer);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

/**
 * The page's compiler session.
 *
 * One instance owns at most one worker at a time. The worker is created
 * lazily by `initialize` and destroyed by `cancel`, `dispose`, a compile
 * timeout, or any protocol violation — and every one of those leaves the
 * instance usable: the next `initialize` (or the `initialize` a queued
 * `compile` does for itself) builds a fresh one.
 */
export class BrowserRustCompiler {
  /** The manifest this session was constructed from. */
  #manifest;

  /** The live worker, or `null` when there is none. */
  #worker = null;

  /**
   * The in-flight-or-settled `initialize` for `#worker`, or `null`.
   *
   * Doubles as the idempotence latch: a second `initialize` returns this
   * promise rather than starting a second toolchain, and a rejection clears
   * it so the call after that starts over.
   */
  #ready = null;

  /**
   * Request id → the pending entry that will settle it.
   *
   * `{ resolve, reject, expects, onProgress, timer, silenceTimer, startedAt }`.
   * `expects` is the message type that may settle it — `'ready'` for an
   * `init`, `'result'` for a `compile` — so a worker answering the right id
   * with the wrong KIND is caught rather than silently resolving the caller
   * with a message of a shape they never asked for.
   */
  #pending = new Map();

  /** Tail of the promise chain that serialises `compile` calls. */
  #queue = Promise.resolve();

  /** The compile the worker is running, or `null`. */
  #inFlight = null;

  /** Monotonic source of request ids. */
  #seq = 0;

  /** What `rustc --version` printed inside the VFS, once `ready` said so. */
  #rustcVersion = null;

  #disposed = false;

  /** The start-up silence window, overridable for tests. */
  #initSilenceMs;

  /**
   * The guest SDK crate every session is started with, as
   * `GET /b/dev/api/guest` answered — or `null` for a session that builds
   * self-contained crates only.
   */
  #guest;

  /**
   * @param {{ version: string, entry: string, target: string }} manifest
   *   `/__impresspress_dev/compiler/manifest.json`, parsed.
   * @param {{ initSilenceMs?: number, guest?: {
   *   version: number,
   *   files: Record<string, string>,
   *   warmup: { crate_name: string, files: Record<string, string> }
   * } }} [options]
   *   `initSilenceMs` overrides [`INIT_SILENCE_TIMEOUT_MS`]. It exists so the
   *   end-to-end test can drive the start-up watchdog in seconds instead of
   *   minutes; the page passes nothing and gets the documented default.
   *
   *   `guest` is `GET /b/dev/api/guest`'s answer, unchanged. Every worker
   *   this instance starts is handed it at `init` and builds it once before
   *   it reports `ready`, so the blocks that depend on it find it already
   *   compiled; its `version` is what every `CompileResult` reports, because
   *   it is the guest those builds were linked against. Without it the
   *   worker builds only crates that need nothing but the sysroot.
   */
  constructor(manifest, options = {}) {
    if (!manifest || typeof manifest !== 'object') {
      throw new TypeError('BrowserRustCompiler: the compiler manifest is required');
    }
    // `entry` becomes the URL of a script this page runs. It arrives over the
    // network, so it is checked here rather than trusted: a root-relative
    // path and nothing else. `//host/x.js` is a protocol-relative URL — it
    // starts with `/` and is cross-origin — which is exactly the shape a
    // tampered manifest would take, so it is excluded by name.
    if (
      typeof manifest.entry !== 'string' ||
      !manifest.entry.startsWith('/') ||
      manifest.entry.startsWith('//')
    ) {
      throw new TypeError(
        'BrowserRustCompiler: manifest.entry must be a same-origin absolute path, got ' +
          JSON.stringify(manifest.entry)
      );
    }
    if (typeof manifest.version !== 'string' || manifest.version === '') {
      throw new TypeError('BrowserRustCompiler: manifest.version must be a non-empty string');
    }
    // The compile message cannot be formed without it, and the sandbox builds
    // for exactly one triple — the one the vendored sysroot is for.
    if (typeof manifest.target !== 'string' || manifest.target === '') {
      throw new TypeError('BrowserRustCompiler: manifest.target must be a non-empty string');
    }
    if (
      options.initSilenceMs !== undefined &&
      (typeof options.initSilenceMs !== 'number' || !(options.initSilenceMs > 0))
    ) {
      throw new TypeError('BrowserRustCompiler: initSilenceMs must be a positive number');
    }
    // Checked here rather than left to the worker: a malformed guest would
    // otherwise surface as a failed warm-up build half a minute into
    // `initialize`, with a cargo error about a crate the page never wrote.
    const guest = options.guest;
    if (guest !== undefined) {
      if (!guest || typeof guest !== 'object') {
        throw new TypeError(
          'BrowserRustCompiler: guest must be the object GET /b/dev/api/guest answers'
        );
      }
      if (!Number.isInteger(guest.version) || guest.version < 0) {
        throw new TypeError('BrowserRustCompiler: guest.version must be a non-negative integer');
      }
      if (!isFileMap(guest.files)) {
        throw new TypeError(
          'BrowserRustCompiler: guest.files must be an object of path → contents'
        );
      }
      if (!guest.warmup || typeof guest.warmup !== 'object') {
        throw new TypeError('BrowserRustCompiler: guest.warmup must be an object');
      }
      if (typeof guest.warmup.crate_name !== 'string' || guest.warmup.crate_name === '') {
        throw new TypeError(
          'BrowserRustCompiler: guest.warmup.crate_name must be a non-empty string'
        );
      }
      if (!isFileMap(guest.warmup.files)) {
        throw new TypeError(
          'BrowserRustCompiler: guest.warmup.files must be an object of path → contents'
        );
      }
    }
    this.#manifest = manifest;
    this.#initSilenceMs = options.initSilenceMs ?? INIT_SILENCE_TIMEOUT_MS;
    this.#guest = guest ?? null;
  }

  /** The version of the guest every worker this instance starts is built with. */
  #guestVersion() {
    return this.#guest ? this.#guest.version : null;
  }

  /** The pinned compiler bundle's version — the rubrc sha the URLs carry. */
  get version() {
    return this.#manifest.version;
  }

  /**
   * Start the toolchain, or join the start already under way.
   *
   * Resolves with the `rustc --version` string the worker reported. Idempotent:
   * calling it again after it resolved is free, and only the FIRST caller's
   * `onProgress` sees the download — a later caller has nothing left to
   * watch. A rejection is not sticky: the failed worker is destroyed and the
   * next call builds another.
   *
   * @param {(progress: { stage: string, loaded?: number, total?: number, detail?: string }) => void} [onProgress]
   * @returns {Promise<string>}
   */
  async initialize(onProgress) {
    if (this.#disposed) {
      throw new Error('the compiler has been disposed');
    }
    if (this.#ready) {
      return this.#ready;
    }
    const ready = this.#start(onProgress);
    this.#ready = ready;
    // Clear the latch on failure so the next call starts a fresh worker.
    // Guarded on identity because a `dispose` or a violation in between has
    // already replaced (or nulled) it, and this handler must not resurrect
    // a state that moved on without it.
    ready.catch(() => {
      if (this.#ready === ready) {
        this.#ready = null;
      }
    });
    return ready;
  }

  /**
   * Compile one crate.
   *
   * Resolves with a `CompileResult` — including for a crate that does not
   * compile, which is an answer and not an error. Rejects only on an adapter
   * failure: a worker that would not start, a protocol violation, a disposed
   * compiler.
   *
   * Calls are serialised. The worker refuses a second `compile` while one is
   * in flight (`protocol.ts`: "a queue would only hide a bug in the page"),
   * so the queue is here, where the page can see it: two calls run in the
   * order they were made, and the second one's `initialize` picks up whatever
   * worker the first one left behind — a fresh one, if the first was
   * cancelled.
   *
   * @param {{ crateName: string, files: Record<string, string>,
   *           onProgress?: (progress: { stage: string, detail?: string }) => void }} request
   * @returns {Promise<CompileResult>}
   */
  compile(request) {
    // Chained on both settlements: one compile's failure must not strand
    // every compile queued behind it.
    const run = this.#queue.then(
      () => this.#runCompile(request),
      () => this.#runCompile(request)
    );
    this.#queue = run.then(
      () => {},
      () => {}
    );
    return run;
  }

  /**
   * Abandon the compile in flight, if there is one.
   *
   * The in-flight `compile` resolves with `{ success: false, cancelled: true }`
   * — it does not reject; the page asked for this. The worker is spent
   * afterwards whatever it answers, so it is destroyed and the next `compile`
   * pays for a fresh `init`.
   *
   * A no-op when nothing is compiling.
   */
  async cancel() {
    await this.#abandonInFlight('cancelled by the workspace');
  }

  /**
   * Terminate the worker and refuse further use of this instance.
   *
   * Anything still in flight rejects: a disposed compiler has no answer to
   * give, and a pending promise that never settles would hang the caller.
   */
  async dispose() {
    this.#disposed = true;
    this.#destroy(new Error('the compiler was disposed'));
  }

  // ---- the worker ---------------------------------------------------------

  /** Create the worker and drive it to `ready`. */
  async #start(onProgress) {
    const worker = new Worker(this.#manifest.entry, { type: 'module' });
    // Bound to `worker`, not to `this.#worker`, so a listener that survives
    // a teardown (the event was already queued when `terminate()` ran) can
    // tell that it belongs to a worker this instance no longer owns.
    worker.addEventListener('message', (event) => this.#onMessage(worker, event));
    // A module worker whose script 404s, or whose top level throws, reports
    // it here and nowhere else — without this the `init` request would simply
    // never be answered and `initialize` would hang for ever.
    worker.addEventListener('error', (event) => {
      event.preventDefault();
      this.#failWorker(worker, 'the compiler worker failed to start: ' + describeErrorEvent(event));
    });
    // A message the page cannot structured-clone back. Nothing in the
    // protocol should ever produce one, which is why it is a failure and not
    // a warning.
    worker.addEventListener('messageerror', () => {
      this.#failWorker(worker, 'the compiler worker sent a message this page could not read');
    });
    this.#worker = worker;

    const id = this.#nextId('init');
    // No ceiling on how long start-up may take — the variable part is a 75 MB
    // download, and refusing slow connections would be the wrong failure. What
    // there is instead is a SILENCE watchdog: the timer below is re-armed by
    // every `progress` message, so a worker that is still talking has as long
    // as it needs, and one that has stopped talking altogether is eventually
    // let go of. See `INIT_SILENCE_TIMEOUT_MS`.
    const init = { type: 'init', id };
    if (this.#guest) {
      // The protocol's spelling (`crateName`) for the API's (`crate_name`);
      // this is the one place the two meet.
      init.guest = {
        files: this.#guest.files,
        warmup: { crateName: this.#guest.warmup.crate_name, files: this.#guest.warmup.files }
      };
    }
    const ready = await this.#request(
      id,
      init,
      {
        expects: 'ready',
        onProgress,
        silenceMs: this.#initSilenceMs,
        onSilence: () =>
          this.#failWorker(
            worker,
            'the compiler worker stopped reporting progress for ' +
              this.#initSilenceMs +
              ' ms while starting up'
          )
      }
    );
    this.#rustcVersion = typeof ready.rustcVersion === 'string' ? ready.rustcVersion : null;
    return this.#rustcVersion;
  }

  /** One compile, once the queue has let it through. */
  async #runCompile({ crateName, files, onProgress }) {
    if (this.#disposed) {
      throw new Error('the compiler has been disposed');
    }
    if (typeof crateName !== 'string' || crateName === '') {
      throw new TypeError('compile: crateName must be a non-empty string');
    }
    if (!files || typeof files !== 'object') {
      throw new TypeError('compile: files must be an object of path → contents');
    }
    // The same callback receives the toolchain's start-up when this compile
    // is the one that has to pay for it — after a cancel, or on the very
    // first build if the page never called `initialize` itself. The stages
    // are the protocol's own, so the caller cannot tell (and need not care)
    // which half of the work it is watching.
    await this.initialize(onProgress);

    const id = this.#nextId('build');
    this.#inFlight = id;
    try {
      return await this.#request(
        id,
        {
          type: 'compile',
          id,
          crateName,
          files,
          target: this.#manifest.target,
          // Always release. The sandbox's artifact ceiling is 4 MiB and a
          // debug build of the same crate is several times the size of the
          // release one, so a debug switch would only offer the page a way
          // to produce artifacts the server refuses.
          release: true
        },
        {
          expects: 'result',
          onProgress,
          timeoutMs: COMPILE_TIMEOUT_MS,
          onTimeout: () =>
            this.#abandonInFlight('the compile did not finish within ' + COMPILE_TIMEOUT_MS + ' ms')
        }
      );
    } finally {
      if (this.#inFlight === id) {
        this.#inFlight = null;
      }
    }
  }

  /**
   * Send one request and wait for the message that settles it.
   *
   * `timeoutMs` is a deadline for the whole request; `silenceMs` is a gap
   * between messages, re-armed by every `progress`. A request may have either,
   * and they mean different things: a compile that takes too long is over
   * budget, a start-up that goes quiet has stopped happening.
   *
   * @param {string} id
   * @param {object} message
   * @param {{ expects: 'ready' | 'result', onProgress?: Function,
   *           timeoutMs?: number, onTimeout?: Function,
   *           silenceMs?: number, onSilence?: Function }} options
   */
  #request(id, message, options = {}) {
    const worker = this.#worker;
    if (!worker) {
      return Promise.reject(new Error('the compiler worker is gone'));
    }
    let entry;
    const answered = new Promise((resolve, reject) => {
      entry = {
        resolve,
        reject,
        expects: options.expects,
        onProgress: options.onProgress,
        startedAt: Date.now(),
        timer: options.timeoutMs
          ? setTimeout(() => {
              options.onTimeout();
            }, options.timeoutMs)
          : null,
        silenceMs: options.silenceMs,
        onSilence: options.onSilence,
        silenceTimer: null
      };
      this.#pending.set(id, entry);
      this.#armSilence(entry);
    });
    // A settlement signal `#abandonInFlight` can wait on without competing
    // for the answer itself — whoever asked for the compile still gets it.
    entry.finished = answered.then(
      () => {},
      () => {}
    );
    worker.postMessage(message);
    return answered;
  }

  /**
   * (Re)start a request's silence watchdog.
   *
   * Called when the request is posted and again on every `progress` for it,
   * which is what makes the window a gap between messages rather than a
   * deadline for the whole request.
   */
  #armSilence(entry) {
    if (!entry.silenceMs) {
      return;
    }
    if (entry.silenceTimer !== null) {
      clearTimeout(entry.silenceTimer);
    }
    entry.silenceTimer = setTimeout(() => {
      entry.onSilence();
    }, entry.silenceMs);
  }

  /** Pop a pending request, clearing both of its timers. */
  #take(id) {
    const entry = this.#pending.get(id);
    if (!entry) {
      return null;
    }
    clearEntryTimers(entry);
    this.#pending.delete(id);
    return entry;
  }

  #nextId(kind) {
    this.#seq += 1;
    return kind + '-' + this.#seq;
  }

  // ---- the protocol -------------------------------------------------------

  #onMessage(worker, event) {
    // A message from a worker this instance has already replaced. It cannot
    // settle anything — everything that worker owed was rejected when it was
    // destroyed — so it is dropped rather than mistaken for the live one.
    if (worker !== this.#worker) {
      return;
    }
    const message = event.data;
    if (!message || typeof message.type !== 'string') {
      this.#violation('the compiler worker sent a message with no type');
      return;
    }

    switch (message.type) {
      case 'progress': {
        const entry = this.#pending.get(message.id);
        // Progress is advisory: it settles nothing, and a late one for a
        // build the page abandoned (or for a request that has just been
        // answered) is a race, not a defect. Only messages that claim to
        // ANSWER something the page never asked are treated as violations.
        if (!entry) {
          return;
        }
        // The worker is still working, so the silence watchdog starts over.
        // Before the callback, not after: a page callback that throws must
        // not leave a request that IS progressing armed against a stale
        // deadline.
        this.#armSilence(entry);
        if (entry.onProgress) {
          // Called last in the branch on purpose: a callback the page
          // supplied is the page's to get right, and if it throws, the
          // exception belongs in the console — not swallowed here, and with
          // nothing left in this handler for it to skip.
          entry.onProgress({
            stage: message.stage,
            loaded: message.loaded,
            total: message.total,
            detail: message.detail
          });
        }
        return;
      }

      case 'ready': {
        const entry = this.#take(message.id);
        if (!entry) {
          this.#unexpectedId('ready', message.id);
          return;
        }
        if (this.#wrongKind(entry, 'ready', message.id)) {
          return;
        }
        entry.resolve(message);
        return;
      }

      case 'result': {
        const entry = this.#take(message.id);
        if (!entry) {
          this.#unexpectedId('result', message.id);
          return;
        }
        if (this.#wrongKind(entry, 'result', message.id)) {
          return;
        }
        // Validated here, synchronously, and not in the async settle below:
        // a violation has to destroy the worker and reject everything it
        // owed, and by the time an `await` had resumed, the queue could
        // already have posted the next compile into a worker that is about
        // to be killed.
        const malformed = malformedResult(message);
        if (malformed) {
          const why = 'the compiler worker sent a result whose ' + malformed;
          entry.reject(new Error(why));
          this.#violation(why);
          return;
        }
        const artifact = message.artifact;
        if (artifact !== undefined) {
          if (!(artifact instanceof ArrayBuffer)) {
            entry.reject(new Error('the compiler worker sent an artifact that is not an ArrayBuffer'));
            this.#violation('the compiler worker sent an artifact that is not an ArrayBuffer');
            return;
          }
          if (artifact.byteLength > MAX_ARTIFACT_BYTES) {
            const tooBig =
              'the compiler produced ' +
              artifact.byteLength +
              ' bytes, over the sandbox limit of ' +
              MAX_ARTIFACT_BYTES;
            entry.reject(new Error(tooBig));
            this.#violation(tooBig);
            return;
          }
        }
        // The digest is the only asynchronous part, and it cannot fail in a
        // way the protocol cares about.
        this.#settleResult(entry, message).catch((error) => entry.reject(error));
        return;
      }

      case 'error': {
        // `protocol.ts` gives `error` three causes, and only two of them mean
        // "terminate me": an `init` that failed, and a `compile` on a worker
        // that is `broken` or not yet `ready`. The third — a `cancel` naming
        // nothing in flight — leaves the worker perfectly usable.
        //
        // Every one of them is terminal HERE, and not because the protocol
        // says so. The recoverable case can only reach this adapter one way:
        // `#abandonInFlight` posts a `cancel` while it still holds the build's
        // pending entry, so the only worker that can answer "nothing in
        // flight" is one whose `result` was already sitting in this page's
        // message queue — i.e. a cancel that lost the race with the answer it
        // meant to stop. In that window the adapter has already decided to
        // replace the worker, the compile has already been settled with its
        // real result, and destroying is what was going to happen anyway. So
        // treating `error` as terminal costs at most a re-`init` nobody was
        // going to skip, and never takes an answer away from a caller.
        const entry = this.#take(message.id);
        const error = new Error('the compiler worker failed: ' + message.message);
        if (entry) {
          entry.reject(error);
        }
        this.#destroy(error);
        return;
      }

      default:
        this.#violation('the compiler worker sent an unknown message type ' + message.type);
    }
  }

  /** Turn a `result` into the page's `CompileResult`. */
  async #settleResult(entry, message) {
    let artifact = null;
    let artifactSha256 = null;
    if (message.artifact !== undefined) {
      // The buffer was TRANSFERRED, not copied: it is the page's now, and
      // the view below is over the same memory rather than a second copy of
      // a multi-megabyte module.
      artifact = new Uint8Array(message.artifact);
      artifactSha256 = await sha256Hex(message.artifact);
    }
    entry.resolve({
      buildId: message.id,
      success: message.success === true,
      cancelled: message.cancelled === true,
      artifact,
      artifactSha256,
      stdout: message.stdout,
      stderr: message.stderr,
      diagnostics: message.diagnostics,
      elapsedMs: message.elapsedMs,
      compilerVersion: this.#rustcVersion,
      guestVersion: this.#guestVersion()
    });
  }

  /**
   * The worker answered a request of ours with a message of the wrong kind.
   *
   * An `init` is settled by `ready` and a `compile` by `result`; swapping them
   * would resolve the caller with an object that has none of the fields they
   * are about to read — a `CompileResult` with no `diagnostics`, or an
   * `initialize` that resolved to `undefined` and left the page believing a
   * toolchain came up. Cheap to detect, and silent for ever if it is not.
   *
   * @returns {boolean} true when the entry has been rejected and the worker
   *   destroyed, so the caller must stop.
   */
  #wrongKind(entry, kind, id) {
    if (entry.expects === kind) {
      return false;
    }
    const why =
      'the compiler worker answered ' +
      kind +
      ' for ' +
      id +
      ', which was a request for ' +
      entry.expects;
    entry.reject(new Error('compiler protocol violation: ' + why));
    this.#violation(why);
    return true;
  }

  #unexpectedId(kind, id) {
    this.#violation(
      'the compiler worker answered ' + kind + ' for a request this page did not make: ' + id
    );
  }

  // ---- teardown -----------------------------------------------------------

  /**
   * Give up on the compile in flight and replace the worker.
   *
   * The single path behind `cancel()` and the compile timeout, because the
   * protocol gives them the same ending: the worker cannot unwind a running
   * compile, so the only way to stop one is to stop the worker.
   */
  async #abandonInFlight(reason) {
    const buildId = this.#inFlight;
    const build = buildId === null ? null : this.#pending.get(buildId);
    if (!build) {
      return;
    }
    this.#inFlight = null;
    // Whatever happens from here this build is over, so its clocks must not
    // fire again on the way out — a timeout that cancelled a compile would
    // otherwise be free to cancel it a second time.
    clearEntryTimers(build);

    const worker = this.#worker;
    if (worker) {
      // The COMPILE's id, not one of ours: a `cancel` naming anything else is
      // answered `error` and changes nothing, which would leave the build
      // running under a worker about to be killed and the page holding a
      // promise nobody will settle.
      worker.postMessage({ type: 'cancel', id: buildId });
      await Promise.race([
        build.finished,
        new Promise((resolve) => setTimeout(resolve, CANCEL_GRACE_MS))
      ]);
    }

    // Still unanswered after the grace — or never asked, because there was no
    // worker left to ask. The adapter says what the worker would not.
    const stranded = this.#take(buildId);
    if (stranded) {
      stranded.resolve({
        buildId,
        success: false,
        cancelled: true,
        artifact: null,
        artifactSha256: null,
        stdout: '',
        stderr: reason,
        diagnostics: [],
        elapsedMs: Date.now() - stranded.startedAt,
        compilerVersion: this.#rustcVersion,
        guestVersion: this.#guestVersion()
      });
    }
    this.#destroy(new Error('the compiler worker was replaced: ' + reason));
  }

  /**
   * The worker broke its own protocol.
   *
   * Nothing it says afterwards can be trusted — not this answer, and not the
   * artifact bytes it might send next — so it is destroyed and everything it
   * owed is rejected. The instance stays usable: the next call starts a new
   * worker, which is the only recovery there is.
   */
  #violation(what) {
    this.#destroy(new Error('compiler protocol violation: ' + what));
  }

  /** A worker that failed as a worker, rather than as a protocol speaker. */
  #failWorker(worker, message) {
    if (worker !== this.#worker) {
      return;
    }
    this.#destroy(new Error(message));
  }

  /** Terminate the worker and reject everything it still owed. */
  #destroy(error) {
    const worker = this.#worker;
    this.#worker = null;
    this.#ready = null;
    this.#inFlight = null;
    const owed = Array.from(this.#pending.values());
    this.#pending.clear();
    if (worker) {
      worker.terminate();
    }
    for (const entry of owed) {
      clearEntryTimers(entry);
      entry.reject(error);
    }
  }
}

/** Stop both of a pending entry's clocks. */
function clearEntryTimers(entry) {
  if (entry.timer !== null) {
    clearTimeout(entry.timer);
    entry.timer = null;
  }
  if (entry.silenceTimer !== null) {
    clearTimeout(entry.silenceTimer);
    entry.silenceTimer = null;
  }
}

/** Whatever a worker `ErrorEvent` will tell us about why it died. */
function describeErrorEvent(event) {
  if (event.message) {
    return event.message;
  }
  if (event.error) {
    return String(event.error);
  }
  return 'no reason given';
}
