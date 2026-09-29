// Run with: node --test crates/impresspress-core/src/ui/assets/test/chrome_error_toast.test.mjs
//
// Pins that a REFUSED htmx request reaches the operator.
//
// htmx 2.0.4's default `responseHandling` ends
// `{code:"[45]..", swap:false, error:true}`: a 4xx is deliberately not swapped
// and htmx raises `htmx:responseError` instead. Until `chrome.js` grew the
// listener these cover, nothing in the tree listened for that event — so a
// refusal produced no swap, no message and no change of any kind. The admin
// Variables modal just sat there.
//
// That is why this is not a cosmetic test. The 2026-09-10 live-server audit
// found that creating a variable with a key that already exists answered 500;
// it answers 409 now, and a 409 nobody can see is the same experience the 500
// was. The status code is only half the fix.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadChrome } from './chrome_harness.mjs';

/** The body `wafer_block::http_codec` renders for every error terminal. */
const envelope = (code, message) => JSON.stringify({ error: code, message });

/**
 * What a 500 ACTUALLY puts on the wire.
 *
 * Every 500 in the tree is `wafer_block::response::err_internal`, which mints a
 * fresh 8-byte correlation id per call and renders
 * `Internal server error (ref: <hex>)`. So two failures of the same endpoint
 * carry two DIFFERENT message strings — which is why suppression cannot be
 * keyed on the message text, and why a fixture with a fixed message would have
 * asserted a collapse that production never performs.
 * `blocks/errors.rs::two_internal_errors_differ_only_by_the_correlation_ref`
 * reads that shape off a real rendered response; this mirrors it.
 */
let refCounter = 0;
const internalServerError = () => {
  refCounter += 1;
  const ref = refCounter.toString(16).padStart(16, '0');
  return envelope('Internal', `Internal server error (ref: ${ref})`);
};

test('a 409 surfaces the message the server wrote, as an error toast', () => {
  const page = loadChrome();
  page.respondWithError({
    status: 409,
    responseText: envelope(
      'AlreadyExists',
      'A variable with the key "SITE_MOTTO" already exists. Choose a different key.'
    )
  });

  assert.deepEqual(page.toasts(), [
    {
      kind: 'error',
      text: 'A variable with the key "SITE_MOTTO" already exists. Choose a different key.'
    }
  ]);
});

test('every other refusal comes through the same listener', () => {
  const page = loadChrome();
  page.respondWithError({ status: 403, responseText: envelope('PermissionDenied', 'Access denied') });
  page.respondWithError({ status: 400, responseText: envelope('InvalidArgument', 'Key is required') });

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'Access denied' },
    { kind: 'error', text: 'Key is required' }
  ]);
});

test('a body with no parseable message still says something, naming the status', () => {
  // The silence this listener exists to remove is not improved by an empty
  // toast, so every branch has to end in text.
  const page = loadChrome();
  page.respondWithError({ status: 502, responseText: '' });
  page.respondWithError({ status: 500, responseText: 'not json at all' });
  page.respondWithError({ status: 409, responseText: '{"error":"AlreadyExists"' });
  page.respondWithError({ status: 404, responseText: envelope('NotFound', '') });

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'Request failed (502)' },
    { kind: 'error', text: 'Request failed (500)' },
    { kind: 'error', text: 'Request failed (409)' },
    { kind: 'error', text: 'Request failed (404)' }
  ]);
});

test('an HTML error page is not poured into the toast', () => {
  // A refusal rendered as a full error page is also a 4xx. Its markup is not a
  // message, and `textContent` would print the whole document as one line.
  const page = loadChrome();
  page.respondWithError({
    status: 403,
    responseText: '<!doctype html><html><body><h1>Forbidden</h1></body></html>'
  });

  assert.deepEqual(page.toasts(), [{ kind: 'error', text: 'Request failed (403)' }]);
});

test('a request that failed before any response still toasts', () => {
  // `htmx:sendError` aside, an `xhr` with neither status nor body reaches this
  // listener whenever the detail is incomplete; it must not produce `(0)` or an
  // empty string.
  const page = loadChrome();
  page.respondWithError({});

  assert.deepEqual(page.toasts(), [{ kind: 'error', text: 'Request failed' }]);
});

test('twenty auto-triggered 500s collapse to one, correlation refs and all', () => {
  // `blocks/llm/ui.rs` renders a status badge per model with
  // `hx-trigger="load"`, and `routes/models.rs` answers each with an error
  // terminal when the backend is unreachable. Twenty models meant twenty
  // identical toasts stacked four seconds deep.
  //
  // Each body carries its OWN correlation ref, because that is what the server
  // sends. Suppression keyed on the message text does nothing here — all twenty
  // strings differ — which is why the key is the status, the envelope's `error`
  // code, and the message with its trailing `(ref: …)` removed.
  const page = loadChrome();
  const bodies = [];
  for (let i = 0; i < 20; i += 1) {
    const body = internalServerError();
    bodies.push(body);
    page.respondWithError({ status: 500, responseText: body });
  }

  assert.equal(new Set(bodies).size, 20, 'the fixture must model twenty DISTINCT bodies');
  const toasts = page.toasts();
  assert.equal(toasts.length, 1, `one failure, reported once: ${JSON.stringify(toasts)}`);
  assert.match(toasts[0].text, /^Internal server error \(ref: [0-9a-f]+\)$/);
});

test('a genuinely different failure in the same burst still toasts', () => {
  // Suppression is on the FAILURE, not on the burst: two distinct facts are two
  // things the operator needs to know, however close together they arrive.
  const page = loadChrome();
  page.respondWithError({ status: 500, responseText: internalServerError() });
  page.respondWithError({ status: 500, responseText: internalServerError() });
  page.respondWithError({ status: 403, responseText: envelope('PermissionDenied', 'Access denied') });

  const toasts = page.toasts();
  assert.equal(toasts.length, 2);
  assert.match(toasts[0].text, /^Internal server error/);
  assert.deepEqual(toasts[1], { kind: 'error', text: 'Access denied' });
});

test('a retry the OPERATOR asked for is never suppressed', () => {
  // The regression the sliding window introduced: an operator who hits a
  // refusal, watches the toast dismiss at four seconds and clicks the same
  // button again got one toast ever and then silence — the "nothing happened"
  // problem this listener exists to remove, now for the interactive case. A
  // repeat a person asked for is a new fact; only the page repeating itself is
  // noise.
  const page = loadChrome();
  const refused = envelope('AlreadyExists', 'A variable with the key "SITE_MOTTO" already exists.');
  for (let i = 0; i < 3; i += 1) {
    page.respondWithError({ status: 409, responseText: refused }, { user: true });
    page.advance(3000);
  }

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'A variable with the key "SITE_MOTTO" already exists.' },
    { kind: 'error', text: 'A variable with the key "SITE_MOTTO" already exists.' },
    { kind: 'error', text: 'A variable with the key "SITE_MOTTO" already exists.' }
  ]);
});

test('a request htmx did not describe is treated as one a person asked for', () => {
  // "Could not tell" must not become "suppress": silence is the failure mode
  // this listener exists to remove, so an absent `requestConfig` shows.
  const page = loadChrome();
  const refused = envelope('PermissionDenied', 'Access denied');
  page.respondWithError({ status: 403, responseText: refused }, { noConfig: true });
  page.respondWithError({ status: 403, responseText: refused }, { noConfig: true });

  assert.equal(page.toasts().length, 2);
});

test('the same auto-triggered failure toasts again once the window has passed', () => {
  // Suppression is a window, not a mute: a failure that stopped and started
  // again is a new event and says so.
  const page = loadChrome();
  page.respondWithError({ status: 500, responseText: internalServerError() });
  page.advance(6000);
  page.respondWithError({ status: 500, responseText: internalServerError() });

  assert.equal(page.toasts().length, 2);
});

test('a steady auto-triggered stream stays quiet, because the window slides', () => {
  // A page polling a broken endpoint every two seconds must not re-toast every
  // five: each repeat resets the window, so it says its piece once and waits
  // for the failure to actually stop and start again.
  const page = loadChrome();
  for (let i = 0; i < 10; i += 1) {
    page.respondWithError({ status: 500, responseText: internalServerError() });
    page.advance(2000);
  }

  assert.equal(page.toasts().length, 1);
});

test('a control that names itself says which control failed', () => {
  // A body that is not the JSON envelope leaves the listener nothing to quote,
  // and `Request failed (502)` tells an operator looking at a payment-link row
  // with an Archive and a Deactivate button neither which one failed nor
  // whether anything was written. `data-error-label` is what the per-button
  // `hx-on--after-request` this listener replaced used to carry.
  const page = loadChrome();
  page.respondWithError(
    { status: 502, responseText: '<html>bad gateway</html>' },
    { user: true, label: 'Could not archive this offer' }
  );

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'Could not archive this offer (502)' }
  ]);
});

test("the server's own message still beats the control's label", () => {
  // The label is a fallback, not an override: "Product is not waiting for
  // moderation" says more than the name of the button that asked.
  const page = loadChrome();
  page.respondWithError(
    { status: 409, responseText: envelope('AlreadyExists', 'Product is not waiting for moderation') },
    { user: true, label: 'Could not archive this offer' }
  );

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'Product is not waiting for moderation' }
  ]);
});

test('a labelled control names itself on a transport failure too', () => {
  // The dropped-connection case is the one the deleted `else` branch covered,
  // and it has the same "which control?" problem.
  const page = loadChrome();
  page.fireTransportEvent('htmx:sendError', { user: true, label: 'Could not restore this product' });
  page.fireTransportEvent('htmx:timeout', { user: true, label: 'Could not restore this product' });

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'Could not restore this product — the server could not be reached.' },
    { kind: 'error', text: 'Could not restore this product — the server did not answer in time.' }
  ]);
});

test('a request that never reached the server says so, in its own words', () => {
  // `xhr.onerror` fires `htmx:afterRequest` and then `htmx:sendError`, and the
  // `responseInfo` both carry has no `successful` field at all — it is assigned
  // only inside `handleAjaxResponse`. So a dropped connection reaches NEITHER
  // the response-error listener above nor any `if(event.detail.successful)`
  // guard, and before this branch existed it produced nothing.
  //
  // Its own sentence, not the response-error one: there is no status and no
  // body, and what the operator needs to know is that nothing was sent, so
  // retrying is the right move rather than a way to create a second row.
  const page = loadChrome();
  page.fireTransportEvent('htmx:sendError');

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'Could not reach the server. Check your connection and try again.' }
  ]);
});

test('a timeout says so too', () => {
  const page = loadChrome();
  page.fireTransportEvent('htmx:timeout');

  assert.deepEqual(page.toasts(), [
    { kind: 'error', text: 'The server did not answer in time. Try again.' }
  ]);
});

test('an abort is silent, because the page is the one that aborted', () => {
  // `hx-sync` superseding an in-flight request and a navigation away both land
  // here. Toasting them would manufacture noise on exactly the pages that abort
  // most, and nothing in this tree aborts a request a person is waiting on.
  const page = loadChrome();
  page.fireTransportEvent('htmx:sendAbort');

  assert.deepEqual(page.toasts(), []);
});

test('a page with no toast container does not throw', () => {
  // The shipped layout always renders one, but the listener chain must not be
  // the thing that breaks a page that does not.
  const page = loadChrome({ toastContainer: false });
  assert.doesNotThrow(() => page.respondWithError({ status: 409, responseText: '' }));
  assert.deepEqual(page.toasts(), []);
});
