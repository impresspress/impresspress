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
//   { type: 'image-load-request',          id, modelId }
//   { type: 'image-unload-request',        id }
//   { type: 'image-generate-stream-request', id, body }   // body = JSON ImageRequest
//   { type: 'image-stream-cancel',         id }
//
// Page → SW reply shapes:
//   { type: 'image-load-response',   id, error? }
//   { type: 'image-unload-response', id, error? }
//   { type: 'image-stream-frame',    id, kind, payload? }
//     `kind` ∈ {'progress','done','error'}. Progress frames carry
//     `{ stage, count?, total? }` during autoregressive token generation.

let _transformers = null;
async function loadTransformers() {
    if (_transformers) return _transformers;
    // Janus-Pro requires MultiModalityCausalLM, which was added in 3.7.x.
    // Pin to 3.7.1 (the version HF's official janus-pro-webgpu example uses).
    _transformers = await import('https://cdn.jsdelivr.net/npm/@huggingface/transformers@3.7.1');
    return _transformers;
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

async function ensureLoaded(modelId, onProgress) {
    if (_processor && _model && _modelId === modelId) return;
    if (_model) {
        try { await _model.dispose?.(); } catch (_e) {}
        _model = null;
        _processor = null;
        _modelId = null;
    }
    const adapter = await requireWebGpuAdapter();
    const { AutoProcessor, MultiModalityCausalLM } = await loadTransformers();
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
    [_processor, _model] = await Promise.all([
        AutoProcessor.from_pretrained(modelId, { progress_callback }),
        MultiModalityCausalLM.from_pretrained(modelId, { dtype, device, progress_callback }),
    ]);
    _modelId = modelId;
}

async function handleLoadEngine(msg) {
    try {
        await ensureLoaded(msg.modelId);
        await swPost({ type: 'image-load-response', id: msg.id });
    } catch (e) {
        await swPost({ type: 'image-load-response', id: msg.id, error: String(e?.message ?? e) });
    }
}

async function handleUnloadEngine(msg) {
    try {
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
    if (!_processor || !_model) {
        await swStreamFrame(msg.id, 'error', 'model not loaded; call load_model first');
        return;
    }
    const ac = new AbortController();
    _activeStreams.set(msg.id, ac);
    try {
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
