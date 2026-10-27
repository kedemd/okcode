'use strict';
// A process that opened okcode's store, indexed a workspace and closed it must exit cleanly. okdb drops
// its callback references in thread-locals at process exit and releases them through okcode's host;
// a host context okcode freed itself before that made node die with SIGSEGV after close().
// The crash was a race (most runs, not all), so the child runs several times.

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const CHILD = `
const okcode = require(${JSON.stringify(path.join(__dirname, '..', 'lib', 'okcode'))});
(async () => {
    const oc = await okcode.open({ path: process.argv[1] });
    const ws = await oc.addWorkspace('w', { access: okcode.access.localFs(process.argv[2]) });
    await ws.find('open');
    await oc.close();
})().catch((e) => { console.error(e); process.exit(1); });
`;

describe('process exit after close()', () => {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-exit-'));
    after(() => fs.rmSync(base, { recursive: true, force: true }));

    it('exits 0, never by a signal, after indexing and closing', () => {
        for (let i = 0; i < 5; i++) {
            const r = spawnSync(process.execPath, ['-e', CHILD, path.join(base, `okdb-${i}`), path.join(__dirname, '..', 'lib')], {
                encoding: 'utf8',
                timeout: 60_000,
            });
            assert.equal(r.signal, null, `run ${i} died by ${r.signal}\n${r.stderr.slice(-2000)}`);
            assert.equal(r.status, 0, `run ${i} exited ${r.status}\n${r.stderr.slice(-2000)}`);
        }
    });
});
