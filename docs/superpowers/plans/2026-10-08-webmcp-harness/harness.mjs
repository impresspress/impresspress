// WebMCP agent harness: a real Chrome (WebMCPTesting) held open, driven over HTTP.
// Tools are listed/executed through Chrome's own navigator.modelContextTesting.
import { chromium } from 'playwright'; // ESM resolves this next to the script: copy harness.mjs into crates/impresspress-web/ (which has playwright) and run it there
import http from 'node:http';
import fs from 'node:fs';
const PORT = +(process.env.PORT || 7788);
const OUT = process.env.OUT || '.';
// BRIDGE=1 aliases navigator.modelContext as document.modelContext, the
// spelling pre-fix builds read. Off by default: the product reads only
// navigator.modelContext, and a default-on alias is what masked that defect.
// Turn it on only to re-test a build from before the fix.
const BRIDGE = process.env.BRIDGE === '1';
const b = await chromium.launch({ channel: 'chrome', headless: true, args: ['--enable-features=WebMCPTesting'] });
const ctx = await b.newContext({ viewport: { width: 1280, height: 900 } });
if (BRIDGE) await ctx.addInitScript(`if (!('modelContext' in document) && navigator.modelContext) Object.defineProperty(document,'modelContext',{value: navigator.modelContext});`);
let page = await ctx.newPage();
const logs = [];
const hook = (p) => { p.on('console', m => logs.push(`[${m.type()}] ${m.text()}`.slice(0, 500))); p.on('pageerror', e => logs.push(`[pageerror] ${e.message}`)); };
hook(page);
let shot = 0;
const handlers = {
  async goto({ url }) { await page.goto(url, { waitUntil: 'load', timeout: 120000 }); await page.waitForTimeout(3000); return { url: page.url(), title: await page.title() }; },
  async tools() { return page.evaluate(async () => (await navigator.modelContextTesting.listTools()).map(t => ({ name: t.name, description: t.description, inputSchema: t.inputSchema }))); },
  async call({ name, args }) { return page.evaluate(async ([n, a]) => { try { return { ok: true, result: await navigator.modelContextTesting.executeTool(n, JSON.stringify(a ?? {})) }; } catch (e) { return { ok: false, error: String(e && e.message || e) }; } }, [name, args]); },
  async eval({ js }) { return page.evaluate(js); },
  async screenshot({ path, fullPage }) { const f = path || `${OUT}/shot-${++shot}.png`; await page.screenshot({ path: f, fullPage: !!fullPage }); return { path: f }; },
  async text() { return { url: page.url(), text: (await page.innerText('body')).slice(0, 8000) }; },
  async logs() { const l = logs.splice(0); return l.slice(-200); },
  async newtab({ url }) { const p = await ctx.newPage(); hook(p); await p.goto(url, { waitUntil: 'load', timeout: 120000 }); await p.waitForTimeout(3000); return { url: p.url(), title: await p.title(), text: (await p.innerText('body')).slice(0, 4000) }; },
  async quit() { setTimeout(() => process.exit(0), 100); await b.close(); return 'bye'; },
};
http.createServer(async (req, res) => {
  let body = ''; for await (const c of req) body += c;
  const cmd = req.url.slice(1);
  try { const out = await handlers[cmd](body ? JSON.parse(body) : {}); res.end(JSON.stringify(out, null, 1)); }
  catch (e) { res.statusCode = 500; res.end(JSON.stringify({ harnessError: String(e.stack || e) })); }
}).listen(PORT, () => console.log('harness on', PORT));
