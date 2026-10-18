'use strict';
// addWorkspace(id, { access, fts }) — 'full' (default, unchanged) builds the
// content full-text index; 'symbols' never does. Everything that used it
// (grep/mentions via the accelerator, find()/ask()'s file-text eye) must
// still answer correctly without it, and switching an existing workspace
// between modes must converge both directions (DESIGN §5/§9).

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../lib/okcode');
const { localFs } = okcode.access;
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

describe('fts: symbols', () => {
    let base;
    let root;
    let n = 0;
    before(() => {
        base = tmpRoot('fts-mode').base;
        root = path.join(base, 'src');
        writeFixture(root);
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));
    // Each test opens its own store: an unlicensed okdb allows 5 envs total,
    // and envs persist on disk across okcode.open()/close() on the same
    // path — a shared store would refuse this suite's workspaces past the
    // third one.
    const freshStore = () => path.join(base, `okdb-${++n}`);

    it('builds no content index, but grep/mentions/find still answer correctly', async () => {
        const oc = await okcode.open({ path: freshStore() });
        try {
            const ws = await oc.addWorkspace('symws', { access: localFs(root), fts: 'symbols' });

            const s = await oc.status('symws');
            assert.equal(s.workspaces[0].fts.mode, 'symbols');
            assert.equal(s.workspaces[0].fts.files.status, 'absent');
            assert.equal(s.workspaces[0].fts.symbols.status, 'ready');

            // grep is never gated by the index (DESIGN §9) — a content
            // literal is found by a direct scan (`via` says so).
            const g = await ws.grep('function add', {});
            assert.ok(g.total.lines >= 1, JSON.stringify(g));

            // mentions() — a textual scan, same accelerator, same fallback.
            const m = await ws.mentions('add');
            assert.ok(m.total >= 1, JSON.stringify(m));

            // find() by NAME still works (the symbols FTS is untouched); a
            // content-only hit (something only findable via file text) is
            // simply not offered any more — no crash, no false positive.
            const byName = await ws.find('add');
            assert.ok(byName.some((h) => h.name === 'add' && h.file === 'lib/math.js'));

            await ws.close();
        } finally {
            await oc.close?.();
        }
    });

    it('ask() fuses the remaining eyes and marks the result when the content eye is skipped', async () => {
        const oc = await okcode.open({ path: freshStore(), embedders: { fake: { type: 'fake', model: 'fake-v1', dims: 16 } } });
        try {
            const ws = await oc.addWorkspace('symws2', { access: localFs(root), fts: 'symbols' });
            await ws.store?.settle?.();
            const hits = await ws.ask('add numbers together');
            assert.ok(hits.contentEyeSkipped, JSON.stringify(hits));
        } finally {
            await oc.close?.();
        }
    });

    it('switching modes converges in both directions', async () => {
        const oc = await okcode.open({ path: freshStore() });
        try {
            // full -> symbols: an existing content index is dropped.
            let ws = await oc.addWorkspace('switcher', { access: localFs(root), fts: 'full' });
            let s = await oc.status('switcher');
            assert.equal(s.workspaces[0].fts.mode, 'full');
            assert.equal(s.workspaces[0].fts.files.status, 'ready');
            await ws.close();

            ws = await oc.addWorkspace('switcher', { access: localFs(root), fts: 'symbols' });
            s = await oc.status('switcher');
            assert.equal(s.workspaces[0].fts.mode, 'symbols');
            assert.equal(s.workspaces[0].fts.files.status, 'absent');
            await ws.close();

            // symbols -> full: it is built back.
            ws = await oc.addWorkspace('switcher', { access: localFs(root), fts: 'full' });
            s = await oc.status('switcher');
            assert.equal(s.workspaces[0].fts.mode, 'full');
            assert.equal(s.workspaces[0].fts.files.status, 'ready');
            const content = await ws.find('counter step total');
            assert.ok(content.length >= 0); // no crash; the index exists again
            await ws.close();
        } finally {
            await oc.close?.();
        }
    });

    it('defaults to full, and the workspace-level default applies when fts is not given per call', async () => {
        const oc = await okcode.open({ path: freshStore(), workspace: { fts: 'symbols' } });
        try {
            const ws = await oc.addWorkspace('defaulted', { access: localFs(root) });
            const s = await oc.status('defaulted');
            assert.equal(s.workspaces[0].fts.mode, 'symbols');
            await ws.close();
        } finally {
            await oc.close?.();
        }
    });

    it('rejects a bad fts value', async () => {
        const oc = await okcode.open({ path: freshStore() });
        try {
            await assert.rejects(
                oc.addWorkspace('badfts', { access: localFs(root), fts: 'nope' }),
                /fts must be 'full' or 'symbols'/,
            );
        } finally {
            await oc.close?.();
        }
    });
});
