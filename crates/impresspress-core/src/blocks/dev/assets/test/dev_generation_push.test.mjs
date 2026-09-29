// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_generation_push.test.mjs
//
// The activation push (design §2.6): the service worker posts
// `{ type: 'dev-generation', id, cause, changed_paths }` to every window when
// a generation goes live, and the page reloads — or restyles — its preview
// from that, once per generation. See `harness.mjs` for how the tail is
// loaded without adding a test hook to the shipped file.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate } from './harness.mjs';

/** One macrotask, which is long enough for the tail's load-time work. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** How many `/b/dev/api/status` requests the tail has made so far. */
const statusCalls = (fetchCalls) =>
  fetchCalls.filter(([url]) => String(url) === '/b/dev/api/status').length;

/** A `<link>` in the preview's document, with the `href` the markup gave it. */
const link = (href) => {
  const attributes = { rel: 'stylesheet', href };
  return {
    getAttribute: (name) => (name in attributes ? attributes[name] : null),
    setAttribute: (name, value) => {
      attributes[name] = value;
    }
  };
};

/**
 * An instance whose preview frame counts its reloads and holds `links`, at
 * `location` (the site is served from `/`, so that is the page on show).
 */
const withPreview = ({ links = [], location = 'http://sandbox.test/', status } = {}) => {
  const instance = instantiate(status === undefined ? {} : { status });
  const frame = {
    reloads: 0,
    contentWindow: {
      location: {
        href: location,
        reload: () => {
          frame.reloads += 1;
        }
      }
    },
    contentDocument: {
      querySelectorAll: (selector) => {
        assert.equal(selector, 'link[rel="stylesheet"]');
        return links;
      }
    },
    getAttribute: () => '/',
    setAttribute: () => {}
  };
  instance.elements.set('dev-preview-frame', frame);
  return { ...instance, frame };
};

const generation = (id, changed_paths = ['site/index.html'], cause = 'site_write') => ({
  type: 'dev-generation',
  id,
  cause,
  changed_paths
});

test('a push for a generation the page has not shown reloads the preview and logs it', async () => {
  const { push, frame, elements } = withPreview();
  await settle();

  push(generation('gen_1'));
  assert.equal(frame.reloads, 1);
  assert.match(elements.get('dev-log').textContent, /live generation: gen_1/);
  // The ladder reads the end of that activation, not whatever the last poll
  // caught it doing.
  assert.equal(elements.get('dev-progress-steps').getAttribute('data-phase'), 'active');
  // And the export button, which waits for something to be live, is enabled.
  assert.equal(elements.get('dev-export').disabled, false);
});

test('the same generation pushed twice reloads the preview once', async () => {
  const { push, frame, elements } = withPreview();
  await settle();

  push(generation('gen_1'));
  push(generation('gen_1'));
  assert.equal(frame.reloads, 1);
  const lines = elements.get('dev-log').textContent.split('\n');
  assert.equal(lines.filter((line) => line.endsWith('live generation: gen_1')).length, 1);

  // A NEW generation is a new reload.
  push(generation('gen_2'));
  assert.equal(frame.reloads, 2);
});

test('a CSS-only generation swaps the matching stylesheets instead of reloading', async () => {
  const site = link('/css/site.css');
  // Relative to the page on show: `/blog/` + `../theme.css` is `/theme.css`.
  const theme = link('../theme.css?v=1');
  const untouched = link('/other.css');
  const { push, frame } = withPreview({
    links: [site, theme, untouched],
    location: 'http://sandbox.test/blog/'
  });
  await settle();

  push(generation('gen_3', ['site/css/site.css', 'site/theme.css']));
  assert.equal(frame.reloads, 0, 'a restyle must not reload the preview');
  assert.equal(site.getAttribute('href'), '/css/site.css?g=gen_3');
  assert.equal(theme.getAttribute('href'), '/theme.css?g=gen_3');
  assert.equal(untouched.getAttribute('href'), '/other.css', 'a stylesheet that did not change is left alone');
});

test('a generation that changed anything but stylesheets reloads, and swaps nothing', async () => {
  const site = link('/site.css');
  const { push, frame } = withPreview({ links: [site] });
  await settle();

  push(generation('gen_4', ['site/site.css', 'site/index.html']));
  assert.equal(frame.reloads, 1);
  assert.equal(site.getAttribute('href'), '/site.css');
});

test('a generation that changed no site file reloads the preview', async () => {
  const { push, frame } = withPreview({ links: [link('/site.css')] });
  await settle();

  // A compile or a block removal: the site is the same, the blocks it calls
  // are not.
  push(generation('gen_5', [], 'block_remove'));
  assert.equal(frame.reloads, 1);
});

test('a CSS-only generation the preview does not link directly reloads it instead', async () => {
  // `@import`ed, or simply not on the page on show: swapping zero links
  // would leave the previous generation's styles standing.
  const { push, frame } = withPreview({ links: [link('/site.css')] });
  await settle();

  push(generation('gen_6', ['site/imported.css']));
  assert.equal(frame.reloads, 1);
});

test('a site generation reads no status; a block-set one reads it once, after the reload', async () => {
  const { push, frame, fetchCalls } = withPreview();
  await settle();
  const before = statusCalls(fetchCalls);

  push(generation('gen_7'));
  await settle();
  assert.equal(statusCalls(fetchCalls), before, 'a site write cannot have rebuilt the runtime');

  push(generation('gen_8', [], 'rollback'));
  assert.equal(frame.reloads, 2);
  await settle();
  assert.equal(statusCalls(fetchCalls), before + 1, 'a rollback may have: observe checks');
});

test('messages that are not generation pushes are ignored', async () => {
  const { push, frame } = withPreview();
  await settle();

  push({ type: 'sw-self-destruct', reason: 'stale build' });
  push(null);
  assert.equal(frame.reloads, 0);
});

test('the catch-up after a mutating call reads no status and does not reload the preview', async () => {
  const { handle, frame, fetchCalls } = withPreview();
  await settle();
  const before = statusCalls(fetchCalls);

  await handle.refreshAfterChange();
  // Through the wrapper too: the last call out runs the catch-up.
  await handle.withProgress(async () => 'written')();
  assert.equal(statusCalls(fetchCalls), before);
  assert.equal(frame.reloads, 0, 'the push reloads the preview; the catch-up must not reload it again');
  assert.ok(
    fetchCalls.some(([url]) => String(url).startsWith('/b/dev/api/files')),
    'the file tree is still refreshed'
  );
});

test('a status answered mid-activation that lands after the push does not undo it', async () => {
  // The poll's request was answered while `gen_9` was publishing; the push
  // that it went live overtook that answer on the way to the page.
  let answer = {};
  const { push, fireInterval, handle, elements } = withPreview({ status: () => answer });
  await settle();
  const call = handle.withProgress(() => new Promise(() => {}));
  call();

  push(generation('gen_9'));
  answer = {
    active_generation: { id: 'gen_8' },
    runtime_generation: 0,
    activation: { generation_id: 'gen_9', phase: 'publishing' }
  };
  fireInterval();
  await settle();

  const steps = elements.get('dev-progress-steps');
  assert.equal(steps.getAttribute('data-phase'), 'active');
  assert.doesNotMatch(elements.get('dev-log').textContent, /live generation: gen_8/);
  handle.abort.abort();
});
