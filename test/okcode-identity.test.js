'use strict';
// The identity of a vector store (docs/DESIGN.md §8) and querying it with a
// vector the host already embedded (§9). A store is addressed by
// (type, endpoint, model, dims): the same model name at two endpoints is two
// stores; changing the url addresses a new one and reports the old one as
// orphaned; an unchanged reopen keeps its store and embeds nothing again. A
// host that holds a query vector checks okcode's reported identity against
// its own and passes the vector — no embed call is made for the query.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const okcode = require('../src/okcode');
const { createTools } = require('../src/tools');
const { pipelineName } = require('../src/identity');
const { localFs } = okcode.access;
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

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

// Every text the fake provider embedded, by the endpoint it was configured
// with — a model "served" at two urls is the same function here, which is
// exactly the case identity must still keep apart.
const calls = { n: 0, byUrl: {} };

const AT = (url, extra = {}) => ({ type: 'bow', model: 'bow-m', dims: 16, url, ...extra });
const identityFor = (url) => JSON.stringify(['bow', url, 'bow-m', 16]);
const pipelineFor = (url) => pipelineName({ type: 'bow', endpoint: url, model: 'bow-m' }, 16);

describe('vector store identity', () => {
    let base;
    let root;
    let db;

    before(async () => {
        base = tmpRoot('ocid').base;
        root = path.join(base, 'src');
        writeFixture(root);
        fs.writeFileSync(path.join(root, 'lib', 'retry.js'), RETRY_JS);
        db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
        db.embeddings.registerEmbedderFactory('bow', (cfg) => {
            const one = (t) => {
                calls.n++;
                calls.byUrl[cfg.url || ''] = (calls.byUrl[cfg.url || ''] || 0) + 1;
                return bow(t, cfg.dims || 16);
            };
            return { embed: async (t) => (Array.isArray(t) ? t.map(one) : one(t)) };
        });
        await db.open();
    });
    after(async () => {
        await db.close().catch(() => {});
        fs.rmSync(base, { recursive: true, force: true });
    });

    const embeddersOf = async (oc) =>
        Object.fromEntries((await oc.status('app')).workspaces[0].embedders.map((e) => [e.name, e]));

    it('the pipeline name carries the identity; an unchanged reopen keeps its store and embeds nothing', async () => {
        let oc = await okcode.open({ db, embedders: { q: AT('http://a:11434') } });
        try {
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            await ws.store.settle();
            const e = (await embeddersOf(oc)).q;
            assert.equal(e.pipeline, pipelineFor('http://a:11434'));
            assert.match(e.pipeline, /^code_bow_m_16_[0-9a-f]{8}$/, 'readable parts plus a hash');
            assert.equal(e.state, 'ready', JSON.stringify(e));
            assert.ok(e.done >= 5);
            assert.equal(e.identity, identityFor('http://a:11434'));
            assert.deepEqual((await oc.status('app')).workspaces[0].orphaned, []);
            // The host-facing description of the profile.
            const [d] = oc.embedders();
            assert.deepEqual(
                { type: d.type, endpoint: d.endpoint, model: d.model, dims: d.dims, identity: d.identity },
                {
                    type: 'bow',
                    endpoint: 'http://a:11434',
                    model: 'bow-m',
                    dims: 16,
                    identity: identityFor('http://a:11434'),
                },
            );
        } finally {
            await oc.close();
        }

        calls.n = 0;
        oc = await okcode.open({ db, embedders: { q: AT('http://a:11434') } });
        try {
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            await ws.store.settle();
            const e = (await embeddersOf(oc)).q;
            assert.equal(e.pipeline, pipelineFor('http://a:11434'), 'the same store');
            assert.equal(e.state, 'ready');
            assert.equal(calls.n, 0, 'nothing re-embedded');
            assert.deepEqual((await oc.status('app')).workspaces[0].orphaned, []);
        } finally {
            await oc.close();
        }
    });

    it('a new url addresses a new store; the old one is orphaned (never served) until removed', async () => {
        const oc = await okcode.open({ db, embedders: { q: AT('http://b:11434') } });
        try {
            calls.byUrl = {};
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            await ws.store.settle();
            const e = (await embeddersOf(oc)).q;
            assert.equal(e.pipeline, pipelineFor('http://b:11434'));
            assert.notEqual(e.pipeline, pipelineFor('http://a:11434'));
            assert.equal(e.state, 'ready', JSON.stringify(e));
            assert.ok(e.done >= 5, 'the new store was built from scratch');
            assert.ok(calls.byUrl['http://b:11434'] > 0, 'by the new endpoint');
            assert.equal(e.identity, identityFor('http://b:11434'));
            assert.equal(oc.embedders()[0].identity, identityFor('http://b:11434'));

            // The old store: reported, with what it still holds.
            const st = (await oc.status('app')).workspaces[0];
            assert.deepEqual(
                st.orphaned.map((o) => o.pipeline),
                [pipelineFor('http://a:11434')],
            );
            assert.ok(st.orphaned[0].done >= 5, JSON.stringify(st.orphaned));
            // ask() searches the new store.
            assert.equal((await ws.ask('patience', { limit: 1 }))[0].file, 'lib/retry.js');

            const r = await oc.removeOrphaned('app');
            assert.deepEqual(r.removed, [{ id: 'app', pipeline: pipelineFor('http://a:11434') }]);
            assert.equal(await ws.store.env.pipelines.getRecord(pipelineFor('http://a:11434')), null);
            assert.deepEqual((await oc.status('app')).workspaces[0].orphaned, []);
            assert.ok(await ws.store.env.pipelines.getRecord(pipelineFor('http://b:11434')), 'the live one stays');
            assert.deepEqual((await oc.removeOrphaned()).removed, []);
        } finally {
            await oc.close();
        }
    });

    it('the same model name and dims at two endpoints are two stores', async () => {
        const oc = await okcode.open({
            db,
            embedders: { q: AT('http://b:11434'), other: AT('http://c:8080/v1') },
            active: 'q',
        });
        try {
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            await ws.store.settle();
            const byName = await embeddersOf(oc);
            assert.equal(byName.q.pipeline, pipelineFor('http://b:11434'));
            assert.equal(byName.other.pipeline, pipelineFor('http://c:8080/v1'));
            assert.notEqual(byName.q.pipeline, byName.other.pipeline);
            assert.notEqual(byName.q.identity, byName.other.identity);
            for (const e of Object.values(byName)) assert.equal(e.state, 'ready', JSON.stringify(e));
            assert.equal(byName.other.done, byName.q.done, 'each holds the whole workspace');
            assert.deepEqual((await oc.status('app')).workspaces[0].orphaned, []);

            // Removing one profile drops only its own store.
            const rm = await oc.removeEmbedder('other');
            assert.deepEqual(rm.removed, [{ id: 'app', pipeline: pipelineFor('http://c:8080/v1') }]);
            assert.ok(await ws.store.env.pipelines.getRecord(pipelineFor('http://b:11434')));
        } finally {
            await oc.close();
        }
    });

    it('a host vector goes through ws.ask, compare and code_ask with no embed call', async () => {
        const oc = await okcode.open({
            db,
            embedders: { q: AT('http://b:11434'), wide: AT('http://b:11434', { dims: 24 }) },
        });
        try {
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            await ws.store.settle();
            const q = oc.embedders().find((e) => e.name === 'q');
            // The host compares identities, then embeds once and shares.
            assert.equal(q.identity, identityFor('http://b:11434'));
            const vector = bow('patience', 16);
            calls.n = 0;

            const hits = await ws.ask({ vector, identity: q.identity }, { limit: 3 });
            assert.equal(hits[0].file, 'lib/retry.js');
            assert.ok(hits.every((h) => h.via === 'vector'));

            const both = await ws.ask({ text: 'backoff', vector }, { limit: 3 });
            assert.equal(both[0].name, 'backoff');

            const cmp = await oc.compare('app', { vector }, ['q'], { limit: 2 });
            assert.equal(cmp.q[0].file, 'lib/retry.js');
            await assert.rejects(oc.compare('app', { vector }, ['q', 'wide']), { code: 'OKCODE_DIMS_MISMATCH' });

            const tools = createTools({
                workspace: async () => ws,
                workspaces: () => [{ id: 'app', root }],
            });
            const text = await tools.render('code_ask', { question: 'patience', vector, identity: q.identity });
            assert.match(text, /lib\/retry\.js/);
            const vectorOnly = await tools.render('code_ask', { vector });
            assert.match(vectorOnly, /^Answering a query vector from app:\n {2}\[meaning\] .*lib\/retry\.js/);
            assert.equal(calls.n, 0, 'the query was never embedded');

            // A vector for the other profile's space is refused.
            await assert.rejects(ws.ask({ vector }, { profile: 'wide' }), { code: 'OKCODE_DIMS_MISMATCH' });
            await assert.rejects(ws.ask({ vector, identity: identityFor('http://a:11434') }, { limit: 1 }), {
                code: 'OKCODE_IDENTITY_MISMATCH',
            });
        } finally {
            await oc.close();
        }
    });

    it('a custom embed profile must name its space (`id`), and the id is its endpoint', async () => {
        await assert.rejects(okcode.open({ db, embedders: { mine: { embed: async (t) => bow(t, 8), dims: 8 } } }), {
            code: 'OKCODE_NEEDS_ID',
        });
        const oc = await okcode.open({
            db,
            embedders: { mine: { embed: async (t) => bow(t, 8), dims: 8, id: 'bow-service@2', model: 'bow8' } },
        });
        try {
            const d = oc.embedders().find((e) => e.name === 'mine');
            assert.equal(d.type, 'custom');
            assert.equal(d.endpoint, 'bow-service@2');
            assert.equal(d.identity, JSON.stringify(['custom', 'bow-service@2', 'bow8', 8]));
        } finally {
            await oc.removeEmbedder('mine');
            await oc.close();
        }
    });
});
