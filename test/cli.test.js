'use strict';
// bin/okcode.js end to end: spawned against the fixture tree with a temp store,
// the way a person at a terminal (or a script) drives it. Human output by
// default, --json parseable on stdout (okdb's own logging must not leak into
// it), `at` on stderr for reads, and --at enforced for edits.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

const BIN = path.join(__dirname, '..', 'bin', 'okcode.js');
const HAVE_OKCODE = fs.existsSync(path.join(__dirname, '..', 'src', 'okcode.js'));
const skip = HAVE_OKCODE ? false : 'src/okcode.js not built yet';

const atOf = (text) => (/\bat=([0-9A-F]{40})\b/.exec(text) || [])[1] || null;

describe('cli', () => {
    let base;
    let root;
    let store;
    let scratch;

    before(() => {
        ({ base, root } = tmpRoot('cli'));
        writeFixture(root);
        store = path.join(base, 'store');
        scratch = path.join(base, 'scratch');
        fs.mkdirSync(scratch, { recursive: true });
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));

    // Runs with the fixture as --root and the temp store; cwd elsewhere so the
    // default root is never what is being tested by accident.
    const run = (args, { withRoot = true, timeout = 120000 } = {}) => {
        const r = spawnSync(
            process.execPath,
            [BIN, ...args, ...(withRoot ? ['--root', root, '--id', 'fx'] : []), '--store', store],
            { cwd: scratch, encoding: 'utf8', timeout, env: { ...process.env, OKCODE_VERBOSE: '' } },
        );
        return { code: r.status, stdout: r.stdout, stderr: r.stderr };
    };
    const tmpFile = (name, text) => {
        const f = path.join(scratch, name);
        fs.writeFileSync(f, text);
        return f;
    };
    const disk = (rel) => fs.readFileSync(path.join(root, rel), 'utf8');

    it('--help and per-command help need no store', () => {
        const h = spawnSync(process.execPath, [BIN, '--help'], { encoding: 'utf8' });
        assert.equal(h.status, 0);
        assert.match(h.stdout, /usage: okcode <command>/);
        for (const c of ['find', 'edit-batch', 'reset', 'remove-workspace']) assert.match(h.stdout, new RegExp(c));
        const one = spawnSync(process.execPath, [BIN, 'help', 'edit'], { encoding: 'utf8' });
        assert.equal(one.status, 0);
        assert.match(one.stdout, /--at HASH/);
        const flag = spawnSync(process.execPath, [BIN, 'read', '--help'], { encoding: 'utf8' });
        assert.match(flag.stdout, /at= on stderr/);
        const none = spawnSync(process.execPath, [BIN], { encoding: 'utf8' });
        assert.equal(none.status, 1);
        const bogus = spawnSync(process.execPath, [BIN, 'bogus'], { encoding: 'utf8' });
        assert.equal(bogus.status, 1);
        assert.match(bogus.stderr, /unknown command "bogus"/);
    });

    it('find: human table and --json', { skip }, () => {
        const r = run(['find', 'add']);
        assert.equal(r.code, 0, r.stderr);
        assert.match(r.stdout, /function\s+add\s+lib\/math\.js:5-7\s+3 lines/);
        const j = run(['find', 'add', '--json']);
        assert.equal(j.code, 0, j.stderr);
        const hits = JSON.parse(j.stdout);
        assert.ok(hits.some((h) => h.name === 'add' && h.file === 'lib/math.js' && /^[0-9A-F]{40}$/.test(h.at)));
        const miss = run(['find', 'zzzNoSuchThing']);
        assert.match(miss.stdout, /no match/);
    });

    it('read: body on stdout, location and at on stderr; misses exit 1', { skip }, () => {
        const r = run(['read', 'add']);
        assert.equal(r.code, 0, r.stderr);
        assert.equal(r.stdout, 'function add(a, b) {\n    return a + b;\n}\n');
        assert.match(r.stderr, /# lib\/math\.js:5-7 {2}\(function add\)/);
        assert.ok(atOf(r.stderr));
        const range = run(['read', 'lib/math.js:9-9']);
        assert.equal(range.stdout, 'const TAX_RATE = 0.17;\n');
        const miss = run(['read', 'noSuchSymbolAnywhere']);
        assert.equal(miss.code, 1);
        assert.match(miss.stderr, /no symbol named/);
        const j = run(['read', 'add', '--json']);
        assert.equal(JSON.parse(j.stdout).at, atOf(r.stderr));
    });

    it('grep, outline, refs, packages, structure, stats', { skip }, () => {
        assert.match(run(['grep', 'TAX_RATE = 0.17']).stdout, /lib\/math\.js:9/);
        const o = run(['outline', 'lib/factory.js']);
        assert.equal(o.code, 0, o.stderr);
        assert.match(o.stdout, /makeCounter/);
        assert.match(o.stdout, /at=[0-9A-F]{40}/);
        assert.match(run(['refs', 'add']).stdout, /in twice/);
        assert.match(run(['packages']).stdout, /widget/);
        assert.match(run(['packages', 'widget']).stdout, /widget@2\.3\.1/);
        assert.match(run(['structure']).stdout, /LARGEST SYMBOLS/);
        const ask = run(['ask', 'how', 'are', 'numbers', 'added']);
        assert.equal(ask.code, 1);
        assert.match(ask.stdout, /no semantic index/);
        const st = JSON.parse(run(['stats', '--json']).stdout);
        assert.equal(st.id, 'fx');
        assert.ok(st.files >= 5);
    });

    it('edit: --at required, applied with it, stale refused', { skip }, () => {
        const at = atOf(run(['read', 'scale']).stderr);
        assert.ok(at);
        const body = tmpFile('scale.txt', 'const scale = (v, by) => by * v;\n');

        const noAt = run(['edit', 'scale', '--file', body]);
        assert.equal(noAt.code, 1);
        assert.match(noAt.stderr, /edit needs --at/);

        const ok = run(['edit', 'scale', '--file', body, '--at', at]);
        assert.equal(ok.code, 0, ok.stderr);
        assert.match(ok.stderr, /^ok {2}lib\/math\.js/);
        assert.match(ok.stdout, /\+ const scale = \(v, by\) => by \* v;/);
        assert.match(disk('lib/math.js'), /by \* v/);
        const newAt = atOf(ok.stderr);
        assert.ok(newAt && newAt !== at);

        const stale = run(['edit', 'scale', '--file', body, '--at', at]);
        assert.equal(stale.code, 1);
        assert.match(stale.stderr, /REFUSED: the file changed/);
        assert.ok(stale.stderr.includes(`current at=${newAt}`));

        const bad = run(['edit', 'scale', '--file', tmpFile('bad.txt', 'const scale = (v, by) => ;'), '--at', newAt]);
        assert.equal(bad.code, 1);
        assert.match(bad.stderr, /REFUSED: candidate introduces/);
        assert.match(disk('lib/math.js'), /by \* v/);
    });

    it('edit-batch: --json batch.json (brain-compatible) and --json receipt', { skip }, () => {
        const at = JSON.parse(run(['read', 'lib/factory.js', '--json']).stdout).at;
        const batch = tmpFile(
            'batch.json',
            JSON.stringify({
                at,
                edits: [{ target: 'makeCounter.bump', body: 'function bump(by) { n += by * 1; return n; }' }],
            }),
        );
        const r = run(['edit-batch', '--json', batch]);
        assert.equal(r.code, 0, r.stderr);
        assert.match(disk('lib/factory.js'), /by \* 1/);
        const noAt = run([
            'edit-batch',
            '--batch',
            tmpFile('b2.json', JSON.stringify({ edits: [{ target: 'x', body: 'y' }] })),
        ]);
        assert.equal(noAt.code, 1);
        assert.match(noAt.stderr, /needs "at"/);
    });

    it('write: --create, then replace with --at; neither is refused', { skip }, () => {
        const text = tmpFile('new.txt', "'use strict';\nmodule.exports = 1;\n");
        const c = run(['write', 'lib/created.js', '--file', text, '--create', '--json']);
        assert.equal(c.code, 0, c.stderr);
        const receipt = JSON.parse(c.stdout);
        assert.equal(receipt.outcome, 'ok');
        assert.equal(disk('lib/created.js'), "'use strict';\nmodule.exports = 1;\n");
        const again = run(['write', 'lib/created.js', '--file', text, '--create']);
        assert.equal(again.code, 1);
        assert.match(again.stderr, /already exists/);
        const neither = run(['write', 'lib/created.js', '--file', text]);
        assert.equal(neither.code, 1);
        assert.match(neither.stderr, /--create .* or --at/);
        const r = run([
            'write',
            'lib/created.js',
            '--file',
            tmpFile('n2.txt', 'module.exports = 2;\n'),
            '--at',
            receipt.readbackHash,
        ]);
        assert.equal(r.code, 0, r.stderr);
        assert.equal(disk('lib/created.js'), 'module.exports = 2;\n');
    });

    it('check: node --check on the target', { skip }, () => {
        const r = run(['check', 'lib/math.js']);
        const j = JSON.parse(r.stdout);
        assert.equal(j.ok, true, r.stdout + r.stderr);
        assert.equal(r.code, 0);
    });

    // Two FTS indexes share one ~fts env; resetting both used to crash okdb's
    // writer (nested txn) and hang fts.ready() — fixed in okdb.
    it('reset --scope fts rebuilds and returns', { skip }, () => {
        const rf = run(['reset', '--scope', 'fts'], { timeout: 30000 });
        assert.equal(rf.code, 0, rf.stderr);
        assert.match(rf.stdout, /reset: symbols, files/);
    });

    it('management: status, sync, workspaces, embedders, reset, remove-workspace', { skip }, () => {
        const s = run(['status', '--json']);
        assert.equal(s.code, 0, s.stderr);
        const status = JSON.parse(s.stdout);
        assert.ok(JSON.stringify(status).includes('"fx"'));
        assert.match(run(['status']).stdout, /fx/);

        fs.writeFileSync(
            path.join(root, 'lib/late.js'),
            'function lateComer() { return 1; }\nmodule.exports = { lateComer };\n',
        );
        const sy = run(['sync', '--force']);
        assert.equal(sy.code, 0, sy.stderr);
        assert.match(sy.stdout, /synced fx: \d+ scanned/);
        assert.match(run(['find', 'lateComer']).stdout, /lib\/late\.js/);

        const ws = JSON.parse(run(['workspaces', '--json'], { withRoot: false }).stdout);
        assert.ok(ws.some((w) => w.id === 'fx'));
        assert.match(run(['workspaces'], { withRoot: false }).stdout, /fx/);

        const e = run(['embedders'], { withRoot: false });
        assert.equal(e.code, 0, e.stderr);
        assert.match(e.stdout, /no embedder profiles/);
        assert.deepEqual(JSON.parse(run(['embedders', '--json'], { withRoot: false }).stdout), []);

        assert.equal(run(['reset']).code, 1);
        const rv = run(['reset', '--scope', 'vectors', '--json']);
        assert.equal(rv.code, 0, rv.stderr);
        assert.deepEqual(JSON.parse(rv.stdout).rebuilt, []);

        const rm = run(['remove-workspace', 'fx', '--json'], { withRoot: false });
        assert.equal(rm.code, 0, rm.stderr);
        assert.equal(JSON.parse(rm.stdout).removed, true);
        assert.ok(!JSON.parse(run(['workspaces', '--json'], { withRoot: false }).stdout).some((w) => w.id === 'fx'));
    });
});
