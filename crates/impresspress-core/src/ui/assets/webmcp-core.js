// Fragment shared by webmcp.js and dev.js; `assets.rs` wraps it in the
// caller's IIFE. Never served on its own.

// ---- the browser's WebMCP registries ---------------------------------------

// Every WebMCP registry this browser itself provides, each once.
//
// Browsers do not put the registry in the same place. Chrome implements it
// as `navigator.modelContext` (secure contexts only). The OpenAI Codex /
// ChatGPT in-app browser defines it from its preload as
// `document.modelContext`, and nowhere else. A page that asked only one of
// the two would hand its tools to no agent in the other browser, so both are
// asked; a browser that puts the same object in both places is handed each
// tool once.
//
// Only registries the BROWSER provides. Nothing here defines one or aliases
// one into the other's place: a registry the page made is one no agent
// reads, and in a test it hides whether the browser's real one is used at
// all (the `document` polyfill #147 deleted did exactly that).
function webmcpRegistries() {
  var registries = [];
  [navigator.modelContext, document.modelContext].forEach(function (registry) {
    if (
      registry &&
      typeof registry.registerTool === 'function' &&
      registries.indexOf(registry) < 0
    ) {
      registries.push(registry);
    }
  });
  return registries;
}

// Hands tools to every registry `webmcpRegistries` found, and takes back
// exactly the ones it handed. Each registering script (`webmcp.js`, `dev.js`)
// has its own: both run on `/b/dev`, and each removes only what it added.
//
// The registries answer `registerTool(options, { signal })` differently, and
// both answers are read:
//
// - Chrome returns nothing and THROWS on a refusal (a duplicate name, a
//   schema it will not take). It ignores `signal` (146/147); a tool leaves
//   through `unregisterTool(name)`.
// - Codex's returns a PROMISE, rejected on a refusal (a duplicate name, an
//   insecure page, a page without an origin-keyed agent cluster). It has no
//   `unregisterTool`; a tool leaves when its `signal` is aborted.
//
// Both take the same tool shape — `name`, `description`, `inputSchema`,
// `execute(input)` returning a JSON-serialisable result — which is what
// `toolOptions` builds, so the options object is handed to each unchanged.
// Every tool is registered with this registrar's current signal, and
// `clear()` both aborts that signal and calls `unregisterTool` by name on
// every registry that has one, for the tools that registry accepted.
function webmcpRegistrar() {
  var registries = webmcpRegistries();
  var lifetime = new AbortController();
  // `{ registry, name }` for every tool a registry has accepted since the
  // last `clear()`. Only accepted ones: a name a registry refused may be
  // another script's tool, and unregistering it would remove that one.
  var accepted = [];

  function unregister(registry, name) {
    if (typeof registry.unregisterTool === 'function') {
      try {
        registry.unregisterTool(name);
      } catch (error) {
        // Already gone: a registry that honours the signal removed it when
        // `clear()` aborted it.
      }
    }
  }

  function accept(registry, name, signal) {
    if (signal.aborted) {
      // A promise-answering registry accepted the tool after the `clear()`
      // that should have removed it. Aborting removed it there already if the
      // registry honours the signal; this covers one that does not.
      unregister(registry, name);
      return;
    }
    accepted.push({ registry: registry, name: name });
  }

  // Registers `options` on every registry. Resolves once each has answered,
  // with whether at least one accepted it and every refusal's error. A
  // registry that answers synchronously is recorded synchronously, so the
  // tool is in the registry, and counted, before this returns.
  function register(options) {
    var signal = lifetime.signal;
    return Promise.all(
      registries.map(function (registry) {
        var answer;
        try {
          answer = registry.registerTool(options, { signal: signal });
        } catch (error) {
          return { refused: error };
        }
        if (answer && typeof answer.then === 'function') {
          return answer.then(
            function () {
              accept(registry, options.name, signal);
              return null;
            },
            function (error) {
              return { refused: error };
            }
          );
        }
        accept(registry, options.name, signal);
        return null;
      })
    ).then(function (answers) {
      var errors = [];
      answers.forEach(function (answer) {
        if (answer !== null) {
          errors.push(answer.refused);
        }
      });
      return { accepted: errors.length < registries.length, errors: errors };
    });
  }

  // Removes every tool this registrar handed out, from every registry.
  function clear() {
    var taken = accepted;
    var ending = lifetime;
    accepted = [];
    lifetime = new AbortController();
    ending.abort();
    taken.forEach(function (entry) {
      unregister(entry.registry, entry.name);
    });
  }

  // The names at least one registry holds, each once.
  function names() {
    var seen = [];
    accepted.forEach(function (entry) {
      if (seen.indexOf(entry.name) < 0) {
        seen.push(entry.name);
      }
    });
    return seen;
  }

  return {
    // Whether this browser provides any registry. It is what "this browser
    // has WebMCP" means everywhere a page says so.
    available: registries.length > 0,
    register: register,
    clear: clear,
    names: names
  };
}

// Substitute {name} path segments, and collect the rest into query or body
// according to the provenance the server recorded.
function buildRequest(invocation, args) {
  var path = invocation.path;
  (invocation.path_params || []).forEach(function (name) {
    // split/join, not String.replace: replace with a string pattern
    // substitutes only the FIRST match, so a template repeating a
    // placeholder (`/x/{id}/{id}`) would keep a literal `{id}` in the URL
    // and 404 forever. The producer dedups placeholder names before
    // comparing them against the declared path params, so such a template
    // passes its eligibility check and reaches us intact.
    path = path.split('{' + name + '}').join(encodeURIComponent(args[name]));
  });

  var query = new URLSearchParams();
  (invocation.query_params || []).forEach(function (name) {
    if (args[name] !== undefined && args[name] !== null) {
      query.append(name, args[name]);
    }
  });
  var qs = query.toString();
  if (qs) {
    path += '?' + qs;
  }

  var init = { method: invocation.method.toUpperCase(), headers: {} };

  var bodyNames = invocation.body_params || [];
  if (bodyNames.length > 0) {
    var body = {};
    bodyNames.forEach(function (name) {
      if (args[name] !== undefined) {
        body[name] = args[name];
      }
    });
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }

  // Same-origin, so the session cookie rides along and the server applies
  // the same authorization it would to any other request. The manifest
  // filter is a UX affordance; the endpoint is still the real gate.
  init.credentials = 'same-origin';

  return { url: path, init: init };
}

// Why a call cannot be made as the agent sent it, or `null` when it can.
//
// Chrome's WebMCP hands `execute` whatever arguments the agent sent; it does
// not check them against `inputSchema`. Unchecked, a missing path argument
// becomes the text `undefined` in the URL, and the agent is told the server's
// answer to that (`404 Product not found`) — a claim about the data, when the
// fault is in its own call. So the call is checked first, from what the tool
// itself declares and nothing else:
//
// - every name in `inputSchema.required` must be present. Present is the
//   JSON Schema meaning — the key is there with a value; whether that value
//   is acceptable (an empty string in a body, say) is the tool's to judge,
//   with its own message. This half needs only the schema, so it applies to
//   any tool, an HTTP one or one the page implements itself.
// - every name in `pathParams` must be a string, a finite number or a
//   boolean, whether or not `required` lists it: the URL segment has to be
//   filled, and those are the values that become one segment of text. An
//   empty string would leave the segment empty, and `null` or `undefined`
//   would be spelled into it as text, so each counts as missing; an object
//   or array would be stringified (`[object Object]`), and a NaN or an
//   infinity spelled as a word, so those are refused as invalid. Only an
//   HTTP tool has path arguments; any other tool passes `[]`.
function argumentError(inputSchema, pathParams, args) {
  var has = function (name) {
    return Object.prototype.hasOwnProperty.call(args, name);
  };
  var missing = [];
  var invalid = [];
  var required = inputSchema && Array.isArray(inputSchema.required) ? inputSchema.required : [];
  required.forEach(function (name) {
    if (!has(name) || args[name] === undefined) {
      missing.push(name);
    }
  });
  pathParams.forEach(function (name) {
    var value = has(name) ? args[name] : undefined;
    if (value === undefined || value === null || value === '') {
      if (missing.indexOf(name) < 0) {
        missing.push(name);
      }
    } else if (
      typeof value !== 'string' &&
      typeof value !== 'boolean' &&
      !(typeof value === 'number' && isFinite(value))
    ) {
      invalid.push(name);
    }
  });
  if (missing.length > 0) {
    return (missing.length === 1 ? 'Missing required argument: ' : 'Missing required arguments: ') +
      missing.join(', ');
  }
  if (invalid.length > 0) {
    return 'Invalid argument' + (invalid.length === 1 ? ': ' : 's: ') + invalid.join(', ') +
      ' must be a string, a number or a boolean';
  }
  return null;
}

// `execute`, refusing a call `argumentError` finds fault with before it
// runs. The refusal is an `isError` result, like a refused request, so the
// agent reads it as its own call failing and can correct the arguments.
function withArgumentCheck(inputSchema, pathParams, execute) {
  return async function (args) {
    args = args || {};
    var refusal = argumentError(inputSchema, pathParams, args);
    if (refusal !== null) {
      return { isError: true, content: [{ type: 'text', text: refusal }] };
    }
    return execute(args);
  };
}

function toolOptions(tool) {
  // `outputSchema` is optional in the manifest — the producer only
  // projects it when the endpoint's declared response schema is a
  // self-contained JSON object it can vouch for (see wafer-core's
  // `agent_output_schema`). A tool without one must still register
  // cleanly, so the key is only set on `options` when present rather than
  // passed through as `undefined`.
  var options = {
    name: tool.name,
    description: tool.description,
    inputSchema: tool.inputSchema,
    // Checked before any request is built: see `argumentError`.
    execute: withArgumentCheck(tool.inputSchema, tool.invocation.path_params || [], async function (args) {
      var req = buildRequest(tool.invocation, args);
      var response = await fetch(req.url, req.init);
      var text = await response.text();

      if (!response.ok) {
        // `isError` is what tells the agent this is a failure. Without it
        // the harness treats the body as a normal result, and a model can
        // relay `Request failed (403): ...` to the customer as if it were
        // product data.
        return {
          isError: true,
          content: [{
            type: 'text',
            text: 'Request failed (' + response.status + '): ' + text
          }]
        };
      }

      var result = { content: [{ type: 'text', text: text }] };

      // When the tool declared an `outputSchema`, the response body IS
      // the JSON value that schema describes (the endpoint's declared
      // response schema and its actual response body are the same
      // contract) — parse it into `structuredContent` so a client can
      // validate/consume it as data instead of re-parsing the text block
      // itself. `content` still carries the raw text unconditionally, both
      // as the backward-compatible fallback for a client that ignores
      // `structuredContent` and for a tool with no `outputSchema` at all.
      // A body that fails to parse as JSON is a server-side schema/
      // response mismatch, not something retrying fixes, so it just falls
      // back to text-only rather than failing the call.
      if (tool.outputSchema) {
        try {
          result.structuredContent = JSON.parse(text);
        } catch (e) {
          // Leave structuredContent unset; `content` above still carries
          // the raw text.
        }
      }

      return result;
    })
  };
  if (tool.outputSchema) {
    options.outputSchema = tool.outputSchema;
  }
  return options;
}
