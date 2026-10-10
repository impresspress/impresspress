// Stand-in for the model libraries the page engines import from
// cdn.jsdelivr.net (Transformers.js, WebLLM), for `engine_probe.test.mjs`.
//
// Every model load waits on `globalThis.__modelGate` (the test opens it), so a
// test can act while a load is in progress, and every call that would do
// model work is recorded in `globalThis.__modelCalls`, so a test can assert it
// did — or did not — happen. A load fails with `globalThis.__modelLoadError`
// when a test sets one, as a model, a library or its runtime failing to load
// does.
//
// Transformers.js's ONNX Runtime settings are `env` below, as the real module
// exports them; each Transformers.js model load records the `numThreads` it
// found set in `globalThis.__onnxThreadsAtLoad`, since ONNX Runtime reads it
// once, when the first model's session is created.

const record = (call) => globalThis.__modelCalls.push(call);
const gate = () => globalThis.__modelGate;
async function load(call) {
    record(call);
    await gate();
    if (globalThis.__modelLoadError) throw new Error(globalThis.__modelLoadError);
}

// Transformers.js — its runtime settings (`env.backends.onnx` is ONNX
// Runtime's own `env`).
export const env = { backends: { onnx: { wasm: {} } } };
const onnxLoad = (call) => {
    globalThis.__onnxThreadsAtLoad?.push(env.backends.onnx.wasm.numThreads);
    return load(call);
};

// Transformers.js — the embedding engine's entry point.
export async function pipeline(task, model) {
    await onnxLoad(`pipeline:${model}`);
    return async () => ({ tolist: () => [] });
}

// Transformers.js — the image engine's (Janus-Pro) classes.
export class BaseStreamer {}
export const AutoProcessor = {
    async from_pretrained(model) {
        await load(`processor:${model}`);
        const processor = async () => ({});
        processor.num_image_tokens = 1;
        return processor;
    },
};
export const MultiModalityCausalLM = {
    async from_pretrained(model) {
        await onnxLoad(`model:${model}`);
        return {
            async generate_images() {
                record('generate');
                return [{ toBlob: async () => new Blob([new Uint8Array([1])]) }];
            },
            async dispose() {},
        };
    },
};

// WebLLM — the LLM engine's entry point.
export async function CreateMLCEngine(model) {
    await load(`engine:${model}`);
    return {
        chat: {
            completions: {
                async create() {
                    record('chat');
                    return (async function* () {})();
                },
            },
        },
        interruptGenerate() {},
        async unload() {},
    };
}
