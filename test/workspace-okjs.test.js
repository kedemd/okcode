'use strict';
// okjs components at the workspace surface: a component's members are
// findable by name (`find('refresh')` reaches `setting-group.context.
// refresh`), readable by path (`read('file#tag.member')`), and the piecewise
// outline still offers the template/style regions it always did — all
// through okcode's own `find`/`outline`/`read`, never through parse.js or
// ext-okjs.js directly (those are covered by test/analysis-okjs.test.js).
// localFs only: the behaviour under test is the extension's symbol/region
// shape, not facade plumbing (that is every other suite's job).

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { localFs } = require('../lib/access');
const okcode = require('../lib/okcode');

let hasTooling = true;
try {
    require.resolve('@kedem/okjs/tooling');
} catch {
    hasTooling = false;
}

const COMPONENT = [
    "// A tiny component fixture, trimmed from setting-group.ok.js's shape:",
    '// a component with a context() factory and a method addressed directly.',
    "import { callFn } from './lib.js';",
    '',
    '// A module-level helper — NOT part of the component definition, so it',
    '// must stay a top-level symbol rather than being nested under the tag.',
    'function clamp(n) {',
    '    return Math.max(0, n);',
    '}',
    '',
    'export default {',
    "    tag: 'setting-group',",
    '    attr: { group: { default: \'\' } },',
    '',
    '    context() {',
    '        return {',
    '            rows: null,',
    '            refresh() {',
    '                return clamp(1);',
    '            },',
    '        };',
    '    },',
    '',
    '    template: `<div class="sg"></div>`,',
    '',
    '    style: `[tag] { display: block; }`,',
    '};',
    '',
].join('\n');

describe('workspace find/outline/read over an okjs component', { skip: !hasTooling && 'okjs tooling not installed' }, () => {
    let base;
    let root;
    let storePath;
    let oc;
    let ws;

    before(async () => {
        base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-okjs-ws-'));
        root = path.join(base, 'root');
        fs.mkdirSync(path.join(root, 'components'), { recursive: true });
        fs.writeFileSync(path.join(root, 'components', 'setting-group.ok.js'), COMPONENT);
        fs.writeFileSync(path.join(root, 'components', 'lib.js'), 'module.exports = { callFn() {} };\n');
        storePath = path.join(base, 'okdb');
        oc = await okcode.open({ path: storePath });
        ws = await oc.addWorkspace('app', { access: localFs(root) });
    });

    after(async () => {
        if (oc) await oc.close();
        fs.rmSync(base, { recursive: true, force: true });
    });

    it('find() reaches a component member by name, nested under the tag', async () => {
        const hits = await ws.find('refresh');
        const hit = hits.find((h) => h.file === 'components/setting-group.ok.js');
        assert.ok(hit, `refresh not found among: ${JSON.stringify(hits)}`);
        assert.equal(hit.path, 'setting-group.context.refresh');
        assert.equal(hit.parent, 'setting-group.context');
    });

    it('find() still reaches the component tag itself', async () => {
        const hits = await ws.find('setting-group');
        const hit = hits.find((h) => h.file === 'components/setting-group.ok.js' && h.path === 'setting-group');
        assert.ok(hit, `tag not found among: ${JSON.stringify(hits)}`);
        assert.equal(hit.kind, 'component');
    });

    it('find() reaches a top-level helper OUTSIDE the component definition, un-nested', async () => {
        const hits = await ws.find('clamp');
        const hit = hits.find((h) => h.file === 'components/setting-group.ok.js');
        assert.ok(hit, `clamp not found among: ${JSON.stringify(hits)}`);
        assert.equal(hit.path, 'clamp');
        assert.equal(hit.parent, null, 'a module-level helper has no parent');
    });

    it('outline() still offers the template/style pieces, not just the one component symbol', async () => {
        const out = await ws.outline('components/setting-group.ok.js');
        assert.equal(out.ok, true);
        assert.equal(out.by, 'region');
        const kinds = out.symbols.map((s) => s.kind);
        assert.ok(kinds.includes('template'), JSON.stringify(kinds));
        assert.ok(kinds.includes('style'), JSON.stringify(kinds));
    });

    it("read('file#tag.member') reads exactly the member's body", async () => {
        const r = await ws.read('components/setting-group.ok.js#setting-group.context.refresh');
        assert.equal(r.ok, true, JSON.stringify(r));
        assert.match(r.body, /return clamp\(1\);/);
        assert.ok(!r.body.includes('template:'), 'the slice is the method, not the whole component');
    });
});
