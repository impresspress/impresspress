import { test, expect, type APIRequestContext, type Browser, type Page } from '@playwright/test';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  bootServiceWorker,
  enterFromWelcome,
  forwardSandboxDiagnostics,
  LLMS_BOOTSTRAP_PORT,
  LLMS_EXPORT_PORT,
  runFromConsole,
  serveDirectory,
} from './fixtures/dev-sandbox';

/**
 * What the sandbox says to a reader that has not got in yet — and that it
 * stops saying it the moment the site has something of its own to say.
 *
 * The sandbox is a JavaScript boot shell in front of a runtime that lives in
 * a service worker. An agent that only fetches the address (or a cloud
 * browser before the worker is installed) used to get a title and
 * "Loading...": nothing that said what the page was, that it was expected to
 * build a site there, or how. Two static files answer that now — `/llms.txt`
 * and the boot page's own text — and the first half of this file reads them
 * the way such a reader does: plain HTTP GETs through Playwright's `request`
 * fixture, which runs no script and has no service worker.
 *
 * The second half is the part that is easy to get wrong. `/llms.txt` is NOT a
 * service-worker bypass: once the worker controls the page the runtime
 * answers the path, so a site an agent builds can ship its own
 * `site/llms.txt`. Until it does, the runtime serves the sandbox's text (the
 * dev block publishes it for a site that has none) rather than the site's
 * fallback document.
 *
 * The blank seed is the bundle on `TEST_PORT`; the bootstrap seed's bundle
 * (`BOOTSTRAP_DIST`, as in `dev-bootstrap.spec.ts`) is served here on a port
 * of its own for its GETs.
 */
const BOOTSTRAP_DIST = process.env.BOOTSTRAP_DIST;
if (!BOOTSTRAP_DIST) {
  throw new Error(
    'BOOTSTRAP_DIST is not set — build the seed with ' +
      '`examples/dev-sandbox/build.sh --seed bootstrap --out <dir>` and point BOOTSTRAP_DIST at <dir>',
  );
}

const sha256 = (text: string) => createHash('sha256').update(text, 'utf8').digest('hex');

/**
 * The title a seed gives its sandbox's boot page — read from the seed's own
 * `sandbox.json`, the one place it is written. `build.sh` hands it to the
 * bundler, so what a bundle's static `/` is titled is that seed's and no
 * other's.
 */
function seedTitle(seed: string): string {
  const sandbox = JSON.parse(
    readFileSync(
      path.join(import.meta.dirname, '../../../../examples/dev-sandbox/seeds', seed, 'sandbox.json'),
      'utf8',
    ),
  );
  expect(typeof sandbox.title).toBe('string');
  return sandbox.title;
}

/** `GET url` with no JavaScript and no service worker. */
async function plainGet(request: APIRequestContext, url: string) {
  const response = await request.get(url);
  return {
    status: response.status(),
    type: response.headers()['content-type'] ?? '',
    text: await response.text(),
  };
}

/**
 * The sandbox's `/llms.txt` from the static host, checked against the
 * bundle's own seed manifest (the hash the seed importer verifies) and for
 * everything a reader has to be told. Returns the text.
 */
async function sandboxLlms(request: APIRequestContext, origin: string, template: string) {
  const llms = await plainGet(request, `${origin}/llms.txt`);
  expect(llms.status).toBe(200);
  expect(llms.type).toMatch(/^text\/plain/);
  const manifest = JSON.parse((await plainGet(request, `${origin}/seed/manifest.json`)).text);
  expect(manifest.sandbox.template).toBe(template);
  expect(manifest.sandbox.llms.path).toBe('llms.txt');
  expect(sha256(llms.text)).toBe(manifest.sandbox.llms.sha256);

  // What this is, and what the reader is expected to do here.
  expect(llms.text).toMatch(/^# ImpressPress build sandbox\n/);
  expect(llms.text).toContain('you are expected to');
  expect(llms.text).toContain(`seeded from the \`${template}\` template`);
  // How to get in: the entry page, and nothing to type.
  expect(llms.text).toContain('`/b/dev/enter`');
  expect(llms.text).toContain('no credentials to');
  expect(llms.text).not.toContain('admin123');
  // Both ways to call the tools: WebMCP, and the Tool console by element id.
  expect(llms.text).toContain('WebMCP');
  expect(llms.text).toContain('`dev_write_files`');
  for (const id of ['#dev-console-tool', '#dev-console-args', '#dev-console-run', '#dev-console-result']) {
    expect(llms.text).toContain(id);
  }
  // How it ends, and what a reader that cannot run the page must do instead.
  expect(llms.text).toContain('`dev_export`');
  expect(llms.text).toContain('You need a browser that runs JavaScript');
  expect(llms.text).toContain('say so to your user');
  return llms.text;
}

/**
 * The boot page, as HTML: the seed's own title, readable text and the two
 * links, with no script run.
 */
async function readableBootPage(request: APIRequestContext, origin: string, seed: string) {
  const boot = await plainGet(request, `${origin}/`);
  expect(boot.status).toBe(200);
  expect(boot.type).toMatch(/^text\/html/);
  // Titled and headed by the seed it was built from. (Neither seed's title
  // has a character HTML escapes.)
  const title = seedTitle(seed);
  expect(title).not.toMatch(/[&<>]/);
  expect(boot.text).toContain(`<title>${title}</title>`);
  expect(boot.text).toContain(`<h1><span data-app-title>${title}</span></h1>`);
  expect(boot.text).toMatch(/<title>[^<]*sandbox[^<]*<\/title>/i);
  expect(boot.text).toContain('This is an ImpressPress build sandbox');
  expect(boot.text).toContain('you are expected');
  expect(boot.text).toContain('<a href="/llms.txt">');
  expect(boot.text).toContain('<a href="/b/dev/enter">');
  expect(boot.text).toMatch(/<noscript>[\s\S]*JavaScript[\s\S]*<\/noscript>/);
  // …and it is still the boot shell `loader.js` drives.
  expect(boot.text).toContain('<p id="status">Loading...</p>');
  expect(boot.text).toContain('<script src="/loader.js"></script>');
}

test('a fetch-only reader gets readable text from / and /llms.txt, on both seeds', async ({
  request,
  baseURL,
}) => {
  const blank = await sandboxLlms(request, baseURL!, 'blank');
  await readableBootPage(request, baseURL!, 'blank');

  const server = await serveDirectory(BOOTSTRAP_DIST, LLMS_BOOTSTRAP_PORT);
  try {
    const origin = `http://127.0.0.1:${LLMS_BOOTSTRAP_PORT}`;
    const bootstrap = await sandboxLlms(request, origin, 'bootstrap');
    await readableBootPage(request, origin, 'bootstrap');
    // Two sandboxes a reader can tell apart before either has loaded: the
    // one built from a template says which.
    expect(seedTitle('bootstrap')).not.toBe(seedTitle('blank'));
    expect(seedTitle('bootstrap')).toMatch(/bootstrap/i);
    // The bootstrap seed says where the framework is; the blank one has none.
    expect(bootstrap).toContain('`site/vendor/bootstrap/`');
    expect(bootstrap).toContain('/vendor/bootstrap/bootstrap.min.css');
    expect(blank).not.toContain('vendor/bootstrap');
  } finally {
    server.kill('SIGKILL');
  }
});

test('with the worker active /llms.txt is the sandbox’s until the site writes its own', async ({
  browser,
  page,
  request,
  baseURL,
}) => {
  test.setTimeout(300_000);
  const fromStaticHost = await sandboxLlms(request, baseURL!, 'blank');

  /** `/llms.txt` as the page gets it — through the service worker. */
  const viaWorker = () =>
    page.evaluate(async () => {
      const response = await fetch('/llms.txt', { cache: 'no-store' });
      return {
        controlled: navigator.serviceWorker.controller !== null,
        status: response.status,
        type: response.headers.get('content-type') ?? '',
        text: await response.text(),
      };
    });

  await bootServiceWorker(page);

  // The worker controls the page and the site has no llms.txt: the runtime
  // answers, with the sandbox's text — not with the site's fallback document,
  // which is what an unknown site path gets.
  const before = await viaWorker();
  expect(before.controlled).toBe(true);
  expect(before.status).toBe(200);
  // The runtime declares the charset the static host leaves out.
  expect(before.type).toBe('text/plain; charset=utf-8');
  expect(before.text).toBe(fromStaticHost);
  const unknown = await page.evaluate(async () => (await fetch('/no-such-file.txt')).text());
  expect(unknown).toContain('<html');

  // The sandbox's text is not a workspace file: the site does not own it.
  await enterFromWelcome(page);
  const listed = await runFromConsole(page, 'dev_list_files', {});
  expect(listed.isError, JSON.stringify(listed)).toBe(false);
  expect(JSON.stringify(listed.result)).not.toContain('llms.txt');

  // The site ships its own — in its own language, which need not be ASCII
  // the way the sandbox's text is. The write is accepted — the path is not
  // one the worker keeps from the runtime — and the runtime serves THAT
  // file, declared as UTF-8.
  const OWN = '# Töpferei Kiln & Co — 窯\n\n> Handgemachte Keramik, in kleinen Bränden gebrannt…\n';
  const written = await runFromConsole(page, 'dev_write_file', {
    path: 'site/llms.txt',
    content: OWN,
  });
  expect(written.isError, JSON.stringify(written)).toBe(false);
  expect(written.result.path).toBe('site/llms.txt');
  const own = await viaWorker();
  expect(own.status).toBe(200);
  expect(own.type).toBe('text/plain; charset=utf-8');
  expect(own.text).toBe(OWN);
  // An export would carry the site's file twice — at the root for the static
  // host, under the seed for the exported runtime — and nothing else by
  // that name. The root copy is three bytes longer: the byte order mark a
  // charset-less static host needs (below).
  const preview = await runFromConsole(page, 'dev_export_manifest', {});
  expect(preview.isError, JSON.stringify(preview)).toBe(false);
  const exported: { path: string; bytes: number }[] = preview.result.files.filter(
    (file: { path: string }) => file.path.endsWith('llms.txt'),
  );
  const ownBytes = new TextEncoder().encode(OWN).length;
  expect(exported).toEqual([
    { path: 'llms.txt', bytes: ownBytes + 3 },
    { path: 'seed/site/llms.txt', bytes: ownBytes },
  ]);
  await exportReadsOnAStaticHost(browser, page, OWN);

  // A reader with no worker is still told about the sandbox: the static host
  // never learned of the site's file.
  expect((await plainGet(request, `${baseURL}/llms.txt`)).text).toBe(fromStaticHost);

  // And when the site's file goes, the sandbox's text is back.
  const deleted = await runFromConsole(page, 'dev_delete_file', {
    path: 'site/llms.txt',
    expected_sha256: written.result.sha256,
  });
  expect(deleted.isError, JSON.stringify(deleted)).toBe(false);
  expect((await viaWorker()).text).toBe(fromStaticHost);
  const without = await runFromConsole(page, 'dev_export_manifest', {});
  expect(
    without.result.files.filter((file: { path: string }) => file.path.endsWith('llms.txt')),
  ).toEqual([]);
});

test('the runtime serves a site’s text files with their type and a UTF-8 charset', async ({ page }) => {
  test.setTimeout(300_000);
  await bootServiceWorker(page);
  await enterFromWelcome(page);

  // Every textual type in the one content-type table declares UTF-8, so
  // non-ASCII text reads back unchanged however the reader decodes it.
  const files: { path: string; content: string; type: string }[] = [
    { path: 'notes.md', content: '# Töpferei — 窯\n', type: 'text/markdown; charset=utf-8' },
    { path: 'feed.xml', content: '<feed>Töpferei — 窯</feed>\n', type: 'application/xml; charset=utf-8' },
    { path: 'data.csv', content: 'name\nTöpferei — 窯\n', type: 'text/csv; charset=utf-8' },
  ];
  for (const file of files) {
    const written = await runFromConsole(page, 'dev_write_file', {
      path: `site/${file.path}`,
      content: file.content,
    });
    expect(written.isError, JSON.stringify(written)).toBe(false);
  }
  for (const file of files) {
    const served = await page.evaluate(async (url) => {
      const response = await fetch(url, { cache: 'no-store' });
      return {
        controlled: navigator.serviceWorker.controller !== null,
        status: response.status,
        type: response.headers.get('content-type') ?? '',
        text: await response.text(),
      };
    }, `/${file.path}`);
    expect(served.controlled).toBe(true);
    expect(served.status, file.path).toBe(200);
    expect(served.type, file.path).toBe(file.type);
    expect(served.text, file.path).toBe(file.content);
  }
});

/**
 * Export the site, unpack it, serve it with a plain static server, and open
 * its root `llms.txt` and `README.md` in a tab with no worker — the reader
 * the root copy exists for.
 *
 * `python3 -m http.server` types `.txt` as `text/plain` and `.md` as a
 * markdown type, with no charset (Cloudflare's asset server sends `.txt` the
 * same way); a browser decodes such a document as windows-1252 unless the
 * file itself says otherwise. Asserting that no charset was sent keeps this a
 * test of such a host, not of one that happens to add one.
 */
async function exportReadsOnAStaticHost(browser: Browser, page: Page, own: string) {
  const scratch = mkdtempSync(path.join(tmpdir(), 'dev-llms-export-'));
  try {
    const downloading = page.waitForEvent('download', { timeout: 120_000 });
    const exported = await runFromConsole(page, 'dev_export', {});
    expect(exported.isError, JSON.stringify(exported)).toBe(false);
    const zipPath = path.join(scratch, 'export.zip');
    await (await downloading).saveAs(zipPath);
    const unpacked = path.join(scratch, 'site');
    execFileSync('python3', [
      '-c',
      'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])',
      zipPath,
      unpacked,
    ]);
    const server = await serveDirectory(unpacked, LLMS_EXPORT_PORT);
    const reader = await browser.newContext({ baseURL: `http://127.0.0.1:${LLMS_EXPORT_PORT}` });
    try {
      const tab = await reader.newPage();
      const llms = await tab.goto('/llms.txt', { waitUntil: 'load' });
      expect(llms!.fromServiceWorker()).toBe(false);
      expect(llms!.headers()['content-type']).toBe('text/plain');
      // (The type is the host's; only the file can say how to decode it.)
      expect(await tab.evaluate(() => document.characterSet)).toBe('UTF-8');
      expect(await tab.locator('body').textContent()).toBe(own);

      const readme = await tab.goto('/README.md', { waitUntil: 'load' });
      const readmeType = readme!.headers()['content-type'] ?? '';
      expect(readmeType).toMatch(/^text\//);
      expect(readmeType).not.toContain('charset');
      expect(await tab.evaluate(() => document.characterSet)).toBe('UTF-8');
      const text = (await tab.locator('body').textContent()) ?? '';
      expect(text).toContain('The runtime shell — ');
      expect(text).not.toContain('â€');
    } finally {
      await reader.close();
      server.kill('SIGKILL');
    }
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

/**
 * Open `/llms.txt` the way a person or an agent does — a top-level
 * navigation, not a `fetch` — and return where the tab ended up, what it
 * shows, and who answered.
 */
async function openLlms(page: Page) {
  const response = await page.goto('/llms.txt', { waitUntil: 'load' });
  return {
    fromWorker: response?.fromServiceWorker() ?? null,
    type: response?.headers()['content-type'] ?? '',
  };
}

/** What the tab is showing: its path, and the text of a plain-text document. */
async function shown(page: Page) {
  return {
    path: new URL(page.url()).pathname,
    text: await page.locator('body').textContent(),
  };
}

test('a navigation to /llms.txt shows the text before, during and after the worker’s install', async ({
  browser,
  request,
  baseURL,
}) => {
  test.setTimeout(300_000);
  const fromStaticHost = await sandboxLlms(request, baseURL!, 'blank');

  // 1. No worker: the static host answers the navigation.
  const fresh = await browser.newContext();
  try {
    const page = await fresh.newPage();
    forwardSandboxDiagnostics(page);
    const answer = await openLlms(page);
    expect(answer.fromWorker).toBe(false);
    expect(answer.type).toMatch(/^text\/plain/);
    expect(await shown(page)).toEqual({ path: '/llms.txt', text: fromStaticHost });
  } finally {
    await fresh.close();
  }

  // 2. While the boot shell is still booting: the worker has taken the page,
  //    the runtime is still starting, and the shell has yet to go on to the
  //    app. A navigation started now is the reader's, and the shell must not
  //    replace it with a reload of `/` once the runtime answers — the two
  //    live runs on 2026-10-08 both landed on the welcome page at `/` that
  //    way.
  const booting = await browser.newContext();
  try {
    const page = await booting.newPage();
    forwardSandboxDiagnostics(page);
    await page.goto('/', { waitUntil: 'commit' });
    await page.waitForFunction(() => navigator.serviceWorker.controller !== null, null, {
      timeout: 120_000,
    });
    const answer = await openLlms(page);
    expect(answer.fromWorker).toBe(true);
    expect(answer.type).toBe('text/plain; charset=utf-8');
    // The runtime is up — the probe the shell was waiting on has its answer
    // by now — and the tab is still where the reader sent it.
    await expect
      .poll(async () => page.evaluate(async () => (await fetch('/b/auth/login')).status), {
        timeout: 120_000,
      })
      .toBe(200);
    expect(await shown(page)).toEqual({ path: '/llms.txt', text: fromStaticHost });
  } finally {
    await booting.close();
  }

  // 3. A page the worker controls: the runtime answers, with the same text.
  const controlled = await browser.newContext();
  try {
    const page = await controlled.newPage();
    await bootServiceWorker(page);
    const answer = await openLlms(page);
    expect(answer.fromWorker).toBe(true);
    expect(answer.type).toBe('text/plain; charset=utf-8');
    expect(await shown(page)).toEqual({ path: '/llms.txt', text: fromStaticHost });
  } finally {
    await controlled.close();
  }
});
