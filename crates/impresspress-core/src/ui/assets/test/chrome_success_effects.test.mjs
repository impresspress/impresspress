// Run with: node --test crates/impresspress-core/src/ui/assets/test/chrome_success_effects.test.mjs
//
// Pins `chrome.js` section 5: what a control does once ITS OWN htmx request
// succeeds, declared as `data-*-on-success` attributes on the element that
// issued the request.
//
// These attributes replaced `hx-on--after-request` handlers, which never ran:
// htmx compiles an `hx-on` value with `new Function`, and the served policy has
// no `'unsafe-eval'`. `ui::tests::pages_carry_no_htmx_eval_attributes` keeps
// the eval-shaped attributes out of the markup, and the Playwright spec
// `crates/impresspress-web/tests/e2e/htmx-success-effects.spec.ts` proves the
// replacements run under the real header on a real page. This file pins each
// effect, and each refusal to act, in isolation.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadChrome } from './chrome_harness.mjs';

test('data-remove-on-success removes the element it names', () => {
  const page = loadChrome();
  const empty = page.element({}, 'empty-state');
  const bystander = page.element({}, 'other');

  page.finishRequest(page.element({ 'data-remove-on-success': 'empty-state' }), true);

  assert.equal(empty.removed, true);
  assert.equal(bystander.removed, false);
});

test('data-reset-on-success resets the issuing form', () => {
  const page = loadChrome();
  const form = page.element({ 'data-reset-on-success': '' });
  form.reset = function () {
    this.resets += 1;
  };

  page.finishRequest(form, true);

  assert.equal(form.resets, 1);
});

test('data-reset-on-success on an element with no reset() does nothing, and does not throw', () => {
  const page = loadChrome();
  const button = page.element({ 'data-reset-on-success': '' });

  assert.doesNotThrow(() => page.finishRequest(button, true));
  assert.equal(button.resets, 0);
});

test('data-scroll-on-success scrolls the list it names to its bottom', () => {
  const page = loadChrome();
  const list = page.element({}, 'messages');
  list.scrollHeight = 480;

  page.finishRequest(page.element({ 'data-scroll-on-success': 'messages' }), true);

  assert.equal(list.scrollTop, 480);
});

test('data-reload-on-success reloads the page', () => {
  const page = loadChrome();

  page.finishRequest(page.element({ 'data-reload-on-success': '' }), true);

  assert.equal(page.reloads(), 1);
});

test('the effects combine on one element', () => {
  const page = loadChrome();
  const empty = page.element({}, 'empty-state');
  const list = page.element({}, 'rows');
  list.scrollHeight = 90;
  const form = page.element({
    'data-remove-on-success': 'empty-state',
    'data-reset-on-success': '',
    'data-scroll-on-success': 'rows',
    'data-reload-on-success': ''
  });
  form.reset = function () {
    this.resets += 1;
  };

  page.finishRequest(form, true);

  assert.equal(empty.removed, true);
  assert.equal(form.resets, 1);
  assert.equal(list.scrollTop, 90);
  assert.equal(page.reloads(), 1);
});

test('a request that did not succeed does none of it', () => {
  // htmx raises `htmx:afterRequest` for a refused request too, with
  // `successful: false`; section 3 toasts that one. A reload here would wipe
  // the toast, and a reset would throw away what the person typed.
  const page = loadChrome();
  const empty = page.element({}, 'empty-state');
  const form = page.element({
    'data-remove-on-success': 'empty-state',
    'data-reset-on-success': '',
    'data-reload-on-success': ''
  });
  form.reset = function () {
    this.resets += 1;
  };

  page.finishRequest(form, false);
  page.finishRequest(form, undefined);

  assert.equal(empty.removed, false);
  assert.equal(form.resets, 0);
  assert.equal(page.reloads(), 0);
});

test('an id that names nothing is ignored', () => {
  const page = loadChrome();

  assert.doesNotThrow(() =>
    page.finishRequest(
      page.element({ 'data-remove-on-success': 'gone', 'data-scroll-on-success': 'gone' }),
      true
    )
  );
});

test('only an Element issuer counts', () => {
  // `detail.elt` is htmx's; anything that is not an element carries no
  // attributes the section may trust.
  const page = loadChrome();

  assert.doesNotThrow(() => page.finishRequest(undefined, true));
  assert.doesNotThrow(() =>
    page.finishRequest({ getAttribute: () => 'x', hasAttribute: () => true }, true)
  );
  assert.equal(page.reloads(), 0);
});
