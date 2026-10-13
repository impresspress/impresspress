import { describe, it, expect } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { IMPRESSPRESS_ASSETS, getImpresspressAssetPath } from "../src/assets";

// The SDK ships the Impresspress brand art in `static/` (see package.json
// "files"): a 64x64 mark and a favicon whose frames are 1:1 renditions.
// There is no raster wordmark — brand text is rendered as text.
const STATIC = resolve(__dirname, "../static");
// The server's copy of the same art. The SDK ships those exact bytes, so a
// new mark cannot reach one and not the other.
const SERVER_ASSETS = resolve(__dirname, "../../../crates/impresspress-core/src/ui/assets");

describe("brand assets", () => {
  it("exposes exactly the square mark and the favicon", () => {
    expect(Object.keys(IMPRESSPRESS_ASSETS).sort()).toEqual(["favicon", "logo"]);
    expect(IMPRESSPRESS_ASSETS.logo).toBe("@impresspress/sdk/static/logo.png");
    expect(IMPRESSPRESS_ASSETS.favicon).toBe("@impresspress/sdk/static/favicon.ico");
    expect(getImpresspressAssetPath("logo")).toBe("/node_modules/@impresspress/sdk/static/logo.png");
  });

  it("ships every listed asset in static/", () => {
    for (const rel of Object.values(IMPRESSPRESS_ASSETS)) {
      expect(existsSync(resolve(STATIC, rel.replace("@impresspress/sdk/static/", "")))).toBe(true);
    }
  });

  it("logo.png is the 64x64 mark", () => {
    const png = readFileSync(resolve(STATIC, "logo.png"));
    expect(png.subarray(1, 4).toString()).toBe("PNG");
    // IHDR: width/height are big-endian u32 at bytes 16 and 20
    expect(png.readUInt32BE(16)).toBe(64);
    expect(png.readUInt32BE(20)).toBe(64);
  });

  it("favicon.ico carries 16, 32 and 48 px frames", () => {
    const ico = readFileSync(resolve(STATIC, "favicon.ico"));
    expect(ico.readUInt16LE(2)).toBe(1);
    const count = ico.readUInt16LE(4);
    const sizes = Array.from({ length: count }, (_, i) => ico[6 + i * 16]);
    expect(sizes).toEqual([16, 32, 48]);
  });

  it("ships the same bytes as the server's built-in mark and favicon", () => {
    expect(readFileSync(resolve(STATIC, "logo.png"))).toEqual(
      readFileSync(resolve(SERVER_ASSETS, "impresspress-logo-2x.png")),
    );
    expect(readFileSync(resolve(STATIC, "favicon.ico"))).toEqual(
      readFileSync(resolve(SERVER_ASSETS, "favicon.ico")),
    );
  });
});
