'use strict';
// Management from a process that does not index (docs/DESIGN.md §9): a
// SEPARATE OS process opens the same store with processors and engines off,
// and status() reports what the active process produced — from durable state
// alone, without the workspace's access facade. Two okdb instances on one
// path must be two processes, never one: this file forks itself as the
// passive side (OKCODE_PASSIVE_CHILD=<store>).

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { fork } = require('child_process');
const okcode = require('../src/okcode');

const FAKE = { type: 'fake', model: 'fake-v1', dims: 16 };
const FAKE_PIPELINE = require('../src/identity').pipelineName({ type: 'fake', endpoint: '', model: 'fake-v1' }, 16);

async function passiveChild(store) {
    const oc = await okcode.open({ path: store, role: { processors: false, engines: false } });
    try {
        const status = await oc.status();
        const workspaces = oc.workspaces();
        // A durable management op from the passive side: re-embed request.
        const reset = await oc.reset('app', { scope: 'vectors' });
        process.send({ status, workspaces, reset: reset.rebuilt.map((r) => r.pipeline) });
    } catch (err) {
        process.send({ error: err.stack || err.message });
    } finally {
        await oc.close().catch(() => {});
    }
}

function runChild(store) {
    return new Promise((resolve, reject) => {
        const child = fork(__filename, [], {
            env: { ...process.env, OKCODE_PASSIVE_CHILD: store },
            stdio: ['ignore', 'ignore', 'pipe', 'ipc'],
        });
        let msg = null;
        let stderr = '';
        child.stderr.on('data', (d) => (stderr += d));
        child.on('message', (m) => (msg = m));
        child.on('error', reject);
        child.on('exit', (code) => {
            if (!msg) return reject(new Error(`passive child exited ${code} without a report\n${stderr.slice(-2000)}`));
            if (msg.error) return reject(new Error(`passive child failed: ${msg.error}`));
            resolve(msg);
        });
    });
}

if (process.env.OKCODE_PASSIVE_CHILD) {
    passiveChild(process.env.OKCODE_PASSIVE_CHILD).then(
        () => process.exit(0),
        () => process.exit(1),
    );
} else {
    const { writeFixture, FIXTURE_FILES, tmpRoot } = require('./fixtures/code-fixture');

    describe('status from a passive process', () => {
        let base;
        let root;
        let store;
        let oc;
        before(async () => {
            base = tmpRoot('ocpass').base;
            root = path.join(base, 'src');
            store = path.join(base, 'okdb');
            writeFixture(root);
            oc = await okcode.open({ path: store, embedders: { fake: FAKE } });
        });
        after(async () => {
            if (oc) await oc.close().catch(() => {});
            fs.rmSync(base, { recursive: true, force: true });
        });

        it('reports the counts the active process produced, and can request a re-embed', async () => {
            const ws = await oc.addWorkspace('app', { access: okcode.access.localFs(root) });
            await ws.flush();
            await ws.store.settle();
            const mine = (await oc.status('app')).workspaces[0];
            assert.equal(mine.embedders[0].state, 'ready', JSON.stringify(mine.embedders));
            const done = mine.embedders[0].done;
            assert.ok(done > 0);

            // The active process stays open while the passive one reads.
            // Count re-embedded docs in THIS (active) process from here on.
            let redone = 0;
            oc.db.events.on('embeddings:indexer:doc:done', () => redone++);
            const got = await runChild(store);
            assert.deepEqual(
                got.workspaces.map((w) => [w.id, w.open]),
                [['app', false]],
            );
            const theirs = got.status.workspaces[0];
            assert.equal(theirs.id, 'app');
            assert.equal(theirs.open, false);
            assert.equal(theirs.files, FIXTURE_FILES.length);
            assert.equal(theirs.files, mine.files);
            assert.equal(theirs.symbols, mine.symbols);
            assert.equal(theirs.fts.symbols.status, 'ready');
            assert.equal(theirs.fts.files.status, 'ready');
            assert.equal(theirs.embedders.length, 1);
            const e = theirs.embedders[0];
            assert.equal(e.name, 'fake');
            assert.equal(e.pipeline, FAKE_PIPELINE);
            assert.equal(e.dims, 16);
            assert.equal(e.active, true);
            assert.equal(e.done, done, JSON.stringify(e));
            assert.equal(e.failed, 0);
            assert.deepEqual(got.status.role, { processors: false, engines: false });
            assert.deepEqual(got.reset, [FAKE_PIPELINE]);

            // The passive process's reset reaches the RUNNING indexer here (okdb
            // command epoch + PROC hint): it re-embeds without a restart.
            const w = oc.workspace('app');
            const deadline = Date.now() + 15000;
            let now;
            for (;;) {
                await w.store.settle();
                now = (await oc.status('app')).workspaces[0].embedders[0];
                if (now.done === done && now.pending === 0) break;
                if (Date.now() > deadline)
                    assert.fail(
                        `re-embed did not converge: ${JSON.stringify(now)} ${JSON.stringify(oc.db.embeddings._durableIndexerDocs(`okcode_app:${FAKE_PIPELINE}`, { status: 'failed', limit: 3 }))}`,
                    );
                await new Promise((r) => setTimeout(r, 200));
            }
            assert.equal(now.state, 'ready');
            assert.ok(redone >= done, `the live indexer re-embedded every doc (${redone}/${done})`);
            assert.ok((await w.ask('add two numbers', { limit: 3 })).length > 0);
            assert.equal(now.failed, 0);
        });
    });
}
