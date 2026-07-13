import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const dist = 'dist';
const sw = readFileSync(join(dist, 'sw.js'), 'utf8');
const assets = readdirSync(join(dist, 'assets'));
const wasm = assets.find((n) => n.endsWith('.wasm'));
if (!wasm) {
  console.error('FAIL: no .wasm emitted in dist/assets');
  process.exit(1);
}
if (!wasm.match(/[-_.][a-zA-Z0-9]{8,}\.wasm$/)) {
  console.error(`FAIL: wasm filename not hashed: ${wasm}`);
  process.exit(1);
}
if (!sw.includes(wasm)) {
  console.error(`FAIL: sw.js does not reference the hashed wasm ${wasm}`);
  process.exit(1);
}
console.log(`OK: ${wasm} referenced from sw.js`);
