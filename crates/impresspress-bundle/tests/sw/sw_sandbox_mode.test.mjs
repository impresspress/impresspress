// What the rendered service worker does in each sandbox mode.
//
// `sw.js` declares the mode once (`const SANDBOX = '…';`, rendered from
// `[sandbox] mode`) and two places read it: the check, before `initialize()`
// is called, that the runtime was compiled as that mode
// (`compiled_sandbox_mode()` — the runtime is never TOLD the mode, it is the
// build's), and the fetch handler's bypass branch, which in a workspace answers every bypassed
// same-origin request itself with the cross-origin-isolation pair the
// in-browser compiler needs. A site — what the sandbox exports — must not:
// isolation is what stops a page from embedding a third-party iframe, so an
// exported site that kept it would be a worse site than the one its owner
// built. The text of the branch is pinned in `tests/bundle_integration.rs`;
// this drives it.
//
// Run by `the_rendered_worker_behaves_per_sandbox_mode`
// in `tests/bundle_integration.rs`, which renders one worker per mode and
// passes them in `SW_JS` ('none'), `SW_JS_SITE` and `SW_JS_WORKSPACE`, plus
// a workspace built with an export shell in `SW_JS_EXPORT_SHELL`.
import test from 'node:test';
import assert from 'node:assert/strict';
import { captureConsole, loadWorker } from './harness.mjs';

const MODES = [
  { sandbox: 'none', variable: 'SW_JS' },
  { sandbox: 'site', variable: 'SW_JS_SITE' },
  { sandbox: 'workspace', variable: 'SW_JS_WORKSPACE' }
];

/// A worker of `variable`'s rendering whose runtime records what
/// `initialize()` was handed and which paths it was asked for.
async function worker(variable) {
  const initialized = [];
  const served = [];
  const loaded = await loadWorker(
    {
      initialize: async (options) => {
        initialized.push(options);
      },
      handle_request: async (request) => {
        served.push(new URL(request.url).pathname);
        return { response: new Response('from the runtime'), after: Promise.resolve() };
      }
    },
    { variable }
  );
  return { ...loaded, initialized, served };
}

for (const { sandbox, variable } of MODES) {
  test(`a '${sandbox}' worker checks its runtime's mode and hands initialize() none`, async (t) => {
    captureConsole(t);
    const sw = await worker(variable);

    const { response } = await sw.request('/b/auth/login');
    assert.equal(await response.text(), 'from the runtime');
    assert.deepEqual(sw.modeChecks, [1], 'asked once, after init()');
    assert.equal(sw.initialized.length, 1);
    // The mode is the build's: neither option that once carried a request
    // for it is handed.
    assert.equal('sandbox' in sw.initialized[0], false);
    assert.equal('dev' in sw.initialized[0], false);
    // None of these was built with an export shell, and says so: the option
    // is handed, as null.
    assert.equal(sw.initialized[0].exportShell, null);
  });

  for (const { sandbox: compiled } of MODES.filter((mode) => mode.sandbox !== sandbox)) {
    test(`a '${sandbox}' worker over a runtime compiled as '${compiled}' stops before initialize()`, async (t) => {
      captureConsole(t);
      const initialized = [];
      const sw = await loadWorker(
        {
          compiledSandboxMode: compiled,
          initialize: async (options) => {
            initialized.push(options);
          }
        },
        { variable }
      );

      const { response } = await sw.request('/b/auth/login');
      assert.equal(response.status, 503);
      const body = await response.json();
      assert.deepEqual(initialized, []);
      assert.equal(body.stage, 'build');
      assert.equal(
        body.cause,
        `this bundle's service worker is built for sandbox mode '${sandbox}' but its runtime was compiled as '${compiled}' — rebuild the bundle`
      );
    });
  }

  test(`a '${sandbox}' worker leaves /seed/ to the static host only if it runs the site runtime`, async (t) => {
    captureConsole(t);
    const sw = await worker(variable);

    await sw.request('/seed/manifest.json');
    // A worker with no sandbox has no seed to import, and the path is the
    // runtime's like any other.
    assert.deepEqual(sw.served, sandbox === 'none' ? ['/seed/manifest.json'] : []);
  });

  test(`a '${sandbox}' worker isolates a bypassed response only in a workspace`, async (t) => {
    captureConsole(t);
    const sw = await worker(variable);

    // `/loader.js` is on every bundle's bypass list.
    const { response } = await sw.request('/loader.js');
    assert.deepEqual(sw.served, [], 'a bypassed path never reaches the runtime');
    if (sandbox === 'workspace') {
      assert.ok(response, 'the workspace answers the bypassed request itself');
      assert.equal(response.headers.get('Cross-Origin-Embedder-Policy'), 'credentialless');
      assert.equal(response.headers.get('Cross-Origin-Opener-Policy'), 'same-origin');
      assert.equal(sw.network.length, 1, 'the answer is the network’s, fetched once');
    } else {
      assert.equal(
        response,
        undefined,
        `a '${sandbox}' worker leaves the request to the browser, untouched`
      );
      assert.equal(sw.network.length, 0);
    }
  });
}

// A workspace built with `[sandbox] export_shell` hands its runtime where the
// shell is: the bundler's `EXPORT_SHELL_ROOT`, as a string.
test('a workspace with an export shell hands initialize() its root', async (t) => {
  captureConsole(t);
  const sw = await worker('SW_JS_EXPORT_SHELL');

  await sw.request('/b/auth/login');
  assert.equal(sw.initialized.length, 1);
  assert.equal(sw.initialized[0].exportShell, '/__impresspress_dev/export-shell/');
});
