// Run with: node --import ./js/test/node-hooks.mjs --test js/test/cookie_header.test.mjs
// (see node-hooks.mjs's header comment for why the --import hook is needed).
//
// `readCookieHeader` is a wasm-bindgen extern import on the Rust side, so what
// it does with the globals it finds can only be asserted here. The case that
// matters is the one with no worker global at all: a bare `self` reference
// throws ReferenceError rather than returning undefined, so a `typeof
// self.cookieStore` guard alone took down every caller outside a worker —
// including `convert::request_to_message`, which the Rust wasm tests drive
// under Node.
import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readCookieHeader } from '../bridge.js';

afterEach(() => {
    delete globalThis.self;
});

test('no worker global at all reads as no cookies', async () => {
    assert.equal(typeof globalThis.self, 'undefined', 'precondition: Node has no `self`');
    assert.equal(await readCookieHeader(), '');
});

test('a worker global without CookieStore reads as no cookies', async () => {
    globalThis.self = {};
    assert.equal(await readCookieHeader(), '');
});

test('a CookieStore is joined into a Cookie header value', async () => {
    globalThis.self = {
        cookieStore: {
            getAll: async () => [
                { name: 'auth_token', value: 'abc' },
                { name: 'theme', value: 'dark' },
            ],
        },
    };
    assert.equal(await readCookieHeader(), 'auth_token=abc; theme=dark');
});

test('a CookieStore that throws reads as no cookies', async () => {
    globalThis.self = {
        cookieStore: {
            getAll: async () => {
                throw new Error('cookie store unavailable');
            },
        },
    };
    assert.equal(await readCookieHeader(), '');
});
