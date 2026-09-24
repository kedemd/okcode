'use strict';
// One odd filename must never fail a whole workspace. The live case: a file
// literally named `d:\test\snake\index.html` (backslashes in the NAME) landed
// in a Linux workspace, and every later open refused the workspace with
// "workspace path must be relative". Path rules follow the facade's dialect
// (POSIX: backslash and `X:` are ordinary characters; Windows: they are not),
// and a name the facade still cannot handle is skipped with a scan warning.

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const okcode = require('../src/okcode');
const { localFs, shell } = require('../src/access');
const { checkPath, pathChecker } = require('../src/access/common');

const ODD = 'd:\\test\\snake\\index.html';
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-odd-'));
after(() => fs.rmSync(TMP, { recursive: true, force: true }));

let seq = 0;
function tree() {
    const dir = path.join(TMP, `t${++seq}`);
    const root = path.join(dir, 'root');
    fs.mkdirSync(path.join(root, 'src'), { recursive: true });
    fs.writeFileSync(path.join(root, 'src', 'a.js'), 'function alpha() { return 1; }\nmodule.exports = { alpha };\n');
    fs.writeFileSync(path.join(root, ODD), '<!doctype html>\n<title>snake</title>\n');
    return { dir, root, store: path.join(dir, 'okdb') };
}

describe('checkPath is dialect-aware', () => {
    it('posix: backslash and drive letters are ordinary name characters', () => {
        for (const p of [ODD, 'C:/x/y.js', 'c:x', '..\\x', 'a\\..\\..\\b', '\\\\host\\share']) {
            assert.equal(checkPath(p, 'posix'), p, p);
        }
        assert.equal(checkPath(ODD), ODD, 'posix is the default');
        for (const bad of ['/etc/passwd', '../x', 'a/../../x', '..', '', 'a\0b']) {
            assert.throws(() => checkPath(bad, 'posix'), undefined, JSON.stringify(bad));
        }
    });

    it('windows: drive letters, rooted, UNC and backslash `..` are refused', () => {
        for (const bad of [ODD, 'C:/x', 'c:x', '\\x', '\\\\host\\share', '..\\x', 'a\\..\\..\\b', '/x', '../x']) {
            assert.throws(() => checkPath(bad, 'windows'), undefined, JSON.stringify(bad));
        }
        assert.equal(checkPath('src/a.js', 'windows'), 'src/a.js');
        assert.throws(() => checkPath('x', 'vms'), /unknown path dialect/);
    });

    it('facades expose their own checker: localFs/bash → posix, powershell → windows', () => {
        const local = localFs(TMP);
        const bash = shell({ root: TMP, run: async () => '' });
        const ps = shell({ root: 'C:\\w', run: async () => '', dialect: 'powershell' });
        if (process.platform !== 'win32') assert.equal(local.checkPath(ODD), ODD);
        assert.equal(bash.checkPath(ODD), ODD);
        assert.throws(() => ps.checkPath(ODD), /must be relative/);
    });
});

const facades = [
    { name: 'localFs', make: (root) => localFs(root) },
    { name: 'shell (bash, local runner)', make: (root) => shell({ root, run: shell.localRunner('bash') }) },
];

for (const fac of facades) {
    describe(`a backslash-named file over ${fac.name}`, { skip: process.platform === 'win32' }, () => {
        it('is indexed like any other file; the workspace opens with no warning', async () => {
            const t = tree();
            const oc = await okcode.open({ path: t.store });
            try {
                const ws = await oc.addWorkspace('w', { access: fac.make(t.root) });
                const paths = (await ws.files()).files.map((f) => f.file);
                assert.ok(paths.includes(ODD), `listed: ${JSON.stringify(paths)}`);
                assert.ok(paths.includes('src/a.js'));
                assert.equal((await ws.stats()).skipped, 0);
                const st = await oc.status('w');
                assert.equal(st.workspaces[0].warnings, undefined);
                // Still addressable after the scan (verify-on-read).
                const got = await ws.access.read([ODD]);
                assert.match(got.get(ODD).toString('utf8'), /snake/);
            } finally {
                await oc.close();
            }
        });
    });
}

describe('a name the facade cannot handle costs itself alone', () => {
    it('refused by the facade dialect → skipped with a scan warning shown in status', async () => {
        const t = tree();
        // A facade whose rules are Windows' (the powershell dialect) seeing a
        // name only a POSIX filesystem can hold.
        const access = { ...localFs(t.root), checkPath: pathChecker('windows') };
        const oc = await okcode.open({ path: t.store });
        try {
            const ws = await oc.addWorkspace('w', { access });
            const paths = (await ws.files()).files.map((f) => f.file);
            assert.ok(paths.includes('src/a.js'));
            assert.ok(!paths.includes(ODD));
            assert.equal((await ws.stats()).skipped, 1);
            const skipped = (await ws.structure()).skipped;
            assert.equal(skipped.length, 1);
            assert.equal(skipped[0].file, ODD);
            assert.match(skipped[0].reason, /must be relative/);
            const [w] = (await oc.status('w')).workspaces;
            assert.equal(w.warnings.length, 1);
            assert.equal(w.warnings[0].kind, 'scan-skipped');
            assert.equal(w.warnings[0].file, ODD);
            // A later walk that can take it in clears the warning.
            fs.rmSync(path.join(t.root, ODD));
            await ws.refresh({ sync: true });
            assert.equal((await ws.stats()).skipped, 0);
        } finally {
            await oc.close();
        }
    });

    it('a batch read that throws on one name → that file skipped, the rest indexed', async () => {
        const t = tree();
        fs.writeFileSync(path.join(t.root, 'src', 'b.js'), 'function beta() {}\n');
        const base = localFs(t.root);
        // A custom facade with no checkPath of its own that throws for the
        // whole batch whenever the odd name is in it (the pre-fix behaviour).
        const access = {
            kind: 'custom',
            list: base.list,
            stat: base.stat,
            commit: base.commit,
            read: async (paths) => {
                if (paths.includes(ODD)) throw new Error(`workspace path must be relative: ${JSON.stringify(ODD)}`);
                return base.read(paths);
            },
        };
        const oc = await okcode.open({ path: t.store });
        try {
            const ws = await oc.addWorkspace('w', { access });
            const paths = (await ws.files()).files.map((f) => f.file);
            assert.ok(paths.includes('src/a.js') && paths.includes('src/b.js'), JSON.stringify(paths));
            assert.ok(!paths.includes(ODD));
            const [w] = (await oc.status('w')).workspaces;
            assert.deepEqual(
                w.warnings.map((x) => x.file),
                [ODD],
            );
        } finally {
            await oc.close();
        }
    });

    it('a read that fails for EVERY file is an outage, not an odd name: the open still fails', async () => {
        const t = tree();
        const base = localFs(t.root);
        const access = {
            kind: 'custom',
            list: base.list,
            stat: base.stat,
            commit: base.commit,
            read: async () => {
                throw new Error('transport down');
            },
        };
        const oc = await okcode.open({ path: t.store });
        try {
            await assert.rejects(() => oc.addWorkspace('w', { access }), /transport down/);
        } finally {
            await oc.close();
        }
    });
});
