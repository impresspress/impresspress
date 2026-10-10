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
//   { type: 'engine-present', id, loaded }        // runs the embedding engine; loaded = model ids with a pipeline
//   { type: 'embed-<op>-response', id, result?, error? }

const PIPELINES = new Map();

const MODEL_HF_PATH = {
    'multilingual-e5-small':                 'Xenova/multilingual-e5-small',
    'paraphrase-multilingual-MiniLM-L12-v2': 'Xenova/paraphrase-multilingual-MiniLM-L12-v2',
};

async function loadPipeline(modelId) {
    if (PIPELINES.has(modelId)) return PIPELINES.get(modelId);
    const hf = MODEL_HF_PATH[modelId];
    if (!hf) throw new Error(`unknown embedding model: ${modelId}`);
    // The same module t2i-engine.js imports (one copy per page), and from
    // cdn.jsdelivr.net: that is the CDN the runtime's pages allow scripts
    // from (`script-src` in impresspress-web's `IMPRESSPRESS_CSP`), so a
    // module served from anywhere else is blocked on every page but the boot
    // shell.
    const { pipeline } = await import('https://cdn.jsdelivr.net/npm/@huggingface/transformers@3.7.1');
    const pipe = await pipeline('feature-extraction', hf, { dtype: 'q8' });
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
            swReply({ type: 'engine-present', id: msg.id, loaded: [...PIPELINES.keys()] });
        }
        return;
    }
    if (!msg.type.startsWith('embed-')) return;

    const reply = (result, error) => {
        swReply({
            type: msg.type.replace('-request', '-response'),
            id: msg.id,
            ...(error ? { error: String(error.message ?? error) } : { result }),
        });
    };

    try {
        if (msg.type === 'embed-create-request') {
            await loadPipeline(msg.modelId);
            reply('ok');
        } else if (msg.type === 'embed-unload-request') {
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
