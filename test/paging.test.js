'use strict';
// No-silent-cuts: refs()/find()/mentions()/importers() must never drop a
// result without saying so, with an exact total where it is cheap, and a way
// to ask for the rest (DESIGN.md §9, "paging" — the same contract as
// ws.grep()'s page/offset). These tests build small fixtures of their own
// (not the shared code-fixture) so the exact hit counts are known ahead of
// time and every page can be checked against ground truth.

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { openWorkspace } = require('../lib/workspace');
const { localFs } = require('../lib/access');
const { tmpRoot } = require('./fixtures/code-fixture');

describe('paging: refs/find/mentions/importers never cut silently', () => {
    const cleanups = [];
    after(async () => {
        for (const c of cleanups) await c().catch(() => {});
    });

    async function openOn(root) {
        const access = localFs(root);
        const ws = await openWorkspace({ id: `pg-${cleanups.length}`, access, store: null });
        cleanups.push(() => ws.close());
        return ws;
    }

    it('refs: the ranked total is exact past the OLD hardCap (limit*20, default 800), and local ranks before textual noise', async () => {
        const { root } = tmpRoot('refs-cap');
        fs.mkdirSync(root, { recursive: true });
        // A real definition + one real use — refs ranks this `local`, ahead of
        // every textual mention below.
        fs.writeFileSync(
            path.join(root, 'def.js'),
            ['const WIDGETCOUNT = 7;', '', 'function report() {', '    return WIDGETCOUNT;', '}', ''].join('\n'),
        );
        // 1000 textual hits, comfortably past the OLD cap (max(40*20,400) =
        // 800): under the bug, the scan stopped in file-list order partway
        // through this noise and never reached def.js at all — the exact
        // failure mode this test exists to catch, regardless of which file
        // the facade happens to list first.
        const NOISE = 1000;
        const lines = [];
        for (let i = 0; i < NOISE; i++) lines.push(`// WIDGETCOUNT appears here (${i})`);
        fs.writeFileSync(path.join(root, 'noise.js'), lines.join('\n') + '\n');
        const ws = await openOn(root);
        await ws.sync();

        const r1 = await ws.refs('WIDGETCOUNT', { limit: 40 });
        assert.equal(r1.capped, false, 'well under rawCap — nothing should be reported as a floor');
        assert.equal(r1.total, NOISE + 1, 'every textual hit AND the one real use, none dropped');
        assert.equal(r1.offset, 0);
        assert.equal(r1.truncated, true);
        assert.equal(r1.next, 40);
        assert.equal(r1[0].label, 'local', 'the real use ranks ahead of 1000 textual hits, not lost among them');
        assert.equal(r1[0].file, 'def.js');

        // Pages disjoint and covering the full ranked list.
        const seen = new Set();
        let offset = 0;
        let pages = 0;
        while (true) {
            const page = await ws.refs('WIDGETCOUNT', { limit: 97, offset });
            assert.equal(page.total, NOISE + 1);
            for (const h of page) {
                const key = `${h.file}:${h.line}`;
                assert.ok(!seen.has(key), `hit ${key} returned twice across pages`);
                seen.add(key);
            }
            pages++;
            if (page.next == null) {
                assert.equal(page.truncated, false);
                assert.equal(offset + page.length, NOISE + 1, 'last page ends exactly at the total');
                break;
            }
            offset = page.next;
            assert.ok(pages < 50, 'paging did not converge');
        }
        assert.equal(seen.size, NOISE + 1, 'every ranked hit was seen exactly once across all pages');
    });

    it('mentions: paging is exact and total is never dropped', async () => {
        const { root } = tmpRoot('mentions-page');
        fs.mkdirSync(root, { recursive: true });
        const N = 12;
        for (let i = 0; i < N; i++) {
            fs.writeFileSync(path.join(root, `f${String(i).padStart(2, '0')}.js`), `const x = 'MENTIONTOKEN';\n`);
        }
        const ws = await openOn(root);
        await ws.sync();

        const first = await ws.mentions('MENTIONTOKEN', { limit: 5 });
        assert.equal(first.total, N);
        assert.equal(first.length, 5);
        assert.equal(first.truncated, true);
        assert.equal(first.next, 5);

        const seen = new Set();
        let offset = 0;
        while (true) {
            const page = await ws.mentions('MENTIONTOKEN', { limit: 5, offset });
            for (const h of page) seen.add(h.rel);
            if (page.next == null) {
                assert.equal(page.truncated, false);
                break;
            }
            offset = page.next;
        }
        assert.equal(seen.size, N, 'every file with the text was reached across pages');

        const none = await ws.mentions('NowhereAtAllInThisFixture');
        assert.deepEqual([...none], []);
        assert.equal(none.total, 0);
        assert.equal(none.truncated, false);
        assert.equal(none.next, null);
    });

    it('find: paging over the ranked list is exact and disjoint', async () => {
        const { root } = tmpRoot('find-page');
        fs.mkdirSync(root, { recursive: true });
        const N = 5;
        for (let i = 0; i < N; i++) {
            fs.writeFileSync(path.join(root, `dup${i}.js`), `function dupFn() { return ${i}; }\nmodule.exports = { dupFn };\n`);
        }
        const ws = await openOn(root);
        await ws.sync();

        const page1 = await ws.find('dupFn', { limit: 2 });
        assert.equal(page1.total, N);
        assert.equal(page1.length, 2);
        assert.equal(page1.truncated, true);
        assert.equal(page1.next, 2);

        const seen = new Set();
        let offset = 0;
        while (true) {
            const page = await ws.find('dupFn', { limit: 2, offset });
            assert.equal(page.total, N);
            for (const h of page) seen.add(h.file);
            if (page.next == null) {
                assert.equal(page.truncated, false);
                break;
            }
            offset = page.next;
        }
        assert.equal(seen.size, N, 'every definition reached across pages');
    });

    it('find: a path-scoped search ranks the FULL set of matches inside the path, not an undersized inner window', async () => {
        const { root } = tmpRoot('find-path-page');
        fs.mkdirSync(path.join(root, 'inside'), { recursive: true });
        // The old code fetched an inner window of `max((off+limit)*5, 200)`
        // top-ranked hits BEFORE filtering by path — so enough outranking
        // noise outside the path could push every real hit out of that
        // window, reporting `total: 0` for a path that actually has
        // matches. Every noise symbol here ties on SCORE (exact name match)
        // with every path symbol, so the tiebreak (smaller span first) is
        // what decides order: 220 one-line noise symbols (span 1) all rank
        // ahead of the path's own symbols (span 5, deliberately larger),
        // well past the old 200-row window.
        const NOISE = 220;
        for (let i = 0; i < NOISE; i++) {
            fs.writeFileSync(path.join(root, `noise${i}.js`), 'function targetSym() { return 1; }\n');
        }
        const INSIDE = 3;
        for (let i = 0; i < INSIDE; i++) {
            fs.writeFileSync(
                path.join(root, 'inside', `f${i}.js`),
                ['function targetSym() {', '  const a = 1;', '  const b = 2;', '  return a + b;', '}', ''].join('\n'),
            );
        }
        const ws = await openOn(root);
        await ws.sync();

        const hits = await ws.find('targetSym', { path: 'inside', limit: 10 });
        assert.equal(hits.total, INSIDE, JSON.stringify({ total: hits.total, capped: hits.capped }));
        assert.equal(hits.length, INSIDE);
        assert.equal(hits.truncated, false);
        assert.ok(hits.every((h) => h.file.startsWith('inside/')));
    });

    it('importers: paging is exact and offers the exact continuation', async () => {
        const { root } = tmpRoot('importers-page');
        fs.mkdirSync(root, { recursive: true });
        fs.writeFileSync(path.join(root, 'target.js'), 'module.exports = { shared: 1 };\n');
        const N = 4;
        for (let i = 0; i < N; i++) {
            fs.writeFileSync(path.join(root, `user${i}.js`), "const { shared } = require('./target');\nshared;\n");
        }
        const ws = await openOn(root);
        await ws.sync();

        const r1 = await ws.importers('target.js', { limit: 1 });
        assert.equal(r1.total, N);
        assert.equal(r1.importers.length, 1);
        assert.equal(r1.truncated, true);
        assert.equal(r1.next, 1);

        const seen = new Set();
        let offset = 0;
        while (true) {
            const r = await ws.importers('target.js', { limit: 1, offset });
            for (const i of r.importers) seen.add(i.file);
            if (r.next == null) {
                assert.equal(r.truncated, false);
                break;
            }
            offset = r.next;
        }
        assert.equal(seen.size, N);
    });
});
