'use strict';
// okdb repro: removeEnvironment(E) leaves E's pipeline member engines in the
// in-process engine registry, so re-creating E and the same pipeline in the
// SAME process fails: "Engine already exists: indexer@E:P".
// Expected: after removeEnvironment the env's engines are gone; recreating works.
// Run: node test/okdb-repro/recreate-env-pipeline.js   (exit 0 = fixed)
const fs = require('fs');
const os = require('os');
const path = require('path');
const OKDB = require('@kedem/okdb');

(async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'okdb-repro-'));
    const db = new OKDB(dir, {});
    await db.open();
    const make = async () => {
        const env = await db.createEnvironment('ws');
        await env.ensureType('docs');
        await env.put('docs', 'a', { text: 'hello world' });
        await db.embeddings.createPipeline('p', {
            source_type: 'docs',
            source_env: 'ws',
            field: 'text',
            dims: 8,
            embedder: { type: 'fake', model: 'f', dims: 8 },
        });
    };
    let code = 0;
    try {
        await make();
        await db.removeEnvironment('ws');
        await make(); // throws: Engine already exists: "indexer@ws:p"
        console.log('OK: pipeline re-created after removeEnvironment');
    } catch (err) {
        console.log('BUG:', err.message);
        code = 1;
    }
    await db.close().catch(() => {});
    fs.rmSync(dir, { recursive: true, force: true });
    process.exit(code);
})();
