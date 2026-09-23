'use strict';
// Freshness (docs/DESIGN.md §6), ported from the brain's
// test/codeindex-freshness.js: reads never walk the tree, and the graph changes
// only at explicit sync points —
//
//   - OPEN and sync() are the only paths that call the facade's list();
//   - a plain lookup after open never lists;
//   - a read re-stats only the ONE file it is about to quote;
//   - an index-wide verb (mentions/find/refs) re-checks only ITS OWN result
//     set and re-runs once if any of it moved;
//   - an OWN write updates its row from the bytes it just published;
//   - a brand-new external file is invisible until sync().
//
// Run through each facade; what is counted is the CALL COUNT into the real
// facade, not a mocked answer.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { openWorkspace } = require('../src/workspace');
const { tmpRoot, facades, counted } = require('./fixtures/code-fixture');

for (const fac of facades()) {
    describe(`freshness over ${fac.name}`, { skip: fac.skip }, () => {
        const bases = [];
        before(async () => {
            if (fac.before) await fac.before();
        });
        after(async () => {
            for (const b of bases) fs.rmSync(b, { recursive: true, force: true });
            if (fac.after) await fac.after();
        });

        async function fixture() {
            const { base, root } = tmpRoot('fresh');
            bases.push(base);
            fs.mkdirSync(root, { recursive: true });
            const write = (rel, text) => fs.writeFileSync(path.join(root, rel), text);
            write('a.js', 'function a() { return 1; }\nmodule.exports = { a };\n');
            write('b.js', 'function b() { return 2; }\nmodule.exports = { b };\n');
            const c = counted(fac.make(root));
            const ws = await openWorkspace({ id: 'fresh', access: c.access });
            await ws.refresh(); // OPEN — the one walk this design allows
            c.reset();
            return { root, ws, c, write };
        }

        it('after open, lookups never walk the tree', async () => {
            const { ws, c } = await fixture();
            assert.equal((await ws.mentions('function')).length, 2);
            assert.equal((await ws.mentions('function')).length, 2);
            await ws.find('a');
            await ws.readFile('a.js');
            await ws.outline('a.js');
            await ws.refs('return');
            assert.equal(c.counts.list, 0);
        });

        it('a read of an unchanged file costs one stat and no read', async () => {
            const { ws, c } = await fixture();
            await ws.readFile('a.js');
            c.reset();
            const r = await ws.readFile('a.js');
            assert.ok(r.ok);
            assert.equal(c.counts.stat, 1);
            assert.equal(c.counts.read, 0);
        });

        it('an external write to a tracked file is visible to the very next read', async () => {
            const { ws, write } = await fixture();
            assert.ok((await ws.readFile('a.js')).body.includes('return 1'));
            write('a.js', 'function a() { return 999; }\nmodule.exports = { a };\n');
            const after = await ws.readFile('a.js');
            assert.ok(after.body.includes('return 999') && !after.body.includes('return 1;'));
        });

        it('an external write is reflected in the next grep that would have returned it', async () => {
            const { ws, write } = await fixture();
            assert.deepEqual(
                (await ws.mentions('return 1')).map((m) => m.rel),
                ['a.js'],
            );
            write('a.js', 'function a() { return 42; }\nmodule.exports = { a };\n');
            assert.deepEqual(await ws.mentions('return 1'), [], 'the result-set check caught the change and re-ran');
            assert.deepEqual(
                (await ws.mentions('return 42')).map((m) => m.rel),
                ['a.js'],
            );
        });

        it('an own write is visible to an immediate read with no walk', async () => {
            const { ws, c } = await fixture();
            const r1 = await ws.read('a');
            const edited = await ws.edit('a', 'function a() { return 777; }', { at: r1.at });
            assert.ok(edited.ok, JSON.stringify(edited));
            const r2 = await ws.readFile('a.js');
            assert.ok(r2.body.includes('return 777'));
            assert.equal(c.counts.list, 0);
        });

        it('a file deleted externally disappears from reads and from grep results', async () => {
            const { root, ws } = await fixture();
            assert.ok((await ws.readFile('b.js')).ok);
            assert.equal((await ws.mentions('return 2')).length, 1);
            fs.unlinkSync(path.join(root, 'b.js'));
            assert.equal((await ws.readFile('b.js')).ok, false);
            assert.deepEqual(await ws.mentions('return 2'), []);
        });

        it('a brand-new external file appears only after sync(), which walks exactly once', async () => {
            const { ws, c, write } = await fixture();
            write('c.js', 'function c() { return 3; }\nmodule.exports = { c };\n');
            assert.deepEqual(await ws.mentions('return 3'), []);
            assert.equal((await ws.readFile('c.js')).ok, false);
            assert.equal(c.counts.list, 0);
            await ws.sync();
            assert.equal(c.counts.list, 1);
            assert.deepEqual(
                (await ws.mentions('return 3')).map((m) => m.rel),
                ['c.js'],
            );
            assert.ok((await ws.readFile('c.js')).ok);
            await ws.sync();
            assert.equal(c.counts.list, 2);
        });

        it('a sync of an untouched tree hashes and reads nothing', async () => {
            const { ws, c } = await fixture();
            const r = await ws.sync();
            assert.equal(r.reparsed, 0);
            assert.equal(c.counts.list, 1);
            assert.equal(c.counts.stat, 0);
            // .gitignore is re-read on every walk (it is how a sync sees it change).
            assert.ok(c.counts.read <= 1);
            const forced = await ws.sync({ force: true });
            assert.equal(forced.reparsed, 0, 'force re-hashes, but re-parses only what moved');
        });
    });
}
