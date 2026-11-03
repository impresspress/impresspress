// The D1 outage toggle and call ledger the `DB` wrapper (`d1-outage.mjs`)
// asks before every D1 call, and the ledger of KV calls the `CONFIG_CACHE`
// wrapper (`kv-ledger.mjs`) reports. One isolate, so module state is the state; a
// harness restart (a cold fixture isolate) starts it again at mode `ok`, and
// the harness sets the mode it wants after each start.
//
//   POST /gate          the wrapper's question: counts the call and answers
//                       `{ error }`, null when the call does not fail in the
//                       current mode; while held, only once
//                       released, in the mode set then
//   PUT  /mode          body: one of the MODES keys
//   PUT  /hold          hold every gate question from now on
//   PUT  /release       answer the held questions, and stop holding
//   GET  /calls         `{ mode, held, calls: [{ op, statements, binds, refused }] }`:
//                       each call's SQL texts and, beside them, their bound
//                       values; `held` the number of questions waiting
//   POST /kv            the KV wrapper's report: `{ op, key }`, recorded
//   GET  /kv            `{ kv: [{ op, key }] }`

/** The table the fixture's `miniflare/init-reader` block reads at `Init` (`fixture/src/lib.rs`). */
const INIT_READER_TABLE = "miniflare__init_reader__state";

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
  // A reset only the fixture's `miniflare/init-reader` block causes: its
  // `Init` statement fails with D1's CPU-limit reset, every other statement
  // is answered (plan I2g, a block whose own `Init` makes D1 reset).
  "init-reset": {
    only: INIT_READER_TABLE,
    error: "D1_ERROR: D1 DB exceeded its CPU time limit and was reset.",
  },
};

/** The error a call fails with in the current mode, or null. */
function failure(statements) {
  const rule = MODES[mode];
  if (rule === null || typeof rule === "string") return rule;
  const touches = statements.some(
    (s) => s.sql.includes(rule.only) || s.binds.some((b) => b === rule.only),
  );
  return touches ? rule.error : null;
}

let mode = "ok";
const calls = [];
const kv = [];
/**
 * Whether gate questions are held. A held question polls this flag on a
 * timer of its own request: workerd does not run a continuation a different
 * request resolves once the creating one has ended, so no promise is shared.
 */
let holding = false;
let held = 0;

export default {
  async fetch(request) {
    const { pathname } = new URL(request.url);
    if (pathname === "/gate" && request.method === "POST") {
      const { op, statements } = await request.json();
      if (holding) {
        held += 1;
        while (holding) await new Promise((resolve) => setTimeout(resolve, 10));
        held -= 1;
      }
      const error = failure(statements);
      calls.push({
        op,
        statements: statements.map((s) => s.sql),
        binds: statements.map((s) => s.binds),
        refused: error !== null,
      });
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
    if (pathname === "/hold" && request.method === "PUT") {
      holding = true;
      return new Response("holding");
    }
    if (pathname === "/release" && request.method === "PUT") {
      holding = false;
      return new Response("released");
    }
    if (pathname === "/kv" && request.method === "POST") {
      kv.push(await request.json());
      return new Response(null, { status: 204 });
    }
    if (pathname === "/kv" && request.method === "GET") {
      return Response.json({ kv });
    }
    if (pathname === "/calls" && request.method === "GET") {
      return Response.json({ mode, held, calls });
    }
    return new Response("not found", { status: 404 });
  },
};
