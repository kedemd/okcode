'use strict';
// The model-facing tools over a real workspace (localFs over the fixture tree,
// a real okdb store in a temp dir): each tool's text carries the essentials a
// model needs next — locations, the `at` token, snippets — bad input is a
// sentence, never a throw, and the edit round trip read → edit(at) → stale at
// refused works through the tools alone.

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('../src/workspace');
const { openStore } = require('../src/store');
const { localFs } = require('../src/access');
const { createTools, TOOLS } = require('../src/tools');
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

const atOf = (text) => (/\bat=([0-9A-F]{40})\b/.exec(text) || [])[1] || null;

describe('tools', () => {
    const cleanups = [];
    let n = 0;

    after(async () => {
        for (const c of cleanups) await c().catch(() => {});
    });

    // Each test gets its own okdb (≈30ms to open) rather than one env apiece in
    // a shared one: every workspace is an okdb env, and an unlicensed okdb
    // allows 5 (incl. default).
    async function setup({ extra = null, second = false } = {}) {
        const { base, root } = tmpRoot();
        writeFixture(root);
        if (extra) extra(root);
        const access = localFs(root);
        const id = `tools-${++n}`;
        const db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
        await db.open();
        const st = await openStore({ db, id, access });
        const ws = await openWorkspace({ id, access, store: st });
        const map = new Map([[id, ws]]);
        const list = [{ id, root: access.root }];
        if (second) {
            map.set('other', ws);
            list.push({ id: 'other', root: '/nowhere' });
        }
        const tools = createTools({
            workspace: async (hint) => (hint ? map.get(hint) || null : null),
            workspaces: () => list,
        });
        cleanups.push(async () => {
            await ws.close();
            await db.close();
            fs.rmSync(base, { recursive: true, force: true });
        });
        return { root, ws, tools, id };
    }

    it('schemas(): one valid definition per tool', async () => {
        const { tools } = await setup();
        const defs = tools.schemas();
        assert.deepEqual(
            defs.map((d) => d.name),
            TOOLS,
        );
        for (const d of defs) {
            assert.equal(typeof d.description, 'string');
            assert.ok(d.description.length > 40, d.name);
            assert.equal(d.parameters.type, 'object');
            assert.ok(Array.isArray(d.parameters.required));
            for (const r of d.parameters.required) assert.ok(d.parameters.properties[r], `${d.name}.${r}`);
            assert.ok(d.parameters.properties.workspace);
        }
        // One workspace: `workspace` is optional.
        assert.ok(!defs.find((d) => d.name === 'code_read').parameters.required.includes('workspace'));
        assert.ok(tools.isTool('code_edit'));
        assert.ok(!tools.isTool('rm_rf'));
        assert.match(defs.find((d) => d.name === 'code_read').description, /at/);
    });

    it('schemas(): workspace required when there is a choice', async () => {
        const { tools } = await setup({ second: true });
        const defs = tools.schemas();
        assert.ok(defs.find((d) => d.name === 'code_find').parameters.required.includes('workspace'));
        const out = await tools.render('code_find', { query: 'add' });
        assert.match(out, /which workspace/);
        assert.match(out, /other/);
    });

    it('code_map: counts, languages, dependencies, largest symbols', async () => {
        const { tools, id, root } = await setup();
        const out = await tools.render('code_map', {});
        assert.match(out, new RegExp(`^${id} \\(`));
        assert.ok(out.includes(path.basename(root)));
        assert.match(out, /\d+ files, \d+ symbols/);
        assert.match(out, /languages: .*javascript/);
        assert.match(out, /widget@2\.3\.1 — A widget\./);
        assert.match(out, /largest symbols:\n.*makeCounter.*lib\/factory\.js/);
    });

    it('code_find: definitions with location and signature; uses by enclosing symbol', async () => {
        const { tools, id } = await setup();
        const out = await tools.render('code_find', { workspace: id, query: 'add' });
        assert.match(out, /Definitions of "add"/);
        assert.match(out, /function add {2}lib\/math\.js:5-7 {2}\(3 lines\)/);
        const uses = await tools.render('code_find', { workspace: id, query: 'add', uses: true });
        assert.match(uses, /Uses of "add"/);
        assert.match(uses, /lib\/math\.js:\d+ {2}in twice/);
        const none = await tools.render('code_find', { query: 'zzzNoSuchThingzzz' });
        assert.match(none, /no definitions found/);
    });

    it('code_grep: exact text → file:line; absence is stated plainly', async () => {
        const { tools } = await setup();
        const out = await tools.render('code_grep', { text: 'TAX_RATE = 0.17' });
        assert.match(out, /lib\/math\.js:9/);
        assert.match(out, /code_read/);
        const miss = await tools.render('code_grep', { text: 'nowhere-in-this-tree' });
        assert.match(miss, /appears nowhere/);
        assert.match(await tools.render('code_grep', {}), /needs `pattern`/);
    });

    it('code_outline: pieces with ranges and the at', async () => {
        const { tools } = await setup();
        const out = await tools.render('code_outline', { file: 'lib/factory.js' });
        assert.match(out, /^lib\/factory\.js — \d+ lines in \d+ symbols {2}at=[0-9A-F]{40}/);
        assert.match(out, /makeCounter/);
        assert.match(out, /bump/);
        const miss = await tools.render('code_outline', { file: 'lib/nope.js' });
        assert.match(miss, /^\(code_outline "lib\/nope\.js"\) — /);
    });

    it('code_read: symbol, qualified symbol, range, absolute path; misses are sentences', async () => {
        const { tools, root } = await setup();
        const sym = await tools.render('code_read', { symbol: 'add' });
        assert.match(sym, /^lib\/math\.js:5-7 \(function add\) {2}at=[0-9A-F]{40}\n/);
        assert.match(sym, /return a \+ b;/);
        const q = await tools.render('code_read', { symbol: 'factory.js#makeCounter.bump' });
        assert.match(q, /bump/);
        const range = await tools.render('code_read', { symbol: 'lib/math.js:9-11' });
        assert.match(range, /^lib\/math\.js:9-11 of \d+ lines {2}at=/);
        assert.match(range, /TAX_RATE/);
        const abs = await tools.render('code_read', { symbol: path.join(root, 'lib', 'math.js') });
        assert.match(abs, /^lib\/math\.js:1-\d+/);
        const miss = await tools.render('code_read', { symbol: 'noSuchSymbolAnywhere' });
        assert.match(miss, /^\(code_read noSuchSymbolAnywhere\) — no symbol named/);
        assert.ok(!/ask\(\)|find\(\)/.test(miss), 'library verbs are translated to tool names');
        assert.match(await tools.render('code_read', {}), /needs `symbol`/);
    });

    it('code_package: one dependency and the list', async () => {
        const { tools } = await setup();
        const one = await tools.render('code_package', { name: 'widget' });
        assert.match(one, /widget@2\.3\.1/);
        assert.match(one, /build/);
        const all = await tools.render('code_package', {});
        assert.match(all, /dependencies of/);
        const miss = await tools.render('code_package', { name: 'nope-pkg' });
        assert.match(miss, /\(code_package nope-pkg\) — /);
    });

    it('code_ask with no embedder: a sentence pointing at the other eyes', async () => {
        const { tools } = await setup();
        const out = await tools.render('code_ask', { question: 'how are numbers added' });
        assert.match(out, /no semantic index/);
        assert.match(out, /code_find/);
    });

    it('bad input never throws', async () => {
        const { tools } = await setup();
        assert.match(
            await tools.render('code_read', { workspace: 'ghost', symbol: 'add' }),
            /no workspace "ghost" — known workspaces/,
        );
        assert.match(await tools.render('not_a_tool', {}), /not an okcode tool/);
        assert.equal(typeof (await tools.render('code_find', null)), 'string');
        assert.match(await tools.render('code_edit', {}), /REFUSED — code_edit needs/);
        assert.match(await tools.render('code_edit', { symbol: 'add' }), /needs a body/);
        const broken = createTools({
            workspace: async () => {
                throw new Error('host exploded');
            },
        });
        assert.match(await broken.render('code_map', { workspace: 'x' }), /\(code_map\) failed: host exploded/);
    });

    it('edit round trip: code_read → code_edit with its at → stale at refused', async () => {
        const { tools, root } = await setup();
        const read = await tools.render('code_read', { symbol: 'add' });
        const at = atOf(read);
        assert.ok(at, read);
        const edited = await tools.render('code_edit', {
            symbol: 'add',
            at,
            body: 'function add(a, b) {\n    return b + a;\n}',
        });
        assert.match(edited, /^applied: lib\/math\.js \(ok\)/, edited);
        const newAt = atOf(edited);
        assert.ok(newAt && newAt !== at);
        assert.match(edited, /\+ {5}return b \+ a;/);
        assert.match(fs.readFileSync(path.join(root, 'lib/math.js'), 'utf8'), /return b \+ a;/);

        // The old token is now stale: refused, with the current at and region.
        const stale = await tools.render('code_edit', { symbol: 'add', at, body: 'function add(a, b) { return 0; }' });
        assert.match(stale, /REFUSED — the file changed since that token was issued/);
        assert.ok(stale.includes(`current at=${newAt}`), stale);
        assert.match(stale, /── CURRENT lib\/math\.js:5-7/);
        assert.match(fs.readFileSync(path.join(root, 'lib/math.js'), 'utf8'), /return b \+ a;/);

        // No at at all.
        assert.match(await tools.render('code_edit', { symbol: 'add', body: 'x' }), /at is required/);
    });

    it('code_edit find form: unique text lands; duplicates and misses teach the retry', async () => {
        const { tools, root } = await setup({
            extra: (r) =>
                fs.writeFileSync(
                    path.join(r, 'lib/dup.js'),
                    ['function a() {', '    return 1;', '}', 'function b() {', '    return 1;', '}', ''].join('\n'),
                ),
        });
        const ok = await tools.render('code_edit', {
            file: 'lib/math.js',
            find: 'const TAX_RATE = 0.17;',
            body: 'const TAX_RATE = 0.18;',
        });
        assert.match(ok, /^applied: lib\/math\.js \(ok\)/, ok);
        assert.match(fs.readFileSync(path.join(root, 'lib/math.js'), 'utf8'), /TAX_RATE = 0\.18/);

        const dup = await tools.render('code_edit', {
            file: 'lib/dup.js',
            find: '    return 1;',
            body: '    return 2;',
        });
        assert.match(dup, /matches 2 places/);
        assert.match(dup, /within/);

        const within = await tools.render('code_edit', {
            file: 'lib/dup.js',
            find: '    return 1;',
            within: 'function b() {',
            body: '    return 2;',
        });
        assert.match(within, /^applied: lib\/dup\.js/, within);
        assert.equal(
            fs.readFileSync(path.join(root, 'lib/dup.js'), 'utf8'),
            ['function a() {', '    return 1;', '}', 'function b() {', '    return 2;', '}', ''].join('\n'),
        );

        const miss = await tools.render('code_edit', { file: 'lib/dup.js', find: 'return 99;', body: 'x' });
        assert.match(miss, /matches NOTHING/);
        assert.match(miss, /code_write/);
    });

    it('code_edit refuses a candidate that breaks the syntax, with diagnostics', async () => {
        const { tools, root } = await setup();
        const before = fs.readFileSync(path.join(root, 'lib/math.js'), 'utf8');
        const at = atOf(await tools.render('code_read', { symbol: 'add' }));
        const out = await tools.render('code_edit', {
            symbol: 'add',
            at,
            body: 'function add(a, b) {\n    return a + ;\n}',
        });
        assert.match(out, /REFUSED — candidate introduces \d+ new diagnostic/);
        assert.match(out, /new diagnostics:/);
        assert.match(out, /── CURRENT lib\/math\.js/);
        assert.equal(fs.readFileSync(path.join(root, 'lib/math.js'), 'utf8'), before);
    });

    it('code_edit batch form: several edits in one file, overlap refused', async () => {
        const { tools, root } = await setup();
        const at = atOf(await tools.render('code_read', { symbol: 'lib/math.js' }));
        const out = await tools.render('code_edit', {
            at,
            edits: [
                { target: 'lib/math.js#scale', body: 'const scale = (v, by) => by * v;' },
                { target: 'lib/math.js#twice', body: 'const twice = (v) => add(v, v) + 0;' },
            ],
        });
        assert.match(out, /^applied: lib\/math\.js \(ok\)/, out);
        const text = fs.readFileSync(path.join(root, 'lib/math.js'), 'utf8');
        assert.match(text, /by \* v/);
        assert.match(text, /add\(v, v\) \+ 0/);
        const at2 = atOf(out);
        const overlap = await tools.render('code_edit', {
            at: at2,
            edits: [
                { target: 'lib/math.js:5-7', body: 'function add(a, b) { return a + b; }' },
                { target: 'lib/math.js:6-6', body: '    return 0;' },
            ],
        });
        assert.match(overlap, /REFUSED — edit targets overlap/);
    });

    it('code_write: create, refuse re-create, replace with at, refuse without', async () => {
        const { tools, root } = await setup();
        const created = await tools.render('code_write', {
            path: 'lib/new.js',
            content: "'use strict';\nmodule.exports = 1;\n",
        });
        assert.match(created, /^applied: lib\/new\.js \(created, ok\).*at=[0-9A-F]{40}/, created);
        assert.equal(fs.readFileSync(path.join(root, 'lib/new.js'), 'utf8'), "'use strict';\nmodule.exports = 1;\n");

        const again = await tools.render('code_write', { path: 'lib/new.js', content: 'x' });
        assert.match(again, /already exists/);
        assert.match(again, /code_read the file and pass its `at`/);

        const bad = await tools.render('code_write', { path: 'lib/bad.js', content: 'function (' });
        assert.match(bad, /REFUSED — candidate does not validate/);
        assert.ok(!fs.existsSync(path.join(root, 'lib/bad.js')));

        const at = atOf(created);
        const replaced = await tools.render('code_write', {
            path: 'lib/new.js',
            content: "'use strict';\nmodule.exports = 2;\n",
            at,
        });
        assert.match(replaced, /^applied: lib\/new\.js \(ok\)/, replaced);
        assert.match(fs.readFileSync(path.join(root, 'lib/new.js'), 'utf8'), /= 2;/);

        const stale = await tools.render('code_write', { path: 'lib/new.js', content: 'module.exports = 3;\n', at });
        assert.match(stale, /REFUSED — the file changed/);
        assert.match(stale, /current at=/);
        assert.match(await tools.render('code_write', { path: 'lib/x.js' }), /needs `content`/);
    });
});
