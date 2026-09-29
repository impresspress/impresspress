// Run with: node --import ./js/test/node-hooks.mjs --test js/test/http_fetch.test.mjs
// (see node-hooks.mjs's header comment for why the --import hook is needed).
//
// These cover the half of the outbound-request gate that lives in JS. The Rust
// half — `network.rs`'s `is_ssrf_blocked_url` precheck — is covered by the
// wasm tests in that module; what cannot be asserted from Rust is the shape of
// the `init` object handed to the page's `fetch`, because `bridge::http_fetch`
// is a wasm-bindgen extern import. So `fetch` is stubbed here and the init is
// inspected directly.
import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { httpFetch } from '../bridge.js';

const originalFetch = globalThis.fetch;

afterEach(() => {
    globalThis.fetch = originalFetch;
});

/** Record every `fetch(url, init)` call and answer with an empty 200. */
function stubFetch(response) {
    const calls = [];
    globalThis.fetch = async (url, init) => {
        calls.push({ url, init });
        return (
            response ?? {
                status: 200,
                headers: new Headers(),
                body: null,
            }
        );
    };
    return calls;
}

// The URL gate in `network.rs` only ever sees the URL a caller asked for. A
// followed `3xx` reaches a second URL that gate never inspected, so
// `https://evil.example/x` answering `302 Location: http://169.254.169.254/…`
// would hand the metadata service's body back to the block. `fetch` defaults
// to `redirect: 'follow'`, so this has to be set explicitly on every request.
test('httpFetch refuses to follow redirects, so the SSRF gate cannot be walked around', async () => {
    const calls = stubFetch();

    await httpFetch('GET', 'https://example.com/x', '{}', new Uint8Array(), 1024);

    assert.equal(calls.length, 1);
    assert.equal(
        calls[0].init.redirect,
        'error',
        "'manual' is not a substitute: a cross-origin redirect is opaque and its Location unreadable",
    );
});

test('httpFetch sets redirect: error on a request that carries a body too', async () => {
    const calls = stubFetch();

    await httpFetch(
        'POST',
        'https://example.com/x',
        '{"content-type":"application/json"}',
        new TextEncoder().encode('{"a":1}'),
        1024,
    );

    assert.equal(calls[0].init.method, 'POST');
    assert.deepEqual(calls[0].init.headers, { 'content-type': 'application/json' });
    assert.equal(calls[0].init.redirect, 'error');
});
