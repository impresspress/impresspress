// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_log.test.mjs
//
// `#dev-log` is a `role="log"` live region: a screen reader announces what
// is added to it. So a new line must be APPENDED as its own node — rewriting
// the whole text would re-read every line the panel holds, each time.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate } from './harness.mjs';

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('a log line is appended as one node, leaving the earlier lines in place', async () => {
  const { handle, elements } = instantiate();
  await settle();
  const logEl = elements.get('dev-log');
  handle.log('first');
  const before = logEl.childNodes.slice();
  handle.log('second');

  assert.equal(logEl.childNodes.length, before.length + 1);
  // The nodes already there are the same objects: nothing was rewritten.
  before.forEach((node, index) => assert.equal(logEl.childNodes[index], node));
  const added = logEl.childNodes[logEl.childNodes.length - 1];
  assert.match(added.textContent, /^\d\d:\d\d:\d\d {2}second\n$/);
  assert.match(logEl.textContent, /first\n.*second\n$/s);
  // Still scrolled to the newest line.
  assert.equal(logEl.scrollTop, logEl.scrollHeight);
});

test('past LOG_LIMIT the oldest line is dropped, one node per line', async () => {
  const { handle, elements } = instantiate();
  await settle();
  const logEl = elements.get('dev-log');
  for (let i = 0; i < handle.LOG_LIMIT + 5; i += 1) {
    handle.log('line ' + i);
  }
  assert.equal(logEl.childNodes.length, handle.LOG_LIMIT);
  // Whatever the page logged on load went first, then lines 0-4.
  assert.match(logEl.firstChild.textContent, /^\d\d:\d\d:\d\d {2}line 5\n$/);
  assert.ok(!logEl.textContent.includes('  line 4\n'), 'the oldest lines are gone');
  assert.ok(logEl.textContent.endsWith(`  line ${handle.LOG_LIMIT + 4}\n`));
});
