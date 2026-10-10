// webllm-engine.js — page-side WebLLM engine.
//
// Runs in the window (WebGPU is window-only). Two consumers:
//
// 1. Page-direct load: import { loadEngine } from '/webllm-engine.js' and
//    call it from a window-side script (the only correct path on Chrome due
//    to its ~5-min FetchEvent cap on SW-routed requests).
//
// 2. SW-routed chat / unload / cancel: receives postMessages from the SW via
//    navigator.serviceWorker.message, runs WebLLM, streams frames back.
//
// SW → Page request shapes (see bridge.js for the producing side):
//   { type: 'engine-probe',            id, family }   // answered when family is 'llm'
//   { type: 'llm-unload-request',      id, modelId }
//   { type: 'llm-chat-stream-request', id, body }     // body = JSON chat request
//   { type: 'llm-stream-cancel',       id }
//
// Page → SW reply shapes:
//   { type: 'engine-present',      id, loaded, loading }   // runs the LLM engine;
//                                                          // loaded / loading = [modelId] or []
//   { type: 'llm-unload-response', id, error? }            // one-shot
//   { type: 'llm-stream-frame',    id, kind, payload?, code? }  // streams
//     `kind` ∈ {'chunk','done','error'}; chat emits 'chunk' frames per token
//     and a terminal 'done' / 'error'.
//
// An error frame whose `code` is ENGINE_UNAVAILABLE is a refusal, as
// bridge.js's own are: the engine cannot take the chat here (the model it
// was sent for is not loaded in this page), and the payload is written for
// whoever made the request. bridge.js passes it on as a refusal, which the
// LLM service reports as `EngineUnavailable` with this message, and the chat
// routes answer as a 503 carrying it; any other error is a fault in the chat
// itself. A failed `loadEngine` is not reported here at all: the load is
// page-direct, so it rejects to the page script that called it.
//
// bridge.js sends a request only to a page that answered its probe, and a
// chat or an unload only to the page whose answer lists the model: the
// engine loaded page-direct lives in this tab alone. The answer costs nothing
// (no model is loaded to give it) and must not wait for anything.
//
// The @mlc-ai/web-llm import is lazy: a top-level static import would block
// DOMContentLoaded for every page that loads this script (it's a multi-MB
// jsdelivr ESM bundle), and most page loads never end up invoking the LLM.
// Defer the import until a handler actually needs it.

/** bridge.js's `ENGINE_UNAVAILABLE`: the `code` of a refusal. */
const ENGINE_UNAVAILABLE = 'engine-unavailable';

let _CreateMLCEngine = null;
async function loadCreateMLCEngine() {
    if (_CreateMLCEngine) return _CreateMLCEngine;
    const mod = await import('https://cdn.jsdelivr.net/npm/@mlc-ai/web-llm@0.2.74/+esm');
    _CreateMLCEngine = mod.CreateMLCEngine;
    return _CreateMLCEngine;
}

let _engine = null;
let _engineModel = null;
const _activeStreams = new Map(); // id -> AbortController (chat only)

async function swPost(payload) {
    const reg = await navigator.serviceWorker.ready;
    reg.active?.postMessage(payload);
}

async function swStreamFrame(id, kind, payload) {
    await swPost({ type: 'llm-stream-frame', id, kind, payload });
}

async function handleUnload(msg) {
    try {
        await settledLoad();
        if (_engine) {
            await _engine.unload();
            _engine = null;
            _engineModel = null;
        }
        await swPost({ type: 'llm-unload-response', id: msg.id });
    } catch (e) {
        await swPost({ type: 'llm-unload-response', id: msg.id, error: String(e) });
    }
}

async function handleChatStream(msg) {
    // Registered before the wait for a load in progress, so a cancel that
    // arrives during the load is honoured: the chat never starts.
    const ac = new AbortController();
    _activeStreams.set(msg.id, ac);
    try {
        await settledLoad();
        if (ac.signal.aborted) {
            await swStreamFrame(msg.id, 'error', 'cancelled');
            return;
        }
        if (!_engine) {
            // Sent here because this page held the model, or was loading it,
            // when it was probed: the load has since failed, or the model was
            // unloaded. The engine cannot take it — a refusal, not a fault.
            await swPost({
                type: 'llm-stream-frame',
                id: msg.id,
                kind: 'error',
                payload: 'the page that took this request no longer holds the LLM model (its load failed, or it was unloaded) — load it again',
                code: ENGINE_UNAVAILABLE,
            });
            return;
        }
        const body = JSON.parse(msg.body);
        const iterator = await _engine.chat.completions.create({
            messages: body.messages,
            tools: body.tools,
            stream: true,
        });
        for await (const chunk of iterator) {
            if (ac.signal.aborted) break;
            await swStreamFrame(msg.id, 'chunk', JSON.stringify(chunk));
        }
        await swStreamFrame(msg.id, 'done');
    } catch (e) {
        await swStreamFrame(msg.id, 'error', String(e));
    } finally {
        _activeStreams.delete(msg.id);
    }
}

function handleCancel(msg) {
    const ac = _activeStreams.get(msg.id);
    if (ac) ac.abort();
    // WebLLM's chat.completions iterator doesn't accept an AbortSignal, but
    // `interruptGenerate()` sets an internal flag the token loop checks —
    // this is the only way to actually stop GPU work mid-generation.
    if (_engine && typeof _engine.interruptGenerate === 'function') {
        _engine.interruptGenerate();
    }
}

// ---------------------------------------------------------------------------
// Page-direct load API (ESM export).
//
// gizza-ai (and any future page-side consumer) imports this to drive
// CreateMLCEngine in the window without going through the SW. Required because
// Chrome's FetchEvent.respondWith() lifetime cap (~5 min) kills the SW-routed
// load path on cold WebLLM downloads.
//
// _engine / _engineModel are the same module-scoped state read by the SW chat
// path's handleChatStream below. ESM modules are singletons within a realm, so
// `import { loadEngine } from '/webllm-engine.js'` from another script that
// lives in the same window shares this state.
// ---------------------------------------------------------------------------
// The load in progress, if any: `{ modelId, promise }`. The probe answer lists
// its model in `loading`, so bridge.js sends this page every request for that
// model while it loads; a chat or an unload waits for it here instead of
// answering "no engine loaded", and a second `loadEngine` of the same model
// shares it instead of loading the model twice.
let _loading = null;

/** Wait out a load in progress, whatever its outcome. */
async function settledLoad() {
    while (_loading) await _loading.promise.catch(() => {});
}

export async function loadEngine(modelId, onProgress) {
    // One load at a time: a load of another model finishes (or fails) first.
    while (_loading && _loading.modelId !== modelId) {
        await _loading.promise.catch(() => {});
    }
    if (_loading) return _loading.promise;
    if (_engineModel === modelId && _engine) {
        return; // already loaded
    }
    const promise = createEngine(modelId, onProgress);
    _loading = { modelId, promise };
    try {
        await promise;
    } finally {
        if (_loading?.promise === promise) _loading = null;
    }
}

async function createEngine(modelId, onProgress) {
    if (_engine) {
        try { await _engine.unload(); } catch (_e) {}
        _engine = null;
        _engineModel = null;
    }
    const CreateMLCEngine = await loadCreateMLCEngine();
    _engine = await CreateMLCEngine(modelId, {
        initProgressCallback: (report) => {
            if (typeof onProgress === 'function') {
                onProgress(String(report?.text ?? ''));
            }
        },
    });
    _engineModel = modelId;
}

// Page-direct unload — releases GPU memory but leaves IndexedDB-cached
// weights intact. Used by the picker's "Download" action: load → unload
// caches the model without keeping it as the active engine.
export async function unloadEngine() {
    await settledLoad();
    if (!_engine) return;
    try { await _engine.unload(); } catch (_e) {}
    _engine = null;
    _engineModel = null;
}

navigator.serviceWorker.addEventListener('message', (event) => {
    const msg = event.data;
    if (!msg || !msg.type) return;
    switch (msg.type) {
        case 'engine-probe':
            if (msg.family === 'llm') {
                swPost({
                    type: 'engine-present',
                    id: msg.id,
                    loaded: _engine && _engineModel ? [_engineModel] : [],
                    loading: _loading ? [_loading.modelId] : [],
                });
            }
            break;
        case 'llm-unload-request':      handleUnload(msg); break;
        case 'llm-chat-stream-request': handleChatStream(msg); break;
        case 'llm-stream-cancel':       handleCancel(msg); break;
    }
});

console.log('webllm-engine.js loaded');
