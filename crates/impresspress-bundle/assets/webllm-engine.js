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
// SW → Page message shapes (see bridge.js for the consuming side):
//   { type: 'llm-unload-response', id, error? }            // one-shot
//   { type: 'llm-stream-frame',    id, kind, payload? }    // streams
//     `kind` ∈ {'chunk','done','error'}; chat emits 'chunk' frames per token
//     and a terminal 'done' / 'error'.
//
// The @mlc-ai/web-llm import is lazy: a top-level static import would block
// DOMContentLoaded for every page that loads this script (it's a multi-MB
// jsdelivr ESM bundle), and most page loads never end up invoking the LLM.
// Defer the import until a handler actually needs it.

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
    if (!_engine) {
        await swStreamFrame(msg.id, 'error', 'no engine loaded');
        return;
    }
    const ac = new AbortController();
    _activeStreams.set(msg.id, ac);
    try {
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
// load path on cold WebLLM downloads — see the handoff at
// docs/superpowers/handoffs/2026-05-07-gizza-ai-model-load-page-direct-handoff.md.
//
// _engine / _engineModel are the same module-scoped state read by the SW chat
// path's handleChatStream below. ESM modules are singletons within a realm, so
// `import { loadEngine } from '/webllm-engine.js'` from another script that
// lives in the same window shares this state.
// ---------------------------------------------------------------------------
export async function loadEngine(modelId, onProgress) {
    if (_engineModel === modelId && _engine) {
        return; // already loaded
    }
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
    if (!_engine) return;
    try { await _engine.unload(); } catch (_e) {}
    _engine = null;
    _engineModel = null;
}

navigator.serviceWorker.addEventListener('message', (event) => {
    const msg = event.data;
    if (!msg || !msg.type) return;
    switch (msg.type) {
        case 'llm-unload-request':      handleUnload(msg); break;
        case 'llm-chat-stream-request': handleChatStream(msg); break;
        case 'llm-stream-cancel':       handleCancel(msg); break;
    }
});

console.log('webllm-engine.js loaded');
