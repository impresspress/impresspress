// Stand-in for the model libraries the page engines import from
// cdn.jsdelivr.net (Transformers.js, WebLLM), for `engine_probe.test.mjs`.
//
// Every model load waits on `globalThis.__modelGate` (the test opens it), so a
// test can act while a load is in progress, and every call that would do
// model work is recorded in `globalThis.__modelCalls`, so a test can assert it
// did — or did not — happen.

const record = (call) => globalThis.__modelCalls.push(call);
const gate = () => globalThis.__modelGate;

// Transformers.js — the embedding engine's entry point.
export async function pipeline(task, model) {
    record(`pipeline:${model}`);
    await gate();
    return async () => ({ tolist: () => [] });
}

// Transformers.js — the image engine's (Janus-Pro) classes.
export class BaseStreamer {}
export const AutoProcessor = {
    async from_pretrained(model) {
        record(`processor:${model}`);
        await gate();
        const processor = async () => ({});
        processor.num_image_tokens = 1;
        return processor;
    },
};
export const MultiModalityCausalLM = {
    async from_pretrained(model) {
        record(`model:${model}`);
        await gate();
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
    record(`engine:${model}`);
    await gate();
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
