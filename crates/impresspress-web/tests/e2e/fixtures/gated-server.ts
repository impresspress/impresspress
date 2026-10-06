import { createReadStream, existsSync, statSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import path from 'node:path';

/**
 * A plain static file server with one extra: it can HOLD its answers to some
 * requests until told to let them go.
 *
 * Plain in the way that matters to the specs that use it — a path with no
 * file is a 404, never `index.html` — and `no-store` on everything, so a
 * request a test wants to hold is not answered from the browser's cache
 * instead.
 *
 * The hold is how a spec gets a runtime that is SLOW to start without
 * touching the worker: the worker's first act is to fetch the wasm module,
 * and while that answer is held the boot probe behind it stays unanswered.
 */

const TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript',
  '.mjs': 'text/javascript',
  '.json': 'application/json',
  '.wasm': 'application/wasm',
  '.css': 'text/css',
  '.txt': 'text/plain; charset=utf-8',
};

export interface GatedServer {
  /** Hold every answer to a path ending in `suffix` until `release()`. */
  hold(suffix: string): void;
  /** How many requests are being held right now. */
  held(): number;
  /** Answer what is held, and hold nothing from here on. */
  release(): void;
  /** Answer the request held last, and go on holding the others. */
  releaseNewest(): void;
  close(): Promise<void>;
}

export async function serveGated(dir: string, port: number): Promise<GatedServer> {
  let holding: string | null = null;
  let waiting: Array<() => void> = [];

  const server: Server = createServer((request, response) => {
    const pathname = decodeURIComponent(new URL(request.url ?? '/', 'http://x').pathname);
    const file = path.join(dir, pathname === '/' ? 'index.html' : pathname);
    const answer = () => {
      const inside = path.resolve(file).startsWith(path.resolve(dir) + path.sep);
      if (!inside || !existsSync(file) || !statSync(file).isFile()) {
        response.writeHead(404, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' });
        response.end('no such file');
        return;
      }
      response.writeHead(200, {
        'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream',
        'Cache-Control': 'no-store',
      });
      createReadStream(file).pipe(response);
    };
    if (holding !== null && pathname.endsWith(holding)) {
      waiting.push(answer);
    } else {
      answer();
    }
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, '127.0.0.1', resolve);
  });

  return {
    hold: (suffix) => {
      holding = suffix;
    },
    held: () => waiting.length,
    release: () => {
      holding = null;
      const answers = waiting;
      waiting = [];
      for (const answer of answers) answer();
    },
    releaseNewest: () => {
      const answer = waiting.pop();
      if (answer) answer();
    },
    close: () =>
      new Promise<void>((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}
