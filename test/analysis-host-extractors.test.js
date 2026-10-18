'use strict';
// A host's own analysers — analysis.registerExtension and analysis.registerExtractor — drive workspace
// indexing too, not only analysis.extract(): a path they claim is found by its symbols, outlined, and
// re-analysed when the analyser's version changes. Paths they do not claim keep the built-in analysis.

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const okcode = require('../lib/okcode');
const { tmpRoot } = require('./fixtures/code-fixture');

const { analysis, access } = okcode;

// "## name" headings of a .widget.md file become widget symbols
let widgetVersion = 'widgets-1';
const widgets = (source) => {
    const symbols = [];
    const lines = source.split('\n');
    let offset = 0;
    lines.forEach((line, i) => {
        const m = /^## (\w+)/.exec(line);
        if (m) symbols.push({ name: m[1], path: m[1], kind: 'widget', lineStart: i + 1, lineEnd: i + 1, start: offset, end: offset + line.length, exported: true, doc: null, signature: line });
        offset += line.length + 1;
    });
    return symbols;
};
analysis.registerExtension({
    id: 'widgets',
    match: (p) => p.endsWith('.widget.md'),
    analyze: ({ source }) => ({ symbols: widgets(source), imports: [], exports: [], parsed: true }),
    version: widgetVersion,
});
// "key = value" lines of an .ini file become setting symbols
analysis.registerExtractor('ini', (p, src) => ({
    symbols: src
        .split('\n')
        .map((line, i) => [/^(\w+)\s*=/.exec(line), i])
        .filter(([m]) => m)
        .map(([m, i]) => ({ name: m[1], path: m[1], kind: 'setting', lineStart: i + 1, lineEnd: i + 1, exported: true })),
    imports: [],
    exports: [],
}));

describe('host extractors and extensions index workspaces', () => {
    const { base, root } = tmpRoot('host-extractors');
    fs.mkdirSync(root, { recursive: true });
    fs.writeFileSync(path.join(root, 'panel.widget.md'), '# Panel\n\n## SidePanel\ntext\n\n## TopBar\n');
    fs.writeFileSync(path.join(root, 'notes.md'), '# Notes\n\n## SidePanelNotes\n');
    fs.writeFileSync(path.join(root, 'app.ini'), 'port = 8080\nhost_name = local\n');
    fs.writeFileSync(path.join(root, 'main.js'), 'function boot() {}\nmodule.exports = { boot };\n');
    let oc;
    after(async () => {
        if (oc) await oc.close();
        fs.rmSync(base, { recursive: true, force: true });
    });

    it('a path claimed by an extension or a registered extractor is indexed by it; others stay built-in', async () => {
        oc = await okcode.open({ path: path.join(base, 'store') });
        const ws = await oc.addWorkspace('app', { access: access.localFs(root) });
        const side = await ws.find('SidePanel');
        assert.ok(side.some((h) => h.file === 'panel.widget.md' && h.kind === 'widget'), JSON.stringify(side));
        assert.ok(!side.some((h) => h.file === 'notes.md' && h.kind === 'widget'), 'a plain .md is not claimed');
        const port = await ws.find('host_name');
        assert.ok(port.some((h) => h.file === 'app.ini' && h.kind === 'setting'), JSON.stringify(port));
        assert.ok((await ws.find('boot')).some((h) => h.file === 'main.js' && h.kind === 'function'), 'built-in JavaScript');
        const outline = await ws.outline('panel.widget.md');
        assert.deepEqual(
            outline.symbols.map((s) => s.name),
            ['SidePanel', 'TopBar'],
            JSON.stringify(outline),
        );
    });

    it('a changed analyser version re-analyses the files it claims', async () => {
        const ws = oc.workspace('app');
        widgetVersion = 'widgets-2';
        analysis.registerExtension({
            id: 'widgets',
            match: (p) => p.endsWith('.widget.md'),
            analyze: ({ source }) => ({ symbols: widgets(source).map((s) => ({ ...s, kind: 'widget2' })), imports: [], exports: [], parsed: true }),
            version: widgetVersion,
        });
        await ws.refresh({ force: true });
        const top = await ws.find('TopBar');
        assert.ok(top.some((h) => h.file === 'panel.widget.md' && h.kind === 'widget2'), JSON.stringify(top));
    });
});
