import { readFileSync } from 'node:fs';
import path from 'node:path';

const TOOLCHAIN_FILE = path.join(import.meta.dirname, '../../../../../rust-toolchain.toml');

/**
 * The environment for a `cargo` or `rustc` child that runs OUTSIDE the
 * repository tree (a crate laid down in a temp directory).
 *
 * rustup picks the toolchain from the `rust-toolchain.toml` it finds walking
 * up from the working directory. A temp directory has none above it, so such
 * a child would run on the machine's default toolchain — a different compiler
 * from the one the rest of the suite uses, and one that need not have the
 * wasm targets the pinned one is installed with. `RUSTUP_TOOLCHAIN` names the
 * toolchain outright; the channel is read from the repository's file so the
 * version lives in one place.
 */
export function pinnedToolchainEnv(): NodeJS.ProcessEnv {
  const channel = /^channel\s*=\s*"([^"]+)"/m.exec(readFileSync(TOOLCHAIN_FILE, 'utf8'))?.[1];
  if (!channel) {
    throw new Error(`${TOOLCHAIN_FILE} declares no toolchain channel`);
  }
  return { ...process.env, RUSTUP_TOOLCHAIN: channel };
}
