// Fragment shared by webmcp.js and dev.js; `assets.rs` wraps it in the
// caller's IIFE. Never served on its own.

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
