import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // jsdom gives PopupAuthSession a real `window`/`MessageEvent` to drive,
    // and is a harmless no-op environment for the plain HTTP-layer tests.
    environment: "jsdom",
    include: ["test/**/*.test.ts"],
  },
});
