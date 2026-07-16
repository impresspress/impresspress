/** Minimal fetch `Response` stand-in — deliberately NOT the real `Response`/
 * `Headers` classes, so these tests don't depend on which globals a given
 * test environment happens to provide. */
export function fakeJsonResponse(body: unknown, status = 200) {
  const text = JSON.stringify(body);
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: {
      get: (name: string) => (name.toLowerCase() === "content-type" ? "application/json" : null),
    },
    text: async () => text,
    json: async () => JSON.parse(text),
  };
}

/** Fake raw-bytes response, e.g. for `GET .../objects/{key}` (not JSON). */
export function fakeBlobResponse(content: string, contentType = "text/plain", status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: {
      get: (name: string) => (name.toLowerCase() === "content-type" ? contentType : null),
    },
    text: async () => content,
    blob: async () => new Blob([content], { type: contentType }),
  };
}
