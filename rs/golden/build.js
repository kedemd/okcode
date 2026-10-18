'use strict';
// Packs a recorded golden run into rs/golden/data/<fn>.jsonl.gz, one file per
// entry point, deduped, in a stable order — what the Rust tests read.
//
//   node rs/golden/build.js [raw.jsonl]   (default ~/.cache/okcode-rs/golden-raw.jsonl)
//
// Refresh: re-run record.js (tests + corpus.js) and this. Seconds.
const fs = require('fs');
const path = require('path');
const zlib = require('zlib');

const RAW = process.argv[2] || path.join(process.env.HOME, '.cache/okcode-rs/golden-raw.jsonl');
const OUT = path.join(__dirname, 'data');

const byFn = new Map();
for (const line of fs.readFileSync(RAW, 'utf8').split('\n')) {
    if (!line) continue;
    const { fn } = JSON.parse(line);
    if (!byFn.has(fn)) byFn.set(fn, new Set());
    byFn.get(fn).add(line);
}

fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });
for (const [fn, lines] of [...byFn].sort(([a], [b]) => a.localeCompare(b))) {
    const body = [...lines].sort().join('\n') + '\n';
    fs.writeFileSync(path.join(OUT, `${fn}.jsonl.gz`), zlib.gzipSync(body, { level: 9 }));
    console.log(`${fn}: ${lines.size}`);
}
