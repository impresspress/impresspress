// The `CONFIG_CACHE` binding the fixture Worker sees: Miniflare's real local
// KV namespace (`env.INNER`), with every call counted by the control Worker
// (`d1-control.mjs`, over the `CONTROL` service binding) before it goes
// through. worker-rs reads a KV binding's `get`/`getWithMetadata`/`put`/
// `list`/`delete` and calls them with the binding as `this`, so a plain object
// with those five methods is what it needs.
//
// The ledger is how a scenario sees the runtime's KV config-stamp probes —
// for example, that an isolate whose prepared plan is bypassed stops probing
// on every request.

export default function (env) {
  const inner = env.INNER;
  const note = async (op, key) => {
    await env.CONTROL.fetch("http://d1-control/kv", {
      method: "POST",
      body: JSON.stringify({ op, key: typeof key === "string" ? key : null }),
    });
  };
  return {
    async get(key, options) {
      await note("get", key);
      return inner.get(key, options);
    },
    async getWithMetadata(key, options) {
      await note("getWithMetadata", key);
      return inner.getWithMetadata(key, options);
    },
    async put(key, value, options) {
      await note("put", key);
      return inner.put(key, value, options);
    },
    async list(options) {
      await note("list", null);
      return inner.list(options);
    },
    async delete(key) {
      await note("delete", key);
      return inner.delete(key);
    },
  };
}
