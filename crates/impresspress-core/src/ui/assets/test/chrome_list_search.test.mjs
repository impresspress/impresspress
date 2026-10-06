// Run with: node --test crates/impresspress-core/src/ui/assets/test/chrome_list_search.test.mjs
//
// Pins `chrome.js` section 6, the list search box: a response for a term the
// box no longer holds is dropped, so typing on while a search is in flight
// never puts the shorter term back in the box; once a search lands, what it
// found is announced in `#search-status`; after Clear, focus is back in the box.
//
// Each event carries the shape htmx gives it: htmx fires `htmx:beforeSwap` and
// `htmx:afterSwap` on the swap target, so `detail.elt` is `main#content` and
// the element that made the request is `detail.requestConfig.elt`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadChrome } from './chrome_harness.mjs';

/** A search box as `SearchInput::render` marks it, holding `value`. */
function searchBox(page, value, { connected = true, status = '', id = 'users-search' } = {}) {
  const box = page.element(
    { id, name: 'search', 'data-search-input': '', 'data-search-status': status },
    id
  );
  box.value = value;
  box.isConnected = connected;
  box.focused = 0;
  box.focus = () => {
    box.focused += 1;
  };
  return box;
}

/** Fire one htmx swap event for a request `elt` made with `search=<sent>`. */
function swapEvent(page, type, elt, sent = '') {
  const detail = {
    elt: page.element({ id: 'content' }),
    shouldSwap: true,
    requestConfig: { elt, formData: new Map([['search', sent]]) }
  };
  page.trigger(type, detail);
  return detail;
}

test('a response for the term the box still holds is swapped', () => {
  const page = loadChrome();
  const box = searchBox(page, 'ali');
  assert.equal(swapEvent(page, 'htmx:beforeSwap', box, 'ali').shouldSwap, true);
});

test('a response for a term the operator has typed past is dropped', () => {
  const page = loadChrome();
  const box = searchBox(page, 'alice');
  assert.equal(swapEvent(page, 'htmx:beforeSwap', box, 'ali').shouldSwap, false);
});

test('a response whose box a later response already replaced is dropped', () => {
  const page = loadChrome();
  const box = searchBox(page, 'ali', { connected: false });
  assert.equal(swapEvent(page, 'htmx:beforeSwap', box, 'ali').shouldSwap, false);
});

test('a request from anything but a search box is left alone', () => {
  const page = loadChrome();
  const button = page.element({ name: 'search' });
  button.value = 'other';
  button.isConnected = true;
  assert.equal(swapEvent(page, 'htmx:beforeSwap', button, 'ali').shouldSwap, true);
  assert.doesNotThrow(() => page.trigger('htmx:beforeSwap', { shouldSwap: true }));
});

test('a landed search announces the new box\'s status and leaves focus alone', () => {
  const page = loadChrome();
  const status = page.element({}, 'search-status');
  const old = searchBox(page, 'ali', { status: '9 results' });
  // The swap put a new box under the same id.
  const fresh = searchBox(page, 'ali', { status: '12 results for “ali”' });
  swapEvent(page, 'htmx:afterSwap', old);
  assert.equal(status.textContent, '12 results for “ali”');
  assert.equal(fresh.focused, 0, 'htmx restores the typing focus itself');
});

test('Clear announces the full list and puts focus back in the box', () => {
  const page = loadChrome();
  const status = page.element({}, 'search-status');
  const box = searchBox(page, '', { status: '40 results' });
  const clear = page.element({ 'data-search-clear': 'users-search' });
  swapEvent(page, 'htmx:afterSwap', clear);
  assert.equal(status.textContent, '40 results');
  assert.equal(box.focused, 1);
});

test('a swap nobody searched for announces nothing', () => {
  const page = loadChrome();
  const status = page.element({}, 'search-status');
  status.textContent = '';
  swapEvent(page, 'htmx:afterSwap', page.element({ 'hx-get': '/b/admin/logs?tab=audit' }));
  assert.equal(status.textContent, '');
  assert.doesNotThrow(() => page.trigger('htmx:afterSwap', {}));
});
