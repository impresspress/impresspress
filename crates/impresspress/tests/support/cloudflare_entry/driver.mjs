// Serves four requests through a Worker's default export the way workerd
// does (a new entrypoint instance per request, then `fetch`), the second one
// trapping, and prints everything that happened as a JSON array of lines.
//
// usage: node driver.mjs <module> <on|off>
const [modulePath, mode] = process.argv.slice(2);
const lines = [];
for (const level of ['log', 'error']) {
  console[level] = (...args) => lines.push(args.map(String).join(' '));
}
// The shim registers a global `error` listener; Node has no such global.
globalThis.addEventListener = () => {};

const { default: Worker } = await import(modulePath);
const env = mode === 'on' ? { IMPRESSPRESS_EXECUTOR_TRACE: '1' } : {};
for (const path of ['/a', '/trap', '/c', '/d']) {
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
