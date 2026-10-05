// Run with: node --test crates/impresspress-core/src/ui/assets/test/chrome_focus_field.test.mjs
//
// Pins `chrome.js` section 3b: the `focusField` HX-Trigger a refused form
// answer carries focuses the field the refusal is about and marks it invalid.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadChrome } from './chrome_harness.mjs';

test('focusField focuses and selects the named field and marks it invalid', () => {
  const page = loadChrome();
  const field = page.element({}, 'current_password');
  let focused = 0;
  let selected = 0;
  field.focus = () => {
    focused += 1;
  };
  field.select = () => {
    selected += 1;
  };
  const other = page.element({}, 'new_password');
  other.focus = () => assert.fail('only the named field takes focus');

  page.trigger('focusField', { id: 'current_password' });

  assert.equal(focused, 1);
  assert.equal(selected, 1);
  assert.equal(field.getAttribute('aria-invalid'), 'true');
  assert.equal(other.getAttribute('aria-invalid'), null);
});

test('focusField naming nothing on the page does nothing, and does not throw', () => {
  const page = loadChrome();
  assert.doesNotThrow(() => page.trigger('focusField', { id: 'missing' }));
  assert.doesNotThrow(() => page.trigger('focusField', undefined));
});
