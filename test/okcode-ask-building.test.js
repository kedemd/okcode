'use strict';
// Vectors are embedded in the background after a workspace is indexed. An ask() that lands before
// they caught up is answered mostly (or wholly) by the lexical eyes — and must say so
// (`vectorsBuilding: { done, pending }`, rendered by code_ask), or a host takes a lexical answer
// for a semantic one. Once the indexer has drained, the mark is gone.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../lib/okcode');
const { localFs } = okcode.access;
const { createTools } = require('../lib/tools');
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

const DIMS = 16;
function bow(text) {
    const v = new Array(DIMS).fill(0);
    for (const w of String(text).toLowerCase().match(/[a-z]{3,}/g) || []) {
        let h = 0;
        for (const c of w) h = (h * 31 + c.charCodeAt(0)) >>> 0;
        v[h % DIMS] += 1;
    }
    const n = Math.hypot(...v) || 1;
    return v.map((x) => x / n);
}

describe('ask() while the vectors are still building', () => {
    let base;
    let root;
    before(() => {
        base = tmpRoot('ask-building').base;
        root = path.join(base, 'src');
        writeFixture(root);
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));

    it('marks the answer vectorsBuilding until the indexer drains, and code_ask says so', async () => {
        // The index's embed calls wait on a gate; the query never embeds (a host vector).
        let open;
        const gate = new Promise((r) => (open = r));
        const oc = await okcode.open({
            path: path.join(base, 'okdb'),
            embedders: {
                q: {
                    id: 'gated-bow',
                    dims: DIMS,
                    embed: async (t) => {
                        await gate;
                        return Array.isArray(t) ? t.map(bow) : bow(t);
                    },
                },
            },
            active: 'q',
        });
        try {
            const ws = await oc.addWorkspace('app', { access: localFs(root) });
            const { identity } = oc.embedders().find((e) => e.active);
            const vector = bow('add two numbers');

            const early = await ws.ask({ text: 'add two numbers', vector, identity }, { limit: 5 });
            assert.ok(early.vectorsBuilding, 'an ask before the vectors exist is marked');
            assert.equal(early.vectorsBuilding.done, 0);
            assert.ok(early.vectorsBuilding.pending > 0, JSON.stringify(early.vectorsBuilding));
            assert.ok(early.every((h) => h.via !== 'vector'), 'no vector hits yet');

            const tools = createTools({ workspace: async () => ws, workspaces: () => [{ id: 'app', root }] });
            const text = await tools.render('code_ask', { question: 'add two numbers', vector, identity });
            assert.match(text, /meaning is still being indexed \(0 of \d+ files embedded\)/);

            open();
            await ws.store.settle();
            const later = await ws.ask({ text: 'add two numbers', vector, identity }, { limit: 5 });
            assert.equal(later.vectorsBuilding, undefined, 'the mark is gone once the indexer drained');
            assert.ok(later.some((h) => h.via === 'vector'));
            assert.doesNotMatch(await tools.render('code_ask', { question: 'add two numbers', vector, identity }), /still being indexed/);
        } finally {
            await oc.close();
        }
    });
});
