// Run with: node --test crates/impresspress-core/src/ui/assets/test/chrome_stale_search.test.mjs
//
// Pins `chrome.js` section 6: a search box's response for a term the box no
// longer holds is dropped, so typing on while a search is in flight never
// puts the shorter term back in the box or pushes its URL.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadChrome } from './chrome_harness.mjs';

/** A search box as `SearchInput::render` marks it, holding `value`. */
function searchBox(page, value, { connected = true } = {}) {
  const box = page.element({ name: 'search', 'data-search-input': '' });
  box.value = value;
  box.isConnected = connected;
  return box;
}

/** Fire `htmx:beforeSwap` for a request `elt` made with `search=<sent>`. */
function beforeSwap(page, elt, sent) {
  const detail = {
    shouldSwap: true,
    requestConfig: { elt, formData: new Map([['search', sent]]) }
  };
  page.trigger('htmx:beforeSwap', detail);
  return detail.shouldSwap;
}

test('a response for the term the box still holds is swapped', () => {
  const page = loadChrome();
  assert.equal(beforeSwap(page, searchBox(page, 'ali'), 'ali'), true);
});

test('a response for a term the operator has typed past is dropped', () => {
  const page = loadChrome();
  assert.equal(beforeSwap(page, searchBox(page, 'alice'), 'ali'), false);
});

test('a response whose box a later response already replaced is dropped', () => {
  const page = loadChrome();
  assert.equal(beforeSwap(page, searchBox(page, 'ali', { connected: false }), 'ali'), false);
});

test('a request from anything but a search box is left alone', () => {
  const page = loadChrome();
  const button = page.element({ name: 'search' });
  button.value = 'other';
  button.isConnected = true;
  assert.equal(beforeSwap(page, button, 'ali'), true);
  assert.doesNotThrow(() => page.trigger('htmx:beforeSwap', { shouldSwap: true }));
});
