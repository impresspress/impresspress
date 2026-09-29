// The `/b/dev` workspace page: registers this page's own agent tools, drives
// the file/editor/preview/progress panes over the sandbox's JSON API, and
// keeps the live-site iframe in step with what the agent publishes.
//
// Authored as a TAIL, not a script. `blocks/dev/assets.rs` composes it with
// `ui/assets/webmcp-core.js` into a single IIFE, so `buildRequest` and
// `toolOptions` are in scope here exactly as they are in `webmcp.js`, and
// nothing declared below reaches `window`. No outer IIFE and no
// `'use strict'` — the wrapper supplies both.
//
// The composed script is an ES MODULE (`<script type="module">`), because it
// imports `BrowserRustCompiler` from `/b/dev/static/compiler-adapter.js` —
// the class that drives the in-browser Rust toolchain. The import declaration
// lives in `assets.rs` (`DEV_JS_IMPORTS`), since an `import` may only stand
// at a module's top level and everything in this file is inside the wrapper's
// function body. The binding is in scope here; the Compile button is what
// will use it. Two consequences for anything written below: a module's top
// level is already strict (the wrapper's directive is now belt and braces),
// and a module has no currentScript — the `document` property that names the
// running script tag is null in one — which is why nothing here reads it.
//
// Every tool registered here is PAGE-scoped: the list comes from
// `/b/dev/api/tools.json` (the curated `dev_*` / `shop_*` allowlist), never
// from the deployment-wide manifest `webmcp.js` owns, and it is torn down
// when this page goes away. An agent on any other page of the site sees the
// site's tools and none of these.

// ---- the API client -------------------------------------------------------

// The lifetime of everything this page registers. Aborting it is the single
// teardown path: a `pagehide` the document does not come back from fires it,
// and so does the first sign that the session behind the page is gone
// (below).
var abort = new AbortController();

// A 401 means this document is still on screen but its session is not; a 403
// that its session no longer carries the admin role the tools need.
// Tearing the tools down is the honest response — an agent left holding
// tools whose every call now fails would keep retrying against a page that
// cannot answer, and the human would see no reason why.
function check(response) {
  if (response.status === 401 || response.status === 403) {
    abort.abort();
  }
  return response;
}

var api = {
  get: function (path) {
    return fetch(path, { credentials: 'same-origin' }).then(check);
  },
  post: function (path, body) {
    return fetch(path, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body || {})
    }).then(check);
  }
};

// Parse a response the caller expects to have succeeded. Reads the body as
// text first so a failure can carry the server's own message into the log
// instead of a bare status — the sandbox's refusals (a path outside the
// workspace, a quota, a stale hash) are all things the human needs to read.
async function json(response) {
  var text = await response.text();
  if (!response.ok) {
    throw new Error('HTTP ' + response.status + ': ' + text);
  }
  return JSON.parse(text);
}

// ---- the log --------------------------------------------------------------

var logEl = document.getElementById('dev-log');
var logLines = [];
var LOG_LIMIT = 200;

function log(line) {
  // Wall-clock time-of-day: the panel's job is to let a human line up what
  // the page did with what they asked the agent for, so the absolute time
  // is more use than a relative one.
  logLines.push(new Date().toISOString().slice(11, 19) + '  ' + line);
  if (logLines.length > LOG_LIMIT) {
    logLines.splice(0, logLines.length - LOG_LIMIT);
  }
  logEl.textContent = logLines.join('\n');
  logEl.scrollTop = logEl.scrollHeight;
}

function logError(error) {
  log('error: ' + (error && error.message ? error.message : String(error)));
}

// ---- progress -------------------------------------------------------------

var steps = document.getElementById('dev-progress-steps');

// The activation ladder, in `ActivationPhase`'s own order and spelling (the
// serde form is what `/b/dev/api/status` sends). `idle` — nothing in flight
// — and `failed` — abandoned, the previous generation still live — are
// states of the whole activation rather than steps within it, so they are
// rendered on the list itself via `data-phase` and never as extra steps.
var PHASES = [
  ['validating', 'Validating'],
  ['building_runtime', 'Building runtime'],
  ['publishing', 'Publishing'],
  ['active', 'Active']
];

var lastPhase = null;
var lastDetail = null;
var lastActiveGeneration = null;

// Draw the ladder for one phase.
//
// `finished` is the difference between "the activation is HERE" and "the
// activation ENDED here": a live poll wants the phase it read marked
// `current`, while an activation that is over wants its last phase marked
// `done` like every step before it. A phase that is not a step at all
// (`idle`, `failed`) leaves every step `pending` — what happened to the
// activation as a whole is stated on the list's own `data-phase`, which is
// what the stylesheet's `[data-phase='failed']` rule reads.
function drawLadder(phase, finished) {
  var reached = -1;
  PHASES.forEach(function (entry, index) {
    if (entry[0] === phase) {
      reached = index;
    }
  });

  steps.setAttribute('data-phase', phase);
  steps.innerHTML = '';
  PHASES.forEach(function (entry, index) {
    var state = 'pending';
    if (reached >= 0 && index < reached) {
      state = 'done';
    } else if (reached >= 0 && index === reached) {
      state = finished ? 'done' : 'current';
    }
    var li = document.createElement('li');
    li.className = 'dev-step';
    li.setAttribute('data-state', state);
    li.textContent = entry[1];
    steps.appendChild(li);
  });
}

// The ladder of an activation that has already FINISHED, and the generation
// it produced — or `null` when this page has not watched one finish.
//
// The journal only describes an activation while it is in flight: the moment
// the swap is recorded the row rests at `idle` (`repo/runtime_state.rs`), so
// `/b/dev/api/status` stops mentioning the phases it passed through. A
// compile that took forty seconds would therefore leave a BLANK ladder a
// third of a second later, because `withProgress` runs a catch-up poll the
// instant the call returns. The one account of those phases that survives is
// the `progress` the staging response carries, so `renderProgress` records it
// here against the generation it published, and `renderStatus` keeps showing
// it for as long as that generation is the live one.
var completed = null;

// Draw the ladder from an activation's own after-the-fact account of itself,
// and log where the time went. `progress` is `ProgressStep[]`
// (`blocks/dev/activation.rs`) — one entry per phase, ending at `active`.
function renderProgress(generationId, progress) {
  if (!progress.length) {
    return;
  }
  completed = { generation: generationId, phase: progress[progress.length - 1].phase };
  drawLadder(completed.phase, true);
  progress.forEach(function (step) {
    log('activation: ' + step.phase + ' — ' + step.ms + ' ms' + (step.detail ? ' (' + step.detail + ')' : ''));
  });
}

function renderStatus(status) {
  var activation = status.activation;
  var phase = activation ? activation.phase : 'idle';
  var activeId = status.active_generation ? status.active_generation.id : null;
  // Nothing in flight, and the generation the last activation published is
  // still the live one: keep that activation's ladder up. Blanking it would
  // be the panel forgetting the very thing it was watching a moment ago,
  // and `data-phase="active"` is not a guess — `ActivationPhase::Active`
  // means "the generation is live", which is exactly what the status just
  // confirmed. Anything else — a phase in flight, or a newer generation this
  // page did not watch land — is drawn from the status, as before.
  if (phase === 'idle' && completed !== null && completed.generation === activeId) {
    drawLadder(completed.phase, true);
  } else {
    drawLadder(phase, false);
  }

  if (phase !== lastPhase) {
    log('activation: ' + phase + (activation ? ' (' + activation.generation_id + ')' : ''));
    lastPhase = phase;
  }
  // `detail` is free text the activation may carry for exactly this panel.
  // Nothing sets it yet, so it is logged only when there is something to
  // log rather than emitting a blank line on every poll.
  if (activation && activation.detail && activation.detail !== lastDetail) {
    log(activation.detail);
    lastDetail = activation.detail;
  }
  if (activeId !== lastActiveGeneration) {
    log('live generation: ' + (activeId || 'none'));
    lastActiveGeneration = activeId;
  }
  // One of the two facts the Export button's state is decided from — the
  // other is whether an export is already running. `updateExportButton` is
  // the only writer of either; see it for why.
  updateExportButton();
}

var polling = null;
var lastRuntimeGeneration = null;

// How many mutating calls are outstanding. A COUNT, not a flag: two of them
// can overlap — an agent firing `shop_create_product` three times without
// awaiting each, or a human saving while a tool call runs — and a flag would
// let the first one to finish stop the interval and run the catch-up while
// the others are still in flight, blanking the panel mid-activation and
// reading a workspace that is about to change again.
var outstanding = 0;

// Whether a status request is already outstanding.
//
// The interval fires on a timer and the request is not instant, so without
// this a slow answer is met by another tick, and another. That matters
// because `/b/dev/api/status` reads the workspace manifest under
// `DevShared::workspace` (`blocks/dev/gc.rs::storage_usage`): during a
// collector pass, which holds that mutex across a loop of sequential deletes,
// every tick issued would take a place in a first-in-first-out queue and the
// user's next save would land behind all of them. With the guard the pass
// costs one waiter, whatever it takes — and a poll that is skipped loses
// nothing, since each response is a complete picture rather than a delta.
var statusInFlight = false;

// ~300 ms while a mutating call is outstanding (design §7.5). There is no
// push channel: the block answers `no-store` precisely so this poll always
// sees the journal as it stands.
function startPolling() {
  if (polling !== null) {
    return;
  }
  polling = setInterval(function () {
    if (statusInFlight) {
      return;
    }
    statusInFlight = true;
    api
      .get('/b/dev/api/status')
      .then(json)
      .then(observe)
      .catch(logError)
      .then(function () {
        // A `then` after the `catch`, so the flag is cleared on a refusal as
        // well as on an answer: a guard that leaked on failure would stop the
        // panel updating for the rest of the session.
        statusInFlight = false;
      });
  }, 300);
}

function stopPolling() {
  if (polling === null) {
    return;
  }
  clearInterval(polling);
  polling = null;
}

// One status response, applied. When the runtime underneath has been rebuilt
// the deployment-wide tool set has changed with it — a compiled block brings
// its own tools, a removed one takes them away — so the site's registrations
// are refreshed. The page-scoped tools registered here are untouched by that
// refresh (`webmcp.js` only drops what `webmcp.js` itself registered).
function observe(status) {
  renderStatus(status);
  if (lastRuntimeGeneration !== null && status.runtime_generation !== lastRuntimeGeneration) {
    log('runtime rebuilt (generation ' + status.runtime_generation + ') — refreshing site tools');
    refreshSiteTools();
  }
  lastRuntimeGeneration = status.runtime_generation;
  return status;
}

// `webmcp.js` is the last script in the body (`ui/layout.rs`) while this one
// sits inside the page body, so on a `defer` load this script runs FIRST and
// `window.__impresspressWebmcp` may not exist yet. Every use is therefore
// guarded rather than assumed — and a browser without WebMCP never gets the
// object at all.
function refreshSiteTools() {
  if (window.__impresspressWebmcp) {
    return window.__impresspressWebmcp.refresh();
  }
  return Promise.resolve();
}

// Wrap a tool's `execute` so the panel is live for the duration of the call
// and the page catches up with what the call changed. The catch-up is in a
// `finally`: a refused write still moved the workspace's mtime nowhere, but
// a PARTIALLY applied one (a site write that published and then failed to
// activate) leaves the page showing a workspace that no longer exists.
function withProgress(execute) {
  return async function (args) {
    // Once the session is gone (`abort.signal.aborted`), the abort handler
    // has already zeroed `outstanding` and stopped the interval — this is a
    // straggler call reaching the wrapper after that (a browser that ignored
    // `registerTool`'s `signal`, or a call already queued on the event
    // loop). Joining the count back in would reopen the exact race this
    // guard exists to close, so run it plain instead of tracking it.
    if (abort.signal.aborted) {
      return execute(args);
    }
    outstanding += 1;
    startPolling();
    try {
      return await execute(args);
    } finally {
      // Clamp rather than trust the increment/decrement to stay paired: the
      // abort handler can reset `outstanding` to 0 out from under a call
      // that is still in flight (it was never given `abort.signal`, so it
      // always reaches this `finally`). Without the clamp that decrement
      // drives the count negative, `outstanding === 0` becomes unreachable,
      // and the next call would start a poll interval nothing could stop.
      outstanding = Math.max(0, outstanding - 1);
      // The LAST call out of the room turns the lights off and does the
      // catch-up once, rather than every call racing to redraw a workspace
      // its siblings are still changing. Skip it once aborted — the handler
      // already stopped polling, and the endpoints below would just refuse.
      if (outstanding === 0 && !abort.signal.aborted) {
        stopPolling();
        await refreshAfterChange();
      }
    }
  };
}

async function refreshAfterChange() {
  try {
    observe(await json(await api.get('/b/dev/api/status')));
    reloadPreview();
    await loadFiles();
  } catch (error) {
    // Never let the catch-up replace the tool's own result: the agent asked
    // for a write, and whether the panel could redraw afterwards is this
    // page's problem, not the answer to that call.
    logError(error);
  }
}

function reloadPreview() {
  var frame = document.getElementById('dev-preview-frame');
  try {
    // The frame keeps `allow-same-origin`, so the parent may drive its
    // location directly. Reassigning `src` would work too but pushes a
    // history entry onto the frame; `reload()` replaces what it shows.
    frame.contentWindow.location.reload();
  } catch (error) {
    frame.setAttribute('src', frame.getAttribute('src'));
  }
}

// ---- tools ----------------------------------------------------------------

// Which tools change the site, and therefore want the progress panel live
// and the panes refreshed afterwards. Everything else is a read, and a read
// must NOT reload the preview: an agent listing the catalog between two
// writes would otherwise flicker the iframe and re-fetch the file tree for
// nothing.
//
// The five `dev_` names are the mutating half of the control plane — its
// reads (`dev_status`, `dev_list_files`, `dev_read_file`,
// `dev_list_generations`, `dev_get_generation`, `dev_read_reference`) change
// nothing. `shop_` covers the products family except its three listers
// (`shop_list_products`, `shop_list_groups`, `shop_list_offers`), which the
// negative lookahead excludes.
//
// `dev_compile_block` is deliberately NOT here, and it is the one absence
// that is a decision rather than an oversight. Almost all of a compile
// happens inside the worker — forty seconds in which `/b/dev/api/status`
// cannot report anything it has not already reported — so wrapping the tool
// would spend ~130 status reads and as many ladder redraws per compile, every
// one of them answering `idle`; on a D1-backed deployment those are real
// reads. `compileBlock` opens the panel around the STAGING call instead,
// which is the half the host does and the only half the journal describes,
// and the compiler's own progress reaches the log through `onProgress`.
// `dev_export` is not here either, for its own reason: it writes nothing — it
// reads the live generation and hands the browser a file — so an export must
// not reload the preview or re-fetch the file tree.
var MUTATING = /^(dev_write_file|dev_delete_file|dev_create_block|dev_rollback|dev_remove_block|shop_(?!list_))/;

// Every name this page registered. `registerTool`'s options bag takes an
// `AbortSignal`, but a browser (or a polyfill) that ignores it would leave
// this page's tools live on the agent after the page is gone — with the
// document's session cookie no longer riding along, so every call is a 401. The
// list is the fallback: on abort, unregister exactly these by name.
var registered = [];

// Register one tool, whichever registrar it came from.
//
// The `withProgress` wrap lives HERE rather than in `registerFromManifest`
// so that both registrars obey the same rule: a page-local tool that mutates
// the site wants the panel live and the panes refreshed for exactly the
// reasons a manifest tool does, and a second copy of the decision would be a
// second place for `MUTATING` to be forgotten. Applied outermost, so the
// panel is live for the whole call — `registerFromManifest` has already
// wrapped the manifest tools' `execute` in `withSessionCheck`, which has to
// see the raw result.
function registerPageTool(options) {
  if (MUTATING.test(options.name)) {
    options.execute = withProgress(options.execute);
  }
  document.modelContext.registerTool(options, { signal: abort.signal });
  registered.push(options.name);
}

function unregisterPageTools() {
  // `unregisterPageTools` runs on every `pagehide` and on every 401/403,
  // regardless of whether registration ever happened — a browser with no
  // WebMCP support at all has no `document.modelContext` (`registered` is
  // already `[]` in that case, from the guard below), so `document
  // .modelContext` must be checked for existence before its own methods
  // are, or this throws on unload in exactly the browsers the top-level
  // guard (`'modelContext' in document`) was written to tolerate.
  if (!document.modelContext || typeof document.modelContext.unregisterTool !== 'function') {
    registered = [];
    return;
  }
  registered.forEach(function (name) {
    try {
      document.modelContext.unregisterTool(name);
    } catch (error) {
      // Already gone — the browser honoured the signal. Both paths are
      // correct; only one of them runs on any given browser.
    }
  });
  registered = [];
}

// The session check for the manifest tools' own requests.
//
// Their `execute` comes from `toolOptions` (webmcp-core.js) and fetches
// without going through this file's `api`, so `check` never sees the
// response — and a refusal arrives as a RESULT (`isError` plus
// `Request failed (401): …`), never as a rejection. Reading that text back
// is what lets the same "the session is gone, take the tools away" rule
// apply to a tool call as to a pane refresh; without it the page would keep
// offering tools that are refused for the rest of the session.
var SESSION_GONE = /^Request failed \((401|403)\)/;

function withSessionCheck(execute) {
  return async function (args) {
    var result = await execute(args);
    var first = result && result.isError && result.content && result.content[0];
    if (first && typeof first.text === 'string' && SESSION_GONE.test(first.text)) {
      abort.abort();
    }
    return result;
  };
}

function registerFromManifest(manifest) {
  if (!manifest || !Array.isArray(manifest.tools)) {
    return;
  }
  manifest.tools.forEach(function (tool) {
    try {
      // `toolOptions` (webmcp-core.js) turns one manifest entry into the
      // registration options, `execute` included — the request it builds is
      // the same same-origin call `webmcp.js` makes for a site tool.
      var options = toolOptions(tool);
      // Session check innermost, so it sees the raw result. The progress
      // wrap goes on outside it, in `registerPageTool`.
      options.execute = withSessionCheck(options.execute);
      registerPageTool(options);
    } catch (error) {
      // One tool the browser rejected is not a reason to lose the rest —
      // the same per-tool guard `webmcp.js` applies.
      logError(error);
    }
  });
  log('registered ' + registered.length + ' workspace tools');
}

// `dev_compile_block` and `dev_export` are the two tools with no HTTP tool
// call behind them, for opposite reasons: compiling happens in a worker on
// this page and never reaches the server as one request, while exporting
// DOES have an endpoint (`GET /b/dev/api/export`) whose answer is a 15 MB zip
// — a file for the browser to download, not a tool result to hand an agent.
// Both are therefore registered here rather than projected from
// `/b/dev/api/tools.json`. (`dev_export_manifest`, which IS small JSON, is in
// that manifest — an agent inspects what an export would contain there.)
function registerPageLocal() {
  registerCompileTool();
  registerExportTool();
}

if ('modelContext' in document && typeof document.modelContext.registerTool === 'function') {
  api
    .get('/b/dev/api/tools.json')
    .then(json)
    .then(function (manifest) {
      registerFromManifest(manifest);
      registerPageLocal();
    })
    .catch(logError);
} else {
  log('this browser has no WebMCP support — the panes below still work');
}

// `pagehide`, not `unload`: it is the event a bfcache-eligible navigation
// actually fires, and it fires on the tab being closed as well.
//
// `event.persisted` is the half that matters. It is `true` when the document
// is going INTO the back/forward cache rather than away for good — it can
// come back, on Back, with its JS state intact and its session cookie still
// valid. Aborting there would be permanent (an `AbortController` cannot be
// reset), so the restored page would look alive while every tool it
// registered had been unregistered, `withProgress` short-circuited on
// `abort.signal.aborted`, and the log claiming the session expired. Nothing
// but a manual reload would bring it back. A frozen document's tools are not
// reachable by an agent on whatever page replaced it, so there is nothing to
// tear down while it sits there; if it is evicted instead of restored, the
// document is discarded and `document.modelContext` goes with it.
window.addEventListener('pagehide', function (event) {
  if (event.persisted) {
    return;
  }
  abort.abort();
});

abort.signal.addEventListener('abort', function () {
  // Nothing in flight is handed `abort.signal` — `api.get`/`api.post` only
  // ever pass `credentials` — so every mutating call still in flight WILL
  // reach its own `finally` and decrement `outstanding`. Resetting it here
  // is therefore just the interval's teardown, not a substitute for those
  // decrements; `withProgress` (above) is what keeps them from taking the
  // count negative or restarting the interval afterwards.
  outstanding = 0;
  stopPolling();
  unregisterPageTools();
  log('session expired — workspace tools removed; sign in again');
});

// ---- files and the editor -------------------------------------------------

// The file in the textarea: its path and the hash it had when it was read.
// The hash is what every write and delete sends back as `expected_sha256`,
// so a change the agent made under the human's feet is refused with a 409
// rather than silently overwritten.
var current = null;

var list = document.getElementById('dev-file-list');
var text = document.getElementById('dev-editor-text');
var title = document.getElementById('dev-editor-title');
var saveButton = document.getElementById('dev-save');

// The textarea and Save move together, and that pairing is load-bearing. A
// binary file leaves the box holding a PLACEHOLDER, not the file; saving it
// would write `(binary file, N bytes)` back as utf8 — with a matching
// `expected_sha256`, so the conflict check waves it through — destroying the
// file and publishing a generation for the loss. Locking the box without
// locking the button leaves exactly that one click available.
function setEditorEnabled(enabled) {
  text.disabled = !enabled;
  saveButton.disabled = !enabled;
}

async function loadFiles() {
  var files = (await json(await api.get('/b/dev/api/files'))).files;
  // The Compile button's block list is a projection of the SAME listing, not
  // a second request: the workspace's blocks are exactly the `blocks/<name>/`
  // prefixes in it, and asking twice would let the two panes disagree about
  // a block an agent had just created or removed.
  renderBlockChoices(files);
  list.innerHTML = '';
  files.forEach(function (file) {
    var li = document.createElement('li');
    var link = document.createElement('a');
    link.href = '#';
    link.textContent = file.path;
    link.setAttribute('data-path', file.path);
    if (current && current.path === file.path) {
      link.setAttribute('data-open', 'true');
    }
    link.addEventListener('click', function (event) {
      event.preventDefault();
      openFile(file.path).catch(logError);
    });
    li.appendChild(link);
    list.appendChild(li);
  });
}

async function openFile(path) {
  var file = await json(await api.post('/b/dev/api/files/read', { path: path }));
  current = { path: file.path, sha256: file.sha256 };
  title.textContent = file.path;
  // A binary file has no text to edit. Say what it is and lock the box,
  // rather than dropping base64 into an editor that would write it back as
  // literal text on the next save.
  text.value =
    file.encoding === 'utf8' ? file.content : '(binary file, ' + file.size + ' bytes)';
  setEditorEnabled(file.encoding === 'utf8');
  await loadFiles();
}

// A refusal's parsed body, or `null` when it has no shape either reader
// below can use.
//
// Every `/b/dev` refusal answers JSON: a `FileConflict` for a stale hash,
// and `{error, message}` for everything else (`no_store_error` /
// `no_store_error_status`, `blocks/dev/mod.rs`). What is NOT guaranteed is
// that the response came from the block at all — an intermediary's HTML
// error page, or a truncated body, parses to nothing or to a bare string,
// and neither `'current_sha256' in body` nor `body.message` may be asked of
// those (`in` throws a `TypeError` on a non-object, which would escape as a
// raw JS error into the log in place of the server's own explanation).
//
// This covers the 401/403 too, which `check` has already turned into a
// teardown by the time a caller gets here. The log line that teardown writes
// says the tools are gone; it does not say the click that provoked it did
// nothing. Both are worth saying, and the human asked for one of them.
async function refusalBody(response) {
  var text = await response.text();
  try {
    var body = JSON.parse(text);
    return body && typeof body === 'object' ? body : null;
  } catch (error) {
    return null;
  }
}

// `/b/dev/api/files/write` answers 409 for two UNRELATED reasons that
// happen to share a status: a real hash conflict (`FileConflict` —
// `path`/`current_sha256`/`current_size`, and `current_sha256` is present
// even when its value is `null`) and the block-count quota refusal
// (`QuotaError::TooManyBlocks`, `files.rs` — a bare `{error, message}`, no
// `current_sha256` at all, because "the workspace's shape conflicts with a
// limit" is not a hash conflict). The status alone cannot tell them apart —
// only the body's shape can.
function isFileConflict(body) {
  return body !== null && 'current_sha256' in body;
}

// The server's own words for a refusal, or `fallback` when the body carries
// none. The refusals the human sees most — a path outside the workspace, a
// quota, a file that is not there — all say something specific and useful,
// and none of it survives being replaced by a status code.
function refusalMessage(body, fallback) {
  return (body && typeof body.message === 'string' && body.message) || fallback;
}

var save = withProgress(async function () {
  // `text.disabled` is the second half of the guard, not a UI detail: a
  // disabled box holds a placeholder rather than the file's content (see
  // `setEditorEnabled`), and the button being disabled too is not something
  // this function may assume — a caller could reach it another way.
  if (!current || text.disabled) {
    return;
  }
  var response = await api.post('/b/dev/api/files/write', {
    path: current.path,
    content: text.value,
    expected_sha256: current.sha256
  });
  // A refusal is not a failure of the request — it is the answer to it.
  // Read before `json()`, which would turn it into a thrown error and lose
  // the body.
  //
  // EVERY refusal, not just the 409. The human clicked Save; whether the
  // answer is a hash conflict (409), a path the workspace will not take
  // (400) or a quota (413), it is a reply to what they just did, and a line
  // in the log they have to go looking for reads as "nothing happened".
  if (!response.ok) {
    var body = await refusalBody(response);
    if (isFileConflict(body)) {
      window.alert('Changed elsewhere (now ' + body.current_sha256 + '). Reopen the file.');
    } else {
      // Not a hash conflict — show the server's own explanation rather
      // than a `current_sha256` this body never carries.
      window.alert(refusalMessage(body, 'Save refused (' + response.status + ').'));
    }
    return;
  }
  var written = await json(response);
  current.sha256 = written.sha256;
  log(
    'saved ' +
      written.path +
      (written.generation ? ' — generation ' + written.generation.id : ' (staged, not published)')
  );
});

var remove = withProgress(async function () {
  if (!current || !window.confirm('Delete ' + current.path + '?')) {
    return;
  }
  var path = current.path;
  var response = await api.post('/b/dev/api/files/delete', {
    path: path,
    expected_sha256: current.sha256
  });
  // Same rule as `save` and `create`: the human clicked Delete and confirmed
  // it. A stale hash (409 — someone else changed the file since it was
  // opened, which is exactly the case `expected_sha256` exists to catch) or
  // a file already gone (404) is the answer to that click, not a log line.
  if (!response.ok) {
    var body = await refusalBody(response);
    if (isFileConflict(body)) {
      window.alert('Changed elsewhere since it was opened. Reopen ' + path + ' before deleting.');
    } else {
      window.alert(
        refusalMessage(body, 'Delete of ' + path + ' was refused (' + response.status + ').')
      );
    }
    return;
  }
  await json(response);
  current = null;
  title.textContent = 'Editor';
  text.value = '';
  setEditorEnabled(true);
  log('deleted ' + path);
});

var create = withProgress(async function () {
  var path = window.prompt('New file path (site/... or blocks/<name>/...)');
  if (!path) {
    return;
  }
  // `expected_sha256: null` is "I expect nothing here" — writing over an
  // existing file by accident is a 409, not a silent overwrite.
  var response = await api.post('/b/dev/api/files/write', {
    path: path,
    content: '',
    expected_sha256: null
  });
  // Told, not logged, on EVERY refusal. The human just typed this path; that
  // it already names a file (409), is not a path the workspace accepts (400
  // — by far the likeliest answer to something typed freehand into a
  // `prompt()`), or that the workspace hit a quota (409/413) is all an answer
  // to what they asked for, and a line in the log they have to go looking
  // for reads as "nothing happened".
  if (!response.ok) {
    var body = await refusalBody(response);
    if (isFileConflict(body)) {
      window.alert(path + ' already exists. Open it from the file list instead.');
    } else {
      window.alert(
        refusalMessage(body, path + ' was refused (' + response.status + ').')
      );
    }
    return;
  }
  await json(response);
  await openFile(path);
  log('created ' + path);
});

document.getElementById('dev-save').addEventListener('click', function () {
  save().catch(logError);
});
document.getElementById('dev-delete').addEventListener('click', function () {
  remove().catch(logError);
});
document.getElementById('dev-new-file').addEventListener('click', function () {
  create().catch(logError);
});
document.getElementById('dev-refresh-tools').addEventListener('click', function () {
  log('refreshing site tools');
  refreshSiteTools();
});

// ---- the packaged compiler -------------------------------------------------

// Where the browser Rust toolchain announces itself.
//
// It is a STATIC file, not an API route: `examples/dev-sandbox/impresspress
// .toml` overlays `compiler/dist/` onto `/__impresspress_dev/compiler/` and
// puts that prefix on the service worker's bypass list, because it is ~72 MiB
// of composed wasm that must never go through the runtime. So the page asks
// the host for it directly (plain `fetch`, not `api.get` — there is no session
// to carry and no 401/403 to react to), and a build that never ran the
// compiler overlay simply 404s.
var COMPILER_MANIFEST_URL = '/__impresspress_dev/compiler/manifest.json';

var compileButton = document.getElementById('dev-compile');
var compilerVersionEl = document.getElementById('dev-compiler-version');

// The manifest this build shipped, or `null` while there is none. What the
// Compile button will hand to `new BrowserRustCompiler(...)`; the page never
// guesses a worker URL, because `manifest.entry` is the only path that
// carries the pinned toolchain's version.
var compilerManifest = null;

// MiB, not MB, and `1048576` rather than a round million: `total_bytes` is a
// byte count, and every other figure published about this toolchain — the
// build script's own summary, the README's, the 24 MiB per-file asset limit
// the verifier enforces — is binary. Labelling 71.6 MiB as "75.1 MB" would be
// the same number told two ways on one page.
function describeCompiler(manifest) {
  return 'Compiler v' + manifest.version + ' · ' + (manifest.total_bytes / 1048576).toFixed(1) + ' MiB';
}

// Ask the host what toolchain this build carries, and say so on the page.
//
// `cache: 'no-store'` because the manifest is the one file in the compiler
// tree whose URL does NOT carry the version — every other one does, which is
// what makes them immutable. A cached manifest is a page pointing at a
// toolchain this build does not ship, and the failure it produces (a 404 on a
// worker script) is a long way from its cause. It is a few hundred bytes once
// per page load.
async function discoverCompiler() {
  var response = await fetch(COMPILER_MANIFEST_URL, { cache: 'no-store' });
  // Not an error: a bundle built without the compiler overlay is a legitimate
  // build — CI's foundations run is one — and the honest thing is a button
  // that says why it cannot be pressed rather than one that fails on click.
  if (response.status === 404) {
    log('no compiler in this build');
    updateCompileButton();
    return;
  }
  if (!response.ok) {
    throw new Error('HTTP ' + response.status + ' for ' + COMPILER_MANIFEST_URL);
  }
  compilerManifest = await response.json();
  compilerVersionEl.textContent = describeCompiler(compilerManifest);
  updateCompileButton();
  log('compiler ' + compilerManifest.version + ' available');
}

// ---- compiling a block ----------------------------------------------------

// Where a block's sources live, and the string every path under one starts
// with. `workspace::BLOCKS_PREFIX` on the other side.
var BLOCKS_PREFIX = 'blocks/';

// Which block the Compile button acts on.
//
// Assigned here rather than beside the file pane's own elements because it
// belongs to the compiler, and the ordering works out either way: nothing
// calls `loadFiles` until the first-paint block at the bottom of this file,
// which is below this line.
var compileSelect = document.getElementById('dev-compile-block');

// The blocks in the workspace, as `renderBlockChoices` last saw them.
var blockNames = [];

// Whether a compile is running, whoever started it.
//
// Both entries into `compileBlock` — the button and `dev_compile_block` —
// set it, and both are REFUSED by it while it is set, because a second build
// is the same waste however it was asked for. Nothing below this line refuses
// one: the WORKER rejects a concurrent `compile`, but the adapter never lets
// it see one (`compiler-adapter.js`: "the queue therefore lives here"), so a
// second call would queue up a whole second snapshot and eighty more seconds
// of rustc for an answer the first one is already producing. Read by
// `updateCompileButton` and by `compileBlock`; see both.
var compileInFlight = false;

// Whether Compile can be pressed, and why not when it cannot.
//
// It takes TWO things this page learns separately and in no fixed order: a
// toolchain (the manifest, fetched once at load) and something to compile (a
// `blocks/<name>/` prefix in the file listing, refreshed after every mutating
// call). Either answer can arrive first, so both write their fact and call
// this, and this is the only place the button's state is decided — two call
// sites each setting `disabled` would race, and whichever landed second would
// win regardless of what it knew.
//
// A disabled button with a reason on it, rather than an enabled one that
// explains itself in an alert after the fact: the page already knows there is
// nothing to compile, and offering the click anyway is a promise it cannot
// keep.
function updateCompileButton() {
  // First, because while a compile runs it is the whole answer: there IS a
  // toolchain and there IS a block, and the button must still not be
  // pressable. Set from `compileBlock`, so an agent's `dev_compile_block`
  // greys it out exactly as the button's own click does.
  if (compileInFlight) {
    compileButton.disabled = true;
    compileButton.title = 'A compile is already running';
    return;
  }
  if (!compilerManifest) {
    compileButton.disabled = true;
    compileButton.title = 'No compiler in this build';
    return;
  }
  if (!blockNames.length) {
    compileButton.disabled = true;
    compileButton.title = 'No block to compile yet — create one with dev_create_block';
    return;
  }
  compileButton.disabled = false;
  compileButton.title = '';
}

// The workspace's blocks, as the Compile button's options.
//
// Derived from the file listing rather than kept as its own state: a block
// IS a `blocks/<name>/` prefix with files under it (there is no block
// record until one compiles), so the listing is the only honest source. The
// listing arrives in path order, so the names come out sorted without
// sorting.
function renderBlockChoices(files) {
  var names = [];
  files.forEach(function (file) {
    if (file.path.indexOf(BLOCKS_PREFIX) !== 0) {
      return;
    }
    var name = file.path.slice(BLOCKS_PREFIX.length).split('/')[0];
    if (name && names.indexOf(name) < 0) {
      names.push(name);
    }
  });
  // Every mutating call refreshes the panes, so this runs while a human may
  // have a block picked and be reaching for Compile. Their choice is read
  // back and restored; a block that has since been removed is simply gone,
  // and the select falls back to its first option.
  var chosen = compileSelect.value;
  compileSelect.innerHTML = '';
  names.forEach(function (name) {
    var option = document.createElement('option');
    option.value = name;
    option.textContent = name;
    compileSelect.appendChild(option);
  });
  if (names.indexOf(chosen) >= 0) {
    compileSelect.value = chosen;
  }
  blockNames = names;
  updateCompileButton();
}

/** Lowercase hex SHA-256 of a string, the form the build row records. */
async function sha256Hex(text) {
  var digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return Array.prototype.map
    .call(new Uint8Array(digest), function (byte) {
      return byte.toString(16).padStart(2, '0');
    })
    .join('');
}

// The artifact as `artifact_base64`.
//
// `btoa` takes a string, so the bytes go through `String.fromCharCode` — and
// that has to be chunked, because `apply` spreads its array into ARGUMENTS
// and a 4 MiB module would be four million of them, which overflows the call
// stack in every engine. 0x8000 is the conventional slice: comfortably under
// every engine's argument limit, and 128 iterations at the sandbox's 4 MiB
// ceiling.
function toBase64(bytes) {
  var CHUNK = 0x8000;
  var parts = [];
  for (var i = 0; i < bytes.length; i += CHUNK) {
    parts.push(String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK)));
  }
  return btoa(parts.join(''));
}

// The compiler's own stages, as log lines.
//
// They are NOT activation phases and are deliberately kept off the ladder:
// `download`/`initializing`/`compiling` happen in a worker on this page,
// before the server has been told anything at all, while every step of the
// ladder is a phase of an activation the SERVER is running. A panel that
// mixed them would be claiming the site was being republished while a
// toolchain downloaded.
//
// De-duplicated on the rendered line, because the toolchain reports its
// ~72 MiB download by chunks and the log holds 200 lines — without this one
// cold start would flush everything else out of the panel.
var lastProgressLine = null;

function appendProgress(progress) {
  var line = 'compiler: ' + progress.stage;
  if (progress.total > 0) {
    line += ' ' + Math.floor((progress.loaded / progress.total) * 100) + '%';
  }
  if (progress.detail) {
    line += ' — ' + progress.detail;
  }
  if (line === lastProgressLine) {
    return;
  }
  lastProgressLine = line;
  log(line);
}

// The one compiler session this page ever has.
//
// Kept across compiles because `BrowserRustCompiler` replaces its own worker
// after a cancel, a timeout or a protocol violation and stays usable
// (compiler-adapter.js): the instance outlives the workers it owns, so
// throwing it away here would only discard a toolchain that is already
// downloaded and instantiated.
var compiler = null;

async function ensureCompiler(onProgress) {
  // Not something the agent did, and not something a retry fixes: this
  // bundle was built without the compiler overlay. `discoverCompiler` has
  // already said so on the button; this is the same fact reaching a tool
  // call, and it is the one compile failure that is an `isError` rather than
  // a result — there is no build to report on.
  if (!compilerManifest) {
    throw new Error('No compiler in this build.');
  }
  if (!compiler) {
    compiler = new BrowserRustCompiler(compilerManifest);
  }
  // Idempotent, and the only place the toolchain's start-up is paid for: a
  // later compile joins whatever worker this one leaves behind.
  await compiler.initialize(onProgress);
  return compiler;
}

// One compiler diagnostic, as the sandbox's own wire type.
//
// A copy, and deliberately nothing more. The two types carry the same six
// fields with the same meanings, and `code` is optional on BOTH sides —
// `compiler/src/protocol.ts` leaves it out when rustc gave the diagnostic no
// number, and `validation::Diagnostic.code` is an `Option<String>` for
// exactly that case — so a diagnostic the compiler did not number goes out as
// `null` rather than being given a value the page made up. The only work here
// is spelling "absent" the way the wire does (`undefined` → `null`), because
// `JSON.stringify` drops an undefined field and serde would then see a
// missing key rather than an explicit one.
//
// Nothing is re-checked: `compiler-adapter.js` validates every diagnostic's
// shape before `compile` resolves (`malformedDiagnostic`) and treats a
// malformed one as a protocol violation, so a guard here would be a second
// opinion about a value that cannot reach this function.
function stageDiagnostic(diagnostic) {
  return {
    severity: diagnostic.severity,
    code: diagnostic.code === undefined ? null : diagnostic.code,
    message: diagnostic.message,
    file: diagnostic.file,
    line: diagnostic.line,
    column: diagnostic.column
  };
}

// What cargo will call the built `.wasm`, from a `Cargo.toml` — or `null`
// when the file names nothing.
//
// Deliberately not a TOML parser: these are the two keys that decide the
// artifact's file name and nothing else is read. The scan is per table, so a
// `name` under `[profile.release]` cannot be mistaken for one of them.
// `[lib] name` wins where it is set, because that is what cargo names a
// cdylib after; hyphens become underscores, which is the form the worker
// asks the VFS for.
function cargoArtifactStem(text) {
  var table = '';
  var names = {};
  text.split('\n').forEach(function (raw) {
    var line = raw.trim();
    if (line.charAt(0) === '[') {
      table = line;
      return;
    }
    var found = /^name\s*=\s*"([^"]*)"/.exec(line);
    if (found && (table === '[package]' || table === '[lib]')) {
      names[table] = found[1];
    }
  });
  var name = names['[lib]'] !== undefined ? names['[lib]'] : names['[package]'];
  return name === undefined ? null : name.replace(/-/g, '_');
}

// Read `blocks/<name>/` out of the workspace, as the compiler wants it.
//
// The worker's VFS is keyed on paths RELATIVE to the crate root — it writes
// `Cargo.toml` and `src/lib.rs`, not `blocks/hello/Cargo.toml` — so the
// workspace prefix is stripped here and nowhere else.
//
// Three things are refused here rather than handed to the compiler, each
// with its own code, because each of them reaches rustc as a failure it can
// only describe badly:
//
//   * `binary-source` — a file the sandbox could not hand back as text. A
//     skip would let the crate compile without it, quietly producing a block
//     whose sources are not the sources on disk; `include_bytes!` of an image
//     is the shape that gets a human here.
//   * `nested-source` — a path in any subdirectory the flat crate the worker
//     writes does not have: anything below `src/`, and anything below a
//     directory of its own beside `Cargo.toml`.
//   * `package-name` — a `Cargo.toml` whose package is not this block.
async function snapshotBlock(name) {
  var prefix = BLOCKS_PREFIX + name + '/';
  var listed = await json(await api.get('/b/dev/api/files?prefix=' + encodeURIComponent(prefix)));
  // A name with nothing under it is a request the caller got wrong, not a
  // verdict on a block — the same thing the files API answers with a 404,
  // and it reaches the agent the same way (`isError`). Handing an empty
  // `files` map to the compiler would instead spend a minute of rustc to
  // report that a crate with no `Cargo.toml` does not build.
  if (!listed.files.length) {
    throw new Error('there is no block at ' + prefix + ' — create one with dev_create_block');
  }
  var files = {};
  var diagnostics = [];
  var guestVersion = null;
  // The source manifest, one `<crate-relative path>\0<sha256>\n` line per
  // file, sorted. NUL rather than a space because a path may contain
  // anything but that, so no two different snapshots can produce one string;
  // crate-relative because that is what the compiler was given, and a digest
  // over paths it never saw would describe a different build than the one
  // that ran. The server stores it without recomputing it — it is provenance
  // for a stored build, so its only contract is with itself.
  var manifest = [];
  for (var i = 0; i < listed.files.length; i += 1) {
    var entry = listed.files[i];
    var rel = entry.path.slice(prefix.length);
    // A manifest at the root and a flat `src/` is the whole shape the worker
    // can write, and this is where that is enforced. It writes every file
    // into the VFS by path, and whether rubrc's write-file event creates the
    // intermediate directories has never been verified — so ANY path that
    // needs one would fail somewhere inside rustc or the VFS forty seconds
    // later, with a message about a module it could not find, instead of
    // failing here with the path in it. That is as true of `tests/smoke.rs`
    // and `assets/logo.svg` as it is of `src/routes/mod.rs`, so the test is
    // "a directory separator this layout does not have" rather than "a
    // second one under `src/`". Refused before the read, because nothing
    // about the contents changes the answer.
    var firstSlash = rel.indexOf('/');
    var nested =
      firstSlash >= 0 &&
      (rel.slice(0, firstSlash + 1) !== 'src/' || rel.indexOf('/', firstSlash + 1) >= 0);
    if (nested) {
      diagnostics.push({
        severity: 'error',
        code: 'nested-source',
        message:
          entry.path +
          ' is in a subdirectory, and the browser toolchain compiles a flat crate: ' +
          'Cargo.toml at the root and every source file directly in src/. ' +
          'Move it beside lib.rs.',
        file: rel,
        line: null,
        column: null
      });
      continue;
    }
    var file = await json(await api.post('/b/dev/api/files/read', { path: entry.path }));
    if (file.encoding !== 'utf8') {
      diagnostics.push({
        severity: 'error',
        code: 'binary-source',
        message:
          entry.path +
          ' is ' +
          file.encoding +
          '-encoded, and the browser toolchain compiles text. Move it out of ' +
          prefix +
          ' or delete it.',
        file: rel,
        line: null,
        column: null
      });
      continue;
    }
    files[rel] = file.content;
    manifest.push(rel + '\0' + file.sha256 + '\n');
    // cargo names the artifact after the crate, and the worker reads that one
    // file back out of the VFS by the BLOCK's name. A renamed crate therefore
    // builds cleanly and leaves nothing to collect — a green build with no
    // module, which the worker can only report as "the artifact is missing".
    // Refusing it here says which line to change.
    if (rel === 'Cargo.toml') {
      var stem = cargoArtifactStem(file.content);
      var wanted = name.replace(/-/g, '_');
      if (stem !== null && stem !== wanted) {
        diagnostics.push({
          severity: 'error',
          code: 'package-name',
          message:
            'Cargo.toml builds ' +
            stem +
            '.wasm, but this block is "' +
            name +
            '" and the sandbox reads back ' +
            wanted +
            '.wasm. `[package] name` — and `[lib] name`, if you set one — must be the ' +
            "block's name.",
          file: rel,
          line: null,
          column: null
        });
      }
    }
    // The vendored module IS the ABI, so the version the block was compiled
    // against is read out of the copy that was compiled — not out of the
    // sandbox's own constant, which would report agreement it cannot see. A
    // block whose module has been edited past recognition simply reports
    // nothing, and staging records `0` — "unknown" — rather than a guess.
    if (rel === 'src/wafer_guest.rs') {
      var found = /WAFER_GUEST_VERSION: u32 = (\d+)/.exec(file.content);
      if (found) {
        guestVersion = Number(found[1]);
      }
    }
  }
  manifest.sort();
  return {
    files: files,
    diagnostics: diagnostics,
    guestVersion: guestVersion,
    sourceSha: await sha256Hex(manifest.join(''))
  };
}

// Snapshot, compile, stage — the whole of what `dev_compile_block` and the
// Compile button do, with the result they both report.
//
// The wrapper is the in-flight bookkeeping and nothing else: `#dev-compile`
// is greyed out for the duration, whichever of the two started the build,
// and it comes back however the build ended — including a throw, which is
// why this is a `finally` and not a line at each exit.
async function compileBlock(name) {
  // The flag is a lock, not just a button state. Two builds may genuinely
  // overlap — `dev_compile_block` while the button's compile runs, or an
  // agent issuing the tool twice — and letting the second through would run
  // it to completion behind the first in the adapter's queue, with the FIRST
  // one's `finally` clearing this flag and re-enabling `#dev-compile` while
  // the second was still going: a button offering a third build over a
  // running one. A throw rather than a `success: false`, per the split
  // `runCompile` documents — a refused request is a failure of the
  // machinery, not a verdict on the block, and reaches an agent as `isError`.
  if (compileInFlight) {
    throw new Error('a compile is already running — wait for it to finish');
  }
  compileInFlight = true;
  updateCompileButton();
  try {
    return await runCompile(name);
  } finally {
    compileInFlight = false;
    updateCompileButton();
  }
}

// The three steps themselves, and the result they produce.
//
// Every failure that is an ANSWER about the block comes back as
// `success: false` with diagnostics: sources the compiler cannot read, a
// crate that does not compile, a module the validator refuses. Only a
// failure of the machinery — no compiler in this build, a worker that broke
// its protocol, a request the sandbox refused — throws, and the callers turn
// that into the tool's `isError`. That split is design §7.4: an agent needs
// to know whether to fix its Rust or to stop trying.
async function runCompile(name) {
  // The ladder this page is about to draw is not the last one's. `completed`
  // survives only while it describes the live generation, and everything
  // below can end without producing a new one — a crate that does not build,
  // a staging refusal, an activation the host abandons — leaving a fully
  // green ladder standing over a compile that never landed. Dropping it here
  // costs nothing when the compile succeeds (`renderProgress` sets it again a
  // few lines later) and is the difference between an empty panel and a lie
  // when it does not. Nothing else needs this: a site write or a rollback
  // publishes a generation with a NEW id, which `renderStatus` already sees
  // is not the one `completed` describes.
  //
  // And the ladder on screen is redrawn here, not left to the next poll:
  // `drawLadder` runs only from `observe`, and the status is no longer polled
  // while the worker compiles (the panel opens at the staging call below), so
  // forgetting `completed` without repainting would leave the PREVIOUS
  // compile's four green steps standing over this one for its whole eighty
  // seconds — the exact lie this line exists to end. `idle` is the phase
  // `renderStatus` itself draws for a sandbox with nothing in flight, and it
  // is not in `PHASES`, so every step comes out `pending`.
  completed = null;
  drawLadder('idle', false);
  var snapshot = await snapshotBlock(name);
  if (snapshot.diagnostics.length) {
    log(
      'compile refused: ' +
        name +
        ' — ' +
        snapshot.diagnostics
          .map(function (diagnostic) {
            return diagnostic.code;
          })
          .join(', ')
    );
    return {
      success: false,
      cancelled: false,
      build_id: null,
      generation: null,
      diagnostics: snapshot.diagnostics,
      stdout: '',
      stderr: '',
      elapsed_ms: 0,
      compiler_version: null,
      progress: []
    };
  }

  var session = await ensureCompiler(appendProgress);
  var built = await session.compile({
    crateName: name,
    files: snapshot.files,
    onProgress: appendProgress
  });
  var diagnostics = built.diagnostics.map(stageDiagnostic);
  // A compile the adapter GAVE UP ON, which is not the same failure as a
  // crate that does not build.
  //
  // `compile` RESOLVES this one rather than rejecting: `#abandonInFlight`
  // answers the abandoned build with `{ success: false, cancelled: true,
  // diagnostics: [] }` because the cancel was the page's own doing — the
  // 120 s budget is the PAGE's policy, not the worker's (compiler-adapter.js).
  // So without a word from here it would reach an agent as "your crate does
  // not compile" with zero diagnostics: the one failure that says nothing at
  // all about the block, and the one an agent would answer by rewriting Rust
  // that was never the problem. Nothing else knows what happened, so the page
  // says it — the same way it produces its own `binary-source` diagnostic.
  if (built.cancelled) {
    diagnostics = [
      {
        severity: 'error',
        code: 'compile-timeout',
        message:
          'the compile exceeded 120 s and was cancelled; the worker was recreated' +
          (built.stderr ? ' (' + built.stderr + ')' : ''),
        file: null,
        line: null,
        column: null
      }
    ].concat(diagnostics);
  }
  if (!built.success) {
    log(
      (built.cancelled ? 'compile cancelled: ' : 'compile failed: ') +
        name +
        ' (' +
        built.elapsedMs +
        ' ms)'
    );
    return {
      success: false,
      cancelled: built.cancelled,
      build_id: null,
      generation: null,
      diagnostics: diagnostics,
      stdout: built.stdout,
      stderr: built.stderr,
      elapsed_ms: built.elapsedMs,
      compiler_version: built.compilerVersion,
      progress: []
    };
  }

  // `compiler_version` is required and identifies the toolchain that
  // produced the stored artifact. `rustc --version` from inside the worker's
  // VFS is the better answer; the manifest's version — the pinned rubrc
  // revision every compiler URL already carries — is the honest fallback for
  // a worker that reached `ready` without reporting one.
  var compilerVersion = built.compilerVersion || compilerManifest.version;
  // The progress panel opens HERE and not around the whole compile. Staging
  // is the only part of this the HOST does — publish, rebuild the runtime,
  // activate — so it is the only part `/b/dev/api/status` can describe, and
  // the ladder is drawn from exactly those phases. Everything above ran in
  // the worker and reported through `appendProgress`; polling the status
  // through it would be a hundred-odd reads of a row that says `idle`.
  var staged = await withProgress(async function () {
    return json(
      await api.post('/b/dev/api/builds/stage', {
        block_name: name,
        artifact_base64: toBase64(built.artifact),
        source_manifest_sha256: snapshot.sourceSha,
        compiler_version: compilerVersion,
        diagnostics: diagnostics,
        wafer_guest_version: snapshot.guestVersion
      })
    );
  })();
  if (staged.success) {
    log('compiled ' + name + ' — generation ' + staged.generation.id);
    renderProgress(staged.generation.id, staged.progress);
  } else {
    log('staging refused ' + name + ': ' + staged.diagnostics.length + ' diagnostics');
  }
  return {
    success: staged.success,
    cancelled: false,
    build_id: staged.build_id,
    generation: staged.generation,
    // The staging response already carries back the diagnostics it was sent,
    // with the validator's appended — so it is the whole account of the
    // build and the local copy is not concatenated onto it again.
    diagnostics: staged.diagnostics,
    stdout: built.stdout,
    stderr: built.stderr,
    elapsed_ms: built.elapsedMs,
    compiler_version: compilerVersion,
    progress: staged.progress
  };
}

function registerCompileTool() {
  registerPageTool({
    name: 'dev_compile_block',
    description:
      'Compile blocks/<name>/ with the in-browser Rust toolchain (wasm32-wasip1, no \
dependencies — the whole SDK is the vendored src/wafer_guest.rs). On success the block is \
validated and activated immediately and its routes are live at /b/<name>/; on failure the result \
carries structured compiler or validator diagnostics and the previous generation keeps serving. \
Only one compile runs at a time.',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'Block name, as used in blocks/<name>/' }
      },
      required: ['name'],
      additionalProperties: false
    },
    outputSchema: {
      type: 'object',
      properties: {
        success: { type: 'boolean' },
        cancelled: { type: 'boolean' },
        build_id: { type: ['string', 'null'] },
        generation: { type: ['object', 'null'] },
        diagnostics: { type: 'array' },
        stdout: { type: 'string' },
        stderr: { type: 'string' },
        elapsed_ms: { type: 'integer' },
        compiler_version: { type: ['string', 'null'] },
        progress: { type: 'array' }
      },
      required: ['success', 'diagnostics']
    },
    execute: async function (args) {
      try {
        var result = await compileBlock(String(args && args.name));
        // Both halves, because an agent may read either: the text block is
        // what a client without `outputSchema` support shows, and
        // `structuredContent` is what one with it reads.
        return {
          content: [{ type: 'text', text: JSON.stringify(result) }],
          structuredContent: result
        };
      } catch (error) {
        // Everything that is not an answer about the block: no compiler in
        // this build, a worker that broke its protocol or would not start, a
        // request the sandbox refused. A TIMEOUT does not come through here
        // — the adapter resolves that one, and `compileBlock` reports it as
        // a `compile-timeout` diagnostic above. For the violations that do,
        // the adapter has already destroyed and replaced the worker by the
        // time this runs, so the next call works.
        logError(error);
        return {
          isError: true,
          content: [
            {
              type: 'text',
              text:
                'dev_compile_block: ' +
                (error && error.message ? error.message : String(error))
            }
          ]
        };
      }
    }
  });
}

// ---- exporting the site ---------------------------------------------------

// The Export button. Ships `disabled` in the markup (`page.rs`) and is
// enabled by `renderStatus` once there is a live generation — an export is a
// snapshot of what is LIVE, and `GET /b/dev/api/export` answers 400 on an
// instance that has published nothing, so a button offered before then is a
// promise the page cannot keep.
//
// Declared here rather than beside `renderStatus`, which reads it, for the
// same reason `compileButton` is declared beside the compiler: `var` hoists
// the binding, and the first `renderStatus` runs from the status fetch at the
// bottom of this file — after every line of it, including this one.
var exportButton = document.getElementById('dev-export');

// The running export's promise, or `null` when none is running.
//
// An export reads the whole runtime shell twice and builds a multi-megabyte
// archive in memory; a second one started on top of the first would do all of
// that again for a download the browser is about to replace anyway. The
// button is taken away for the duration — and, because the button is not the
// only caller, `exportSite` returns THIS promise rather than starting a
// second export (a `dev_export` tool call racing a click is two callers, and
// only one of them is a button).
var exportInFlight = null;

// Whether Export can be pressed, and why not when it cannot.
//
// TWO facts, arriving from different places and in no fixed order — is
// anything published (from `renderStatus`, which polls) and is an export
// already running (from `exportSite`) — so this is the single owner of the
// button's state, exactly as `updateCompileButton` is for Compile. Two call
// sites each setting `disabled` would race, and whichever landed second would
// win regardless of what it knew: a status poll landing mid-export would
// re-enable the button under the running call.
//
// A disabled button with a reason on it, rather than an enabled one that
// explains itself after the click: the page already knows.
function updateExportButton() {
  if (exportInFlight) {
    exportButton.disabled = true;
    exportButton.title = 'Export in progress…';
    return;
  }
  // An export is a snapshot of the LIVE generation, and there is nothing to
  // snapshot until something has been published. The endpoint says the same
  // thing with a 400; the button says it without making anyone click to find
  // out.
  if (lastActiveGeneration === null) {
    exportButton.disabled = true;
    exportButton.title = 'Nothing published yet — write a site file first';
    return;
  }
  exportButton.disabled = false;
  exportButton.title = '';
}

// How long the object URL is kept alive after the click.
//
// Revoking it immediately is the classic bug: `a.click()` starts the download
// asynchronously, and a URL revoked in the same turn can be gone before the
// browser has read a byte of it. A minute is far longer than any local read
// of a blob already in memory needs, and the URL is released rather than
// leaked for the life of the page.
var OBJECT_URL_TTL_MS = 60000;

// Export once, no matter how many callers ask.
//
// A second call while one is running returns the RUNNING export's promise —
// the same object, resolving to the same manifest, having made no second
// request and started no second download. Not `null`, and not a throw: the
// tool's `execute` reads `manifest.generation_id` off whatever this returns,
// and the honest answer to "export the site" while an export of that same
// site is in flight is that export.
//
// A plain function rather than an `async` one so the promise a second caller
// gets is IDENTICAL to the first's; an `async` wrapper would adopt it into a
// new one and the no-op would be observable only by counting requests.
function exportSite() {
  if (exportInFlight) {
    return exportInFlight;
  }
  // Assigned before anything awaits — `runExport()` returns its promise
  // synchronously — so two calls in the same turn cannot both get past the
  // check above. Released on EVERY exit, including the failures: a refused
  // export that left the button disabled would take the feature away for the
  // life of the page, which is a worse outcome than the refusal itself.
  exportInFlight = runExport().finally(function () {
    exportInFlight = null;
    updateExportButton();
  });
  updateExportButton();
  return exportInFlight;
}

// Fetch the manifest, fetch the zip, hand it to the browser as a download.
//
// The manifest first, and not only for the filename: it is what the tool
// returns as `structuredContent`, so an agent that called `dev_export` learns
// what it just downloaded — how many files, how big, how many product rows —
// without a second call. It is a summary of the very entry list the archive
// is built from (`blocks/dev/export.rs`), so the two cannot disagree.
async function runExport() {
  var manifest = await json(await api.get('/b/dev/api/export/manifest'));
  var response = await api.get('/b/dev/api/export');
  if (!response.ok) {
    throw new Error('export failed: HTTP ' + response.status + ': ' + (await response.text()));
  }
  var blob = await response.blob();
  var url = URL.createObjectURL(blob);
  var link = document.createElement('a');
  link.href = url;
  link.download = 'impresspress-site-' + manifest.generation_id.slice(0, 8) + '.zip';
  // Appended to the document before the click and removed after: a detached
  // anchor's `click()` is honoured by Chromium but not by every engine, and
  // this is the one line that makes the download work everywhere.
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(function () {
    URL.revokeObjectURL(url);
  }, OBJECT_URL_TTL_MS);
  log(
    'exported generation ' +
      manifest.generation_id.slice(0, 8) +
      ' — ' +
      manifest.files.length +
      ' files, ' +
      (manifest.total_bytes / 1048576).toFixed(1) +
      ' MiB'
  );
  return manifest;
}

function registerExportTool() {
  registerPageTool({
    name: 'dev_export',
    description:
      'Export the current site as a runnable static bundle (zip) and hand it to the browser as \
a download: the runtime, the site files, compiled blocks and their source, and a data snapshot of \
products, offers and settings. Serve the unzipped folder over http to view it. Returns what the \
bundle contains; use dev_export_manifest to see that without downloading.',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false },
    outputSchema: {
      type: 'object',
      properties: {
        generation_id: { type: 'string' },
        files: { type: 'array' },
        total_bytes: { type: 'integer' },
        shell_files: { type: 'integer' },
        site_files: { type: 'integer' },
        blocks: { type: 'integer' },
        tables: { type: 'object' }
      },
      required: ['generation_id', 'files', 'total_bytes']
    },
    execute: async function () {
      try {
        var manifest = await exportSite();
        return {
          content: [{ type: 'text', text: JSON.stringify(manifest) }],
          structuredContent: manifest
        };
      } catch (error) {
        // Everything here is a failure of the machinery — nothing published
        // yet, a shell the runtime could not read, a download the browser
        // refused. None of them is an answer ABOUT the site, so all of them
        // are `isError` rather than a result with diagnostics (the split
        // `dev_compile_block` makes for the same reason).
        logError(error);
        return {
          isError: true,
          content: [
            {
              type: 'text',
              text: 'dev_export: ' + (error && error.message ? error.message : String(error))
            }
          ]
        };
      }
    }
  });
}

exportButton.addEventListener('click', function () {
  exportSite().catch(logError);
});

// The button runs the same function on the same block, and reports the same
// way: the panel is the human's copy of what the agent would have been told.
async function compileSelected() {
  // `updateCompileButton` keeps the button disabled unless there is both a
  // toolchain and a block, so this is unreachable through the UI — it is here
  // because `compileBlock` would otherwise be handed an empty name and spend
  // a request finding out.
  var name = compileSelect.value;
  if (!name) {
    log('nothing to compile: the workspace has no blocks');
    return;
  }
  await compileBlock(name);
}

compileButton.addEventListener('click', function () {
  compileSelected().catch(logError);
});

// ---- first paint ----------------------------------------------------------

loadFiles().catch(logError);
api.get('/b/dev/api/status').then(json).then(observe).catch(logError);
discoverCompiler().catch(logError);
