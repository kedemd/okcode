'use strict';
// Navigation without an index: a workspace added with `index: 'none'` is
// served the way library code is — listed, read, grepped and outlined straight
// from its facade — with no okdb env, no store and no vectors; the read-only
// facades (access.fn, access.memory); the titled outline of texts without
// symbols (regions) and `file#Heading` reads; and the context a host's embed
// function receives (which workspace it embeds for).

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../lib/okcode');
const { regions, sectionOf } = require('../lib/analysis/chunk');
const { envNameFor } = require('../lib/store');
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

const MD = [
    'Fetched from https://example.test/guide', //  1
    '', //                                          2
    '# Guide', //                                   3
    'An introduction.', //                          4
    '', //                                          5
    '## Install', //                                6
    'npm install widget', //                        7
    'then restart.', //                             8
    '', //                                          9
    '## Usage', //                                 10
    '```sh', //                                    11
    '# a comment, not a heading', //               12
    '```', //                                      13
    '', //                                         14
    '# Appendix', //                               15
    'last words', //                               16
    '',
].join('\n');
const LOG =
    Array.from({ length: 450 }, (_, i) => `12:00:${String(i).padStart(3, '0')} step ${i + 1} done`).join('\n') + '\n';
const JSON_DOC = JSON.stringify({ name: 'widget', versions: [1, 2, 3], config: { a: 1 } }, null, 2) + '\n';

const memoryMap = () =>
    new Map([
        ['reads/guide.md', { bytes: MD, mtime: 1 }],
        ['reads/run.txt', { bytes: Buffer.from(LOG), mtime: 1 }],
        ['reads/data.json', { bytes: JSON_DOC, mtime: 1 }],
    ]);

describe('regions(): the titled outline of a text without symbols', () => {
    it('markdown: headings nest, fenced code is not a heading, the preamble is a section', () => {
        const r = regions('guide.md', MD);
        assert.deepEqual(
            r.map((g) => [g.kind, g.title, g.lineStart, g.lineEnd, g.level || 0]),
            [
                ['section', 'Fetched from https://example.test/guide', 1, 1, 0],
                ['heading', 'Guide', 3, 13, 1],
                ['heading', 'Install', 6, 8, 2],
                ['heading', 'Usage', 10, 13, 2],
                ['heading', 'Appendix', 15, 16, 1],
            ],
        );
    });
    it('a long log: line runs of a sensible size; prose: merged paragraphs', () => {
        const r = regions('run.txt', LOG);
        assert.deepEqual(
            r.map((g) => [g.kind, g.lineStart, g.lineEnd]),
            [
                ['lines', 1, 200],
                ['lines', 201, 400],
                ['lines', 401, 450],
            ],
        );
        assert.match(r[1].title, /step 201 done/);
        const prose = regions('notes.txt', 'First para\nstill first\n\nSecond para\n\n\nThird\n');
        assert.deepEqual(
            prose.map((g) => [g.kind, g.title, g.lineStart, g.lineEnd]),
            [['paragraph', 'First para', 1, 7]],
        );
        const small = regions('notes.txt', 'a\n\nb\n', { maxLines: 1 });
        assert.deepEqual(
            small.map((g) => [g.title, g.lineStart, g.lineEnd]),
            [
                ['a', 1, 1],
                ['b', 3, 3],
            ],
        );
    });
    it('JSON members by key, arrays grouped; pages by form feed; HTML headings; code is not a document', () => {
        assert.deepEqual(
            regions('d.json', JSON_DOC).map((g) => [g.kind, g.title, g.lineStart, g.lineEnd]),
            [
                ['member', 'name', 2, 2],
                ['member', 'versions', 3, 7],
                ['member', 'config', 8, 10],
            ],
        );
        assert.deepEqual(
            regions('a.json', JSON.stringify([{ x: 1 }, { x: 2 }, { x: 3 }], null, 2)).map((g) => g.title),
            ['[0]–[2]'],
        );
        assert.deepEqual(
            regions('doc.txt', 'one\ntext\n\fsecond page\nmore\n\fthird\n').map((g) => [
                g.kind,
                g.title,
                g.lineStart,
                g.lineEnd,
            ]),
            [
                ['page', 'page 1', 1, 2],
                ['page', 'page 2', 3, 4],
                ['page', 'page 3', 5, 5],
            ],
        );
        assert.deepEqual(
            regions('p.html', '<p>x</p>\n<h1>Top <b>news</b></h1>\n<p>y</p>\n<h2>Sub</h2>\nz\n').map((g) => [
                g.title,
                g.lineStart,
                g.lineEnd,
            ]),
            [
                ['<p>x</p>', 1, 1],
                ['Top news', 2, 5],
                ['Sub', 4, 5],
            ],
        );
        assert.equal(regions('x.js', 'const a = 1;\n'), null);
    });
    it('sectionOf: by title as written, then ignoring case and leading #s; ambiguity said', () => {
        assert.equal(sectionOf('g.md', MD, 'Install').lineStart, 6);
        assert.equal(sectionOf('g.md', MD, '## install').lineEnd, 8);
        assert.equal(sectionOf('g.md', MD, 'Nope'), null);
        assert.equal(sectionOf('g.md', '# A\nx\n# A\ny\n', 'A').ambiguous.length, 2);
    });
});

describe('access.fn / access.memory: read-only facades', () => {
    it('memory: list (confined to under, limited), stat derived from list, read as Buffers, no commit', async () => {
        const map = memoryMap();
        // `.git` is on the fixed PRUNE_DOT_DIRS list (pruned, like every
        // other facade); `.storybook` is not — it is real source in a real
        // project and must survive, same as local-fs/shell/fn all now agree.
        map.set('reads/.git/x.md', { bytes: 'x', mtime: 1 });
        map.set('reads/.storybook/main.md', { bytes: 's', mtime: 1 });
        map.set('reads/node_modules/y.md', { bytes: 'y', mtime: 1 });
        const a = okcode.access.memory(map);
        assert.equal(typeof a.commit, 'undefined');
        assert.deepEqual(
            (await a.list()).map((r) => r.path).sort(),
            ['reads/.storybook/main.md', 'reads/data.json', 'reads/guide.md', 'reads/run.txt'].sort(),
        );
        assert.deepEqual(
            (await a.list({ under: 'reads/guide.md' })).map((r) => r.path),
            ['reads/guide.md'],
        );
        const lim = await a.list({ limit: 2 });
        assert.equal(lim.length, 2);
        assert.equal(lim.truncated, true);
        const st = await a.stat(['reads/guide.md', 'reads/none.md'], { hash: true });
        assert.equal(st.get('reads/guide.md').size, Buffer.byteLength(MD));
        assert.match(st.get('reads/guide.md').hash, /^[0-9A-F]{40}$/);
        assert.deepEqual(st.get('reads/none.md'), { missing: true });
        const got = await a.read(['reads/guide.md', 'reads/none.md']);
        assert.ok(Buffer.isBuffer(got.get('reads/guide.md')));
        assert.equal(got.has('reads/none.md'), false);
        await assert.rejects(a.read(['../etc/passwd']), /escapes the root/);
    });
    it('fn: the host functions, a plain-object read answer accepted', async () => {
        const a = okcode.access.fn({
            list: async () => [
                { path: 'a.txt', size: 3, mtime: 5 },
                { path: '/abs.txt', size: 1, mtime: 1 },
            ],
            read: async (paths) => Object.fromEntries(paths.filter((p) => p === 'a.txt').map((p) => [p, 'abc'])),
        });
        assert.deepEqual(await a.list(), [{ path: 'a.txt', size: 3, mtime: 5 }]);
        assert.deepEqual((await a.stat(['a.txt'])).get('a.txt'), { size: 3, mtime: 5 });
        assert.equal((await a.read(['a.txt'])).get('a.txt').toString(), 'abc');
        assert.throws(() => okcode.access.fn({ list: () => [] }), /list and read are required/);
    });
});

describe("addWorkspace(id, { index: 'none' })", () => {
    let base;
    let oc;
    let ws;
    before(async () => {
        base = tmpRoot('unindexed').base;
        oc = await okcode.open({ path: path.join(base, 'okdb') });
        ws = await oc.addWorkspace('reads:task-1', { access: okcode.access.memory(memoryMap()), index: 'none' });
    });
    after(async () => {
        await oc.close();
        fs.rmSync(base, { recursive: true, force: true });
    });

    it('creates no okdb env and no registry record', async () => {
        assert.equal(ws.index, 'none');
        assert.equal(ws.readOnly, true);
        await assert.rejects(oc.db.openEnv(envNameFor('reads:task-1')), (e) => e.code === 'ENV_NOT_FOUND');
        assert.equal(oc.meta.get('workspaces', 'reads:task-1'), undefined);
        const s = await oc.status('reads:task-1');
        assert.equal(s.workspaces[0].index, 'none');
        assert.equal(s.workspaces[0].env, null);
        assert.deepEqual(
            oc.workspaces().find((w) => w.id === 'reads:task-1'),
            { id: 'reads:task-1', env: null, index: 'none', open: true },
        );
    });
    it('outline: a document by its headings, a log by line runs, the root by files', async () => {
        const o = await ws.outline('reads/guide.md');
        assert.equal(o.ok, true);
        assert.equal(o.by, 'section');
        assert.deepEqual(
            o.symbols.map((s) => [s.kind, s.name, s.path]),
            [
                ['section', 'Fetched from https://example.test/guide', 'reads/guide.md:1-1'],
                ['heading', 'Guide', 'reads/guide.md#Guide'],
                ['heading', 'Install', 'reads/guide.md#Install'],
                ['heading', 'Usage', 'reads/guide.md#Usage'],
                ['heading', 'Appendix', 'reads/guide.md#Appendix'],
            ],
        );
        const l = await ws.outline('reads/run.txt');
        assert.deepEqual(
            l.symbols.map((s) => s.path),
            ['reads/run.txt:1-200', 'reads/run.txt:201-400', 'reads/run.txt:401-450'],
        );
        const all = await ws.outline('.');
        assert.equal(all.by, 'files');
        assert.deepEqual(
            all.entries.map((e) => e.file),
            ['reads/data.json', 'reads/guide.md', 'reads/run.txt'],
        );
        assert.deepEqual(
            all.entries[1].symbols.map((x) => x.name),
            ['Guide', 'Install', 'Usage', 'Appendix'],
        );
    });
    it('read: a heading section, a range, a whole file — bare paths, no index needed', async () => {
        const sec = await ws.read('reads/guide.md#Install');
        assert.equal(sec.ok, true);
        assert.equal(sec.body, '## Install\nnpm install widget\nthen restart.');
        assert.deepEqual([sec.lineStart, sec.lineEnd], [6, 8]);
        const r = await ws.read('reads/run.txt:42-43');
        assert.equal(r.body, '12:00:041 step 42 done\n12:00:042 step 43 done');
        const whole = await ws.read('reads/data.json');
        assert.equal(whole.body, JSON_DOC);
        const miss = await ws.read('reads/guide.md#Nowhere');
        assert.equal(miss.ok, false);
        assert.match(miss.reason, /no symbol or heading "Nowhere"/);
    });
    it('grep and find: every file, straight from the facade; glob and files list it', async () => {
        const g = await ws.grep('step 300 done');
        assert.deepEqual(
            g.matches.map((m) => [m.file, m.line]),
            [['reads/run.txt', 300]],
        );
        assert.equal(g.indexed, false);
        const scoped = await ws.grep('widget', { paths: ['reads/guide.md'] });
        assert.deepEqual(
            scoped.matches.map((m) => m.line),
            [7],
        );
        const f = await ws.find('install');
        assert.deepEqual(
            f.map((h) => [h.file, h.name, h.lines]),
            [['reads/guide.md', 'Install', '6-8']],
        );
        assert.deepEqual(
            (await ws.glob('*.json')).files.map((x) => x.file),
            ['reads/data.json'],
        );
        assert.equal((await ws.files()).total, 3);
    });
    it('ask, refs and every edit refuse; reset and compare refuse', async () => {
        await assert.rejects(ws.ask('how to install'), (e) => e.code === 'OKCODE_NOT_INDEXED');
        await assert.rejects(ws.refs('widget'), (e) => e.code === 'OKCODE_NOT_INDEXED');
        const e1 = await ws.editBatch({ at: 'X', edits: [{ target: 'reads/guide.md:1-1', body: 'y' }] });
        assert.equal(e1.code, 'OKCODE_READ_ONLY');
        const e2 = await ws.writeWholeFile('reads/new.md', 'x', { create: true });
        assert.equal(e2.code, 'OKCODE_READ_ONLY');
        await assert.rejects(oc.reset('reads:task-1', { scope: 'all' }), (e) => e.code === 'OKCODE_NOT_INDEXED');
        await assert.rejects(oc.compare('reads:task-1', 'x'), (e) => e.code === 'OKCODE_NOT_INDEXED');
    });
    it('sees the facade live: a read added later is there on the next call', async () => {
        const map = memoryMap();
        const w2 = await oc.addWorkspace('reads:task-2', { access: okcode.access.memory(map), index: 'none' });
        assert.equal((await w2.grep('late arrival')).total.lines, 0);
        map.set('reads/late.txt', { bytes: 'a late arrival\n', mtime: 2 });
        assert.equal((await w2.grep('late arrival')).total.lines, 1);
        assert.deepEqual(await oc.removeWorkspace('reads:task-2'), { id: 'reads:task-2', removed: true });
        assert.equal(oc.workspace('reads:task-2'), null);
    });
    it('an id an indexed workspace holds is refused', async () => {
        const root = path.join(base, 'src');
        writeFixture(root);
        await oc.addWorkspace('app', { access: okcode.access.localFs(root) });
        await assert.rejects(
            oc.addWorkspace('app', { access: okcode.access.memory(memoryMap()), index: 'none' }),
            (e) => e.code === 'OKCODE_WORKSPACE_EXISTS',
        );
        await assert.rejects(
            oc.addWorkspace('x', { access: okcode.access.memory(memoryMap()), index: 'some' }),
            /index must be/,
        );
    });
});

describe('indexed workspaces: sections, headings, read-only facades', () => {
    let base;
    let oc;
    before(async () => {
        base = tmpRoot('sections').base;
        oc = await okcode.open({ path: path.join(base, 'okdb') });
    });
    after(async () => {
        await oc.close();
        fs.rmSync(base, { recursive: true, force: true });
    });

    it('outline of a markdown file is its sections; read file#Heading reads one', async () => {
        const root = path.join(base, 'src');
        writeFixture(root);
        fs.writeFileSync(path.join(root, 'GUIDE.md'), MD);
        const ws = await oc.addWorkspace('docs', { access: okcode.access.localFs(root) });
        const o = await ws.outline('GUIDE.md');
        assert.equal(o.by, 'section');
        assert.ok(o.at);
        assert.deepEqual(
            o.symbols.map((s) => s.name),
            ['Fetched from https://example.test/guide', 'Guide', 'Install', 'Usage', 'Appendix'],
        );
        const r = await ws.read('GUIDE.md#Usage');
        assert.equal(r.ok, true);
        assert.deepEqual([r.lineStart, r.lineEnd], [10, 13]);
        assert.equal(r.at, o.at);
        // a code file without that symbol still says "no symbol"
        assert.equal((await ws.read('lib/math.js#nothere')).ok, false);
    });
    it('an indexed workspace over a facade without commit refuses edits', async () => {
        const map = new Map([['notes.md', { bytes: '# Notes\nhello\n', mtime: 1 }]]);
        const ws = await oc.addWorkspace('ro', { access: okcode.access.memory(map) });
        assert.equal(ws.index, 'full');
        assert.equal(ws.readOnly, true);
        const read = await ws.read('notes.md');
        assert.equal(read.ok, true);
        const e = await ws.editBatch({ at: read.at, edits: [{ target: 'notes.md:2-2', body: 'bye' }] });
        assert.equal(e.code, 'OKCODE_READ_ONLY');
        assert.match(e.reason, /no commit/);
    });
});

describe('embed(texts, { workspace }): a custom embedder is told whose text it embeds', () => {
    let base;
    before(() => {
        base = tmpRoot('embedctx').base;
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));

    it('each workspace pipeline passes its own id; a one-argument embed still works', async () => {
        const seen = [];
        const embed = async (texts, ctx) => {
            seen.push(ctx && ctx.workspace);
            return texts.map(() => [1, 0, 0, 0]);
        };
        const oc = await okcode.open({
            path: path.join(base, 'okdb'),
            embedders: { c: { id: 'ctx@1', dims: 4, embed } },
        });
        try {
            for (const id of ['alpha', 'beta']) {
                const root = path.join(base, id);
                fs.mkdirSync(root, { recursive: true });
                fs.writeFileSync(
                    path.join(root, 'a.js'),
                    `// ${id}: a file long enough to be worth a vector.\n`.repeat(4) + 'function f() {}\n',
                );
                await oc.addWorkspace(id, { access: okcode.access.localFs(root) });
            }
            for (let i = 0; i < 100 && !(seen.includes('alpha') && seen.includes('beta')); i++) {
                await new Promise((r) => setTimeout(r, 100));
            }
            assert.ok(seen.includes('alpha') && seen.includes('beta'), JSON.stringify(seen));
            assert.ok(
                seen.every((w) => w === 'alpha' || w === 'beta'),
                JSON.stringify(seen),
            );
        } finally {
            await oc.close();
        }
    });
});
