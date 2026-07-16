import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { PopupAuthSession } from "../src/popup-auth-session";

/** Minimal `window.open` stand-in: just enough for the close-poll + `.close()`. */
function makeFakePopup() {
  return {
    closed: false,
    close() {
      this.closed = true;
    },
  };
}

describe("PopupAuthSession", () => {
  let popup: ReturnType<typeof makeFakePopup>;

  beforeEach(() => {
    vi.useFakeTimers();
    popup = makeFakePopup();
    vi.spyOn(window, "open").mockReturnValue(popup as unknown as Window);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("resolves and closes the popup when an allowlisted message arrives", async () => {
    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      onMessage: (data) => ((data as { ok?: boolean })?.ok ? "done" : undefined),
    });

    window.dispatchEvent(
      new MessageEvent("message", {
        data: { ok: true },
        origin: "https://provider.example",
        source: popup as unknown as Window,
      }),
    );

    await expect(promise).resolves.toBe("done");
    expect(popup.closed).toBe(true);
  });

  it("ignores a message whose source is not the popup itself (spoofing guard)", async () => {
    const spoofer = makeFakePopup();
    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      onMessage: () => "from-message",
      onClosed: () => "from-close-fallback",
    });

    window.dispatchEvent(
      new MessageEvent("message", {
        data: {},
        origin: "https://provider.example",
        source: spoofer as unknown as Window,
      }),
    );

    // The spoofed message must not have finalized the session — closing the
    // real popup still has to drive the close-poll fallback.
    popup.close();
    await vi.advanceTimersByTimeAsync(600);

    await expect(promise).resolves.toBe("from-close-fallback");
  });

  it("ignores a message from a non-allowlisted origin", async () => {
    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      onMessage: () => "from-message",
      onClosed: () => "from-close-fallback",
    });

    window.dispatchEvent(
      new MessageEvent("message", {
        data: {},
        origin: "https://evil.example",
        source: popup as unknown as Window,
      }),
    );

    popup.close();
    await vi.advanceTimersByTimeAsync(600);

    await expect(promise).resolves.toBe("from-close-fallback");
  });

  it("is idempotent — a close-poll tick after a message already finalized is a no-op", async () => {
    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      onMessage: () => "from-message",
      onClosed: () => "from-close-fallback",
    });

    window.dispatchEvent(
      new MessageEvent("message", {
        data: {},
        origin: "https://provider.example",
        source: popup as unknown as Window,
      }),
    );

    // finalize() already closed the popup as part of handling the message
    // above; let the poll interval fire again and prove it doesn't
    // re-resolve/reject.
    await vi.advanceTimersByTimeAsync(1000);

    await expect(promise).resolves.toBe("from-message");
  });

  it("rejects with a timeout when nothing ever happens", async () => {
    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      timeoutMs: 1000,
      onMessage: () => undefined,
    });

    // Attach the rejection assertion before advancing fake timers — the
    // timeout fires and rejects synchronously-ish inside
    // `advanceTimersByTimeAsync`, and an unattached rejection at that point
    // is flagged by Node as an unhandled rejection even though we await it
    // on the next line.
    const assertion = expect(promise).rejects.toThrow("Authentication timeout");
    await vi.advanceTimersByTimeAsync(1000);
    await assertion;
  });

  it("rejects immediately when the AbortSignal is already aborted", async () => {
    const controller = new AbortController();
    controller.abort();

    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      signal: controller.signal,
      onMessage: () => undefined,
    });

    await expect(promise).rejects.toThrow("Authentication cancelled");
  });

  it("rejects when aborted mid-flight, cleaning up listeners/timers", async () => {
    const controller = new AbortController();
    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      signal: controller.signal,
      onMessage: () => undefined,
    });

    controller.abort();

    await expect(promise).rejects.toThrow("Authentication cancelled");
    expect(popup.closed).toBe(true);
  });

  it("rejects with the popup-closed message when there is no onClosed fallback", async () => {
    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      onMessage: () => undefined,
    });

    const assertion = expect(promise).rejects.toThrow("Authentication popup was closed");
    popup.close();
    await vi.advanceTimersByTimeAsync(600);
    await assertion;
  });

  it("rejects when window.open is blocked by a popup blocker", async () => {
    vi.spyOn(window, "open").mockReturnValue(null);

    const promise = PopupAuthSession.open<string>({
      url: "https://provider.example/auth",
      allowedOrigins: ["https://provider.example"],
      onMessage: () => undefined,
    });

    await expect(promise).rejects.toThrow(/popup blocker/i);
  });
});
