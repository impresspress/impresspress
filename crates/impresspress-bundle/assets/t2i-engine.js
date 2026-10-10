// t2i-engine.js — page-side text-to-image engine.
//
// Runs in the window (WebGPU is window-only). Driven by SW postMessages from
// bridge.js's `image*` family, OR called page-direct via ESM exports (mirrors
// webllm-engine.js).
//
// Engine: Janus-Pro-1B (DeepSeek's unified multimodal model). HF's official
// blessed path for in-browser T2I via transformers.js — there is no
// `pipeline('text-to-image')` abstraction in transformers.js (the
// `pipeline('text-to-image', ...)` call throws "Unsupported pipeline"). Janus
// is autoregressive (token-stream → 384×384 image) rather than a diffusion
// pipeline. Reference impl:
// github.com/huggingface/transformers.js-examples/tree/main/janus-pro-webgpu
//
// SW → Page request shapes (see bridge.js for the producing side):
//   { type: 'engine-probe',                id, family }   // answered when family is 'image'
//   { type: 'image-load-request',          id, modelId }
//   { type: 'image-unload-request',        id }
//   { type: 'image-generate-stream-request', id, body }   // body = JSON ImageRequest
//   { type: 'image-stream-cancel',         id }
//
// Page → SW reply shapes:
//   { type: 'engine-present',        id, loaded, loading }    // runs the image engine;
//                                     // loaded / loading = [modelId] or []
//   { type: 'image-load-response',   id, error?, code? }
//   { type: 'image-unload-response', id, error? }
//   { type: 'image-stream-frame',    id, kind, payload?, code? }
//     `kind` ∈ {'progress','done','error'}. Progress frames carry
//     `{ stage, count?, total? }` during autoregressive token generation.
//
// A load reply or an error frame whose `code` is ENGINE_UNAVAILABLE is a
// refusal, as bridge.js's own are: the engine could not load here (no WebGPU,
// its library or runtime failed to load, the model failed to load), and the
// message is a fixed one written for whoever made the request (see
// `engineUnavailable`). bridge.js passes it on as a
// refusal, which the image service reports as `EngineUnavailable` with this
// message; any other error is a fault in the generation itself.

/** bridge.js's `ENGINE_UNAVAILABLE`: the `code` of a refusal. */
const ENGINE_UNAVAILABLE = 'engine-unavailable';

let _transformers = null;
async function loadTransformers() {
    if (_transformers) return _transformers;
    // Janus-Pro requires MultiModalityCausalLM, which was added in 3.7.x.
    // Pin to 3.7.1 (the version HF's official janus-pro-webgpu example uses),
    // the same module embed-engine.js imports: one copy per page.
    const transformers = await import('https://cdn.jsdelivr.net/npm/@huggingface/transformers@3.7.1');
    // One ONNX Runtime thread, as embed-engine.js's `importTransformers` sets
    // and explains: with more, a cross-origin-isolated page (every page of a
    // dev-sandbox deployment) has ONNX Runtime import its wasm glue from a
    // `blob:` URL, which the pages' `script-src` refuses, and no backend loads
    // — not even for the WebGPU sessions, which run on the same wasm runtime.
    // Set before the first model is created, which is when it is read.
    transformers.env.backends.onnx.wasm.numThreads = 1;
    _transformers = transformers;
    return _transformers;
}

/**
 * `e` as a refusal: the image engine could not load in this page.
 *
 * The message is fixed, not the library's or the browser's own error, which
 * can name hosts, file URLs and `blob:` URLs: `EngineUnavailable`'s message
 * reaches the caller as written, so it must carry none (wafer-core's contract
 * for the variant). The cause is logged to this page's console and kept as
 * the error's `cause` for a page-direct caller of `loadEngine`.
 *
 * No WebGPU keeps WEBGPU_UNAVAILABLE_MARKER in its message, which callers
 * match on, and does not say "reload": reloading gives the page no adapter.
 * Anything else does say it, because retrying in the same page cannot help
 * when the failure was ONNX Runtime's backend: onnxruntime-web records a
 * backend whose `init` failed as aborted and answers every later session with
 * the same error (`tryResolveAndInitializeBackend`), for as long as the page
 * keeps the module.
 */
function engineUnavailable(e) {
    console.error('t2i-engine.js: the image engine could not load:', e);
    const message = String(e?.message ?? e).startsWith(WEBGPU_UNAVAILABLE_MARKER)
        ? `${WEBGPU_UNAVAILABLE_MARKER}: this browser exposes no WebGPU adapter, which the image engine needs`
        : 'the image engine could not load in this page (its library, runtime or model failed to load) — reload the page and try again';
    return Object.assign(new Error(message, { cause: e }), { code: ENGINE_UNAVAILABLE });
}

// Recognizable marker callers (e.g. gizza-ai/imagine) can match on to
// surface a friendly "WebGPU is missing" message. Keep stable; if you
// rename it, update the consumer in gizza-ai/blocks/imagine/src/lib.rs.
const WEBGPU_UNAVAILABLE_MARKER = 'webgpu-unavailable';

// Throws an Error whose message starts with WEBGPU_UNAVAILABLE_MARKER when
// the browser exposes no WebGPU adapter at all. The model layers all run
// on device `webgpu`, so without an adapter loading would fail later
// inside transformers.js with an opaque message. Failing upfront with a
// stable marker lets consumers render a clear bubble.
async function requireWebGpuAdapter() {
    if (!navigator.gpu) {
        throw new Error(`${WEBGPU_UNAVAILABLE_MARKER}: navigator.gpu is undefined`);
    }
    let adapter;
    try {
        adapter = await navigator.gpu.requestAdapter();
    } catch (e) {
        throw new Error(`${WEBGPU_UNAVAILABLE_MARKER}: requestAdapter threw: ${e?.message ?? e}`);
    }
    if (!adapter) {
        throw new Error(`${WEBGPU_UNAVAILABLE_MARKER}: no WebGPU adapter available`);
    }
    return adapter;
}

// shader-f16 is preferred (smaller / faster) but not required — the dtype
// table below has an fp32 fallback for adapters without it.
let _fp16Supported = null;
async function detectFp16(adapter) {
    if (_fp16Supported !== null) return _fp16Supported;
    _fp16Supported = !!adapter?.features?.has('shader-f16');
    return _fp16Supported;
}

let _processor = null;
let _model = null;
let _modelId = null;
const _activeStreams = new Map(); // id -> AbortController

async function swPost(payload) {
    const reg = await navigator.serviceWorker.ready;
    reg.active?.postMessage(payload);
}

async function swStreamFrame(id, kind, payload) {
    await swPost({ type: 'image-stream-frame', id, kind, payload });
}

// The load in progress, if any: `{ modelId, promise }`. The probe answer lists
// its model in `loading`, so bridge.js sends this page every request for that
// model while it loads; each waits for it here instead of failing ("model not
// loaded") or loading the model a second time.
let _loading = null;

async function ensureLoaded(modelId, onProgress) {
    // One load at a time: a load of another model finishes (or fails) first.
    while (_loading && _loading.modelId !== modelId) {
        await _loading.promise.catch(() => {});
    }
    if (_loading) return _loading.promise;
    if (_processor && _model && _modelId === modelId) return;
    const promise = loadModel(modelId, onProgress);
    _loading = { modelId, promise };
    try {
        await promise;
    } finally {
        if (_loading?.promise === promise) _loading = null;
    }
}

/** Wait out a load in progress, whatever its outcome. */
async function settledLoad() {
    while (_loading) await _loading.promise.catch(() => {});
}

async function loadModel(modelId, onProgress) {
    if (_model) {
        try { await _model.dispose?.(); } catch (_e) {}
        _model = null;
        _processor = null;
        _modelId = null;
    }
    let adapter;
    let AutoProcessor;
    let MultiModalityCausalLM;
    try {
        adapter = await requireWebGpuAdapter();
        ({ AutoProcessor, MultiModalityCausalLM } = await loadTransformers());
    } catch (e) {
        throw engineUnavailable(e);
    }
    const fp16 = await detectFp16(adapter);
    const dtype = fp16
        ? {
              prepare_inputs_embeds: 'q4',
              language_model: 'q4f16',
              lm_head: 'fp16',
              gen_head: 'fp16',
              gen_img_embeds: 'fp16',
              image_decode: 'fp32',
          }
        : {
              prepare_inputs_embeds: 'fp32',
              language_model: 'q4',
              lm_head: 'fp32',
              gen_head: 'fp32',
              gen_img_embeds: 'fp32',
              image_decode: 'fp32',
          };
    const device = {
        // `prepare_inputs_embeds` runs on wasm in HF's reference example —
        // there's an open WebGPU bug for that subgraph. Match their choice.
        prepare_inputs_embeds: 'wasm',
        language_model: 'webgpu',
        lm_head: 'webgpu',
        gen_head: 'webgpu',
        gen_img_embeds: 'webgpu',
        image_decode: 'webgpu',
    };
    const progress_callback = onProgress
        ? (report) => {
              onProgress(String(report?.status ?? report?.file ?? ''));
          }
        : undefined;
    try {
        [_processor, _model] = await Promise.all([
            AutoProcessor.from_pretrained(modelId, { progress_callback }),
            MultiModalityCausalLM.from_pretrained(modelId, { dtype, device, progress_callback }),
        ]);
    } catch (e) {
        throw engineUnavailable(e);
    }
    _modelId = modelId;
}

async function handleLoadEngine(msg) {
    try {
        await ensureLoaded(msg.modelId);
        await swPost({ type: 'image-load-response', id: msg.id });
    } catch (e) {
        await swPost({
            type: 'image-load-response',
            id: msg.id,
            error: String(e?.message ?? e),
            ...(e?.code === ENGINE_UNAVAILABLE ? { code: ENGINE_UNAVAILABLE } : {}),
        });
    }
}

async function handleUnloadEngine(msg) {
    try {
        await settledLoad();
        if (_model) {
            try { await _model.dispose?.(); } catch (_e) {}
        }
        _processor = null;
        _model = null;
        _modelId = null;
        await swPost({ type: 'image-unload-response', id: msg.id });
    } catch (e) {
        await swPost({ type: 'image-unload-response', id: msg.id, error: String(e) });
    }
}

// Internal helper used by both SW-routed and ESM-direct generate paths.
async function generateOnce(prompt, { onProgress, signal } = {}) {
    if (!_processor || !_model) {
        throw new Error('model not loaded; call loadEngine first');
    }
    const { BaseStreamer } = await loadTransformers();
    const conversation = [{ role: '<|User|>', content: prompt }];
    const inputs = await _processor(conversation, { chat_template: 'text_to_image' });

    const num_image_tokens = _processor.num_image_tokens;
    class ProgressStreamer extends BaseStreamer {
        constructor() { super(); this.count = null; this.start = null; }
        put(_value) {
            if (this.count === null) { this.count = 0; this.start = performance.now(); return; }
            this.count++;
            onProgress?.({
                stage: 'generate',
                count: this.count,
                total: num_image_tokens,
                progress: this.count / num_image_tokens,
                time_ms: performance.now() - this.start,
            });
        }
        end() {}
    }
    const streamer = new ProgressStreamer();

    const outputs = await _model.generate_images({
        ...inputs,
        min_new_tokens: num_image_tokens,
        max_new_tokens: num_image_tokens,
        do_sample: true,
        streamer,
    });
    if (signal?.aborted) throw new Error('cancelled');
    const blob = await outputs[0].toBlob();
    return new Uint8Array(await blob.arrayBuffer());
}

async function handleGenerateStream(msg) {
    // Registered before the wait for a load in progress, so a cancel that
    // arrives during the load is honoured: the generation never starts.
    const ac = new AbortController();
    _activeStreams.set(msg.id, ac);
    try {
        await settledLoad();
        if (ac.signal.aborted) {
            await swStreamFrame(msg.id, 'error', 'cancelled');
            return;
        }
        if (!_processor || !_model) {
            // Sent here because this page held the model, or was loading it,
            // when it was probed: the load has since failed, or the model was
            // unloaded. The engine cannot take it — a refusal, not a fault.
            await swPost({
                type: 'image-stream-frame',
                id: msg.id,
                kind: 'error',
                payload: 'the page that took this request no longer holds the image model (its load failed, or it was unloaded) — load it again',
                code: ENGINE_UNAVAILABLE,
            });
            return;
        }
        const req = JSON.parse(msg.body);
        const pngBytes = await generateOnce(req.prompt, {
            signal: ac.signal,
            onProgress: (p) => { swStreamFrame(msg.id, 'progress', p); },
        });
        if (ac.signal.aborted) {
            await swStreamFrame(msg.id, 'error', 'cancelled');
            return;
        }
        const data = uint8ToBase64(pngBytes);
        await swStreamFrame(msg.id, 'done', { data, mime_type: 'image/png' });
    } catch (e) {
        await swStreamFrame(msg.id, 'error', String(e?.message ?? e));
    } finally {
        _activeStreams.delete(msg.id);
    }
}

function handleCancel(msg) {
    const ac = _activeStreams.get(msg.id);
    if (ac) ac.abort();
}

function uint8ToBase64(u8) {
    let binary = '';
    const chunk = 0x8000;
    for (let i = 0; i < u8.length; i += chunk) {
        binary += String.fromCharCode.apply(null, u8.subarray(i, i + chunk));
    }
    return btoa(binary);
}

navigator.serviceWorker.addEventListener('message', (event) => {
    const msg = event.data;
    if (!msg || !msg.type) return;
    switch (msg.type) {
        case 'engine-probe':
            if (msg.family === 'image') {
                swPost({
                    type: 'engine-present',
                    id: msg.id,
                    loaded: _model && _modelId ? [_modelId] : [],
                    loading: _loading ? [_loading.modelId] : [],
                });
            }
            break;
        case 'image-load-request':             handleLoadEngine(msg); break;
        case 'image-unload-request':           handleUnloadEngine(msg); break;
        case 'image-generate-stream-request':  handleGenerateStream(msg); break;
        case 'image-stream-cancel':            handleCancel(msg); break;
    }
});

// ---------------------------------------------------------------------------
// Page-direct API (ESM exports).
//
// Mirrors `webllm-engine.js::{loadEngine,unloadEngine}`. Page-side consumers
// (e.g. the gizza-ai image composer) import these to drive Janus-Pro directly
// in the window without bouncing through the service-worker bridge. Required
// because Chrome's FetchEvent.respondWith() lifetime cap kills SW-routed
// model downloads on cold loads (Janus-Pro is ~700 MB - 1.5 GB depending on
// quantization).
// ---------------------------------------------------------------------------
export async function loadEngine(modelId, onProgress) {
    await ensureLoaded(modelId, onProgress);
}

export async function unloadEngine() {
    await settledLoad();
    if (_model) {
        try { await _model.dispose?.(); } catch (_e) {}
    }
    _processor = null;
    _model = null;
    _modelId = null;
}

// Page-direct one-shot generate. Returns the PNG bytes as a Uint8Array.
// Same engine state (_processor / _model) as the SW-routed path.
export async function generateImage(prompt, opts = {}) {
    if (!_processor || !_model) {
        throw new Error('model not loaded; call loadEngine first');
    }
    return await generateOnce(prompt, opts);
}

console.log('t2i-engine.js loaded (Janus-Pro)');
