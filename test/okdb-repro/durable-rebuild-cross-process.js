'use strict';
// okdb repro — two issues around a re-embed requested from ANOTHER process,
// for a pipeline over a RESOLVED field (env.resolveField):
//
// 1. Process A runs the pipeline (full roles). Process B (processors:false,
//    engines:false, same path) calls embeddings.durableRebuild('ws:p'). The
//    vectors and doc_status rows are dropped and the cursor reset — but A's
//    running indexer never re-embeds: stats stay done 0 / pending 5 and search
//    is empty until A restarts.
//    Expected: the owner converges (re-embeds) like other durable cursor resets.
//
// 2. A restarts. The env (and its indexer) open inside db.open(), before the
//    app can call env.resolveField — the replay reads the resolved field, finds
//    no resolver, and marks every doc FAILED ("no resolver is registered in this
//    process"). They stay failed once the resolver is registered.
//    Expected: a missing resolver is "not yet" (docs stay pending / the drain
//    waits), not a permanent per-doc failure.
//
// Run: node test/okdb-repro/durable-rebuild-cross-process.js   (exit 0 = both fixed)
const fs = require('fs');
const os = require('os');
const path = require('path');
const { fork } = require('child_process');
const OKDB = require('@kedem/okdb');

const resolver = { batch: async (items) => items.map(({ row }) => `text of ${row.name}`) };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function child(dir) {
    const db = new OKDB(dir, { processors: false, engines: false });
    await db.open();
    await db.openEnv('ws');
    const r = await db.embeddings.durableRebuild('ws:p');
    process.send({ rebuilt: r && r.rebuilt, vectorsDropped: r && r.vectorsDropped });
    await db.close();
}

async function stats(db) {
    const idx = db.embeddings.indexer('ws:p');
    if (idx) await idx.flush();
    const s = idx ? await idx.stats() : db.embeddings._durableIndexerStats('ws:p');
    return s.doc_counts;
}

async function main() {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'okdb-repro-'));
    let db = new OKDB(dir, {});
    await db.open();
    let env = await db.createEnvironment('ws');
    await env.ensureType('docs');
    await env.resolveField('docs', 'body', null, resolver);
    for (let i = 0; i < 5; i++) await env.put('docs', `d${i}`, { name: `doc ${i}` });
    await db.embeddings.createPipeline('p', {
        source_type: 'docs',
        source_env: 'ws',
        field: 'body',
        dims: 8,
        embedder: { type: 'fake', model: 'f', dims: 8 },
    });
    console.log('A before:', JSON.stringify(await stats(db)));

    const msg = await new Promise((resolve, reject) => {
        const c = fork(__filename, ['child', dir], { stdio: ['ignore', 'ignore', 'inherit', 'ipc'] });
        let m = null;
        c.on('message', (x) => (m = x));
        c.on('exit', () => (m ? resolve(m) : reject(new Error('child died'))));
    });
    console.log('B durableRebuild:', JSON.stringify(msg));

    await sleep(3000);
    const a1 = await stats(db);
    console.log('A 3s after B rebuilt (still running):', JSON.stringify(a1));
    const bug1 = a1.done !== 5;
    if (bug1) console.log('BUG 1: the running indexer did not re-embed after a cross-process durableRebuild');

    await db.close();
    db = new OKDB(dir, {});
    await db.open();
    console.log('open envs right after db.open():', [...db._envs.keys()].join(', '));
    await sleep(500); // the app is still starting up
    env = await db.openEnv('ws');
    await env.resolveField('docs', 'body', null, resolver);
    const a2 = await stats(db);
    console.log('A after restart + resolver:', JSON.stringify(a2));
    const failed = db.embeddings._durableIndexerDocs('ws:p', { status: 'failed', limit: 1 });
    if (failed && failed[0]) console.log('first failure:', failed[0].error);
    const bug2 = a2.failed > 0 || a2.done !== 5;
    if (bug2) console.log('BUG 2: docs failed/unindexed for want of a resolver during startup');
    await db.close().catch(() => {});
    fs.rmSync(dir, { recursive: true, force: true });
    process.exit(bug1 || bug2 ? 1 : 0);
}

if (process.argv[2] === 'child')
    child(process.argv[3]).then(
        () => process.exit(0),
        (e) => {
            console.error(e);
            process.exit(1);
        },
    );
else main();
