// Stand-in WebMCP registries for the Node unit tests of `webmcp.js` and
// `dev.js`, one per shape a real browser provides. The scripts are handed
// these where the browser would put its own — `navigator.modelContext`,
// `document.modelContext`, or both — so a test can see which registry each
// tool reached and how each one let it go. (The e2e suite does not use them:
// it runs against Chrome's real registry.)

/// Chrome's `navigator.modelContext` (146/147): `registerTool` returns
/// nothing and throws on a duplicate name; it ignores the `signal`, so a tool
/// leaves only through `unregisterTool(name)`.
export function chromeRegistry() {
  const tools = new Map();
  return {
    tools,
    registerTool(options) {
      if (tools.has(options.name)) {
        throw new DOMException(`Duplicate tool name: ${options.name}`, 'InvalidStateError');
      }
      tools.set(options.name, options);
    },
    unregisterTool(name) {
      if (!tools.has(name)) {
        throw new DOMException(`No tool named ${name}`, 'InvalidStateError');
      }
      tools.delete(name);
    }
  };
}

/// The OpenAI Codex / ChatGPT in-app browser's `document.modelContext`, as
/// its preload defines it: `registerTool` is async — it records the tool at
/// once and resolves on a later task, and a refusal (a duplicate name, an
/// already-aborted signal) is a REJECTED promise, never a throw. There is no
/// `unregisterTool`: aborting the registration's `signal` is the only way a
/// tool leaves.
export function codexRegistry() {
  const tools = new Map();
  return {
    tools,
    async registerTool(options, { signal } = {}) {
      if (tools.has(options.name)) {
        throw new DOMException(
          `WebMCP tool ${JSON.stringify(options.name)} is already registered.`,
          'InvalidStateError'
        );
      }
      if (signal?.aborted) {
        throw signal.reason;
      }
      tools.set(options.name, options);
      signal?.addEventListener(
        'abort',
        () => {
          if (tools.get(options.name) === options) {
            tools.delete(options.name);
          }
        },
        { once: true }
      );
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  };
}
