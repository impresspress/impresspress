/** Minimal fetch `Response` stand-in — deliberately NOT the real `Response`/
 * `Headers` classes, so these tests don't depend on which globals a given
 * test environment happens to provide.
 *
 * The body is single-use, like a real `Response`: reading it twice throws.
 * That matters because the HTTP client has to decide how to read a body
 * BEFORE reading it — a `responseType: "blob"` download whose content-type
 * happens to be `application/json` must not be pre-read as text.
 */
function singleUseBody(
  text: string,
  contentType: string,
  status: number,
  extraHeaders: Record<string, string> = {},
) {
  let used = false;
  const consume = () => {
    if (used) throw new TypeError("Body is unusable: Body has already been read");
    used = true;
    return text;
  };
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: {
      get: (name: string) => {
        const wanted = name.toLowerCase();
        if (wanted === "content-type") return contentType;
        return extraHeaders[wanted] ?? null;
      },
    },
    text: async () => consume(),
    json: async () => JSON.parse(consume()),
    blob: async () => new Blob([consume()], { type: contentType }),
  };
}

export function fakeJsonResponse(body: unknown, status = 200) {
  return singleUseBody(JSON.stringify(body), "application/json", status);
}

/**
 * A response the server really sends, recorded as `{status, headers, body}`
 * with lower-case header names and the body as its exact text. A Rust test
 * compares each such fixture with what the server answers, so a mock built
 * from one cannot drift from the server the way a hand-written body can.
 */
export interface RecordedResponse {
  status: number;
  headers: Record<string, string>;
  body: string;
}

export function recordedResponse(recorded: RecordedResponse) {
  const { "content-type": contentType = "", ...rest } = recorded.headers;
  return singleUseBody(recorded.body, contentType, recorded.status, rest);
}

/** Fake raw-bytes response, e.g. for `GET .../objects/{key}` (not JSON). */
export function fakeBlobResponse(content: string, contentType = "text/plain", status = 200) {
  return singleUseBody(content, contentType, status);
}

/**
 * A `fetch` stand-in that never settles on its own — it only rejects when the
 * `AbortSignal` it was handed fires, exactly as a real `fetch` does. Lets a
 * test prove whether a timeout timer was armed at all.
 */
export function hangingFetch() {
  const captured: { signal?: AbortSignal } = {};
  const fetchFn = (_url: string, init: RequestInit) => {
    captured.signal = init.signal ?? undefined;
    return new Promise<never>((_resolve, reject) => {
      init.signal?.addEventListener(
        "abort",
        () => reject(new DOMException("The operation was aborted.", "AbortError")),
        { once: true },
      );
    });
  };
  return { fetchFn, captured };
}
