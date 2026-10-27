// Stands in for the wasm-bindgen module worker-build's shim imports as
// `./index.js`, emulating what the Rust side does with the shared state:
// one "linear memory" per instance, replaced by `__wbg_reset_state`, and
// `__impresspressExecutorProbe` drawing the instance's identity on its first
// traced call and reporting worker 0.8's `__worker_init_state.instanceId`
// (`impresspress_cloudflare::executor_trace::probe`).
//
// worker 0.8's `init.rs` keeps that state in one object per isolate, created
// by its `inline_js` snippet when the module is evaluated and returned by its
// `__worker_init_state` export; its start function, run at instantiation and
// again by every `__wbg_reset_state`, adds 1 to `instanceId`. The shim reads
// the object once at top level and adds its own 1 on a reset it runs.
const initState = (globalThis.__worker_init_state = { criticalError: false, instanceId: 0 });
let instances = 0;
let memory = start();

function start() {
  initState.instanceId += 1;
  return { id: null, generation: null };
}

export function __worker_init_state() {
  return initState;
}

export function __wbg_reset_state() {
  memory = start();
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
  const answer = { wasm: memory.id, generation: memory.generation, stack: 1024 };
  // worker 0.8's instance counter, read as the Rust probe reads it.
  const wbi = globalThis.__worker_init_state?.instanceId;
  if (Number.isInteger(wbi) && wbi >= 0) answer.wbi = wbi;
  return answer;
}

// A request whose URL ends in `/trap` traps synchronously, as a stranded
// RefCell borrow does under panic=abort.
export function fetch(request, env, ctx) {
  if (request.url.endsWith('/trap')) {
    throw new WebAssembly.RuntimeError('unreachable');
  }
  return Promise.resolve({ status: 200, url: request.url, sameEnv: env === ctx.env });
}
