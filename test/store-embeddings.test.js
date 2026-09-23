'use strict';
// Embedding profiles (docs/DESIGN.md §8) with a fake embedder registered on
// the db — no model, no network. The fake is a bag of words with a tiny
// synonym table, so it has just enough "meaning" for the vector eye to find a
// chunk no lexical eye can: "patience" appears nowhere in the code, but it
// shares a concept with backoff/retry/wait.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const OKDB = require('@kedem/okdb');
const { openWorkspace } = require('../src/workspace');
const { openStore, envNameFor } = require('../src/store');
const { localFs } = require('../src/access');
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');
const { pipelineName } = require('../src/identity');
// A pipeline's name for an identity (src/identity.js).
const pn = (type, model, dims, endpoint = '') => pipelineName({ type, endpoint, model }, dims);

const CONCEPTS = {
    backoff: 'RETRY',
    retry: 'RETRY',
    retries: 'RETRY',
    wait: 'RETRY',
    longer: 'RETRY',
    again: 'RETRY',
    patience: 'RETRY',
    invoice: 'MONEY',
    invoices: 'MONEY',
    billing: 'MONEY',
    ledger: 'MONEY',
    accounting: 'MONEY',
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
        // A concept weighs more than an incidental word.
        v[(h >>> 0) % dims] += CONCEPTS[raw] ? 4 : 1;
    }
    const n = Math.hypot(...v) || 1;
    for (let i = 0; i < dims; i++) v[i] /= n;
    return v;
}
const embedCalls = { n: 0 };
const factory = (cfg) => {
    const dims = cfg.dims || 8; // an unconfigured model "is" 8-dimensional
    return {
        dims,
        embed: async (t) => {
            embedCalls.n++;
            return bow(t, dims);
        },
    };
};

// First function: ~1.5 KB of an unrelated subject, so the retry code starts a
// chunk of its own and a hit can be attributed to its symbol.
const LEDGER = Array.from(
    { length: 24 },
    (_, i) => `    // Invoice line ${i}: accounting posts the billing entry to the ledger.`,
).join('\n');
const RETRY_JS = [
    "'use strict';",
    '',
    'function postInvoices(ledger, invoices) {',
    LEDGER,
    '    for (const inv of invoices) ledger.push(inv);',
    '    return ledger.length;',
    '}',
    '',
    '// When the server refuses, wait longer each time before trying again.',
    'function backoff(attempt) {',
    '    // Retry with a delay that doubles on every attempt: wait, then retry again.',
    '    const wait = Math.min(30000, 2 ** attempt * 100);',
    '    return wait; // longer and longer between retries',
    '}',
    '',
    'module.exports = { postInvoices, backoff };',
    '',
].join('\n');

describe('embedding profiles', () => {
    let base;
    let db;
    let root;
    before(async () => {
        base = tmpRoot('emb').base;
        root = path.join(base, 'src');
        writeFixture(root);
        fs.writeFileSync(path.join(root, 'lib', 'retry.js'), RETRY_JS);
        db = new OKDB(path.join(base, 'okdb'), { auth: { open: true } });
        db.embeddings.registerEmbedderFactory('bow', factory);
        await db.open();
    });
    after(async () => {
        await db.close().catch(() => {});
        fs.rmSync(base, { recursive: true, force: true });
    });

    // The drop tests get an okdb of their own: the suite's shared db already
    // holds default + 4 workspace envs, and an unlicensed okdb allows 5. It
    // also narrows the byte scan to exactly what that one workspace wrote.
    async function ownDb(tag) {
        const dir = path.join(base, `okdb-${tag}`);
        const own = new OKDB(dir, { auth: { open: true } });
        own.embeddings.registerEmbedderFactory('bow', factory);
        await own.open();
        return { db: own, dir };
    }

    const PROFILES = [
        { name: 'small', embedder: { type: 'bow', model: 'bow' }, dims: 64 },
        { name: 'wide', embedder: { type: 'bow', model: 'bow' }, dims: 96 },
    ];

    it('ask is unavailable without a profile', async () => {
        const st = await openStore({ db, id: 'noprofile', access: localFs(root) });
        const ws = await openWorkspace({ id: 'noprofile', access: localFs(root), store: st });
        await assert.rejects(ws.ask('patience'), { code: 'OKCODE_NO_EMBEDDINGS' });
        await assert.rejects(st.ask('patience'), { code: 'OKCODE_NO_EMBEDDINGS' });
        const bare = await openWorkspace({ id: 'nostore', access: localFs(root) });
        await assert.rejects(bare.ask('patience'), { code: 'OKCODE_NO_EMBEDDINGS' });
        await ws.close();
    });

    it('a profile embeds file content through the symbol-aware chunker; ask finds the right file and symbol', async () => {
        const st = await openStore({ db, id: 'emb', access: localFs(root), profiles: PROFILES });
        const ws = await openWorkspace({ id: 'emb', access: localFs(root), store: st });
        await ws.sync();
        await st.settle();

        const profiles = await st.profiles();
        assert.deepEqual(
            profiles.map((p) => [p.name, p.pipeline, p.dims, p.error]),
            [
                ['small', pn('bow', 'bow', 64), 64, null],
                ['wide', pn('bow', 'bow', 96), 96, null],
            ],
        );
        for (const p of profiles) {
            assert.equal(p.status.doc_counts.failed, 0, JSON.stringify(p.status));
            assert.ok(p.status.doc_counts.done >= 5, JSON.stringify(p.status));
            assert.ok(p.status.vector_count > 0);
        }
        // Both profiles hold the same chunks, each in its own space.
        assert.equal(profiles[0].status.vector_count, profiles[1].status.vector_count);

        // No lexical eye can answer this: "patience" is nowhere in the code.
        assert.deepEqual(await ws.find('patience'), []);
        for (const profile of ['small', 'wide']) {
            const hits = await ws.ask('patience', { profile, limit: 5 });
            assert.ok(hits.length > 0, profile);
            const top = hits[0];
            assert.equal(top.via, 'vector');
            assert.equal(top.file, 'lib/retry.js', JSON.stringify(hits.map((h) => `${h.file}#${h.name}`)));
            assert.equal(top.name, 'backoff', JSON.stringify(top));
            assert.match(top.at, /^[0-9A-F]{40}$/);
            assert.ok(top.window && /backoff|wait/.test(top.window.text));
        }
        // The default profile is the first one.
        assert.equal((await ws.ask('patience', { limit: 1 }))[0].name, 'backoff');

        // The store-level answer: a file, a chunk and its offsets into the file.
        const raw = await st.ask('patience', { profile: 'small', limit: 3, text: true });
        assert.equal(raw[0].file, 'lib/retry.js');
        const src = fs.readFileSync(path.join(root, 'lib', 'retry.js'), 'utf8');
        assert.ok(src.slice(raw[0].start, raw[0].end).includes('function backoff'));
        assert.ok(
            raw[0].text && raw[0].text.includes('function backoff'),
            'chunk text re-derived through the resolver',
        );
        await assert.rejects(ws.ask('patience', { profile: 'nope' }), { code: 'OKCODE_NO_EMBEDDINGS' });
        await ws.close();
    });

    it('a precomputed query vector is searched as-is: no embed call, dims and identity checked', async () => {
        const st = await openStore({ db, id: 'vec', access: localFs(root), profiles: PROFILES });
        const ws = await openWorkspace({ id: 'vec', access: localFs(root), store: st });
        await ws.sync();
        await st.settle();
        const [small] = await st.profiles();
        assert.equal(small.identity, JSON.stringify(['bow', '', 'bow', 64]));

        // The string path embeds the query (once).
        let before = embedCalls.n;
        const byText = await st.ask('patience', { profile: 'small', limit: 3 });
        assert.equal(embedCalls.n - before, 1, 'a string query is embedded');

        // The same question, embedded by the host: the same answer, and the
        // embedder is never called — at the store, in ws.ask with and
        // without text, and with the host's identity attached.
        const vector = bow('patience', 64);
        before = embedCalls.n;
        const byVector = await st.ask({ vector }, { profile: 'small', limit: 3 });
        assert.deepEqual(
            byVector.map((h) => [h.file, h.chunkHash]),
            byText.map((h) => [h.file, h.chunkHash]),
        );
        const plain = await st.ask({ vector: Array.from(vector) }, { profile: 'small', limit: 3 });
        assert.equal(plain[0].file, byText[0].file, 'a plain array is accepted too');

        const semanticOnly = await ws.ask({ vector }, { profile: 'small', limit: 5 });
        assert.ok(semanticOnly.length > 0);
        assert.ok(
            semanticOnly.every((h) => h.via === 'vector'),
            'no text → no lexical eyes',
        );
        assert.equal(semanticOnly[0].name, 'backoff');

        // With text too, the lexical eyes run and fuse: `backoff` is an exact
        // name, so the name match leads.
        const fused = await ws.ask({ text: 'backoff', vector: bow('backoff', 64) }, { profile: 'small', limit: 5 });
        assert.equal(fused[0].name, 'backoff');
        assert.notEqual(fused[0].via, 'vector');

        const withId = await ws.ask({ vector, identity: small.identity }, { profile: 'small', limit: 1 });
        assert.equal(withId[0].name, 'backoff');
        assert.equal(embedCalls.n, before, 'no embed call for any vector query');

        // A vector from another space is refused, not searched.
        await assert.rejects(ws.ask({ vector: bow('patience', 96) }, { profile: 'small' }), {
            code: 'OKCODE_DIMS_MISMATCH',
        });
        await assert.rejects(st.ask({ vector }, { profile: 'wide' }), { code: 'OKCODE_DIMS_MISMATCH' });
        await assert.rejects(
            ws.ask({ vector, identity: JSON.stringify(['ollama', '', 'bow', 64]) }, { profile: 'small' }),
            { code: 'OKCODE_IDENTITY_MISMATCH' },
        );
        await assert.rejects(ws.ask({}, { profile: 'small' }), { code: 'OKCODE_BAD_QUERY' });
        await assert.rejects(ws.ask({ vector: 'nope' }, { profile: 'small' }), { code: 'OKCODE_BAD_QUERY' });
        assert.equal(embedCalls.n, before);
        await ws.close();
    });

    it('an edit re-embeds; reopening the store reuses the pipelines', async () => {
        const st = await openStore({ db, id: 'emb', access: localFs(root), profiles: PROFILES });
        const ws = await openWorkspace({ id: 'emb', access: localFs(root), store: st });
        const r = await ws.read('backoff');
        const e = await ws.edit('backoff', 'function backoff(attempt) {\n    return attempt;\n}', { at: r.at });
        assert.ok(e.ok, JSON.stringify(e));
        await ws.flush();
        await st.settle();
        assert.ok(
            (await st.profiles()).every((p) => !p.error),
            'reopening reused both pipelines',
        );
        // The chunk that now holds `backoff` is re-derived from the NEW text:
        // the index followed the file.
        const raw = await st.ask('patience', { profile: 'small', limit: 3, text: true });
        const hit = raw.find((h) => h.file === 'lib/retry.js' && h.text && h.text.includes('function backoff'));
        assert.ok(hit, JSON.stringify(raw));
        assert.ok(hit.text.includes('return attempt;'), hit.text);
        const status = (await st.profiles()).find((p) => p.name === 'small').status;
        assert.equal(status.doc_counts.failed, 0);
        await ws.close();
    });

    it('a profile without dims learns them from the model', async () => {
        const st = await openStore({
            db,
            id: 'probe',
            access: localFs(root),
            profiles: [{ name: 'auto', embedder: { type: 'bow', model: 'bow-auto' } }],
        });
        const [p] = await st.profiles();
        assert.equal(p.error, null);
        assert.equal(p.dims, 8);
        assert.equal(p.pipeline, pn('bow', 'bow-auto', 8));
        const ws = await openWorkspace({ id: 'probe', access: localFs(root), store: st });
        await ws.sync();
        await st.settle();
        assert.ok((await ws.ask('patience', { limit: 3 })).length > 0);
        await ws.close();
        // Reopened: dims come from the pipeline left behind, not a new probe.
        const again = await openStore({
            db,
            id: 'probe',
            access: localFs(root),
            profiles: [{ name: 'auto', embedder: { type: 'bow', model: 'bow-auto' } }],
        });
        assert.equal((await again.profiles())[0].pipeline, pn('bow', 'bow-auto', 8));
    });

    it('vectors carry no file text, and drop() removes the pipelines with the env', async () => {
        const PHRASE = 'seventeen lighthouses negotiate tariffs with a marmalade octopus';
        fs.writeFileSync(
            path.join(root, 'lib', 'lighthouse.js'),
            `'use strict';\nfunction harbour() {\n    // ${PHRASE}, and the octopus always wins.\n    return 17;\n}\nmodule.exports = { harbour };\n`,
        );
        const { db, dir } = await ownDb('bytes');
        try {
            const st = await openStore({ db, id: 'bytes', access: localFs(root), profiles: [PROFILES[0]] });
            const ws = await openWorkspace({ id: 'bytes', access: localFs(root), store: st });
            await ws.sync();
            await st.settle();
            assert.ok((await st.ask('lighthouses octopus', { limit: 3 })).some((h) => h.file === 'lib/lighthouse.js'));
            await ws.close();
            const walk = (d, out = []) => {
                for (const e of fs.readdirSync(d, { withFileTypes: true })) {
                    const p = path.join(d, e.name);
                    if (e.isDirectory()) walk(p, out);
                    else out.push(p);
                }
                return out;
            };
            const leaks = walk(dir).filter((p) => fs.readFileSync(p).indexOf(PHRASE) >= 0);
            assert.deepEqual(leaks, [], 'file text is not stored anywhere');

            await st.drop();
            await assert.rejects(db.openEnv(envNameFor('bytes')), { code: 'ENV_NOT_FOUND' });
            assert.equal(fs.existsSync(path.join(dir, envNameFor('bytes'))), false);
        } finally {
            await db.close();
        }
    });

    it('drop() leaves no directory of the workspace env behind (okdb d195f80: sub-env paths resolved under the root)', async () => {
        const { db, dir } = await ownDb('leftover');
        try {
            const st = await openStore({ db, id: 'leftover', access: localFs(root), profiles: [PROFILES[0]] });
            const ws = await openWorkspace({ id: 'leftover', access: localFs(root), store: st });
            await ws.sync();
            await st.settle();
            await ws.close();
            await st.drop();
            const left = fs.readdirSync(dir).filter((d) => d.includes(envNameFor('leftover')));
            assert.deepEqual(left, []);
        } finally {
            await db.close();
        }
    });
});
