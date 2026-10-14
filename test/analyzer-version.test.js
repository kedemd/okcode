'use strict';
// Every parsed row carries the analyzer version it was extracted under, and a
// warm reopen re-ingests a row only when that version moved. Two failure
// modes this guards: a row stamped with nothing (or a version that can never
// match) re-parses on EVERY open; a row from an older extractor is trusted
// forever and silently lacks what the new one extracts (F3's call sites).

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../src/okcode');
const { extract, analyzerVersionFor } = require('../src/analysis/parse');
const { tmpRoot } = require('./fixtures/code-fixture');

const { localFs } = okcode.access;

describe('analyzer version', () => {
    const bases = [];
    after(() => {
        for (const b of bases) fs.rmSync(b, { recursive: true, force: true });
    });

    it('extract() stamps every parsed language, success or failure', () => {
        assert.equal(extract('a.js', 'function a() {}\n').analyzerVersion, analyzerVersionFor('a.js'));
        assert.equal(extract('bad.js', 'function (\n').analyzerVersion, analyzerVersionFor('bad.js'));
        assert.match(analyzerVersionFor('a.js'), /^js:/);
        assert.equal(extract('a.py', 'def a():\n    pass\n').analyzerVersion, analyzerVersionFor('a.py'));
    });

    it('a warm reopen reads nothing for unchanged files', async () => {
        const { base, root } = tmpRoot('anver');
        bases.push(base);
        const store = path.join(base, 'store');
        fs.mkdirSync(root, { recursive: true });
        fs.writeFileSync(path.join(root, 'a.js'), 'function a() { return b(); }\nmodule.exports = { a };\n');
        fs.writeFileSync(path.join(root, 'b.ts'), 'export function b(): number { return 1; }\n');
        fs.writeFileSync(path.join(root, 'c.py'), 'def c():\n    return 1\n');

        let oc = await okcode.open({ path: store });
        try {
            await oc.addWorkspace('app', { access: localFs(root) });
        } finally {
            await oc.close();
        }

        // The walk reads `.gitignore` itself; any SOURCE read is a re-parse.
        const read = [];
        const fsAccess = localFs(root);
        const access = { ...fsAccess, read: (paths, ...rest) => (read.push(...paths), fsAccess.read(paths, ...rest)) };
        oc = await okcode.open({ path: store });
        try {
            const ws = await oc.addWorkspace('app', { access });
            await ws.sync();
            assert.deepEqual(read.filter((p) => p !== '.gitignore'), [], 'an unchanged row was re-parsed on reopen');
        } finally {
            await oc.close();
        }
    });
});
