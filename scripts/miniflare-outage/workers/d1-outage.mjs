// The `DB` binding the fixture Worker sees: Miniflare's real local D1
// (`env.INNER`), behind the same JS surface a D1 binding has — a
// `D1Database` (worker-rs checks the class name) whose `prepare`/`bind` are
// synchronous and whose `all`/`run`/`first`/`raw`/`batch`/`exec`/`dump` are
// the calls D1 is sent.
//
// Each call first asks the control Worker (`d1-control.mjs`, over the
// `CONTROL` service binding) whether it fails, telling it the call's
// statements and their bound values, which also counts it. A call that does
// not fail goes to the real D1; one that does throws the error text the mode
// names, as D1 would, and the real D1 is not reached. A wrapped binding
// runs inside the fixture's own isolate and has no route of its own, which is
// why the toggle lives in a separate Worker.

class D1PreparedStatement {
  constructor(gate, inner, sql, binds = []) {
    this.gate = gate;
    this.inner = inner;
    this.sql = sql;
    this.binds = binds;
  }
  bind(...values) {
    return new D1PreparedStatement(this.gate, this.inner.bind(...values), this.sql, values);
  }
  /** What the control Worker is told of this statement. */
  get described() {
    return { sql: this.sql, binds: this.binds };
  }
  async all(...args) {
    await this.gate("all", [this.described]);
    return this.inner.all(...args);
  }
  async run(...args) {
    await this.gate("run", [this.described]);
    return this.inner.run(...args);
  }
  async first(...args) {
    await this.gate("first", [this.described]);
    return this.inner.first(...args);
  }
  async raw(...args) {
    await this.gate("raw", [this.described]);
    return this.inner.raw(...args);
  }
}

class D1Database {
  constructor(gate, inner) {
    this.gate = gate;
    this.inner = inner;
  }
  prepare(sql) {
    return new D1PreparedStatement(this.gate, this.inner.prepare(sql), sql);
  }
  async batch(statements) {
    await this.gate("batch", statements.map((s) => s.described));
    return this.inner.batch(statements.map((s) => s.inner));
  }
  async exec(sql) {
    await this.gate("exec", [{ sql, binds: [] }]);
    return this.inner.exec(sql);
  }
  async dump() {
    await this.gate("dump", []);
    return this.inner.dump();
  }
}

export default function (env) {
  const gate = async (op, statements) => {
    const answer = await env.CONTROL.fetch("http://d1-control/gate", {
      method: "POST",
      body: JSON.stringify({ op, statements }),
    });
    const { error } = await answer.json();
    if (error !== null) {
      throw new Error(error);
    }
  };
  return new D1Database(gate, env.INNER);
}
