// Stands in for the wasm-bindgen module worker-build's shim imports as
// `./index.js`, emulating what the Rust side does with the shared state:
// one "linear memory" per instance, replaced by `__wbg_reset_state`, and
// `__impresspressExecutorProbe` drawing the instance's identity on its first
// traced call (`impresspress_cloudflare::executor_trace::probe`).
let instances = 0;
let memory = freshMemory();

function freshMemory() {
  return { id: null, generation: null };
}

export function __wbg_reset_state() {
  memory = freshMemory();
}

// The driver's `probe-trap` mode sets `__stubProbeTrapsOnCall` to make the
// probe itself trap on that call, as it would on a broken instance (an
// exhausted shadow stack).
let probeCalls = 0;

export function __impresspressExecutorProbe() {
  probeCalls += 1;
  if (probeCalls === globalThis.__stubProbeTrapsOnCall) {
    throw new WebAssembly.RuntimeError('memory access out of bounds');
  }
  const state = globalThis.__impresspressExecutorTrace;
  if (state === undefined) return undefined;
  if (memory.id === null) {
    state.wasmGeneration = (state.wasmGeneration ?? 0) + 1;
    memory.generation = state.wasmGeneration;
    instances += 1;
    memory.id = `wasm-${instances}`;
  }
  return { wasm: memory.id, generation: memory.generation, stack: 1024 };
}

// A request whose URL ends in `/trap` traps synchronously, as a stranded
// RefCell borrow does under panic=abort.
export function fetch(request, env, ctx) {
  if (request.url.endsWith('/trap')) {
    throw new WebAssembly.RuntimeError('unreachable');
  }
  return Promise.resolve({ status: 200, url: request.url, sameEnv: env === ctx.env });
}
