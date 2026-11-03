// The D1 outage toggle and call ledger the `DB` wrapper (`d1-outage.mjs`)
// asks before every D1 call, and the ledger of KV calls the `CONFIG_CACHE`
// wrapper (`kv-ledger.mjs`) reports. One isolate, so module state is the state; a
// harness restart (a cold fixture isolate) starts it again at mode `ok`, and
// the harness sets the mode it wants after each start.
//
//   POST /gate          the wrapper's question: counts the call and answers
//                       `{ error }`, null in mode `ok`
//   PUT  /mode          body: one of the MODES keys
//   GET  /calls         `{ mode, calls: [{ op, statements, refused }] }`
//   POST /kv            the KV wrapper's report: `{ op, key }`, recorded
//   GET  /kv            `{ kv: [{ op, key }] }`

const MODES = {
  ok: null,
  // The text D1 returned in the 2026-10-30 outage, with D1's prefix
  // (`impresspress-core`'s `sqlite_text_error` tests quote it).
  quota:
    "D1_ERROR: Your account has exceeded D1's free tier daily row read limit. " +
    "Upgrade to a paid plan or wait until tomorrow (midnight UTC) to continue. " +
    "See https://developers.cloudflare.com/d1/platform/limits/ for more details.",
  // A permanent fault: a statement against a table that does not exist.
  "no-such-table": "D1_ERROR: no such table: variables: SQLITE_ERROR",
};

let mode = "ok";
const calls = [];
const kv = [];

export default {
  async fetch(request) {
    const { pathname } = new URL(request.url);
    if (pathname === "/gate" && request.method === "POST") {
      const { op, statements } = await request.json();
      const error = MODES[mode];
      calls.push({ op, statements, refused: error !== null });
      return Response.json({ error });
    }
    if (pathname === "/mode" && request.method === "PUT") {
      const next = await request.text();
      if (!(next in MODES)) {
        return new Response(`unknown mode ${next}`, { status: 400 });
      }
      mode = next;
      return new Response(mode);
    }
    if (pathname === "/kv" && request.method === "POST") {
      kv.push(await request.json());
      return new Response(null, { status: 204 });
    }
    if (pathname === "/kv" && request.method === "GET") {
      return Response.json({ kv });
    }
    if (pathname === "/calls" && request.method === "GET") {
      return Response.json({ mode, calls });
    }
    return new Response("not found", { status: 404 });
  },
};
