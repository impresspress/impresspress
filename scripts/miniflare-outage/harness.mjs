// The Miniflare D1-outage harness: deploys the fixture Worker
// (`fixture/`) the way `impresspress deploy --target cloudflare` does, onto a
// local Miniflare whose `DB` binding can be put into an outage, and gives a
// scenario what it needs to drive and observe it.
//
// The deploy is the CLI's own code (`crates/impresspress/examples/
// miniflare_deploy.rs`): its build (worker-build at the CLI's pinned version),
// its entry module, its release staging and R2 upload, its candidate config;
// then the candidate's real `/_deploy/prepare`, the CLI's decode and packaging
// of the plan, and its final config with the prepared shim. The harness's own
// part is turning a generated wrangler config into Miniflare options
// (`miniflareWorker`), which is what `wrangler versions upload` does in
// production.
//
// What Miniflare does not get, on purpose:
// - the password-hasher Worker. Its Durable Object binding is left out; a
//   missing binding fails only password operations
//   (`impresspress-cloudflare/src/crypto_service.rs`), and no scenario hashes.
//
// `DB` is Miniflare's real local D1 behind a wrapped binding
// (`workers/d1-outage.mjs`) that asks a control Worker
// (`workers/d1-control.mjs`) before each call: the outage toggle, and the
// count of D1 calls a scenario asserts on. `CONFIG_CACHE` is Miniflare's real
// local KV behind a wrapper (`workers/kv-ledger.mjs`) that reports each call
// to the same control Worker.

import { execFile, execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";

import { Miniflare } from "miniflare";
import { parse as parseToml } from "smol-toml";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO = resolve(HERE, "../..");
export const FIXTURE = join(HERE, "fixture");

/** The deploy token and JWT secret every harness deployment binds. */
const DEPLOY_TOKEN = "miniflare-outage-deploy-token";
const JWT_SECRET = "miniflare-outage-jwt-secret-0123456789abcdef0123456789abcdef";

/** The Worker version ids the candidate and the final version carry. */
const VERSION_IDS = {
  candidate: "00000000-0000-4000-8000-00000000c0de",
  final: "00000000-0000-4000-8000-00000000f1a1",
};

/** Placeholders the CLI's config loader requires; nothing reaches Cloudflare. */
const LOADER_ENV = {
  CLOUDFLARE_ACCOUNT_ID: "miniflare-outage-account",
  IMPRESSPRESS_CLOUDFLARE_D1_DATABASE_ID: "miniflare-outage-d1",
};

/**
 * Vars the harness adds to every version: the fixture's shape, the debug log
 * level (whose `Server-Timing` header says whether a request built), and a
 * zero rows threshold, so every invocation that read D1 logs its `d1_cost`.
 */
function harnessVars(shape) {
  return {
    MINIFLARE_FIXTURE_SHAPE: shape,
    IMPRESSPRESS_CF_LOG_LEVEL: "debug",
    IMPRESSPRESS_D1_ROWS_LOG_THRESHOLD: "0",
  };
}

/** worker-build must be the CLI's pinned version: the build installs it globally otherwise. */
function checkWorkerBuild() {
  const source = readFileSync(
    join(REPO, "crates/impresspress/src/cli/helpers/cloudflare/build.rs"),
    "utf8",
  );
  const pinned = /pub const WORKER_BUILD_VERSION: &str = "([^"]+)";/.exec(source)?.[1];
  if (!pinned) throw new Error("WORKER_BUILD_VERSION not found in the CLI's build.rs");
  let found;
  try {
    found = execFileSync("worker-build", ["--version"], { encoding: "utf8" }).trim();
  } catch {
    found = "(not on PATH)";
  }
  if (found !== pinned) {
    throw new Error(
      `worker-build on PATH is ${found}; the CLI pins ${pinned}. Put ` +
        `\`cargo install worker-build --version =${pinned} --root <dir>\`'s <dir>/bin first on PATH.`,
    );
  }
}

const DRIVER = ["run", "--quiet", "--locked", "-p", "impresspress", "--example", "miniflare_deploy", "--"];

/** Run the deploy driver (`miniflare_deploy`), the CLI's own build and packaging. */
function driver(args) {
  execFileSync("cargo", [...DRIVER, ...args], {
    cwd: REPO,
    stdio: "inherit",
    env: { ...process.env, ...LOADER_ENV },
  });
}

/**
 * Run the deploy driver without blocking this process, for a step that sends
 * requests to the running Worker: Miniflare's log streams are read on this
 * process's event loop, and a blocked loop would stall a Worker that logs.
 * Resolves to `{ ok, output }`, its exit status and combined output.
 */
function driverAsync(args) {
  return new Promise((resolve) => {
    execFile(
      "cargo",
      [...DRIVER, ...args],
      { cwd: REPO, env: { ...process.env, ...LOADER_ENV }, maxBuffer: 16 * 1024 * 1024 },
      (error, stdout, stderr) => resolve({ ok: error === null, output: `${stdout}${stderr}` }),
    );
  });
}

/**
 * Build the fixture and stage its candidate: once per harness run, shared by
 * every scenario. Returns `{ config, r2 }` (`miniflare_deploy candidate`).
 */
export function buildCandidate(stateRoot) {
  checkWorkerBuild();
  const state = join(stateRoot, "build");
  mkdirSync(state, { recursive: true });
  driver(["candidate", FIXTURE, state, VERSION_IDS.candidate]);
  return JSON.parse(readFileSync(join(state, "candidate.json"), "utf8"));
}

// Top-level keys of a generated config, and what Miniflare makes of each.
const HANDLED_KEYS = new Set([
  "main",
  "compatibility_date",
  "compatibility_flags",
  "vars",
  "d1_databases",
  "r2_buckets",
  "kv_namespaces",
  "durable_objects",
  "version_metadata",
  "rules",
]);
// Deployment settings with no meaning to a local isolate.
const IGNORED_KEYS = new Set([
  "name",
  "account_id",
  "workers_dev",
  "preview_urls",
  "observability",
  "triggers",
  "limits",
  "upload_source_maps",
  "keep_vars",
]);

/** Miniflare's module rule for a wrangler `[[rules]]` entry. */
function moduleRule(rule) {
  return { type: rule.type, include: rule.globs, fallthrough: rule.fallthrough ?? false };
}

/**
 * The fixture Worker's Miniflare options for the generated wrangler config at
 * `configPath`, as `versionId`, with `vars` added.
 */
function miniflareWorker(configPath, versionId, vars) {
  const config = parseToml(readFileSync(configPath, "utf8"));
  const unknown = Object.keys(config).filter((k) => !HANDLED_KEYS.has(k) && !IGNORED_KEYS.has(k));
  if (unknown.length > 0) {
    throw new Error(`${configPath}: keys the harness does not map: ${unknown.join(", ")}`);
  }
  const configDir = dirname(configPath);
  const d1 = config.d1_databases ?? [];
  if (d1.length !== 1) throw new Error(`${configPath}: expected one D1 database`);
  const kv = config.kv_namespaces ?? [];
  if (kv.length !== 1) throw new Error(`${configPath}: expected one KV namespace`);
  return {
    worker: {
      name: "fixture",
      modules: true,
      scriptPath: resolve(configDir, config.main),
      modulesRoot: FIXTURE,
      modulesRules: [
        ...(config.rules ?? []).map(moduleRule),
        // wrangler's defaults for a modules Worker, which worker-build's
        // output relies on: ES modules in `.js`, compiled Wasm.
        { type: "ESModule", include: ["**/*.js", "**/*.mjs"] },
        { type: "CompiledWasm", include: ["**/*.wasm"] },
      ],
      compatibilityDate: config.compatibility_date,
      compatibilityFlags: config.compatibility_flags ?? [],
      bindings: {
        ...(config.vars ?? {}),
        ...vars,
        IMPRESSPRESS_DEPLOY_TOKEN: DEPLOY_TOKEN,
        WAFER_RUN__AUTH__JWT_SECRET: JWT_SECRET,
        [config.version_metadata.binding]: {
          id: versionId,
          tag: "",
          timestamp: "2026-11-03T00:00:00.000Z",
        },
      },
      wrappedBindings: {
        [d1[0].binding]: { scriptName: "d1-outage" },
        [kv[0].binding]: { scriptName: "kv-ledger" },
      },
      r2Buckets: Object.fromEntries(
        (config.r2_buckets ?? []).map((b) => [b.binding, b.bucket_name]),
      ),
    },
    databaseId: d1[0].database_id,
    kvNamespaceId: kv[0].id,
    bucket: (config.r2_buckets ?? [])[0],
  };
}

/** Every file under `dir`, relative to it, `/`-separated. */
function filesUnder(dir, prefix = "") {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    const key = prefix ? `${prefix}/${name}` : name;
    return statSync(path).isDirectory() ? filesUnder(path, key) : [key];
  });
}

/** What one fetch answered. */
export class Answer {
  constructor(status, headers, body, sentAt, doneAt) {
    this.status = status;
    this.headers = headers;
    this.body = body;
    /** When the request was sent and its body read, `Date.now()` ms. */
    this.sentAt = sentAt;
    this.doneAt = doneAt;
  }
  get retryAfter() {
    const value = this.headers.get("retry-after");
    return value === null ? null : Number(value);
  }
  /** `Server-Timing`'s cache outcome, e.g. `cold-built (build 1)`; null when absent. */
  get cacheOutcome() {
    const timing = this.headers.get("server-timing");
    return timing === null ? null : (/cache;desc="([^"]*)"/.exec(timing)?.[1] ?? null);
  }
  /** Whether this request built a runtime and served it. */
  get built() {
    return /\(build \d+\)/.test(this.cacheOutcome ?? "");
  }
}

/**
 * One fixture deployment: its own local D1, KV and R2 (persisted under
 * `stateRoot/<name>`), deployed through the candidate and the final version.
 */
export class Deployment {
  constructor({ name, shape, candidate, stateRoot, vars = {} }) {
    this.name = name;
    this.shape = shape;
    this.candidate = candidate;
    this.dir = join(stateRoot, name);
    this.vars = { ...harnessVars(shape), ...vars };
    this.logs = [];
    this.mf = null;
    this.stage = null;
    this.r2Seeded = false;
    this.startVars = {};
    rmSync(this.dir, { recursive: true, force: true });
    mkdirSync(this.dir, { recursive: true });
  }

  /** The config the given stage runs. */
  configFor(stage) {
    if (stage === "candidate") return this.candidate.config;
    return JSON.parse(readFileSync(join(this.dir, "final.json"), "utf8")).config;
  }

  /**
   * Start `stage` ("candidate" or "final") on a fresh workerd process: every
   * isolate cold, D1, KV and R2 as the last process left them. The control
   * Worker starts in mode `ok`; `mode` sets it before any fixture request.
   * `vars` are added to this start's Worker vars only. Every start is a fresh
   * process, so nothing an isolate cached survives it, whatever the vars.
   */
  async start(stage, { mode = "ok", vars = {} } = {}) {
    await this.stop();
    const { worker, databaseId, kvNamespaceId, bucket } = miniflareWorker(
      this.configFor(stage),
      VERSION_IDS[stage],
      { ...this.vars, ...vars },
    );
    this.stage = stage;
    this.startVars = vars;
    this.mf = new Miniflare({
      // A loopback port of its own, so the CLI's deploy smoke (`smoke()`) can
      // reach the Worker over HTTP as it reaches a preview URL.
      host: "127.0.0.1",
      port: 0,
      workers: [
        worker,
        {
          name: "d1-outage",
          modules: true,
          scriptPath: join(HERE, "workers/d1-outage.mjs"),
          d1Databases: { INNER: databaseId },
          serviceBindings: { CONTROL: "d1-control" },
        },
        {
          name: "kv-ledger",
          modules: true,
          scriptPath: join(HERE, "workers/kv-ledger.mjs"),
          kvNamespaces: { INNER: kvNamespaceId },
          serviceBindings: { CONTROL: "d1-control" },
        },
        {
          name: "d1-control",
          modules: true,
          scriptPath: join(HERE, "workers/d1-control.mjs"),
          compatibilityDate: worker.compatibilityDate,
        },
      ],
      d1Persist: join(this.dir, "d1"),
      kvPersist: join(this.dir, "kv"),
      r2Persist: join(this.dir, "r2"),
      handleRuntimeStdio: (stdout, stderr) => {
        for (const stream of [stdout, stderr]) {
          let pending = "";
          stream.on("data", (chunk) => {
            pending += chunk.toString();
            const lines = pending.split("\n");
            pending = lines.pop();
            for (const line of lines) {
              this.logs.push(line);
              if (process.env.MINIFLARE_OUTAGE_VERBOSE) console.log(`  [${this.name}] ${line}`);
            }
          });
        }
      },
    });
    this.url = (await this.mf.ready).toString();
    if (!this.r2Seeded) {
      await this.seedR2(bucket.binding);
      this.r2Seeded = true;
    }
    await this.setMode(mode);
  }

  /** Load what the deploy driver uploaded into Miniflare's R2. */
  async seedR2(binding) {
    const r2 = await this.mf.getR2Bucket(binding);
    const contentTypes = JSON.parse(
      readFileSync(join(this.candidate.r2, "content-types.json"), "utf8"),
    );
    const objects = join(this.candidate.r2, "objects");
    for (const key of filesUnder(objects)) {
      await r2.put(key, readFileSync(join(objects, key)), {
        httpMetadata: { contentType: contentTypes[key] },
      });
    }
  }

  /**
   * A cold isolate of the same version: the workerd process restarted, with
   * the last start's added vars unless `vars` replaces them.
   */
  async restart({ mode = "ok", vars = this.startVars } = {}) {
    await this.start(this.stage, { mode, vars });
  }

  async stop() {
    if (this.mf !== null) {
      await this.mf.dispose();
      this.mf = null;
    }
  }

  /**
   * The deploy funnel: the candidate's `/_deploy/prepare` (migrate, seed,
   * seal the plan), the CLI's packaging of the plan, then the final version on
   * a cold isolate.
   */
  async deploy({ mode = "ok" } = {}) {
    await this.start("candidate");
    const answer = await this.prepare();
    if (answer.status !== 200) {
      throw new Error(`/_deploy/prepare answered ${answer.status}: ${answer.body}\n${this.tail()}`);
    }
    const response = join(this.dir, "prepare-response.json");
    writeFileSync(response, answer.body);
    driver(["final", FIXTURE, this.dir, response]);
    await this.start("final", { mode });
  }

  /** The running version's `/_deploy/prepare`, authorized as a deploy is. */
  async prepare() {
    return this.fetch("/_deploy/prepare", {
      method: "POST",
      headers: { "X-Deploy-Token": DEPLOY_TOKEN },
    });
  }

  async control(path, init) {
    const control = await this.mf.getWorker("d1-control");
    return control.fetch(`http://d1-control${path}`, init);
  }

  /** Put `DB` into `mode`: `ok`, `quota` or `no-such-table`. */
  async setMode(mode) {
    const answer = await this.control("/mode", { method: "PUT", body: mode });
    if (answer.status !== 200) throw new Error(await answer.text());
  }

  /**
   * Hold every D1 call from now on: each waits, before the mode applies,
   * until `release()`. For a request that has to be in flight while another
   * is served.
   */
  async hold() {
    const answer = await this.control("/hold", { method: "PUT" });
    if (answer.status !== 200) throw new Error(await answer.text());
  }

  /** Let the held D1 calls go on (in the mode then set), and stop holding. */
  async release() {
    const answer = await this.control("/release", { method: "PUT" });
    if (answer.status !== 200) throw new Error(await answer.text());
  }

  /** How many D1 calls are waiting on `hold()`. */
  async held() {
    return (await (await this.control("/calls")).json()).held;
  }

  /** Every D1 call the fixture has made since this process started. */
  async calls() {
    return (await (await this.control("/calls")).json()).calls;
  }

  /** Every KV call the fixture has made since this process started. */
  async kvCalls() {
    return (await (await this.control("/kv")).json()).kv;
  }

  /** GET (or `init`) `path` on the fixture Worker. */
  async fetch(path, init = {}) {
    const sentAt = Date.now();
    const response = await this.mf.dispatchFetch(`http://localhost${path}`, {
      redirect: "manual",
      ...init,
    });
    const body = await response.text();
    return new Answer(response.status, response.headers, body, sentAt, Date.now());
  }

  /** `fetch(path)`, and the D1 and KV calls it made. */
  async fetchCounted(path, init) {
    const before = (await this.calls()).length;
    const kvBefore = (await this.kvCalls()).length;
    const answer = await this.fetch(path, init);
    answer.d1Calls = (await this.calls()).slice(before);
    answer.kvCalls = (await this.kvCalls()).slice(kvBefore);
    return answer;
  }

  /** Move the KV config stamp, as an admin settings save does. */
  async bumpConfigStamp() {
    // The real namespace behind the fixture's `CONFIG_CACHE` wrapper.
    const kv = await this.mf.getKVNamespace("INNER", "kv-ledger");
    await kv.put("cfg:v1:config_version", `miniflare-bump-${Date.now()}`);
  }

  /** Wait until the Worker's log has been quiet for `quietMs`. */
  async settleLogs(quietMs = 300) {
    let seen = -1;
    while (seen !== this.logs.length) {
      seen = this.logs.length;
      await sleep(quietMs);
    }
  }

  /** A position in the Worker's log, for `d1Costs(since)` and `logLine`. */
  mark() {
    return this.logs.length;
  }

  /**
   * The first Worker log line since `since` (a `mark()`) that `matches`,
   * waiting for it: the Worker writes a line before it answers, but its
   * stream reaches this process later. The deadline only stops a hung run.
   */
  async logLine(matches, since = 0, { withinMs = 10_000 } = {}) {
    const deadline = Date.now() + withinMs;
    for (;;) {
      const line = this.logs.slice(since).find(matches);
      if (line !== undefined) return line;
      if (Date.now() > deadline) {
        throw new Error(`no Worker log line matched within ${withinMs} ms\n${this.tail()}`);
      }
      await sleep(50);
    }
  }

  /**
   * The CLI's pre-promotion smoke against the running version, as
   * `impresspress deploy` runs it on the final candidate's preview
   * (`miniflare_deploy smoke`): the `/health` GET, then the 160-request
   * concurrency smoke over the config's `deploy_smoke_paths`. Resolves to
   * `{ ok, output }`; `ok` false is a deploy that stops before promotion.
   */
  async smoke() {
    return driverAsync(["smoke", FIXTURE, this.url, DEPLOY_TOKEN]);
  }

  /** The `metric=d1_cost` lines logged since `since` (a `mark()`), parsed. */
  d1Costs(since = 0) {
    return this.logs
      .slice(since)
      .filter((line) => line.includes("metric=d1_cost"))
      .map((line) => Object.fromEntries([...line.matchAll(/(\w+)=(\S+)/g)].map((m) => [m[1], m[2]])));
  }

  /** The last log lines, for a failure message. */
  tail(n = 40) {
    return this.logs.slice(-n).join("\n");
  }
}

/** Sleep past an answer's `Retry-After`, plus `slackMs`. */
export async function waitOut(answer, slackMs = 300) {
  if (answer.retryAfter === null) throw new Error(`no Retry-After on a ${answer.status}`);
  await sleep(answer.retryAfter * 1000 + slackMs);
}

export { sleep };
