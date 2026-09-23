'use strict';
// Listing without reading (docs/DESIGN.md §9): a host builds its menus from
// metadata — sizes, line counts, symbol tables — and never opens a body to
// list. `ws.files()` and `ws.symbols()` answer from the stored rows; on a WARM
// store (reopened, so no text is cached in memory) they make no facade call at
// all, and `stat: true` costs one metadata stat. outline() and structure()
// quote rows too, so they read nothing for a file that did not move. Run over
// every facade — the remote one is where a read is a round trip.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../src/okcode');
const { createTools } = require('../src/tools');
const { writeFixture, FIXTURE_FILES, tmpRoot, facades, counted } = require('./fixtures/code-fixture');

for (const fac of facades()) {
    describe(`listing without reading over ${fac.name}`, { skip: fac.skip }, () => {
        let base;
        let root;
        let store;
        before(async () => {
            if (fac.before) await fac.before();
            ({ base, root } = tmpRoot('list'));
            store = path.join(base, 'okdb');
            writeFixture(root);
            // Build the store once; every test reopens it warm.
            const oc = await okcode.open({ path: store });
            await oc.addWorkspace('app', { access: fac.make(root) });
            await oc.close();
        });
        after(async () => {
            fs.rmSync(base, { recursive: true, force: true });
            if (fac.after) await fac.after();
        });

        // A warm open: the rows come from the store, no text in memory.
        async function warm() {
            const oc = await okcode.open({ path: store });
            const c = counted(fac.make(root));
            const ws = await oc.addWorkspace('app', { access: c.access });
            c.reset();
            return { oc, ws, c };
        }
        const calls = (c) => Object.values(c.counts).reduce((a, n) => a + n, 0);

        it('files() lists sizes, lines and languages from the rows — no facade call', async () => {
            const { oc, ws, c } = await warm();
            try {
                const all = await ws.files();
                assert.equal(all.total, FIXTURE_FILES.length);
                assert.deepEqual(
                    all.files.map((f) => f.file),
                    [...FIXTURE_FILES].sort(),
                );
                const math = all.files.find((f) => f.file === 'lib/math.js');
                assert.equal(math.size, fs.statSync(path.join(root, 'lib/math.js')).size);
                assert.equal(math.lines, fs.readFileSync(path.join(root, 'lib/math.js'), 'utf8').split('\n').length);
                assert.equal(math.lang, 'javascript');
                assert.ok(math.symbols >= 4);
                assert.match(math.hash, /^[0-9A-F]{40}$/);
                assert.ok(all.asOf > 0);

                const lib = await ws.files({ dir: 'lib' });
                assert.ok(lib.files.length >= 4 && lib.files.every((f) => f.file.startsWith('lib/')));
                assert.deepEqual((await ws.files({ dir: 'li' })).files, [], 'a directory, not a string prefix');
                assert.deepEqual(
                    (await ws.files({ dir: './lib/', lang: 'javascript' })).files.map((f) => f.file),
                    ['lib/esm.mjs', 'lib/factory.js', 'lib/math.js', 'lib/unicode.js'],
                );
                const one = await ws.files({ limit: 2 });
                assert.equal(one.files.length, 2);
                assert.equal(one.truncated, true);
                assert.equal(calls(c), 0, `no facade call: ${JSON.stringify(c.counts)}`);

                const checked = await ws.files({ dir: 'lib', stat: true });
                assert.ok(checked.files.every((f) => !f.moved && !f.gone));
                assert.equal(c.counts.stat, 1, 'stat: true is one metadata call');
                assert.equal(c.counts.read, 0);
            } finally {
                await oc.close();
            }
        });

        it('symbols() is the stored symbol table of one file — no facade call', async () => {
            const { oc, ws, c } = await warm();
            try {
                const s = await ws.symbols('lib/math.js');
                assert.equal(s.ok, true);
                assert.equal(s.file, 'lib/math.js');
                assert.equal(s.lang, 'javascript');
                const byName = Object.fromEntries(s.symbols.map((x) => [x.name, x]));
                for (const n of ['add', 'TAX_RATE', 'scale', 'twice']) assert.ok(byName[n], n);
                assert.equal(byName.add.lineStart, 5);
                assert.equal(byName.add.lineEnd, 7);
                assert.equal(byName.add.exported, true);
                assert.match(byName.add.signature, /a, b/);
                assert.match(byName.add.doc, /Adds two numbers/);

                const nested = await ws.symbols('factory.js'); // a basename resolves
                assert.ok(nested.symbols.some((x) => x.path === 'makeCounter.bump' && x.parent === 'makeCounter'));
                assert.equal((await ws.symbols('lib/math.js', { kind: 'function' })).symbols[0].name, 'add');
                const miss = await ws.symbols('lib/nope.js');
                assert.equal(miss.ok, false);
                assert.equal(calls(c), 0, `no facade call: ${JSON.stringify(c.counts)}`);
            } finally {
                await oc.close();
            }
        });

        it('outline(), structure() and the code_outline / code_map tools read nothing that did not move', async () => {
            const { oc, ws, c } = await warm();
            try {
                const o = await ws.outline('lib/math.js');
                assert.equal(o.ok, true);
                assert.equal(o.by, 'symbol');
                assert.ok(o.symbols.some((x) => x.name === 'add'));
                const st = await ws.structure();
                assert.equal(st.files, FIXTURE_FILES.length);
                assert.ok(st.largest.length > 0);
                assert.equal(c.counts.read, 0, `reads: ${JSON.stringify(c.counts)}`);
                // code_map also lists dependencies, which are learned from
                // package.json once and then kept.
                await ws.packages();
                c.reset();
                const tools = createTools({ workspace: async () => ws, workspaces: () => [{ id: 'app' }] });
                assert.match(await tools.render('code_outline', { file: 'lib/math.js' }), /add/);
                assert.match(await tools.render('code_map', {}), /files/);
                await ws.outline('lib/factory.js');
                await ws.structure();
                assert.equal(c.counts.read, 0, `reads: ${JSON.stringify(c.counts)}`);
                assert.equal(c.counts.list, 0);
            } finally {
                await oc.close();
            }
        });

        it('a file changed since the scan: stat flags it, the listing still reads nothing; outline re-reads it', async () => {
            const { oc, ws, c } = await warm();
            const file = path.join(root, 'lib/math.js');
            const original = fs.readFileSync(file, 'utf8');
            try {
                fs.writeFileSync(file, `${original}function extra() { return 1; }\n`);
                const s = await ws.symbols('lib/math.js', { stat: true });
                assert.equal(s.moved, true);
                assert.ok(!s.symbols.some((x) => x.name === 'extra'), 'the rows are as of the scan');
                const f = await ws.files({ dir: 'lib', stat: true });
                assert.deepEqual(
                    f.files.filter((x) => x.moved).map((x) => x.file),
                    ['lib/math.js'],
                );
                assert.equal(c.counts.read, 0);

                // The verifying verb re-reads exactly the moved file.
                const o = await ws.outline('lib/math.js');
                assert.ok(o.symbols.some((x) => x.name === 'extra'));
                assert.equal(c.counts.read, 1);
                const after = await ws.symbols('lib/math.js', { stat: true });
                assert.ok(
                    after.symbols.some((x) => x.name === 'extra'),
                    'the re-ingest updated the rows',
                );
                assert.equal(after.moved, undefined);
            } finally {
                fs.writeFileSync(file, original);
                await oc.close();
            }
        });
    });
}
