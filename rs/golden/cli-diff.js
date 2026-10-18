#!/usr/bin/env node
'use strict';
// Side-by-side: bin/okcode.js and the Rust okcode binary over the same argv list, each on its own
// copy of the code fixture with its own store. Diffs stdout, stderr and exit code after
// normalising paths, timings, timestamps and operation ids. Cheap: one process per case.
//
//   node rs/golden/cli-diff.js <rust-binary> [--only substr] [--verbose]
// Prints `cli-diff: <same>/<total>` and the first lines of each difference; exit 1 on any.

const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');
const { writeFixture } = require('../../test/fixtures/code-fixture');

const RUST = process.argv[2];
if (!RUST || !fs.existsSync(RUST)) {
    console.error('usage: cli-diff.js <rust okcode binary> [--only substr] [--verbose]');
    process.exit(2);
}
const ONLY = process.argv.includes('--only') ? process.argv[process.argv.indexOf('--only') + 1] : null;
const VERBOSE = process.argv.includes('--verbose');
const JS = [process.execPath, path.join(__dirname, '..', '..', 'bin', 'okcode.js')];

const base = fs.mkdtempSync(path.join(process.env.TMPDIR || os.tmpdir(), 'cli-diff-'));
const side = (name) => {
    const root = path.join(base, name, 'root');
    writeFixture(root);
    const scratch = path.join(base, name, 'scratch');
    fs.mkdirSync(scratch, { recursive: true });
    fs.writeFileSync(path.join(scratch, 'body.txt'), 'const scale = (v, by) => v * by * 1;\n');
    fs.writeFileSync(path.join(scratch, 'new.txt'), 'module.exports = 1;\n');
    fs.writeFileSync(path.join(scratch, 'bad.json'), '{ "at": ');
    fs.writeFileSync(path.join(scratch, 'noat.json'), '{ "edits": [] }');
    return { name, root, store: path.join(base, name, 'store'), scratch };
};
const sides = { js: side('js'), rs: side('rs') };

// argv per case; `{root}` style flags are appended per side. `bare` runs without --root/--store.
const CASES = [
    { argv: ['--help'], bare: true },
    { argv: ['help', 'grep'], bare: true },
    { argv: ['read', '--help'], bare: true },
    { argv: ['bogus'], bare: true },
    { argv: ['help', 'bogus'], bare: true },
    { argv: ['find', 'add'] },
    { argv: ['find', 'add', '--json'] },
    { argv: ['find', 'zzzNoSuchThing'] },
    { argv: ['find', 'a', '--limit', '1'] },
    { argv: ['find'] },
    { argv: ['refs', 'add'] },
    { argv: ['refs', 'add', '--json'] },
    { argv: ['importers', 'lib/math.js'] },
    { argv: ['importers', 'lib/nope.js'] },
    { argv: ['grep', 'TAX_RATE = 0.17'] },
    { argv: ['grep', '^function \\w+', '--regex', '--glob', 'lib/*.js', '--glob', '!factory.js', '--context', '1'] },
    { argv: ['grep', 'add', '--path', 'lib', '--max-per-file', '1', '--json'] },
    { argv: ['grep', 'ad+', '--regex', '--path', 'lib', '--output', 'matches'] },
    { argv: ['grep', 'add', '--path', 'lib', '--output', 'files'] },
    { argv: ['grep', '('] },
    { argv: ['glob', 'lib/*.js,!lib/unicode.js'] },
    { argv: ['glob', '*.mjs', '--json'] },
    { argv: ['outline', 'lib/factory.js'] },
    { argv: ['outline', 'lib/factory.js', '--json'] },
    { argv: ['outline', 'lib/nope.js'] },
    { argv: ['read', 'add'] },
    { argv: ['read', 'lib/math.js:9-9'] },
    { argv: ['read', 'noSuchSymbolAnywhere'] },
    { argv: ['read', 'add', '--json'] },
    { argv: ['read-at', 'lib/math.js', '5', '7'] },
    { argv: ['read-at', 'lib/math.js', '5', '7', '--at', 'deadbeef'] },
    { argv: ['packages'] },
    { argv: ['packages', 'widget'] },
    { argv: ['packages', 'nope-pkg'] },
    { argv: ['structure'] },
    { argv: ['structure', '--json'] },
    { argv: ['ask', 'how', 'are', 'numbers', 'added'] },
    { argv: ['stats', '--json'] },
    { argv: ['edit', 'scale', '--file', '{scratch}/body.txt'] },
    { argv: ['edit', 'scale', '--file', '{scratch}/body.txt', '--at', '0000000000000000000000000000000000000000'] },
    { argv: ['edit', 'scale', '--file', '{scratch}/missing.txt', '--at', 'x'] },
    { argv: ['edit-batch', '--json', '{scratch}/noat.json'] },
    { argv: ['edit-batch'] },
    { argv: ['write', 'lib/created.js', '--file', '{scratch}/new.txt'] },
    { argv: ['write', 'lib/created.js', '--file', '{scratch}/new.txt', '--create', '--at', 'x'] },
    { argv: ['write', 'lib/created.js', '--file', '{scratch}/new.txt', '--create'] },
    { argv: ['write', 'lib/created.js', '--file', '{scratch}/new.txt', '--create'] },
    { argv: ['check', 'lib/math.js'] },
    { argv: ['status'] },
    { argv: ['status', '--json'] },
    { argv: ['embedders'] },
    { argv: ['workspaces'] },
    { argv: ['reset'] },
    { argv: ['reset', '--scope', 'fts'] },
    { argv: ['sync', '--no-wait'] },
    { argv: ['remove-workspace'] },
    { argv: ['--fts', 'bogus', 'find', 'add'] },
];

const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
function normalise(text, s) {
    return text
        .replace(new RegExp(esc(s.root), 'g'), '<root>')
        .replace(new RegExp(esc(s.store), 'g'), '<store>')
        .replace(new RegExp(esc(s.scratch), 'g'), '<scratch>')
        .replace(new RegExp(esc(base), 'g'), '<base>')
        .replace(/\b\d+ms\b/g, '<n>ms')
        .replace(/("(?:ms|elapsedMs|tookMs|durationMs|lastSync|lastScanAt|added|at|updated|updatedAt|createdAt|startedAt)":\s*)\d+(\.\d+)?/g, '$1<n>')
        .replace(/\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z/g, '<iso>')
        .replace(/operation=\S+/g, 'operation=<op>')
        .replace(/"operationId":\s*"[^"]*"/g, '"operationId": "<op>"')
        .replace(/^(\s*(?:ms|lastSync|lastScanAt|added|elapsedMs): )\d+/gm, '$1<n>');
}

function run(cmd, c, s) {
    const argv = c.argv.map((a) => a.replace('{scratch}', s.scratch));
    const extra = c.bare ? [] : ['--root', s.root, '--id', 'fx', '--store', s.store];
    const r = spawnSync(cmd[0], [...cmd.slice(1), ...argv, ...extra], {
        cwd: s.scratch,
        encoding: 'utf8',
        timeout: 120000,
        env: { ...process.env, OKCODE_VERBOSE: '' },
    });
    return { code: r.status, stdout: normalise(r.stdout || '', s), stderr: normalise(r.stderr || '', s) };
}

function firstDiff(a, b) {
    const la = a.split('\n');
    const lb = b.split('\n');
    for (let i = 0; i < Math.max(la.length, lb.length); i++) {
        if (la[i] !== lb[i]) return `line ${i + 1}:\n      js: ${JSON.stringify(la[i])}\n      rs: ${JSON.stringify(lb[i])}`;
    }
    return null;
}

let same = 0;
let total = 0;
const diffs = [];
for (const c of CASES) {
    const label = c.argv.join(' ');
    if (ONLY && !label.includes(ONLY)) continue;
    total++;
    const t0 = Date.now();
    const js = run(JS, c, sides.js);
    const t1 = Date.now();
    const rs = run([RUST], c, sides.rs);
    const t2 = Date.now();
    const bad = [];
    if (js.code !== rs.code) bad.push(`exit js=${js.code} rs=${rs.code}`);
    for (const k of ['stdout', 'stderr']) {
        const d = firstDiff(js[k], rs[k]);
        if (d) bad.push(`${k} ${d}`);
    }
    if (VERBOSE) console.log(`  ${bad.length ? 'DIFF' : 'same'}  ${label}  (js ${t1 - t0}ms, rs ${t2 - t1}ms)`);
    if (bad.length) diffs.push({ label, bad });
    else same++;
}
console.log(`cli-diff: ${same}/${total}`);
for (const d of diffs) {
    console.log(`\n  okcode ${d.label}`);
    for (const b of d.bad) console.log(`    ${b}`);
}
fs.rmSync(base, { recursive: true, force: true });
process.exit(diffs.length ? 1 : 0);
