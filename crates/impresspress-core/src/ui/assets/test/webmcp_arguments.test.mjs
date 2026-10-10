// Run with: node --test crates/impresspress-core/src/ui/assets/test/webmcp_arguments.test.mjs
//
// Chrome's WebMCP hands a tool's `execute` whatever arguments the agent sent;
// it does not check them against the tool's `inputSchema`. So the check that a
// call carries what the tool requires is `webmcp-core.js`'s to make, before it
// builds a request. Without it a missing path argument became the text
// `undefined` in the URL and the agent was told `404 Product not found` — a
// statement about data, when the truth was a statement about its own call.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate, settle } from './harness.mjs';

// The shape `shop_publish_offer` has in the served manifest
// (`tests/snapshots/dev.tools.json`): two path arguments, both required.
const PUBLISH_OFFER = {
  name: 'shop_publish_offer',
  description: 'Publish an offer.',
  inputSchema: {
    type: 'object',
    properties: { offer_id: { type: 'string' }, product_id: { type: 'string' } },
    required: ['offer_id', 'product_id']
  },
  invocation: {
    method: 'post',
    path: '/b/products/products/{product_id}/offers/{offer_id}/publish',
    path_params: ['offer_id', 'product_id'],
    query_params: [],
    body_params: []
  }
};

// A required argument that travels in the body, beside optional ones.
const CREATE_PRODUCT = {
  name: 'shop_create_product',
  description: 'Create a product.',
  inputSchema: {
    type: 'object',
    properties: { name: { type: 'string' }, description: { type: ['string', 'null'] } },
    required: ['name']
  },
  invocation: {
    method: 'post',
    path: '/b/products/products',
    path_params: [],
    query_params: [],
    body_params: ['name', 'description']
  }
};

// Every response: an empty manifest for the tail's own load, `{}` for a tool.
const ok = () => ({ ok: true, status: 200, json: async () => ({ tools: [] }), text: async () => '{}' });

/// Run `tool` with `args` through the real `toolOptions`, and report what it
/// returned and every request it made.
async function run(tool, args) {
  const { handle, fetchCalls } = instantiate({ respond: ok });
  // The tail loads its manifest on its own; only the tool's requests count.
  await settle();
  fetchCalls.length = 0;
  const result = await handle.toolOptions(tool).execute(args);
  return { result, fetchCalls };
}

const refusal = (text) => ({ isError: true, content: [{ type: 'text', text }] });

test('a missing required path argument is named, and no request is made', async () => {
  const { result, fetchCalls } = await run(PUBLISH_OFFER, { offer_id: 'off_1' });
  assert.deepEqual(result, refusal('Missing required argument: product_id'));
  assert.deepEqual(fetchCalls, []);
});

test('every missing required argument is named, in the schema’s order', async () => {
  const { result, fetchCalls } = await run(PUBLISH_OFFER, {});
  assert.deepEqual(result, refusal('Missing required arguments: offer_id, product_id'));
  assert.deepEqual(fetchCalls, []);
});

test('no arguments at all is the same as an empty object', async () => {
  const { result, fetchCalls } = await run(PUBLISH_OFFER, undefined);
  assert.deepEqual(result, refusal('Missing required arguments: offer_id, product_id'));
  assert.deepEqual(fetchCalls, []);
});

test('a missing required body argument is named, and no request is made', async () => {
  const { result, fetchCalls } = await run(CREATE_PRODUCT, { description: 'x' });
  assert.deepEqual(result, refusal('Missing required argument: name'));
  assert.deepEqual(fetchCalls, []);
});

test('a path argument that is empty or null has no URL segment to fill, so it is missing', async () => {
  for (const empty of ['', null]) {
    const { result, fetchCalls } = await run(PUBLISH_OFFER, { offer_id: 'off_1', product_id: empty });
    assert.deepEqual(result, refusal('Missing required argument: product_id'), String(empty));
    assert.deepEqual(fetchCalls, [], String(empty));
  }
});

test('a path argument the schema does not list as required is still checked', async () => {
  // The producer forces every path argument into `required`, but the URL
  // needing a value is a fact of the template, not of that list.
  const tool = {
    ...PUBLISH_OFFER,
    inputSchema: { ...PUBLISH_OFFER.inputSchema, required: ['offer_id'] }
  };
  const { result, fetchCalls } = await run(tool, { offer_id: 'off_1' });
  assert.deepEqual(result, refusal('Missing required argument: product_id'));
  assert.deepEqual(fetchCalls, []);
});

test('a path argument that is not a single value is refused, not stringified into the URL', async () => {
  for (const value of [{ id: 'p' }, ['p'], Number.NaN, Number.POSITIVE_INFINITY]) {
    const { result, fetchCalls } = await run(PUBLISH_OFFER, { offer_id: 'off_1', product_id: value });
    assert.deepEqual(
      result,
      refusal('Invalid argument: product_id must be a string, a number or a boolean'),
      JSON.stringify(value)
    );
    assert.deepEqual(fetchCalls, [], JSON.stringify(value));
  }
});

test('present required arguments go through: an empty body string is the server’s to judge', async () => {
  // `required` means the key is present. Whether an empty name in the body
  // is acceptable is the endpoint's to judge; only a path argument has no
  // way to carry an empty value at all.
  const { result, fetchCalls } = await run(CREATE_PRODUCT, { name: '', description: null });
  assert.deepEqual(result, { content: [{ type: 'text', text: '{}' }] });
  assert.equal(fetchCalls.length, 1);
  assert.equal(fetchCalls[0][0], '/b/products/products');
  assert.deepEqual(JSON.parse(fetchCalls[0][1].body), { name: '', description: null });
});

test('complete path arguments, including a number, fill the URL', async () => {
  const { result, fetchCalls } = await run(PUBLISH_OFFER, { offer_id: 7, product_id: 'p/1' });
  assert.equal(result.isError, undefined);
  assert.equal(fetchCalls.length, 1);
  assert.equal(fetchCalls[0][0], '/b/products/products/p%2F1/offers/7/publish');
});

test('an inherited name is not a present argument', async () => {
  // `{}` answers `constructor` through its prototype; a required argument
  // that happens to share a name with one must still count as missing.
  const tool = {
    ...CREATE_PRODUCT,
    inputSchema: { ...CREATE_PRODUCT.inputSchema, required: ['constructor'] },
    invocation: { ...CREATE_PRODUCT.invocation, body_params: ['constructor'] }
  };
  const { result, fetchCalls } = await run(tool, {});
  assert.deepEqual(result, refusal('Missing required argument: constructor'));
  assert.deepEqual(fetchCalls, []);
});
