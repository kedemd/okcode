'use strict';
// A host's function embedder (a custom `embed`) survives a reopen: okdb restarts a store's persisted embedder
// engines inside open(), so okcode registers the host's derived factory types BEFORE it (embedders.js preRegister).
// Seen live: every restart logged "Unknown embedder type" for the workspace's engine.

const { it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../lib/okcode');
const { localFs } = okcode.access;
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

function vec(text, dims) {
    const v = new Array(dims).fill(0);
    for (const w of String(text).toLowerCase().split(/[^a-z0-9]+/)) if (w.length > 2) v[w.length % dims] += 1;
    const n = Math.hypot(...v) || 1;
    return v.map((x) => x / n);
}
const embedders = () => ({
    code: { id: 'reopen-test@1', dims: 16, embed: async (t) => (Array.isArray(t) ? t.map((x) => vec(x, 16)) : vec(t, 16)) },
});

it('a custom embedder engine starts again on reopen: no unknown type, the pipeline keeps embedding', async () => {
    const { base } = tmpRoot('ocreopen');
    const root = path.join(base, 'src');
    const store = path.join(base, 'okdb');
    writeFixture(root);
    const failures = [];
    const orig = console.error;
    const origWarn = console.warn;
    const watch = (...a) => {
        const s = a.map(String).join(' ');
        if (/Unknown embedder type|failed to start engine/.test(s)) failures.push(s);
    };
    console.error = (...a) => { watch(...a); };
    console.warn = (...a) => { watch(...a); };
    try {
        let oc = await okcode.open({ path: store, embedders: embedders(), active: 'code' });
        let ws = await oc.addWorkspace('app', { access: localFs(root) });
        await ws.store.settle();
        await oc.close();

        oc = await okcode.open({ path: store, embedders: embedders(), active: 'code' });
        try {
            ws = await oc.addWorkspace('app', { access: localFs(root) });
            fs.writeFileSync(path.join(root, 'lib', 'later.js'), '// added after the reopen\nfunction later() {}\n');
            await ws.sync();
            await ws.store.settle();
            const st = await oc.status('app');
            const e = st.workspaces[0].embedders.find((x) => x.name === 'code');
            assert.equal(e.state, 'ready', JSON.stringify(e));
            assert.equal(e.failed, 0, JSON.stringify(e));
        } finally {
            await oc.close();
        }
        assert.deepEqual(failures, [], 'no engine failed to start on reopen');
    } finally {
        console.error = orig;
        console.warn = origWarn;
        fs.rmSync(base, { recursive: true, force: true });
    }
});
