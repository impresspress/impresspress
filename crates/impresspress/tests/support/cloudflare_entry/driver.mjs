// Serves four requests through a Worker's default export the way workerd
// does (a new entrypoint instance per request, then `fetch`), and prints
// everything that happened as a JSON array of lines. In `on` and `off` mode
// the second request traps in `fetch`; in `probe-trap` mode (traced) it is
// the executor probe that traps on that request instead.
//
// usage: node driver.mjs <module> <on|off|probe-trap>
const [modulePath, mode] = process.argv.slice(2);
const lines = [];
for (const level of ['log', 'error']) {
  console[level] = (...args) => lines.push(args.map(String).join(' '));
}
// The shim registers a global `error` listener; Node has no such global.
globalThis.addEventListener = () => {};

const { default: Worker } = await import(modulePath);
const env = mode === 'off' ? {} : { IMPRESSPRESS_EXECUTOR_TRACE: '1' };
if (mode === 'probe-trap') globalThis.__stubProbeTrapsOnCall = 2;
const second = mode === 'probe-trap' ? '/b' : '/trap';
for (const path of ['/a', second, '/c', '/d']) {
  const request = { url: `https://example.test${path}` };
  const ctx = { env };
  try {
    const response = await new Worker(ctx, env).fetch(request);
    lines.push(`response ${response.status} ${response.url} sameEnv=${response.sameEnv}`);
  } catch (error) {
    lines.push(`threw ${error.name}`);
  }
  const state = globalThis.__impresspressExecutorTrace;
  lines.push(`state ${state === undefined ? 'absent' : `seq=${state.requests.get(request)}`}`);
}
process.stdout.write(JSON.stringify(lines));
