/**
 * Single abstraction for driving an OAuth popup window to completion.
 *
 * This replaces two previously-duplicated implementations in
 * `auth.service.ts` (`signInWithOAuthPopup` and `signInWithPopup`) that each
 * hand-rolled their own popup lifecycle with inconsistent bugs: one
 * validated `event.origin` but never `event.source`, so any same-origin
 * iframe/window (not just the popup itself) could spoof a completion
 * message; neither had a single idempotent finalizer, so a message
 * arriving right as the close-poll fired could resolve/reject twice or
 * leak the interval/timeout.
 *
 * The real server (`crates/impresspress-core/src/blocks/auth_ui`) does not
 * post a `message` back to the opener itself — the OAuth callback sets an
 * httpOnly cookie and redirects the popup to `WAFER_RUN_SHARED__FRONTEND_URL`.
 * So the only mechanism guaranteed to work against the shipped server is:
 * poll for the popup closing, then verify the session via a cookie-backed
 * API call. The `message` listener remains as an OPT-IN fast path for
 * deployments whose `FRONTEND_URL` redirect target `postMessage`s the
 * opener (a common pattern for consumer-built bridge pages) — it just must
 * never be the only path, since the server doesn't require it.
 */
export interface PopupAuthSessionOptions<T> {
  /** URL to open the popup at. */
  url: string;
  /** Window name passed to `window.open`. */
  name?: string;
  width?: number;
  height?: number;
  /** Overall timeout before the session is abandoned. Default 5 minutes. */
  timeoutMs?: number;
  /** Abort the session early (e.g. consumer navigates away / cancels). */
  signal?: AbortSignal;
  /** Origins allowed to post a completion message into this session. */
  allowedOrigins: string[];
  /**
   * Called for every `message` event whose `source` is the popup and whose
   * `origin` is allowlisted. Return a result to finalize the session, throw
   * to fail it, or return `undefined` to keep waiting.
   */
  onMessage: (data: unknown) => T | undefined;
  /**
   * Called once, when the popup closes without `onMessage` ever finalizing.
   * Return a result to finalize the session as a success (e.g. after
   * re-checking the session via a cookie-backed API call), or `undefined`/
   * throw to finalize as a failure.
   */
  onClosed?: () => T | undefined | Promise<T | undefined>;
}

const DEFAULT_TIMEOUT_MS = 5 * 60 * 1000;
const CLOSE_POLL_INTERVAL_MS = 500;

export class PopupAuthSession {
  /** Open a popup and resolve once the session finalizes (see options doc). */
  static open<T>(options: PopupAuthSessionOptions<T>): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const {
        url,
        name = "impresspress_auth_popup",
        width = 500,
        height = 650,
        timeoutMs = DEFAULT_TIMEOUT_MS,
        signal,
        allowedOrigins,
        onMessage,
        onClosed,
      } = options;

      const left = window.screenX + (window.innerWidth - width) / 2;
      const top = window.screenY + (window.innerHeight - height) / 2;
      const popup = window.open(
        url,
        name,
        `width=${width},height=${height},left=${left},top=${top},toolbar=no,menubar=no,location=no,status=no`,
      );

      if (!popup) {
        reject(
          new Error(
            "Failed to open authentication popup. Please check your popup blocker settings.",
          ),
        );
        return;
      }

      let settled = false;
      // Held in a single mutable object (rather than two `let` bindings) so
      // `cleanup` can close over stable handles that are only ever assigned
      // once each, at the point the interval/timeout are actually created.
      const handles: {
        poll?: ReturnType<typeof setInterval>;
        timeout?: ReturnType<typeof setTimeout>;
      } = {};

      const cleanup = () => {
        window.removeEventListener("message", handleMessage);
        if (handles.poll !== undefined) clearInterval(handles.poll);
        if (handles.timeout !== undefined) clearTimeout(handles.timeout);
        if (signal) signal.removeEventListener("abort", handleAbort);
      };

      // Idempotent finalizer: every completion path (message, close-poll,
      // timeout, abort) routes through here, so only the first call ever
      // takes effect — no double resolve/reject, no leaked listeners/timers.
      const finalize = (fn: () => void) => {
        if (settled) return;
        settled = true;
        cleanup();
        if (!popup.closed) popup.close();
        fn();
      };

      const handleMessage = (event: MessageEvent) => {
        // Reject messages from anything other than the popup itself — the
        // prior implementation checked `event.origin` alone, which does not
        // prove the message came from this popup (any same-origin window
        // could send it).
        if (event.source !== popup) return;
        if (!allowedOrigins.includes(event.origin)) return;

        let result: T | undefined;
        try {
          result = onMessage(event.data);
        } catch (err) {
          finalize(() => reject(err instanceof Error ? err : new Error(String(err))));
          return;
        }
        if (result !== undefined) {
          finalize(() => resolve(result as T));
        }
      };

      const handleAbort = () => {
        finalize(() => reject(new Error("Authentication cancelled")));
      };

      window.addEventListener("message", handleMessage);

      if (signal) {
        if (signal.aborted) {
          handleAbort();
          return;
        }
        signal.addEventListener("abort", handleAbort);
      }

      handles.poll = setInterval(() => {
        if (settled || !popup.closed) return;
        if (!onClosed) {
          finalize(() => reject(new Error("Authentication popup was closed")));
          return;
        }
        Promise.resolve()
          .then(() => onClosed())
          .then(
            (result) => {
              if (result !== undefined) {
                finalize(() => resolve(result as T));
              } else {
                finalize(() => reject(new Error("Authentication popup was closed")));
              }
            },
            (err) => finalize(() => reject(err instanceof Error ? err : new Error(String(err)))),
          );
      }, CLOSE_POLL_INTERVAL_MS);

      handles.timeout = setTimeout(() => {
        finalize(() => reject(new Error("Authentication timeout")));
      }, timeoutMs);
    });
  }
}
