'use strict';
// okdb repro: env.pipelines.remove(P) uninstalls P's engines (and the
// indexer's doc_status rows) but leaves the `vec:P` rows in the per-type env
// `~<env>:emb:<type>` — the vectors of a removed pipeline stay on disk forever.
// Expected: removing a pipeline removes its vectors.
// Run: node test/okdb-repro/pipeline-remove-keeps-vectors.js   (exit 0 = fixed)
const fs = require('fs');
const os = require('os');
const path = require('path');
const OKDB = require('@kedem/okdb');

(async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'okdb-repro-'));
    const db = new OKDB(dir, {});
    await db.open();
    const env = await db.createEnvironment('ws');
    await env.ensureType('docs');
    for (let i = 0; i < 5; i++) await env.put('docs', `d${i}`, { text: `hello world number ${i}` });
    await db.embeddings.createPipeline('p', {
        source_type: 'docs',
        source_env: 'ws',
        field: 'text',
        dims: 8,
        embedder: { type: 'fake', model: 'f', dims: 8 },
    });
    await db.embeddings.indexer('ws:p').flush();
    const typeEnv = await db.openEnv('~ws:emb:docs');
    const count = () => (typeEnv.hasType('vec:p') ? typeEnv.getCount('vec:p') : 0);
    const before = count();
    await env.pipelines.remove('p');
    const after = count();
    console.log(`vectors before remove: ${before}, after: ${after}`);
    const code = before > 0 && after === 0 ? 0 : 1;
    console.log(code ? 'BUG: pipeline removed, vectors kept' : 'OK');
    await db.close().catch(() => {});
    fs.rmSync(dir, { recursive: true, force: true });
    process.exit(code);
})();
