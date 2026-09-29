// Run with: node --import ./js/test/node-hooks.mjs --test js/test/http_fetch_stream.test.mjs
// (see node-hooks.mjs's header comment for why the --import hook is needed).
//
// The streaming half of the outbound-request bridge. Two properties cannot be
// asserted from Rust, because `bridge::http_fetch_stream` and
// `bridge::reader_next_chunk` are wasm-bindgen extern imports: the shape of the
// `init` handed to `fetch` (the streaming path must carry the SAME
// `redirect: 'error'` the buffered one does — it is half of the SSRF gate), and
// the reader registry's end-of-stream and unknown-id contracts, which the Rust
// consumer relies on to tell "finished" from "you asked about a stream that
// does not exist".
import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { httpFetchStream, readerNextChunk, readerCancel } from '../bridge.js';

const originalFetch = globalThis.fetch;

afterEach(() => {
    globalThis.fetch = originalFetch;
});

/** A `ReadableStream` over `chunks`, like a real response body. */
function bodyOf(chunks) {
    let i = 0;
    return new ReadableStream({
        pull(controller) {
            if (i < chunks.length) {
                controller.enqueue(chunks[i++]);
            } else {
                controller.close();
            }
        },
    });
}

function stubFetch(response) {
    const calls = [];
    globalThis.fetch = async (url, init) => {
        calls.push({ url, init });
        return response;
    };
    return calls;
}

test('httpFetchStream refuses to follow redirects, exactly like the buffered path', async () => {
    const calls = stubFetch({ status: 200, headers: new Headers(), body: null });

    await httpFetchStream('GET', 'https://example.com/x', '{}', new Uint8Array());

    assert.equal(calls.length, 1);
    assert.equal(
        calls[0].init.redirect,
        'error',
        'the URL gate only sees the first URL; a followed 3xx would reach one it never saw',
    );
});

test('httpFetchStream sends the request headers and body it was given', async () => {
    const calls = stubFetch({ status: 200, headers: new Headers(), body: null });

    await httpFetchStream(
        'POST',
        'https://example.com/x',
        '{"authorization":"Bearer t"}',
        new Uint8Array([1, 2, 3]),
    );

    assert.equal(calls[0].init.method, 'POST');
    assert.deepEqual(calls[0].init.headers, { authorization: 'Bearer t' });
    assert.deepEqual([...calls[0].init.body], [1, 2, 3]);
});

test('the response head comes back with every value of a repeated header', async () => {
    const headers = new Headers();
    headers.append('set-cookie', 'session=abc; Path=/');
    headers.append('content-type', 'text/html');
    headers.append('set-cookie', 'csrf=xyz; Path=/');
    stubFetch({ status: 200, headers, body: bodyOf([]) });

    const head = await httpFetchStream('GET', 'https://example.com/x', '{}', new Uint8Array());

    assert.equal(head.status, 200);
    const cookies = head.headers.filter(([name]) => name === 'set-cookie').map(([, v]) => v);
    assert.deepEqual(cookies, ['session=abc; Path=/', 'csrf=xyz; Path=/']);
    await readerCancel(head.stream_id);
});

test('a bodyless response reports stream_id null rather than failing', async () => {
    stubFetch({ status: 204, headers: new Headers(), body: null });

    const head = await httpFetchStream('GET', 'https://example.com/x', '{}', new Uint8Array());

    assert.equal(head.status, 204);
    assert.equal(
        head.stream_id,
        null,
        'the buffered path answers an empty body for a 204; this must not be an error either',
    );
});

test('chunks arrive in order and end of stream is null, once', async () => {
    stubFetch({
        status: 200,
        headers: new Headers(),
        body: bodyOf([new Uint8Array([1, 2]), new Uint8Array([3])]),
    });

    const head = await httpFetchStream('GET', 'https://example.com/x', '{}', new Uint8Array());

    assert.deepEqual([...(await readerNextChunk(head.stream_id))], [1, 2]);
    assert.deepEqual([...(await readerNextChunk(head.stream_id))], [3]);
    assert.equal(await readerNextChunk(head.stream_id), null, 'null is the end-of-stream signal');

    // Draining to `null` deregisters the reader, so pulling again is now an
    // unknown id — which must throw, not answer `null`.
    await assert.rejects(
        () => readerNextChunk(head.stream_id),
        /unknown stream id/,
        'a drained stream must not keep answering "finished": that would hide a bookkeeping bug',
    );
});

test('readerCancel releases the stream and is idempotent', async () => {
    let cancelled = false;
    const body = new ReadableStream({
        pull(controller) {
            controller.enqueue(new Uint8Array([7]));
        },
        cancel() {
            cancelled = true;
        },
    });
    stubFetch({ status: 200, headers: new Headers(), body });

    const head = await httpFetchStream('GET', 'https://example.com/x', '{}', new Uint8Array());
    assert.deepEqual([...(await readerNextChunk(head.stream_id))], [7]);

    await readerCancel(head.stream_id);
    assert.ok(cancelled, 'the underlying source must be released, not just forgotten');

    // Called again from a second Rust error path: must not throw.
    await readerCancel(head.stream_id);
    await readerCancel('bytes-does-not-exist');
});

test('a read failure rejects and deregisters, so it cannot be retried forever', async () => {
    const body = new ReadableStream({
        pull() {
            throw new Error('connection reset');
        },
    });
    stubFetch({ status: 200, headers: new Headers(), body });

    const head = await httpFetchStream('GET', 'https://example.com/x', '{}', new Uint8Array());

    await assert.rejects(() => readerNextChunk(head.stream_id), /connection reset/);
    await assert.rejects(() => readerNextChunk(head.stream_id), /unknown stream id/);
});
