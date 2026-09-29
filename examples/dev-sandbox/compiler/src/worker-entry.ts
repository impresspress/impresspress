/**
 * The compiler worker: `new Worker('<version>/worker.js', { type: 'module' })`.
 *
 * It speaks `src/protocol.ts` to the page and drives Rubrc's virtualised rust
 * toolchain underneath. The split between this file and `vfs-runner.ts` is
 * forced by `@oligami/browser_wasi_shim-threads`: the WASI *farm* services
 * calls for every thread of the guest, and the guest's threads block on
 * `Atomics.wait` until it does, so the farm cannot live on the same thread as
 * the guest that calls it. This worker is the farm (and the protocol); the
 * runner worker it spawns is the guest.
 *
 *   page ── protocol ──▶ worker-entry (farm, sysroot, artifact capture)
 *                             │  wasi_ref + dispatch requests
 *                             ▼
 *                        vfs-runner (WASIFarmAnimal + vfs.core.wasm)
 *                             │  thread-spawn
 *                             ▼
 *                        N thread workers ── WASI calls ──▶ farm
 *
 * What the guest is, is a terminal: rubrc composes rustc, cargo, llvm and a
 * shell into one component, and the only way in is to type at session 0 and
 * read what comes back. So `compile` writes the crate's files through the
 * VFS's write-file event, cleans that one package out of `/target`, types a
 * cargo line, waits for the prompt to come back, and asks the shell to
 * `download` the artifact — which is delivered back through the same host
 * bridge as chunks.
 *
 * The VFS is laid out like an export archive: a block lives at
 * `/blocks/<crate>/`, the guest SDK it depends on by path at `/wafer_guest/`,
 * and every build of the session shares one `--target-dir /target`. `init`
 * builds the guest into `/target` once (the warm-up), so a `compile` rebuilds
 * only the block.
 */

/// <reference lib="webworker" />

import {
  Directory,
  Fd,
  File,
  type Inode,
  PreopenDirectory,
} from "@bjorn3/browser_wasi_shim";
import { WASIFarm, wait_async_polyfill } from "@oligami/browser_wasi_shim-threads";
import { createHttpBridge, isHttpBridgeMessage } from "rubrc-lib/http_bridge";
import {
  createChildProcessBridge,
  isChildProcessMessage,
} from "rubrc-lib/child_process_bridge";
import { fetch_compressed_stream } from "rubrc-lib/brotli_stream";
import { parseTar } from "rubrc-lib/parse_tar";
import childProcessWorkerUrl from "rubrc-worker/vfs_bindings/child_process_worker.ts?worker&url";
import VfsRunner from "./vfs-runner.ts?worker";
import type {
  Diagnostic,
  PageMessage,
  ProgressStage,
  ResultMessage,
  Severity,
  WorkerMessage,
} from "./protocol";
import { stripAnsi } from "./ansi";

wait_async_polyfill();

/** The shell's prompt is `<cwd> $ `; a bare `$ ` tail means it is idle. */
const PROMPT = "$ ";
/** `input_string` with this session id is the VFS's write-file event. */
const WRITE_FILE_SESSION = 0xeeeeeeee;
const SESSION = 0;
/**
 * The worker's hard backstop, NOT the sandbox's compile budget.
 *
 * The page owns the policy — Plan 0 budgets a compile at 120 s and the adapter
 * enforces it by sending `cancel` and terminating the worker. This ceiling
 * exists only so that a compile which has genuinely wedged (a shell that never
 * returns its prompt) fails with a message instead of holding the worker for
 * ever, and it is deliberately far above any budget a caller would set.
 */
const COMPILE_TIMEOUT_MS = 10 * 60 * 1000;
/** Same idea for init: the sysroot is 18.9 MB of tar streamed into the VFS. */
const SYSROOT_TIMEOUT_MS = 5 * 60 * 1000;
/** The only target the sandbox builds for, and the sysroot we vendor. */
const SYSROOT_TRIPLE = "wasm32-wasip1";

// ---------------------------------------------------------------- terminal

/**
 * Everything session 0 has printed since the last `reset()`.
 *
 * Both the runner and the farm see `terminalWrite` — the shell's main thread
 * calls it through the component's own import, its session threads through
 * the farm — so both funnel into this one buffer, in arrival order.
 */
class Transcript {
  private text = "";
  private waiters: { test: (text: string) => boolean; resolve: (text: string) => void }[] = [];

  append(chunk: string) {
    this.text += chunk;
    for (const waiter of [...this.waiters]) {
      if (waiter.test(this.text)) {
        this.waiters.splice(this.waiters.indexOf(waiter), 1);
        waiter.resolve(this.text);
      }
    }
  }

  reset() {
    this.text = "";
  }

  /** Resolves once `test` holds; rejects after `timeoutMs`. */
  wait(test: (text: string) => boolean, timeoutMs: number, what: string): Promise<string> {
    if (test(this.text)) return Promise.resolve(this.text);
    return new Promise((resolve, reject) => {
      const waiter = { test, resolve };
      this.waiters.push(waiter);
      setTimeout(() => {
        const at = this.waiters.indexOf(waiter);
        if (at === -1) return;
        this.waiters.splice(at, 1);
        reject(new Error(`timed out after ${timeoutMs}ms waiting for ${what}`));
      }, timeoutMs);
    });
  }
}

const transcript = new Transcript();
/** fd 2 of the guest, which is not the shell's own session stream. */
let stderrText = "";

const idle = (text: string) => stripAnsi(text).endsWith(PROMPT);

// -------------------------------------------------------------- the runner

let runner: Worker | undefined;
let vfsReady: { resolve: () => void; reject: (e: Error) => void } | undefined;

const runnerReady = new Promise<void>((resolve, reject) => {
  vfsReady = { resolve, reject };
});

const startRunner = (wasiRef: unknown) => {
  const started: Worker = new VfsRunner();
  runner = started;
  started.addEventListener("message", (event: MessageEvent) => {
    const data = event.data;
    switch (data?.type) {
      case "terminal":
        transcript.append(new TextDecoder().decode(toBytes(data.data)));
        break;
      case "progress":
        postProgress(currentId, data.stage as ProgressStage, {
          loaded: data.loaded,
          total: data.total,
          detail: data.detail,
        });
        break;
      case "ready":
        vfsReady?.resolve();
        break;
      case "error":
        vfsReady?.reject(new Error(String(data.message)));
        break;
    }
  });
  started.postMessage({ type: "start", wasi_ref: wasiRef });
};

const send = (line: string) => {
  runner?.postMessage({ type: "input", sessionId: SESSION, data: line });
};

const writeFile = (path: string, content: string) => {
  runner?.postMessage({
    type: "input",
    sessionId: WRITE_FILE_SESSION,
    data: JSON.stringify({ path, content }),
  });
};

/**
 * Write a crate under `root`.
 *
 * Nothing is deleted first, so a file an earlier compile wrote under
 * `/blocks/<crate>/src/` and this one no longer has stays on disk. That is
 * harmless: rustc compiles only the files reached from `lib.rs` through
 * `mod`, and every file the block still has is rewritten here — a leftover
 * is a file nothing names.
 */
const writeCrate = (root: string, files: Record<string, string>) => {
  for (const [path, content] of Object.entries(files)) {
    writeFile(`${root}/${path.replace(/^\/+/, "")}`, content);
  }
};

/**
 * Type a line at session 0 and hand back what it printed.
 *
 * The prompt is the only completion signal the shell offers, so the transcript
 * is cleared first: a stale prompt in the buffer would otherwise read as "this
 * command is already done".
 */
const runCommand = async (line: string, timeoutMs: number, what: string): Promise<string> => {
  await transcript.wait(idle, timeoutMs, `the shell prompt before \`${line}\``);
  transcript.reset();
  send(`${line}\r`);
  const raw = await transcript.wait(idle, timeoutMs, what);
  const text = stripAnsi(raw);
  // Drop the echoed command line and the prompt that closes the output.
  const body = text.slice(text.indexOf("\n") + 1);
  return body.slice(0, body.lastIndexOf("\n") + 1);
};

/** The flags every cargo invocation on a block shares. */
const blockFlags = (crateName: string, target: string, release: boolean) =>
  `${release ? " --release" : ""} --manifest-path /blocks/${crateName}/Cargo.toml ` +
  `--target-dir /target --target ${target}`;

/**
 * Force the block's own package to be rebuilt, and nothing else.
 *
 * Cargo decides freshness by comparing source mtimes with the last build's,
 * and in rubrc's VFS those cannot be trusted: the write-file event does not
 * move a file's mtime, and the VFS's timestamps are nanosecond-scale
 * counters rather than times. A crate with no dependencies came back
 * `Fresh` — the previous module, for code that had just been edited — and
 * `touch` did not change that (tried 2026-09-30). A block that depends on
 * `wafer_guest` happens to rebuild every time only because those counters
 * make cargo think the guest was rebuilt, which is nothing to rely on.
 * `cargo clean -p <crate>` is cargo's own way to rebuild one package: it
 * removes that package's artifacts and leaves `/wafer_guest`'s build in
 * `/target` alone, so the guest is still compiled once per session. The
 * upstream fix is real file timestamps in rubrc's VFS; with those, this
 * could go.
 */
const cleanBlock = async (crateName: string, target: string, release: boolean) =>
  runCommand(
    `cargo clean -p ${crateName}${blockFlags(crateName, target, release)}`,
    COMPILE_TIMEOUT_MS,
    `cargo clean -p ${crateName}`,
  );

const buildCommand = (crateName: string, target: string, release: boolean) =>
  `cargo build${blockFlags(crateName, target, release)} --message-format=json`;

/** Where cargo leaves a block's module, and what `download` reads. */
const artifactPath = (crateName: string, target: string, release: boolean) =>
  `/target/${target}/${release ? "release" : "debug"}/${crateName.replace(/-/g, "_")}.wasm`;

/**
 * Remove the block's previous module before building it again.
 *
 * `/target` is kept for the whole session, so the last build's module is
 * still there when the next build starts — and this toolchain's cargo does
 * not report every failure: `build-finished` says `success: true` after
 * rustc has failed (rustc's status never reaches cargo), and a build that
 * dies before cargo emits any JSON can leave no diagnostic at all (a path
 * dependency that is not there prints neither JSON nor a `-->` span). Either
 * way the previous module would be downloaded and answered
 * as this compile's — a green build of code nobody wrote. With it removed,
 * a build that fails for ANY reason leaves nothing at `artifactPath`, and
 * `download` finding nothing is answered `success: false`. `cleanBlock`
 * does not cover this: a manifest cargo cannot resolve fails the clean as
 * well as the build, and the module stays. This works around rubrc (cargo's
 * `build-finished` does not reflect a rustc failure); the root-cause fix is
 * upstream and belongs to the next pin bump. `-f` because the first build
 * of a block has nothing to remove.
 */
const removeArtifact = async (crateName: string, target: string, release: boolean) =>
  runCommand(
    `rm -f ${artifactPath(crateName, target, release)}`,
    COMPILE_TIMEOUT_MS,
    `rm for ${crateName}`,
  );

/**
 * `download` a file out of the VFS and hand back its bytes, if it exists.
 *
 * `download` prints "File not found" and streams nothing when the path is
 * wrong, so the name the bridge reported is checked rather than assumed:
 * chunks left over from an earlier request must never be served as this
 * request's artifact.
 */
const readArtifact = async (path: string): Promise<{ bytes?: ArrayBuffer; output: string }> => {
  downloadChunks = [];
  downloadName = "";
  const output = await runCommand(`download ${path}`, COMPILE_TIMEOUT_MS, "download");
  let bytes: ArrayBuffer | undefined;
  if (downloadChunks.length > 0 && downloadName === path) {
    const total = downloadChunks.reduce((n, c) => n + c.byteLength, 0);
    const joined = new Uint8Array(total);
    let at = 0;
    for (const chunk of downloadChunks) {
      joined.set(chunk, at);
      at += chunk.byteLength;
    }
    bytes = joined.buffer;
  }
  downloadChunks = [];
  downloadName = "";
  return { bytes, output };
};

// ------------------------------------------------------------ the WASI farm

/** A file descriptor that collects what the guest writes into a string. */
class CapturedFd extends Fd {
  constructor(private readonly sink: (text: string) => void) {
    super();
  }
  fd_write(data: Uint8Array) {
    this.sink(new TextDecoder().decode(data));
    return { ret: 0, nwritten: data.byteLength };
  }
  fd_seek() {
    return { ret: 8, offset: 0n }; // ERRNO_BADF
  }
  fd_filestat_get() {
    return { ret: 8, filestat: null };
  }
}

const toBytes = (data: unknown): Uint8Array => {
  if (data instanceof Uint8Array) return data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  if (Array.isArray(data)) return new Uint8Array(data);
  if (data && typeof data === "object") {
    return new Uint8Array(Object.values(data as Record<string, number>));
  }
  return new Uint8Array();
};

const toMap = (entries: [string, Inode][]) => new Map<string, Inode>(entries);

/**
 * The filesystem the guest starts from.
 *
 * `/sysroot` is filled by `load_sysroot`, and the crates arrive through the
 * write-file event, which creates `/wafer_guest` and `/blocks/<crate>` as it
 * goes, so this is only the skeleton: an empty cargo config (cargo insists on
 * one) and the directory the sysroot is loaded into.
 */
const rootDir = new PreopenDirectory(
  "/",
  toMap([
    ["sysroot", new Directory([])],
    [".cargo", new Directory(toMap([["config.toml", new File(new Uint8Array())]]))],
  ]),
);

/** Chunks of the file the shell's `download` command is streaming out. */
let downloadChunks: Uint8Array[] = [];
let downloadName = "";

/** The sysroot tar, one entry at a time, as the guest pulls it. */
type SysrootEntry = { name: Uint8Array; data: Uint8Array; is_directory: boolean };
let sysrootQueue: SysrootEntry[] = [];
let sysrootCurrent: SysrootEntry | null = null;

/**
 * The vendored sysroot, NOT `oligamiq.github.io`.
 *
 * Rubrc fetches `https://oligamiq.github.io/rust_wasm/v0.2.0/<triple>.tar.br`
 * at runtime (`lib/src/sysroot.ts`). A sandbox that compiles code cannot have
 * its standard library come from a host we do not control, so
 * `build-compiler.sh` vendors the tarball into `<version>/sysroot/` with its
 * sha256 in `PIN.json`, and this resolves it from there. `import.meta.url` is
 * the worker chunk, which lives in `<version>/`.
 */
//
// `assetBase` is not a pointless alias. Vite rewrites a `new URL` whose first
// argument is a template literal into a lookup in a glob map built at BUILD
// time — and `sysroot/` does not exist until `build-compiler.sh` vendors the
// tarball, so that map is empty and the URL resolves to `undefined`. Reading
// `import.meta.url` through a variable keeps the resolution at runtime, where
// the file exists. Do not inline it.
const assetBase = import.meta.url;
const sysrootUrl = (triple: string) => new URL(`./sysroot/${triple}.tar.br`, assetBase).href;

const loadSysrootQueue = async (triple: string) => {
  sysrootQueue = [];
  const stream = await fetch_compressed_stream(sysrootUrl(triple));
  await parseTar(stream, (file: { name: string; data?: Uint8Array; type: string }) => {
    sysrootQueue.push({
      name: new TextEncoder().encode(file.name),
      data: file.data ?? new Uint8Array(),
      is_directory: file.type === "directory",
    });
  });
};

let farm: WASIFarm;

// No registry. A block's only dependency is the guest SDK, by path (see the
// templates), so an outbound request from the toolchain is a block doing
// something it cannot do, not a fetch to proxy: refusing it keeps the sandbox
// offline by construction rather than by policy. Rubrc's own page points this
// at a crates.io proxy worker instead.
const httpBridge = createHttpBridge((input: RequestInfo | URL) =>
  Promise.reject(new Error(`the sandbox toolchain has no network access: ${String(input)}`)),
);

const childBridge = createChildProcessBridge({
  getWasiRef: () => farm.get_ref(),
  workerUrl: childProcessWorkerUrl,
  filesystemRoot: rootDir.dir,
  uploadTimeoutMs: 30_000,
  executionTimeoutMs: 120_000,
});

farm = new WASIFarm(
  new CapturedFd(() => {}),
  new CapturedFd((text) => transcript.append(text)),
  new CapturedFd((text) => {
    stderrText += text;
  }),
  [rootDir],
  {
    allocator_size: 100 * 1024 * 1024,
    base_call_allocator_size: 64 * 1024 * 1024,
    // The shim types this argument as `unknown`, which is honest: it is
    // whatever the guest asked the host to do. Every branch below is keyed on
    // the name, so it is narrowed once, here.
    unknown_fn: async (message: unknown) => {
      const unknown = message as { name: string; args: Record<string, unknown> };
      if (isHttpBridgeMessage(unknown)) return await httpBridge(unknown);
      if (isChildProcessMessage(unknown)) return await childBridge(unknown);

      switch (unknown.name) {
        case "terminalWrite":
          transcript.append(new TextDecoder().decode(toBytes(unknown.args.data)));
          return;

        case "downloadFileStart":
          downloadName = String(unknown.args.name ?? "");
          downloadChunks = [];
          return;
        case "downloadFileChunk":
          downloadChunks.push(toBytes(unknown.args.data));
          return;
        case "downloadFileEnd":
          return;

        case "sysrootStartFetch":
          await loadSysrootQueue(String(unknown.args.triple));
          return {};
        case "sysrootGetNextFileMeta": {
          sysrootCurrent = sysrootQueue.shift() ?? null;
          if (!sysrootCurrent) return { has_file: false, name_len: 0, data_len: 0 };
          return {
            has_file: true,
            name_len: sysrootCurrent.name.length,
            data_len: sysrootCurrent.is_directory ? -1 : sysrootCurrent.data.length,
          };
        }
        case "sysrootReadFileName":
          if (!sysrootCurrent) throw new Error("sysrootReadFileName without a current file");
          return { name: Array.from(sysrootCurrent.name) };
        case "sysrootReadFileChunk": {
          if (!sysrootCurrent) return { chunk: [] };
          const want = Number(unknown.args.chunk_len ?? 0);
          const chunk = sysrootCurrent.data.slice(0, want);
          sysrootCurrent.data = sysrootCurrent.data.slice(want);
          return { chunk: Array.from(chunk) };
        }

        default:
          console.warn("[compiler] unhandled host call", unknown);
          return;
      }
    },
  },
);

// -------------------------------------------------------------- diagnostics

/**
 * Split a build's transcript into diagnostics, what rustc rendered, and the
 * lines that were not cargo's JSON protocol.
 *
 * `--message-format=json` is tried first and is what actually runs (see the
 * README's confirmed list). The regex is the fallback for a build that dies
 * before cargo emits any JSON at all — a malformed `Cargo.toml`, say.
 */
const TEXT_DIAGNOSTIC =
  /^(error|warning)(?:\[(E\d+)\])?: (.*)\n\s+--> ([^:\n]+):(\d+):(\d+)/gm;

type ParsedBuild = {
  diagnostics: Diagnostic[];
  /** rustc's own rendering of each diagnostic, exactly as it would print it. */
  rendered: string[];
  /** Transcript lines that were not cargo's JSON protocol: its status output. */
  plain: string[];
  fromJson: boolean;
  /** `build-finished`'s verdict, when cargo got far enough to give one. */
  buildFinished?: boolean;
};

const parseBuild = (text: string): ParsedBuild => {
  const diagnostics: Diagnostic[] = [];
  const rendered: string[] = [];
  const plain: string[] = [];
  let fromJson = false;
  let buildFinished: boolean | undefined;

  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("{")) {
      plain.push(line);
      continue;
    }
    let value: Record<string, unknown>;
    try {
      value = JSON.parse(trimmed);
    } catch {
      // A line that opens with `{` but is not JSON is human output.
      plain.push(line);
      continue;
    }
    if (value.reason === "build-finished") {
      fromJson = true;
      buildFinished = Boolean(value.success);
      continue;
    }
    if (value.reason !== "compiler-message") continue;
    fromJson = true;
    const message = value.message as {
      level?: string;
      message?: string;
      rendered?: string;
      code?: { code?: string } | null;
      spans?: {
        file_name?: string;
        line_start?: number;
        column_start?: number;
        is_primary?: boolean;
      }[];
    };
    const level = message?.level;
    if (level !== "error" && level !== "warning") continue;
    if (message.rendered) rendered.push(message.rendered.replace(/\n$/, ""));
    const span = message.spans?.find((s) => s.is_primary) ?? message.spans?.[0];
    diagnostics.push({
      file: span?.file_name ?? "",
      line: span?.line_start ?? 0,
      column: span?.column_start ?? 0,
      severity: level as Severity,
      message: message.message ?? "",
      ...(message.code?.code ? { code: message.code.code } : {}),
    });
  }

  if (fromJson) return { diagnostics, rendered, plain, fromJson, buildFinished };

  TEXT_DIAGNOSTIC.lastIndex = 0;
  for (const match of text.matchAll(TEXT_DIAGNOSTIC)) {
    diagnostics.push({
      file: match[4],
      line: Number(match[5]),
      column: Number(match[6]),
      severity: match[1] as Severity,
      message: match[3],
      ...(match[2] ? { code: match[2] } : {}),
    });
  }
  return { diagnostics, rendered, plain, fromJson };
};

// ------------------------------------------------------------ the protocol

/**
 * `new` -> `initializing` -> `ready` <-> `compiling`, and `broken` from any of
 * them. **`broken` is terminal**: the worker never leaves it, because the only
 * two ways in are a failed `init` and a `cancel`, and after a `cancel` a
 * session thread is still sitting inside cargo where nothing outside it can
 * reach. The adapter's answer to `broken` is `terminate()` plus a fresh
 * worker, not another request.
 */
type State = "new" | "initializing" | "ready" | "compiling" | "broken";
let state: State = "new";
/** The id of the compile that is running, if one is. */
let inFlight: string | undefined;
/**
 * Compiles already answered with `cancelled: true`.
 *
 * The build they belong to keeps running (nothing can stop it), so when it
 * finally settles its `result` and any late `progress` are dropped: one
 * request gets exactly one terminal message.
 */
const cancelledIds = new Set<string>();
let currentId = "";
let rustcVersion = "";

const post = (message: WorkerMessage, transfer: Transferable[] = []) => {
  (globalThis as unknown as Worker).postMessage(message, transfer);
};

const postProgress = (
  id: string,
  stage: ProgressStage,
  extra: { loaded?: number; total?: number; detail?: string } = {},
) => {
  if (cancelledIds.has(id)) return;
  post({ type: "progress", id, stage, ...extra });
};

const init = async (message: Extract<PageMessage, { type: "init" }>) => {
  const id = message.id;
  state = "initializing";
  currentId = id;
  postProgress(id, "download", { loaded: 0, total: 0 });
  startRunner(farm.get_ref());
  await runnerReady;

  postProgress(id, "initializing", { detail: "waiting for the shell" });
  await transcript.wait(idle, SYSROOT_TIMEOUT_MS, "the shell's first prompt");

  postProgress(id, "initializing", { detail: "loading the sysroot" });
  await runCommand(`load_sysroot ${SYSROOT_TRIPLE}`, SYSROOT_TIMEOUT_MS, "load_sysroot");

  rustcVersion = (await runCommand("rustc --version", SYSROOT_TIMEOUT_MS, "rustc --version")).trim();

  if (message.guest) {
    postProgress(id, "initializing", { detail: "writing the guest crate" });
    writeCrate("/wafer_guest", message.guest.files);
    writeCrate(`/blocks/${message.guest.warmup.crateName}`, message.guest.warmup.files);
    await cleanBlock(message.guest.warmup.crateName, SYSROOT_TRIPLE, true);
    await removeArtifact(message.guest.warmup.crateName, SYSROOT_TRIPLE, true);
    postProgress(id, "initializing", { detail: "building wafer_guest once for this session" });
    const output = await runCommand(
      buildCommand(message.guest.warmup.crateName, SYSROOT_TRIPLE, true),
      COMPILE_TIMEOUT_MS,
      "the warm-up build",
    );
    const { diagnostics, buildFinished, plain } = parseBuild(output);
    const firstError = diagnostics.find((d) => d.severity === "error");
    if (buildFinished === false || firstError) {
      throw new Error(
        `the guest crate does not build on this toolchain` +
          (firstError ? `: ${firstError.file}:${firstError.line}: ${firstError.message}` : ""),
      );
    }
    // No error was reported, which on this toolchain does not mean the build
    // worked (see `removeArtifact`): the warm-up's module has to be there.
    const warmupPath = artifactPath(message.guest.warmup.crateName, SYSROOT_TRIPLE, true);
    const { bytes, output: downloadOutput } = await readArtifact(warmupPath);
    if (!bytes) {
      throw new Error(
        `the guest crate does not build on this toolchain: the warm-up left no ${warmupPath} ` +
          `(\`download\` said: ${downloadOutput.trim() || "nothing"}); cargo said: ` +
          (plain.join("\n").trim() || "nothing"),
      );
    }
  }

  state = "ready";
  post({ type: "ready", id, rustcVersion });
};

const compile = async (message: Extract<PageMessage, { type: "compile" }>) => {
  const started = Date.now();
  state = "compiling";
  inFlight = message.id;
  currentId = message.id;
  stderrText = "";
  /** Shell output that is not the build's own: `cargo clean -p`, `rm`, `download`. */
  let shellLog = "";
  const builtPath = artifactPath(message.crateName, message.target, message.release);

  writeCrate(`/blocks/${message.crateName}`, message.files);
  shellLog += await cleanBlock(message.crateName, message.target, message.release);
  shellLog += await removeArtifact(message.crateName, message.target, message.release);
  const output = await runCommand(
    buildCommand(message.crateName, message.target, message.release),
    COMPILE_TIMEOUT_MS,
    "cargo build",
  );
  const { diagnostics, rendered, plain, buildFinished } = parseBuild(output);
  const errored = diagnostics.some((d) => d.severity === "error");
  // An error diagnostic fails the build whatever cargo concludes, because
  // cargo's verdict is not reliable here: a syntax error in the block comes
  // back with rustc's error rendered and then cargo's `Finished` and
  // `"build-finished", "success": true` (see `removeArtifact`). Removing the
  // previous module already keeps such a build from being answered with it;
  // this check is what makes the answer carry rustc's diagnostics rather
  // than only "artifact missing".
  const built = buildFinished !== false && !errored;

  let artifact: ArrayBuffer | undefined;
  if (built) {
    postProgress(message.id, "compiling", { detail: `reading ${builtPath}` });
    const { bytes, output: downloadOutput } = await readArtifact(builtPath);
    shellLog += downloadOutput;
    if (bytes) {
      artifact = bytes;
    } else {
      // Nothing reported an error and there is nothing at the path derived
      // from `crateName`: either the build failed without a diagnostic this
      // worker can parse (a malformed `Cargo.toml`, an internal compiler
      // error — `stderr` has cargo's own words; `removeArtifact` is why the
      // previous module is not here to be mistaken for this one), or a
      // `Cargo.toml` whose `[package] name` was changed, since that is what
      // cargo names the file after.
      // Without this the request goes back as `success: false` with an EMPTY
      // diagnostics list: the one answer that tells an agent nothing at all,
      // and the exact shape `compile-timeout` exists to avoid. It is a
      // diagnostic rather than a throw because it IS an answer about the
      // crate, and the worker is fine.
      diagnostics.push({
        file: "",
        line: 0,
        column: 0,
        severity: "error",
        code: "artifact-missing",
        message:
          `the build produced no ${builtPath} ` +
          `(\`download\` said: ${downloadOutput.trim() || "nothing"}). ` +
          "Either the build failed without a diagnostic (see stderr for cargo's " +
          "output), or `[package] name` in Cargo.toml is not the block's name — " +
          "cargo names the artifact after the package.",
      });
    }
  }

  // How the two text fields are filled, and why they are not fd 1 and fd 2:
  // the guest's streams reach us already merged into one terminal transcript,
  // so the split is by content. `stderr` is what a human would have seen from
  // the build — rustc's own rendering of each diagnostic, then cargo's status
  // output, then anything the guest wrote to fd 2 outside the shell's stream.
  // `stdout` is the rest of the session: `cargo clean -p`, `rm` and
  // `download`. Cargo's `--message-format=json` protocol lines appear in
  // neither; they are what `diagnostics` is made of.
  const humanBuildOutput = [...rendered, plain.join("\n").trim()]
    .filter((part) => part.length > 0)
    .join("\n");

  deliver(
    message.id,
    {
      type: "result",
      id: message.id,
      success: built && artifact !== undefined,
      ...(artifact ? { artifact } : {}),
      stdout: shellLog.trim(),
      stderr: [humanBuildOutput, stderrText.trim()].filter((p) => p.length > 0).join("\n"),
      diagnostics,
      elapsedMs: Date.now() - started,
    },
    artifact ? [artifact] : [],
  );
};

/**
 * Answer a compile, unless a `cancel` already answered it.
 *
 * This is also the only place that leaves the `compiling` state, and it will
 * not leave `broken` — a cancelled worker stays cancelled even though the
 * build it abandoned eventually finishes underneath it.
 */
const deliver = (id: string, result: ResultMessage, transfer: Transferable[] = []) => {
  inFlight = undefined;
  if (cancelledIds.has(id)) return;
  if (state === "compiling") state = "ready";
  post(result, transfer);
};

const failed = (id: string, message: string): ResultMessage => ({
  type: "result",
  id,
  success: false,
  stdout: "",
  stderr: message,
  diagnostics: [],
  elapsedMs: 0,
});

globalThis.addEventListener("message", (event: MessageEvent) => {
  const message = event.data as PageMessage;
  if (!message || typeof message.type !== "string") return;

  switch (message.type) {
    case "init":
      if (state !== "new") {
        post({ type: "error", id: message.id, message: `init in state ${state}` });
        return;
      }
      init(message).catch((error) => {
        state = "broken";
        post({ type: "error", id: message.id, message: String(error) });
      });
      return;

    case "compile":
      // A worker that cannot serve this request AT ALL answers `error`, and
      // the adapter is expected to terminate it. A worker that is merely busy
      // answers a failed `result`: that request is refused, the worker is not.
      if (state === "broken" || state === "new" || state === "initializing") {
        post({ type: "error", id: message.id, message: `compile in state ${state}` });
        return;
      }
      if (state === "compiling") {
        post(failed(message.id, `a compile is already in flight (${inFlight})`));
        return;
      }
      compile(message).catch((error) => {
        if (state === "compiling") state = "broken";
        deliver(message.id, failed(message.id, String(error)));
      });
      return;

    case "cancel": {
      // Nothing in flight: a stray cancel — a double click, or one that raced
      // the result it meant to cancel — must not brick a healthy worker, so it
      // is answered with `error` and changes no state.
      if (inFlight === undefined) {
        post({ type: "error", id: message.id, message: "nothing in flight" });
        return;
      }
      if (inFlight !== message.id) {
        post({
          type: "error",
          id: message.id,
          message: `nothing in flight for ${message.id}; ${inFlight} is running`,
        });
        return;
      }
      // The session thread is inside cargo and nothing outside it can unwind
      // that: the compile is answered as cancelled and the worker is spent.
      // The adapter terminates it and starts a fresh one — see `CancelMessage`
      // in protocol.ts.
      cancelledIds.add(message.id);
      // Cleared for the same reason `deliver` clears it: this request has had
      // its one terminal message. A second `cancel` with the same id would
      // otherwise pass both guards above and post a second one. The adapter
      // does not send that, but the invariant is the file's, not the
      // adapter's.
      inFlight = undefined;
      state = "broken";
      post({ ...failed(message.id, "cancelled"), cancelled: true });
      return;
    }
  }
});
