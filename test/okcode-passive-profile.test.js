'use strict';
// A passive process (no engines, no processors) that opens a FRESH store
// before any indexing process has created the embeddings pipeline. It must
// never try to create it (createPipeline needs engines): the profile is
// "pending", not an error, and it attaches lazily — on the next ask() or
// status() — once the indexing process has created the pipeline. Two okdb
// instances on one path must be two OS processes: this file forks itself as
// the passive side (OKCODE_PASSIVE_PROFILE_CHILD=<store>\t<root>).

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { fork } = require('child_process');
const okcode = require('../src/okcode');

const FAKE = { type: 'fake', model: 'fake-v1', dims: 16 };
const FAKE_PIPELINE = require('../src/identity').pipelineName({ type: 'fake', endpoint: '', model: 'fake-v1' }, 16);
const QUERY = 'add two numbers';

// ── the passive side ─────────────────────────────────────────────────────
async function passiveChild(store, root) {
    const logs = [];
    const log = (m) => logs.push(String(m));
    const oc = await okcode.open({
        path: store,
        role: { processors: false, engines: false },
        embedders: { fake: FAKE },
        log: { info: log, warn: log, error: log },
    });
    const ws = await oc.addWorkspace('app', { access: okcode.access.localFs(root) });
    const embedderOf = async () => (await oc.status('app')).workspaces[0].embedders[0];
    process.send({ phase: 'opened', logs: logs.slice(), embedder: await embedderOf() });

    process.on('message', async (m) => {
        try {
            if (m === 'status') {
                process.send({ phase: 'status', embedder: await embedderOf(), logs: logs.slice() });
            } else if (m === 'ask') {
                // Semantic only (compare → the store's vector query), so a
                // lexical hit cannot mask a missing vector eye. The local
                // search view tails the vec rows; give it a few seconds.
                let semantic = [];
                let error = null;
                const deadline = Date.now() + 10000;
                for (;;) {
                    try {
                        semantic = (await oc.compare('app', QUERY, 'fake', { limit: 3 })).fake || [];
                        error = null;
                    } catch (err) {
                        error = `${err.code || ''} ${err.message}`;
                    }
                    if (semantic.length || Date.now() > deadline) break;
                    await new Promise((r) => setTimeout(r, 250));
                }
                const fused = await ws.ask(QUERY, { limit: 5 }).catch((err) => ({ error: err.message }));
                process.send({
                    phase: 'asked',
                    semantic,
                    error,
                    fused,
                    embedder: await embedderOf(),
                    logs: logs.slice(),
                });
            } else if (m === 'close') {
                await oc.close().catch(() => {});
                process.exit(0);
            }
        } catch (err) {
            process.send({ phase: 'error', error: err.stack || err.message });
        }
    });
}

function startChild(store, root) {
    const child = fork(__filename, [], {
        env: { ...process.env, OKCODE_PASSIVE_PROFILE_CHILD: `${store}\t${root}` },
        stdio: ['ignore', 'ignore', 'pipe', 'ipc'],
    });
    let stderr = '';
    child.stderr.on('data', (d) => (stderr += d));
    const waiting = new Map();
    child.on('message', (m) => {
        const w = waiting.get(m.phase) || waiting.get('*');
        if (w) w.resolve(m);
    });
    child.on('exit', (code) => {
        for (const w of waiting.values()) w.reject(new Error(`passive child exited ${code}\n${stderr.slice(-2000)}`));
    });
    const next = (phase, timeoutMs = 60000) =>
        new Promise((resolve, reject) => {
            const t = setTimeout(() => reject(new Error(`passive child: no "${phase}" in ${timeoutMs}ms`)), timeoutMs);
            const done = (fn) => (v) => {
                clearTimeout(t);
                waiting.delete(phase);
                waiting.delete('*');
                fn(v);
            };
            const entry = { resolve: done(resolve), reject: done(reject) };
            waiting.set(phase, entry);
            waiting.set('*', {
                resolve: (m) =>
                    m.phase === 'error' ? entry.reject(new Error(`passive child: ${m.error}`)) : undefined,
                reject: entry.reject,
            });
        });
    const stop = () =>
        new Promise((resolve) => {
            if (child.exitCode != null) return resolve();
            child.once('exit', resolve);
            child.send('close');
            setTimeout(() => child.kill('SIGKILL'), 10000).unref();
        });
    return { child, next, stop };
}

if (process.env.OKCODE_PASSIVE_PROFILE_CHILD) {
    const [store, root] = process.env.OKCODE_PASSIVE_PROFILE_CHILD.split('\t');
    passiveChild(store, root).catch((err) => {
        process.send({ phase: 'error', error: err.stack || err.message });
        process.exit(1);
    });
} else {
    const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

    describe('a passive process that opens before the indexing process', () => {
        let base;
        let root;
        let store;
        let oc;
        let passive;
        before(() => {
            base = tmpRoot('ocpprof').base;
            root = path.join(base, 'src');
            store = path.join(base, 'okdb');
            writeFixture(root);
        });
        after(async () => {
            if (passive) await passive.stop();
            if (oc) await oc.close().catch(() => {});
            fs.rmSync(base, { recursive: true, force: true });
        });

        it('waits for the pipeline (pending, no error) and attaches once it exists', async () => {
            // 1. Passive first, on a fresh store: nothing to attach to yet.
            passive = startChild(store, root);
            const opened = await passive.next('opened');
            const unavailable = opened.logs.filter((l) => /unavailable/.test(l));
            assert.deepEqual(unavailable, [], `passive logged: ${JSON.stringify(opened.logs)}`);
            assert.equal(opened.embedder.state, 'pending', JSON.stringify(opened.embedder));
            assert.equal(opened.embedder.error ?? null, null);

            // 2. The indexing process creates the pipeline and embeds.
            oc = await okcode.open({ path: store, embedders: { fake: FAKE } });
            const ws = await oc.addWorkspace('app', { access: okcode.access.localFs(root) });
            await ws.flush();
            await ws.store.settle();
            const mine = (await oc.status('app')).workspaces[0].embedders[0];
            assert.equal(mine.state, 'ready', JSON.stringify(mine));
            assert.equal(mine.pipeline, FAKE_PIPELINE);
            assert.ok(mine.done > 0);
            assert.ok((await oc.compare('app', QUERY, 'fake', { limit: 3 })).fake.length > 0);

            // 3. The passive process — still the same process, no restart —
            //    attaches on its next status() or query, and has vectors.
            passive.child.send('status');
            const st = await passive.next('status');
            assert.equal(st.embedder.state, 'ready', JSON.stringify(st.embedder));
            assert.equal(st.embedder.pipeline, FAKE_PIPELINE);
            assert.equal(st.embedder.done, mine.done);
            passive.child.send('ask');
            const asked = await passive.next('asked');
            assert.equal(asked.error, null, asked.error);
            assert.ok(asked.semantic.length > 0, `passive semantic hits: ${JSON.stringify(asked)}`);
            assert.ok(
                asked.semantic.some((h) => h.file === 'lib/math.js'),
                JSON.stringify(asked.semantic),
            );
            assert.ok(Array.isArray(asked.fused) && asked.fused.length > 0, JSON.stringify(asked.fused));
            assert.equal(asked.embedder.state, 'ready', JSON.stringify(asked.embedder));
            assert.equal(asked.embedder.pipeline, FAKE_PIPELINE);
            assert.equal(asked.embedder.done, mine.done);
            assert.deepEqual(
                asked.logs.filter((l) => /unavailable|OKDBEngines/.test(l)),
                [],
                JSON.stringify(asked.logs),
            );
        });
    });
}
