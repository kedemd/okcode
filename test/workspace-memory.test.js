'use strict';
// Memory regression guard. A cold index must not let tree-sitter parsing, the
// in-memory text cache or the store write path accumulate without bound as
// the repo grows. This generates a few hundred small files and asserts peak
// PRIVATE memory (RssAnon) sampled every ~10ms stays under a bound with
// headroom, for both `fts` modes. Most of today's peak is V8 heap high-water
// from okdb's full-text write churn (DESIGN §5); this bound catches a
// regression on top of that, not the floor itself. /proc/self/status is
// Linux-only.

const os = require('os');
const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../src/okcode');
const { localFs } = okcode.access;

const LINUX = os.platform() === 'linux';
const FILE_COUNT = 300;
// Generous headroom over the ~100-145MB measured on this machine for 300
// small files — the point is catching a regression that re-introduces
// unbounded growth (a doubling or worse), not pinning an exact number that
// drifts with Node/V8/okdb versions.
const PEAK_BUDGET_MB = 220;

function rssAnonMB() {
    for (const line of fs.readFileSync('/proc/self/status', 'utf8').split('\n')) {
        const m = /^RssAnon:\s+(\d+)/.exec(line);
        if (m) return +m[1] / 1024;
    }
    return null;
}

function makeRepo(root) {
    fs.mkdirSync(root, { recursive: true });
    for (let i = 0; i < FILE_COUNT; i++) {
        const body = [
            `// module ${i} — generated for the memory regression test`,
            `function fn${i}(a, b) { return a + b + ${i}; }`,
            `const VALUE_${i} = ${i};`,
            `class Thing${i} { get value() { return VALUE_${i}; } }`,
            `module.exports = { fn${i}, VALUE_${i}, Thing${i} };`,
            '',
        ].join('\n');
        fs.writeFileSync(path.join(root, `mod${i}.js`), body);
    }
}

(LINUX ? describe : describe.skip)('cold-index memory', () => {
    let base;
    let root;
    before(() => {
        base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-memtest-'));
        root = path.join(base, 'src');
        makeRepo(root);
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));

    for (const fts of ['symbols', 'full']) {
        it(`fts: '${fts}' stays under ${PEAK_BUDGET_MB}MB private peak`, async () => {
            let peak = 0;
            const sampler = setInterval(() => {
                const v = rssAnonMB();
                if (v !== null && v > peak) peak = v;
            }, 10);
            try {
                const oc = await okcode.open({ path: path.join(base, `store-${fts}`) });
                try {
                    await oc.addWorkspace('m', { access: localFs(root), fts });
                } finally {
                    await oc.close();
                }
            } finally {
                clearInterval(sampler);
            }
            assert.ok(peak < PEAK_BUDGET_MB, `peak private memory was ${peak.toFixed(0)}MB, budget ${PEAK_BUDGET_MB}MB`);
        });
    }
});
