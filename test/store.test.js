'use strict';
// The store: one okdb environment per workspace, the graph without its
// content, full-text over symbols and (resolved) file content. The store path
// has its own failure modes — FTS silently disabled on re-open, an options bag
// passed where ftsQuery wants a filter — each of which left the index looking
// healthy while search returned nothing, so these run against a real okdb.

const { describe, it, beforeEach, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('../src/workspace');
const { openStore, envNameFor, quoteLines } = require('../src/store');
const { localFs } = require('../src/access');
const { writeFixture, FIXTURE_FILES, tmpRoot, counted } = require('./fixtures/code-fixture');

// A phrase that only ever exists in a workspace file. Its WORDS may be FTS
// terms; the text itself must never be written by okdb.
const PHRASE = 'the quick marmalade octopus negotiates tariffs with seventeen lighthouses';
const CODE = 'return lighthouseTariff(octopus) * 17; }';

/** Every byte okdb wrote under `dir`. */
function storeBytes(dir) {
    const chunks = [];
    const walk = (d) => {
        for (const e of fs.readdirSync(d, { withFileTypes: true })) {
            const p = path.join(d, e.name);
            if (e.isDirectory()) walk(p);
            else chunks.push(fs.readFileSync(p));
        }
    };
    walk(dir);
    return Buffer.concat(chunks);
}

describe('envNameFor', () => {
    it('is okcode_<slug>, never contains ":" or a leading "~", and never collides', () => {
        assert.equal(envNameFor('app'), 'okcode_app');
        for (const id of ['App', 'a.b', 'a_b', 'my:ws', '~tilde', 'D:/project/brain', 'x'.repeat(200), 'ünï']) {
            const n = envNameFor(id);
            assert.match(n, /^okcode_[a-z0-9_]+$/, n);
            assert.ok(n.length <= 60, n);
        }
        assert.notEqual(envNameFor('App'), envNameFor('app'));
        assert.notEqual(envNameFor('a.b'), envNameFor('a_b'));
        assert.equal(envNameFor('a.b'), envNameFor('a.b'), 'deterministic');
        assert.throws(() => envNameFor(''));
    });
});

describe('quoteLines', () => {
    it('quotes the line matching the most query terms, stopwords ignored', () => {
        const text = 'failure here\nthe worst failure mode is silence\nmode\n';
        const q = quoteLines(text, 'the worst failure mode');
        assert.equal(q[0].line, 2);
        assert.equal(q[0].n, 3);
    });
});

describe('store', () => {
    let base;
    let dbDir;
    let db;
    // A fresh okdb per test (≈30ms to open): each test builds its own
    // workspace env, and an unlicensed okdb allows 5 envs (incl. default), so
    // one db shared by the whole suite would refuse the fifth workspace.
    beforeEach(async () => {
        base = tmpRoot('store').base;
        dbDir = path.join(base, 'okdb');
        db = new OKDB(dbDir, { auth: { open: true } });
        await db.open();
    });
    afterEach(async () => {
        if (db) await db.close().catch(() => {});
        fs.rmSync(base, { recursive: true, force: true });
    });

    function project(tag) {
        const root = path.join(base, tag);
        writeFixture(root);
        return root;
    }

    it('persists the graph: a second workspace on the same db + id opens warm', async () => {
        const root = project('warm');
        const st1 = await openStore({ db, id: 'warm', access: localFs(root) });
        assert.equal(st1.fts, true);
        assert.equal(st1.contentFts, true);
        const ws1 = await openWorkspace({ id: 'warm', access: localFs(root), store: st1 });
        await ws1.sync();
        await ws1.close();

        const c = counted(localFs(root));
        const st2 = await openStore({ db, id: 'warm', access: c.access });
        const loaded = st2.load();
        assert.equal(loaded.files.size, FIXTURE_FILES.length);
        assert.ok(
            [...loaded.files.values()].every((f) => f.content === undefined),
            'rows carry no content',
        );
        const ws2 = await openWorkspace({ id: 'warm', access: c.access, store: st2 });
        const s = await ws2.structure();
        assert.equal(s.files, FIXTURE_FILES.length);
        assert.ok(s.symbols >= 6);
        // Warm: one list, no hashing, nothing re-read but .gitignore.
        assert.equal(c.counts.list, 1);
        assert.equal(c.counts.stat, 1, 'only the result-set check of structure()');
        assert.ok(c.counts.read <= 2, `reads: ${c.counts.read}`);
        const hits = await ws2.find('add');
        assert.equal(hits[0].name, 'add');
        await ws2.close();
    });

    it('re-opening must not disable full-text search, and FTS answers across a db restart', async () => {
        const root = project('restart');
        const st = await openStore({ db, id: 'restart', access: localFs(root) });
        const ws = await openWorkspace({ id: 'restart', access: localFs(root), store: st });
        await ws.sync();
        await ws.close();
        await db.close();
        db = new OKDB(dbDir, { auth: { open: true } });
        await db.open();
        const st2 = await openStore({ db, id: 'restart', access: localFs(root) });
        assert.equal(st2.fts, true, 'ensure is idempotent on re-open');
        const sym = st2.search('prose symbol', 10, 'and');
        assert.ok(
            sym.some((h) => h.name === 'add' && h.file === 'lib/math.js' && h.relevance > 0),
            JSON.stringify(sym),
        );
        const content = st2.searchContent('counter step total', 5);
        assert.deepEqual(
            content.map((h) => h.file),
            ['lib/factory.js'],
        );
        const ws2 = await openWorkspace({ id: 'restart', access: localFs(root), store: st2 });
        const found = await ws2.find('indexed prose symbol');
        assert.ok(found.some((h) => h.name === 'add' && (h.via === 'fts' || (h.alsoVia || []).includes('fts'))));
        await ws2.close();
    });

    it('edits and deletions re-index: new words found, old words and removed files gone', async () => {
        const root = project('reindex');
        const st = await openStore({ db, id: 'reindex', access: localFs(root) });
        const ws = await openWorkspace({ id: 'reindex', access: localFs(root), store: st });
        await ws.sync();
        const at = (await ws.read('TAX_RATE')).at;
        const e = await ws.edit('TAX_RATE', '// Levied on zanzibar imports.\nconst TAX_RATE = 0.17;', { at });
        assert.ok(e.ok, JSON.stringify(e));
        await ws.flush();
        assert.deepEqual(
            st.searchContent('zanzibar imports', 5).map((h) => h.file),
            ['lib/math.js'],
        );
        assert.ok(
            st.search('zanzibar', 5).some((h) => h.name === 'TAX_RATE'),
            'symbol prose re-indexed',
        );
        fs.unlinkSync(path.join(root, 'lib', 'math.js'));
        await ws.sync();
        assert.deepEqual(st.searchContent('zanzibar', 5), []);
        assert.equal(st.env.get('files', 'lib/math.js'), undefined);
        assert.equal(st.search('add', 5).filter((h) => h.file === 'lib/math.js').length, 0, 'its symbols went with it');
        await ws.close();
    });

    it('the content resolver reuses text the workspace already read', async () => {
        const root = project('reuse');
        const c = counted(localFs(root));
        const st = await openStore({ db, id: 'reuse', access: c.access });
        const ws = await openWorkspace({ id: 'reuse', access: c.access, store: st });
        await ws.sync(); // cold: walk, read (gitignore + one batch), parse, persist, FTS drain
        assert.ok(c.counts.read <= 2, `FTS indexing re-read files: ${c.counts.read} read calls`);
        assert.deepEqual(
            st.searchContent('counter step total', 5).map((h) => h.file),
            ['lib/factory.js'],
        );
        await ws.close();
    });

    it('secrets are hashed, never read — not by the scan, not by the resolver', async () => {
        const root = project('secret');
        fs.writeFileSync(path.join(root, '.env'), 'API_SECRET=hunter2zebra\n');
        const st = await openStore({ db, id: 'secret', access: localFs(root) });
        const ws = await openWorkspace({ id: 'secret', access: localFs(root), store: st });
        await ws.sync();
        const row = st.env.get('files', '.env');
        assert.ok(row && row.indexed === false && /^[0-9A-F]{40}$/.test(row.hash), JSON.stringify(row));
        assert.deepEqual(st.searchContent('hunter2zebra', 5), []);
        assert.deepEqual(await ws.mentions('hunter2zebra'), []);
        assert.equal((await ws.readFile('.env')).ok, false);
        await ws.close();
    });

    it('drop() removes the workspace env; a fresh open starts empty', async () => {
        const root = project('dropme');
        const st = await openStore({ db, id: 'dropme', access: localFs(root) });
        const ws = await openWorkspace({ id: 'dropme', access: localFs(root), store: st });
        await ws.sync();
        await ws.close();
        const envDir = st.env.path;
        await st.drop();
        await assert.rejects(db.openEnv(envNameFor('dropme')), { code: 'ENV_NOT_FOUND' });
        assert.equal(fs.existsSync(envDir), false);
        const again = await openStore({ db, id: 'dropme', access: localFs(root) });
        assert.equal(again.load().files.size, 0);
    });

    it('no file content lands in okdb — rows, change log, FTS', async () => {
        // Its own db, so the byte scan sees exactly what indexing this wrote.
        const own = tmpRoot('nocontent').base;
        const root = path.join(own, 'src');
        writeFixture(root);
        fs.writeFileSync(
            path.join(root, 'lib', 'secret-sauce.js'),
            // Inside the body: a doc comment ABOVE a symbol is stored as the
            // symbol's `doc` (derived metadata, by design); body text is not.
            `'use strict';\nfunction tariff(octopus) {\n    // ${PHRASE}\n    ${CODE}\nmodule.exports = { tariff };\n`,
        );
        const dir = path.join(own, 'okdb');
        const db2 = new OKDB(dir, { auth: { open: true } });
        await db2.open();
        try {
            const st = await openStore({ db: db2, id: 'nocontent', access: localFs(root) });
            const ws = await openWorkspace({ id: 'nocontent', access: localFs(root), store: st });
            await ws.sync();
            assert.deepEqual(
                st.searchContent('marmalade lighthouses', 5).map((h) => h.file),
                ['lib/secret-sauce.js'],
                'the words are searchable',
            );
            await ws.close();
        } finally {
            await db2.close();
        }
        const bytes = storeBytes(dir);
        assert.equal(bytes.indexOf(PHRASE), -1, 'file text not stored');
        assert.equal(bytes.indexOf(CODE), -1, 'code not stored');
        fs.rmSync(own, { recursive: true, force: true });
    });
});
