// Page-resident Transformers.js host, loaded with webllm-engine.js and
// t2i-engine.js on the boot page and on the pages the runtime renders.
// Listens for `embed-*-request` messages from the SW and runs them through
// `@huggingface/transformers` v3.
//
// SW → Page request shapes (see bridge.js for the producing side):
//   { type: 'engine-probe',          id, family }   // answered when family is 'embed'
//   { type: 'embed-create-request',  id, modelId }
//   { type: 'embed-unload-request',  id, modelId }
//   { type: 'embed-run-request',     id, modelId, texts }   // texts = JSON array of strings
//
// Page → SW reply shapes:
//   { type: 'engine-present', id, loaded, loading }  // runs the embedding engine; model ids with a
//                                                    // pipeline / with one being created
//   { type: 'embed-<op>-response', id, result?, error?, code? }
//
// A reply whose `code` is ENGINE_UNAVAILABLE is a refusal, as bridge.js's own
// are: the engine could not load here (its library, its runtime or the
// model), and `error` is UNABLE_TO_LOAD, written for whoever made the request.
// bridge.js passes it on as a refusal, which the embedding service reports as
// `EngineUnavailable` and the vector block's routes answer as a 503 carrying
// this message; any other `error` is a fault in the run itself.

/** bridge.js's `ENGINE_UNAVAILABLE`: the `code` of a refusal. */
const ENGINE_UNAVAILABLE = 'engine-unavailable';

/**
 * What a caller is told when the engine could not load here. Fixed, not the
 * library's own error: that names hosts, file URLs and `blob:` URLs, and
 * `EngineUnavailable`'s message reaches the caller as written, so it must
 * carry none (wafer-core's contract for the variant). The cause is logged to
 * this page's console instead.
 *
 * "Reload" because retrying in the same page cannot help when the failure was
 * ONNX Runtime's backend: onnxruntime-web records a backend whose `init`
 * failed as aborted and answers every later session with the same error
 * (`tryResolveAndInitializeBackend`), for as long as the page keeps the
 * module.
 */
const UNABLE_TO_LOAD =
    'the embedding engine could not load in this page (its library, runtime or model failed to load) — reload the page and try again';

const PIPELINES = new Map();
// Pipelines being created, by model id. The probe answer lists them in
// `loading`, so bridge.js sends this page every request for such a model;
// each shares the one creation instead of loading the model a second time.
const LOADING = new Map();

const MODEL_HF_PATH = {
    'multilingual-e5-small':                 'Xenova/multilingual-e5-small',
    'paraphrase-multilingual-MiniLM-L12-v2': 'Xenova/paraphrase-multilingual-MiniLM-L12-v2',
};

async function loadPipeline(modelId) {
    if (PIPELINES.has(modelId)) return PIPELINES.get(modelId);
    if (LOADING.has(modelId)) return LOADING.get(modelId);
    const promise = createPipeline(modelId);
    LOADING.set(modelId, promise);
    try {
        return await promise;
    } finally {
        LOADING.delete(modelId);
    }
}

/**
 * Transformers.js, with its ONNX Runtime set to run on the page's own thread.
 *
 * The same module t2i-engine.js imports (one copy per page), and from
 * cdn.jsdelivr.net: that is the CDN the runtime's pages allow scripts from
 * (`script-src` in impresspress-web's `IMPRESSPRESS_CSP`), so a module served
 * from anywhere else is blocked on every page but the boot shell.
 *
 * `numThreads = 1` is what keeps ONNX Runtime's own code on that CDN too. On
 * a cross-origin-isolated page — every page a dev-sandbox deployment serves,
 * which is isolated deployment-wide for the in-browser compiler
 * (`cross_origin_isolation` in impresspress-web's `runtime_factory.rs`) —
 * ONNX Runtime defaults to several threads, and its threads are workers
 * started from its wasm glue module (`ort-wasm-simd-threaded.jsep.mjs`).
 * A worker's script must be same-origin, so for a glue module on a CDN it
 * fetches the file and imports it from a `blob:` URL instead
 * (`importWasmModule` in onnxruntime-web: `needPreload` is "multi-threaded
 * and cross-origin"). The pages' `script-src` has no `blob:`, so that import
 * fails and no backend loads ("no available backend found"). With one thread
 * there is no worker to start, and the glue is imported from the CDN as it
 * is. One thread is also what every page that is NOT cross-origin isolated
 * already ran: without `SharedArrayBuffer` ONNX Runtime falls back to it.
 *
 * Set before the first model is created: ONNX Runtime reads it once, when its
 * wasm backend initializes, and both engines set it, as either may be the
 * first to create a model on the page.
 */
async function importTransformers() {
    const transformers = await import('https://cdn.jsdelivr.net/npm/@huggingface/transformers@3.7.1');
    transformers.env.backends.onnx.wasm.numThreads = 1;
    return transformers;
}

async function createPipeline(modelId) {
    const hf = MODEL_HF_PATH[modelId];
    if (!hf) throw new Error(`unknown embedding model: ${modelId}`);
    let pipe;
    try {
        const { pipeline } = await importTransformers();
        pipe = await pipeline('feature-extraction', hf, { dtype: 'q8' });
    } catch (e) {
        console.error(`embed-engine.js: ${modelId} could not load:`, e);
        throw Object.assign(new Error(UNABLE_TO_LOAD, { cause: e }), { code: ENGINE_UNAVAILABLE });
    }
    PIPELINES.set(modelId, pipe);
    return pipe;
}

async function swReply(payload) {
    const reg = await navigator.serviceWorker.ready;
    reg.active?.postMessage(payload);
}

navigator.serviceWorker.addEventListener('message', async (event) => {
    const msg = event.data;
    if (!msg || typeof msg.type !== 'string') return;
    if (msg.type === 'engine-probe') {
        if (msg.family === 'embed') {
            swReply({
                type: 'engine-present',
                id: msg.id,
                loaded: [...PIPELINES.keys()],
                loading: [...LOADING.keys()],
            });
        }
        return;
    }
    if (!msg.type.startsWith('embed-')) return;

    const reply = (result, error) => {
        swReply({
            type: msg.type.replace('-request', '-response'),
            id: msg.id,
            ...(error
                ? {
                      error: String(error.message ?? error),
                      ...(error.code === ENGINE_UNAVAILABLE ? { code: ENGINE_UNAVAILABLE } : {}),
                  }
                : { result }),
        });
    };

    try {
        if (msg.type === 'embed-create-request') {
            await loadPipeline(msg.modelId);
            reply('ok');
        } else if (msg.type === 'embed-unload-request') {
            await LOADING.get(msg.modelId)?.catch(() => {});
            PIPELINES.delete(msg.modelId);
            reply('ok');
        } else if (msg.type === 'embed-run-request') {
            const pipe = await loadPipeline(msg.modelId);
            const texts = JSON.parse(msg.texts);
            const out = await pipe(texts, { pooling: 'mean', normalize: true });
            // out.tolist() => [[...], [...]]; out.dims = [batch, dim]
            const vectors = out.tolist();
            const dims = vectors[0]?.length ?? 0;
            reply(JSON.stringify({ vectors, dims }));
        }
    } catch (e) {
        reply(null, e);
    }
});

console.log('embed-engine.js loaded');
