'use strict';
// Embedder profiles on the host surface (docs/DESIGN.md §8): a built-in type
// (a fake factory registered on okdb), an apiKey FUNCTION that must never
// reach disk, a custom embed function; adding a profile alongside, switching
// the active one, comparing, removing; and a persisted profile whose function
// the host did not supply again.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const okcode = require('../src/okcode');
const { localFs } = okcode.access;
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

// A bag of words with a tiny synonym table (as in store-embeddings): enough
// "meaning" for a vector to find what no lexical eye can.
const CONCEPTS = {
    backoff: 'RETRY',
    retry: 'RETRY',
    wait: 'RETRY',
    longer: 'RETRY',
    again: 'RETRY',
    patience: 'RETRY',
};
function bow(text, dims) {
    const v = new Float32Array(dims);
    for (const raw of String(text)
        .toLowerCase()
        .split(/[^a-z0-9]+/)) {
        if (raw.length < 3) continue;
        const t = CONCEPTS[raw] || raw;
        let h = 2166136261;
        for (let i = 0; i < t.length; i++) h = Math.imul(h ^ t.charCodeAt(i), 16777619);
        v[(h >>> 0) % dims] += CONCEPTS[raw] ? 4 : 1;
    }
    const n = Math.hypot(...v) || 1;
    for (let i = 0; i < dims; i++) v[i] /= n;
    return v;
}

const RETRY_JS = [
    "'use strict';",
    '// When the server refuses, wait longer each time before trying again.',
    'function backoff(attempt) {',
    '    // Retry with a delay that doubles on every attempt: wait, then retry again.',
    '    const wait = Math.min(30000, 2 ** attempt * 100);',
    '    return wait; // longer and longer between retries',
    '}',
    'module.exports = { backoff };',
    '',
].join('\n');

const SECRET = 'sk-okcode-test-SECRET-7f3a9c1e5b';

/** Every byte under `dir`. */
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

describe('embedder profiles', () => {
    let base;
    let root;
    let dbPath;
    let db;
    // What the 'keyed' provider saw: the api_key okdb handed it, per call.
    const keysSeen = [];
    const keyCalls = { n: 0 };
    const customCalls = { single: 0, batch: 0 };

    before(async () => {
        base = tmpRoot('ocemb').base;
        root = path.join(base, 'src');
        dbPath = path.join(base, 'okdb');
        writeFixture(root);
        fs.writeFileSync(path.join(root, 'lib', 'retry.js'), RETRY_JS);
        db = new OKDB(dbPath, { auth: { open: true } });
        // A "built-in" provider as okdb sees one: a registered factory type.
        db.embeddings.registerEmbedderFactory('bow', (cfg) => ({
            embed: async (t) => (Array.isArray(t) ? t.map((x) => bow(x, cfg.dims || 8)) : bow(t, cfg.dims || 8)),
        }));
        // A provider that needs an api key (stands in for 'openai').
        db.embeddings.registerEmbedderFactory('keyed', (cfg) => {
            if (!cfg.api_key) throw new Error('keyed: api_key required');
            return {
                embed: async (t) => {
                    keysSeen.push(cfg.api_key);
                    return bow(t, cfg.dims || 8);
                },
                embedBatch: async (texts) => {
                    keysSeen.push(cfg.api_key);
                    return texts.map((t) => bow(t, cfg.dims || 8));
                },
            };
        });
        await db.open();
    });
    after(async () => {
        await db.close().catch(() => {});
        fs.rmSync(base, { recursive: true, force: true });
    });

    const PROFILES = () => ({
        small: { type: 'bow', model: 'bow', dims: 32 },
        secret: {
            type: 'keyed',
            model: 'keyed-model',
            dims: 24,
            apiKey: async () => {
                keyCalls.n++;
                return SECRET;
            },
        },
        custom: {
            model: 'my-local',
            dims: 40,
            // Written batch-shaped: texts in, vectors out (plain arrays).
            embed: async (texts) => {
                if (Array.isArray(texts)) {
                    customCalls.batch++;
                    return texts.map((t) => Array.from(bow(t, 40)));
                }
                customCalls.single++;
                return Array.from(bow(texts, 40));
            },
        },
    });

    it('builds every profile; ask follows the active one; the apiKey function is called, never stored', async () => {
        const oc = await okcode.open({ db, embedders: PROFILES(), active: 'small' });
        try {
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            await ws.store.settle();

            const st = await oc.status('app');
            const byName = Object.fromEntries(st.workspaces[0].embedders.map((e) => [e.name, e]));
            assert.deepEqual(Object.keys(byName).sort(), ['custom', 'secret', 'small']);
            assert.equal(byName.small.pipeline, 'code_bow_32');
            assert.equal(byName.secret.pipeline, 'code_keyed_model_24');
            assert.equal(byName.custom.pipeline, 'code_my_local_40');
            for (const e of Object.values(byName)) {
                assert.equal(e.state, 'ready', JSON.stringify(e));
                assert.ok(e.done >= 5, JSON.stringify(e));
                assert.equal(e.failed, 0);
            }
            assert.equal(byName.small.active, true);
            assert.equal(oc.active, 'small');

            assert.ok(keyCalls.n > 0, 'the key function was called');
            assert.ok(keysSeen.length > 0 && keysSeen.every((k) => k === SECRET), 'the provider got the key');
            assert.ok(customCalls.batch + customCalls.single > 0, 'the custom embed ran');

            // Every profile answers; the default follows useEmbedder.
            for (const profile of ['small', 'secret', 'custom']) {
                const hits = await ws.ask('patience', { profile, limit: 3 });
                assert.equal(hits[0].file, 'lib/retry.js', `${profile}: ${JSON.stringify(hits)}`);
            }
            const before = keyCalls.n;
            oc.useEmbedder('secret');
            assert.equal(oc.active, 'secret');
            await ws.ask('patience', { limit: 2 });
            assert.ok(keyCalls.n > before, 'ask() without a profile embedded the query with the active profile');
            assert.throws(() => oc.useEmbedder('nope'), { code: 'OKCODE_UNKNOWN_EMBEDDER' });
            await assert.rejects(ws.ask('patience', { profile: 'nope' }), { code: 'OKCODE_NO_EMBEDDINGS' });

            const cmp = await oc.compare('app', 'patience', ['small', 'custom'], { limit: 2 });
            assert.deepEqual(Object.keys(cmp), ['small', 'custom']);
            for (const hits of Object.values(cmp)) assert.equal(hits[0].file, 'lib/retry.js');
            const all = await oc.compare('app', 'patience');
            assert.deepEqual(Object.keys(all).sort(), ['custom', 'secret', 'small']);
        } finally {
            await oc.close();
        }
        // The secret is nowhere in the store — not in okcode's records, not in
        // okdb's embedder records.
        assert.equal(storeBytes(dbPath).indexOf(SECRET), -1, 'the api key reached disk');
        const rec = db.env('okcode').get('embedders', 'secret');
        assert.equal(rec.apiKey, 'function');
        assert.equal(JSON.stringify(rec).includes(SECRET), false);
        assert.equal(typeof rec.fields, 'object');
    });

    it('a persisted profile whose function was not supplied again is needs-config, not dropped', async () => {
        // Only 'small' re-supplied: 'secret' (apiKey fn) and 'custom' (embed fn)
        // come from the store alone.
        const oc = await okcode.open({ db, embedders: { small: PROFILES().small } });
        try {
            const ems = Object.fromEntries(oc.embedders().map((e) => [e.name, e]));
            assert.equal(ems.small.state, 'configured');
            assert.equal(ems.secret.state, 'needs-config');
            assert.equal(ems.custom.state, 'needs-config');
            // The active profile was 'secret' (persisted), which cannot run
            // here: ask falls back to the first usable one.
            assert.equal(oc.active, 'small');

            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            const st = await oc.status('app');
            const byName = Object.fromEntries(st.workspaces[0].embedders.map((e) => [e.name, e]));
            assert.equal(byName.secret.state, 'needs-config');
            assert.match(byName.secret.error, /apiKey/);
            assert.equal(byName.custom.state, 'needs-config');
            assert.match(byName.custom.error, /embed/);
            // Its pipeline and vectors are still there, reported as such.
            assert.equal(byName.secret.pipeline, 'code_keyed_model_24');
            assert.ok(byName.secret.done >= 5);
            assert.equal(byName.small.state, 'ready');

            assert.throws(() => oc.useEmbedder('secret'), { code: 'OKCODE_NEEDS_CONFIG' });
            await assert.rejects(ws.ask('patience', { profile: 'custom' }), { code: 'OKCODE_NO_EMBEDDINGS' });
            assert.equal((await ws.ask('patience', { limit: 1 }))[0].file, 'lib/retry.js');

            // Re-supplying the function makes it usable again, on its existing
            // pipeline.
            const r = await oc.addEmbedder('secret', PROFILES().secret);
            assert.deepEqual(
                r.workspaces.map((w) => [w.id, w.pipeline, w.error]),
                [['app', 'code_keyed_model_24', null]],
            );
            assert.equal(oc.embedders().find((e) => e.name === 'secret').state, 'configured');
            assert.equal((await ws.ask('patience', { profile: 'secret', limit: 1 }))[0].file, 'lib/retry.js');
        } finally {
            await oc.close();
        }
    });

    it('addEmbedder builds alongside; removeEmbedder drops its pipeline and vectors', async () => {
        const oc = await okcode.open({ db, embedders: { small: PROFILES().small } });
        try {
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            await assert.rejects(oc.addEmbedder('small', PROFILES().small), { code: 'OKCODE_EMBEDDER_EXISTS' });

            const r = await oc.addEmbedder('wide', { type: 'bow', model: 'bow', dims: 48 });
            assert.deepEqual(
                r.workspaces.map((w) => [w.id, w.pipeline]),
                [['app', 'code_bow_48']],
            );
            await ws.store.settle();
            let byName = Object.fromEntries((await oc.status('app')).workspaces[0].embedders.map((e) => [e.name, e]));
            assert.equal(byName.wide.state, 'ready', JSON.stringify(byName.wide));
            assert.equal(byName.wide.done, byName.small.done, 'the new profile covers the same files');
            assert.equal((await ws.ask('patience', { profile: 'wide', limit: 1 }))[0].file, 'lib/retry.js');

            const env = ws.store.env;
            const typeEnv = await db.openEnv(`~${env.name}:emb:files`);
            const vecs = (k) => (typeEnv.hasType(`vec:${k}`) ? typeEnv.getCount(`vec:${k}`) : 0);
            assert.ok(vecs('code_bow_48') > 0);

            oc.useEmbedder('wide');
            const rm = await oc.removeEmbedder('wide');
            assert.deepEqual(rm.removed, [{ id: 'app', pipeline: 'code_bow_48' }]);
            assert.ok(!(await env.pipelines.getRecord('code_bow_48')));
            assert.equal(vecs('code_bow_48'), 0, 'its vectors are gone');
            assert.ok(vecs('code_bow_32') > 0, 'the other profile is untouched');
            assert.equal(oc.active, 'small', 'the active profile moved off the removed one');
            assert.ok(!db.env('okcode').get('embedders', 'wide'));
            byName = Object.fromEntries((await oc.status('app')).workspaces[0].embedders.map((e) => [e.name, e]));
            assert.equal(byName.wide, undefined);
            await assert.rejects(ws.ask('patience', { profile: 'wide' }), { code: 'OKCODE_NO_EMBEDDINGS' });
            assert.equal((await ws.ask('patience', { limit: 1 }))[0].file, 'lib/retry.js');
            await assert.rejects(oc.removeEmbedder('wide'), { code: 'OKCODE_UNKNOWN_EMBEDDER' });
        } finally {
            await oc.close();
        }
    });

    it("translation: a string key goes to okdb as api_key, never into okcode's record; functions never persist", () => {
        const emb = require('../src/embedders');
        const registered = [];
        const fakeDb = {
            embeddings: {
                registerEmbedderFactory: (t) => registered.push(t),
                _embedderFactories: new Map([['openai', () => ({})]]),
            },
        };
        const s = emb.fromHost(fakeDb, 'oa', {
            type: 'openai',
            model: 'text-embedding-3-small',
            apiKey: 'sk-string',
            baseUrl: 'http://x',
        });
        assert.deepEqual(s.profile.embedder, {
            type: 'openai',
            model: 'text-embedding-3-small',
            api_key: 'sk-string',
            base_url: 'http://x',
        });
        assert.equal(JSON.stringify(s.record).includes('sk-string'), false);
        assert.equal(s.record.apiKey, 'string');
        assert.deepEqual(registered, []);
        const f = emb.fromHost(fakeDb, 'OA 2', { type: 'openai', model: 'm', apiKey: () => 'k', dims: 8 });
        assert.equal(f.profile.embedder.type, 'okcode-oa_2');
        assert.equal(f.profile.embedder.api_key, undefined);
        assert.deepEqual(registered, ['okcode-oa_2']);
        assert.doesNotThrow(() => JSON.stringify(f.record));
        assert.equal(
            Object.values(f.record).some((v) => typeof v === 'function'),
            false,
        );
        assert.equal(emb.fromRecord(fakeDb, f.record).needsConfig, true);
        assert.equal(emb.fromRecord(fakeDb, s.record).needsConfig, false);
        assert.throws(() => emb.fromHost(fakeDb, 'bad', { type: 'nope', apiKey: () => 'k' }), {
            code: 'OKCODE_UNKNOWN_EMBEDDER_TYPE',
        });
        assert.throws(() => emb.fromHost(fakeDb, 'bad', { model: 'x' }), /needs a type/);
    });
});
