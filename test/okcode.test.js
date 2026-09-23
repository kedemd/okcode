'use strict';
// The host-facing surface: open (own store or host's db), the durable
// workspace registry, sync, status, removal and reset. Embedder profiles have
// their own suite (okcode-embedders); the cross-process passive status is in
// okcode-passive.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const okcode = require('../src/okcode');
const { envNameFor } = require('../src/store');
const { localFs } = okcode.access;
const { writeFixture, FIXTURE_FILES, tmpRoot, counted } = require('./fixtures/code-fixture');
const { pipelineName } = require('../src/identity');
// A pipeline's name for an identity (src/identity.js).
const pn = (type, model, dims, endpoint = '') => pipelineName({ type, endpoint, model }, dims);

// okdb's built-in 'fake' embedder: deterministic, no network, no function —
// so its profile persists completely.
const FAKE = { type: 'fake', model: 'fake-v1', dims: 16 };

describe('okcode.open({ path })', () => {
    let base;
    let root;
    let store;
    before(() => {
        base = tmpRoot('oc').base;
        root = path.join(base, 'src');
        store = path.join(base, 'okdb');
        writeFixture(root);
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));

    it('exports the surface', () => {
        assert.equal(typeof okcode.open, 'function');
        assert.equal(typeof okcode.access.localFs, 'function');
        assert.equal(typeof okcode.access.shell, 'function');
        assert.equal(typeof okcode.analysis, 'object');
    });

    it('refuses without path or db, or with both', async () => {
        await assert.rejects(okcode.open({}), /path or an open okdb/);
        await assert.rejects(okcode.open({ path: store, db: {} }), /either path or db/);
    });

    it('adds a workspace, reports it, syncs it; the registry survives a reopen and a re-add is warm', async () => {
        let oc = await okcode.open({ path: store, embedders: { fake: FAKE } });
        try {
            assert.deepEqual(oc.workspaces(), []);
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            assert.equal(oc.workspace('app'), ws);
            assert.equal(oc.workspace('nope'), null);
            assert.ok((await ws.find('add')).some((h) => h.name === 'add' && h.file === 'lib/math.js'));

            const list = oc.workspaces();
            assert.equal(list.length, 1);
            assert.equal(list[0].id, 'app');
            assert.equal(list[0].open, true);
            assert.equal(list[0].env, envNameFor('app'));
            assert.equal(list[0].access.kind, 'localFs');
            assert.ok(list[0].lastSync > 0);
            assert.equal(list[0].lastScan.changed, FIXTURE_FILES.length);

            // A new file, picked up by sync-all.
            fs.writeFileSync(
                path.join(root, 'lib', 'extra.js'),
                'function extra() { return 1; }\nmodule.exports = { extra };\n',
            );
            const synced = await oc.sync();
            assert.equal(synced.length, 1);
            assert.equal(synced[0].id, 'app');
            assert.equal(synced[0].changed, 1);
            assert.equal(synced[0].removed, 0);
            fs.rmSync(path.join(root, 'lib', 'extra.js'));
            const again = await oc.sync('app', { force: true });
            assert.equal(again[0].removed, 1);
            assert.deepEqual((await oc.sync({ force: false }))[0].changed, 0, 'sync({ force }) with no id');

            await ws.flush();
            const st = await oc.status();
            assert.equal(st.workspaces.length, 1);
            const w = st.workspaces[0];
            assert.equal(w.id, 'app');
            assert.equal(w.files, FIXTURE_FILES.length);
            assert.equal(w.symbols, (await ws.stats()).symbols);
            assert.ok(w.symbols > 5);
            assert.equal(w.fts.symbols.status, 'ready');
            assert.equal(w.fts.files.status, 'ready');
            assert.equal(w.embedders.length, 1);
            const e = w.embedders[0];
            assert.equal(e.name, 'fake');
            assert.equal(e.pipeline, pn('fake', 'fake-v1', 16));
            assert.equal(e.dims, 16);
            assert.equal(e.active, true);
            assert.equal(st.active, 'fake');
            const one = await oc.status('app');
            assert.equal(one.workspaces.length, 1);
            assert.equal(one.workspaces[0].files, w.files);
            await assert.rejects(oc.status('nope'), { code: 'OKCODE_UNKNOWN_WORKSPACE' });
        } finally {
            await oc.close();
        }

        // Reopen: the registry knows the workspace, nothing is open until the
        // host re-adds it (the facade is code).
        oc = await okcode.open({ path: store });
        try {
            const list = oc.workspaces();
            assert.deepEqual(
                list.map((x) => [x.id, x.open]),
                [['app', false]],
            );
            assert.equal(oc.workspace('app'), null);
            // The profile persisted with it — no embedders passed this time.
            assert.deepEqual(
                oc.embedders().map((x) => [x.name, x.state, x.active]),
                [['fake', 'configured', true]],
            );
            // Status works on durable state without the workspace open.
            const st = await oc.status('app');
            assert.equal(st.workspaces[0].open, false);
            assert.equal(st.workspaces[0].files, FIXTURE_FILES.length);
            assert.equal(st.workspaces[0].embedders[0].pipeline, pn('fake', 'fake-v1', 16));
            await assert.rejects(oc.sync('app'), { code: 'OKCODE_WORKSPACE_NOT_OPEN' });

            const c = counted(localFs(root));
            const ws = await oc.addWorkspace('app', { access: c.access });
            assert.equal(oc.workspaces()[0].lastScan.changed, 0, 'warm: nothing re-parsed');
            assert.equal(c.counts.stat, 0, 'warm: no file re-hashed');
            assert.ok((await ws.find('makeCounter')).length > 0);
        } finally {
            await oc.close();
        }
    });

    it('removeWorkspace drops the env and forgets the id; the files are untouched', async () => {
        const before = FIXTURE_FILES.map((f) => fs.readFileSync(path.join(root, f), 'utf8'));
        const oc = await okcode.open({ path: store });
        try {
            await oc.addWorkspace('gone', { access: localFs(root) });
            assert.ok(fs.existsSync(path.join(store, envNameFor('gone'))));
            const r = await oc.removeWorkspace('gone');
            assert.equal(r.removed, true);
            assert.equal(oc.workspace('gone'), null);
            assert.ok(!oc.workspaces().some((w) => w.id === 'gone'));
            await assert.rejects(oc.db.openEnv(envNameFor('gone')), { code: 'ENV_NOT_FOUND' });
            assert.equal(fs.existsSync(path.join(store, envNameFor('gone'))), false);
            assert.deepEqual(
                FIXTURE_FILES.map((f) => fs.readFileSync(path.join(root, f), 'utf8')),
                before,
            );
            // A registered workspace not open in this process is removable too.
            assert.ok(oc.workspaces().some((w) => w.id === 'app'));
            await oc.removeWorkspace('app');
            assert.deepEqual(oc.workspaces(), []);
            await assert.rejects(oc.db.openEnv(envNameFor('app')), { code: 'ENV_NOT_FOUND' });
        } finally {
            await oc.close();
        }
    });
});

describe('okcode.open({ db })', () => {
    let base;
    let root;
    let db;
    before(async () => {
        base = tmpRoot('ocdb').base;
        root = path.join(base, 'src');
        writeFixture(root);
        db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
        await db.open();
    });
    after(async () => {
        await db.close().catch(() => {});
        fs.rmSync(base, { recursive: true, force: true });
    });

    it('uses the host db and never closes it; host data stays separate', async () => {
        const host = await db.createEnvironment('hostdata');
        await host.ensureType('notes');
        await host.put('notes', 'n1', { text: 'mine' });
        const oc = await okcode.open({ db });
        assert.equal(oc.db, db);
        const ws = await oc.addWorkspace('app', { access: localFs(root) });
        assert.ok((await ws.find('greet')).length > 0);
        await oc.close();
        // Still open and usable.
        assert.deepEqual(db.env('hostdata').get('notes', 'n1'), { text: 'mine' });
        const oc2 = await okcode.open({ db });
        assert.deepEqual(
            oc2.workspaces().map((w) => w.id),
            ['app'],
        );
        await oc2.close();
    });

    it('reset: vectors re-embed, fts rebuilds, all drops and rescans', async () => {
        db.embeddings.registerEmbedderFactory('fake2', (cfg) => ({
            embed: async (t) => {
                const v = new Float32Array(cfg.dims || 8);
                for (let i = 0; i < v.length; i++) v[i] = ((String(t).charCodeAt(i) || 1) % 7) + 1;
                return v;
            },
        }));
        const oc = await okcode.open({ db, embedders: { f: { type: 'fake2', model: 'f2', dims: 8 } } });
        try {
            const ws = await oc.addWorkspace('rs', { access: localFs(root) });
            await ws.store.settle();
            let e = (await oc.status('rs')).workspaces[0].embedders[0];
            assert.equal(e.state, 'ready', JSON.stringify(e));
            const done = e.done;
            assert.ok(done > 0);

            const rv = await oc.reset('rs', { scope: 'vectors' });
            assert.equal(rv.rebuilt.length, 1);
            assert.equal(rv.rebuilt[0].pipeline, pn('fake2', 'f2', 8));
            await ws.store.settle();
            e = (await oc.status('rs')).workspaces[0].embedders[0];
            assert.equal(e.done, done, 're-embedded everything');
            assert.equal(e.failed, 0);

            const rf = await oc.reset('rs', { scope: 'fts' });
            assert.deepEqual(rf.reset, ['symbols', 'files']);
            const w = (await oc.status('rs')).workspaces[0];
            assert.equal(w.fts.symbols.status, 'ready');
            assert.equal(w.fts.files.status, 'ready');
            assert.ok((await ws.find('makeCounter')).length > 0);
            assert.ok((await ws.find('marker')).length > 0);

            const ra = await oc.reset('rs', { scope: 'all' });
            assert.equal(ra.dropped, true);
            assert.equal(ra.rescanned, true);
            assert.equal(ra.changed, FIXTURE_FILES.length, 'rescanned from nothing');
            const ws2 = oc.workspace('rs');
            assert.notEqual(ws2, ws, 'a fresh workspace object');
            assert.ok((await ws2.find('makeCounter')).length > 0);
            await ws2.store.settle();
            const after = (await oc.status('rs')).workspaces[0];
            assert.equal(after.files, FIXTURE_FILES.length);
            assert.equal(after.embedders[0].done, done, JSON.stringify(after.embedders));

            await assert.rejects(oc.reset('rs', { scope: 'bogus' }), /unknown scope/);
            await assert.rejects(oc.reset('nope', { scope: 'vectors' }), { code: 'OKCODE_UNKNOWN_WORKSPACE' });
        } finally {
            await oc.close();
        }
    });
});
