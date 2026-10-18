'use strict';
// Real-size golden inputs (tier 2): run the recorded entry points over whole
// files that may be committed — okcode's own tree and a sample of the pinned
// okdb Rust sources. Preload record.js:
//
//   GOLDEN_OUT=<file.jsonl> node --require ./rs/golden/record.js rs/golden/corpus.js
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const ROOT = path.resolve(__dirname, '../..');
const OKDB_RS = path.join(process.env.HOME, '.cache/okcode-rs/okdb-pin/rs/okdb-core/src');

const analysis = require(path.join(ROOT, 'src/analysis'));
const treesitter = require(path.join(ROOT, 'src/analysis/treesitter'));

function okcodeFiles() {
    return execFileSync('git', ['ls-files'], { cwd: ROOT, encoding: 'utf8' })
        .split('\n')
        .filter((f) => f && !f.startsWith('rs/') && !/\.(wasm|png|jpg|gz)$/.test(f) && !f.includes('package-lock'))
        .map((f) => ({ rel: f, abs: path.join(ROOT, f) }));
}

function okdbRustSample(n = 40) {
    const out = [];
    const walk = (d) => {
        for (const e of fs.readdirSync(d, { withFileTypes: true })) {
            const p = path.join(d, e.name);
            if (e.isDirectory()) walk(p);
            else if (e.name.endsWith('.rs')) out.push(p);
        }
    };
    if (fs.existsSync(OKDB_RS)) walk(OKDB_RS);
    out.sort();
    const step = Math.max(1, Math.floor(out.length / n));
    return out.filter((_, i) => i % step === 0).slice(0, n).map((abs) => ({ rel: 'okdb/' + path.relative(OKDB_RS, abs), abs }));
}

(async () => {
    const files = [...okcodeFiles(), ...okdbRustSample()].filter((f) => fs.statSync(f.abs).size < 400_000);
    await treesitter.preload(files.map((f) => f.rel));
    for (const f of files) {
        const text = fs.readFileSync(f.abs, 'utf8');
        analysis.extract(f.rel, text);
        analysis.validateSource({ rel: f.rel, text });
        analysis.regions(f.rel, text);
    }
    console.log(`corpus: ${files.length} files`);
})();
