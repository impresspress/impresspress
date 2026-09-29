/**
 * The message protocol between the page-side adapter and the compiler worker.
 *
 * This file is the contract. `worker-entry.ts` implements the worker half;
 * the adapter (`/b/dev`) implements the page half. Both halves are in this
 * repo, so the protocol is versioned by `PIN.json`'s `version` — the URL the
 * page loads the worker from already carries it.
 *
 * One compile at a time. The worker answers `compile` with exactly one
 * terminal message carrying that request's `id`, and a `compile` that arrives
 * while another is in flight is answered with a failed `result` rather than
 * queued: the sandbox has one editor and one build button, and a queue would
 * only hide a bug in the page.
 *
 * # States, and which of them the adapter must give up on
 *
 * The worker is `new`, then `initializing`, then `ready`, and `compiling`
 * while a build runs. `broken` is terminal — nothing leaves it. Two things
 * put it there: a failed `init`, and any `cancel`.
 *
 * * `compile` on a worker that is `broken` (or not yet `ready`) is answered
 *   with `{ type: 'error' }`. That is the adapter's signal to `terminate()`
 *   and start a fresh worker; sending another request is pointless.
 * * `compile` on a worker that is merely busy is answered with a failed
 *   `result`. THAT request is refused; the worker is fine.
 *
 * # Who owns the compile budget
 *
 * The worker does not. Its internal ceiling (10 minutes) is a backstop for a
 * shell that has wedged; the 120 s budget the sandbox promises is the
 * ADAPTER's policy, enforced by sending `cancel` and then terminating. A
 * worker that answers slowly is not misbehaving — it is compiling.
 */

/** The guest SDK crate and a block to build it with, handed over at `init`. */
export type GuestCrate = {
  /** `Cargo.toml`, `src/lib.rs` — crate-relative, written under `/wafer_guest/`. */
  files: Record<string, string>;
  /** Built once after the sysroot loads, so `/target` holds the guest before the first real compile. */
  warmup: { crateName: string; files: Record<string, string> };
};

/**
 * `init` starts the toolchain: download, instantiate, load the sysroot — and,
 * when `guest` is given, build it once. `ready` is posted after that build,
 * so the first `compile` finds the guest already in `/target`. Without
 * `guest` the worker is ready as soon as the sysroot is loaded and builds
 * whatever crate it is handed, self-contained or not.
 */
export type InitMessage = { type: "init"; id: string; guest?: GuestCrate };

/** `compile` writes `files` into the VFS and runs cargo over them. */
export type CompileMessage = {
  type: "compile";
  id: string;
  crateName: string;
  /** Paths relative to the crate root, written under `/blocks/<crateName>/`. */
  files: Record<string, string>;
  target: string;
  release: boolean;
};

/**
 * `cancel` abandons the in-flight compile. `id` is the compile's id.
 *
 * Rubrc's shell has no way to interrupt a running command from outside the
 * session thread that is blocked in it, so the worker cannot unwind a compile
 * in progress. It answers with `{ success: false, cancelled: true }` and is
 * then UNUSABLE: the adapter must `terminate()` it and `init` a fresh one.
 * The `cancelled` result is the signal to do that, not a resumption point.
 * The abandoned build keeps running underneath until the worker is
 * terminated; its `result` and any further `progress` for that id are
 * dropped, so a cancelled compile still gets exactly one terminal message.
 *
 * A `cancel` that names nothing in flight — a double click, or one that raced
 * the `result` it meant to cancel — is answered `{ type: 'error', id,
 * message: 'nothing in flight' }` and changes nothing. It must not be able to
 * brick a healthy worker.
 */
export type CancelMessage = { type: "cancel"; id: string };

export type PageMessage = InitMessage | CompileMessage | CancelMessage;

export type ProgressStage = "download" | "initializing" | "compiling";

export type ProgressMessage = {
  type: "progress";
  id: string;
  stage: ProgressStage;
  /** Bytes of the compiler image fetched so far (`download` only). */
  loaded?: number;
  total?: number;
  /** A line of compiler output, or the step being run (`compiling`). */
  detail?: string;
};

export type ReadyMessage = {
  type: "ready";
  id: string;
  /** Whatever `rustc --version` printed inside the VFS. */
  rustcVersion: string;
};

export type Severity = "error" | "warning";

export type Diagnostic = {
  file: string;
  line: number;
  column: number;
  severity: Severity;
  message: string;
  /** `E0425` and friends, when rustc gave one. */
  code?: string;
};

export type ResultMessage = {
  type: "result";
  id: string;
  success: boolean;
  /** Transferred, not copied — the adapter owns the buffer after this. */
  artifact?: ArrayBuffer;
  /**
   * The session's other output, ANSI stripped: `touch` and the `download`
   * that reads the artifact out of the VFS.
   *
   * The guest's streams reach the worker already merged into one terminal
   * transcript, so these two fields are split by content rather than by file
   * descriptor — see `stderr`.
   */
  stdout: string;
  /**
   * The build as a human would have seen it, ANSI stripped: rustc's own
   * rendering of each diagnostic, then cargo's status output, then anything
   * the guest wrote to fd 2 outside the shell's stream.
   *
   * Cargo runs under `--message-format=json`, and those protocol lines are in
   * neither field: they are what `diagnostics` is made of.
   */
  stderr: string;
  diagnostics: Diagnostic[];
  elapsedMs: number;
  cancelled?: boolean;
};

/**
 * Sent when the worker cannot serve a request at all.
 *
 * `init` that fails, `compile` on a `broken` or not-yet-`ready` worker, and a
 * `cancel` with nothing in flight. The first two mean "terminate me"; the
 * third means "you cancelled nothing" and leaves the worker usable.
 */
export type ErrorMessage = { type: "error"; id: string; message: string };

export type WorkerMessage =
  | ProgressMessage
  | ReadyMessage
  | ResultMessage
  | ErrorMessage;
