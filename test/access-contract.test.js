'use strict';
// ONE contract suite (docs/ACCESS.md), run against every shipped facade:
// localFs, shell over a local bash, shell over ssh to a throwaway sshd, and
// shell over PowerShell where one is installed.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');
const { localFs, shell } = require('../src/access');
const { startSshd, sshdAvailable } = require('./helpers/sshd');

const isRoot = typeof process.getuid === 'function' && process.getuid() === 0;
const sha1 = (b) => crypto.createHash('sha1').update(b).digest('hex').toUpperCase();
const hasBin = (b) => spawnSync('sh', ['-c', `command -v ${b}`], { stdio: 'ignore' }).status === 0;
const pwshBin = process.platform !== 'win32' && hasBin('pwsh') ? 'pwsh' : null;

const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-access-'));
after(() => fs.rmSync(TMP, { recursive: true, force: true }));

// A fresh workspace per test. The root is itself a DOT directory on purpose:
// pruning must be anchored below the root, never match the root's own name.
let seq = 0;
function workspace(files = {}) {
    const root = path.join(TMP, `w${++seq}`, '.ws-root');
    fs.mkdirSync(root, { recursive: true });
    const abs = (p) => path.join(root, ...p.split('/'));
    const write = (p, content) => {
        fs.mkdirSync(path.dirname(abs(p)), { recursive: true });
        fs.writeFileSync(abs(p), content);
    };
    for (const [p, c] of Object.entries(files)) write(p, c);
    return { root, abs, write };
}

function walkFiles(dir) {
    const out = [];
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, e.name);
        if (e.isDirectory()) out.push(...walkFiles(full));
        else out.push(full);
    }
    return out;
}

function deadPid() {
    const r = spawnSync(process.execPath, ['-e', '']);
    return r.pid;
}

function contract(make) {
    it('list: nested files, skip dirs at any depth, dot-dirs pruned, unreadable skipped', async () => {
        const w = workspace({
            'a.js': 'a',
            '.env': 'dotfile at top level is a file, listed',
            'src/b.js': 'bb',
            'src/deep/c.js': 'ccc',
            'src/dist': 'a FILE named like a skip dir is still listed',
            'node_modules/x.js': 'x',
            'src/node_modules/y.js': 'y',
            'build/z.js': 'z',
            '.git/HEAD': 'ref',
            'src/.cache/d.js': 'd',
            'locked.txt': 'secret',
        });
        fs.symlinkSync('a.js', w.abs('link.js'));
        fs.chmodSync(w.abs('locked.txt'), 0o000);
        const f = make(w.root);
        const rows = await f.list();
        const byPath = new Map(rows.map((r) => [r.path, r]));
        const expected = ['.env', 'a.js', 'src/b.js', 'src/deep/c.js', 'src/dist'];
        if (isRoot) expected.push('locked.txt');
        assert.deepEqual([...byPath.keys()].sort(), expected.sort());
        assert.equal(byPath.get('src/deep/c.js').size, 3);
        for (const r of rows) assert.equal(typeof r.mtime, 'number');
        assert.ok(!('hash' in byPath.get('a.js')));

        const custom = (await f.list({ skip: ['src'] })).map((r) => r.path).sort();
        assert.ok(custom.includes('node_modules/x.js'));
        assert.ok(custom.includes('build/z.js'));
        assert.ok(!custom.some((p) => p.startsWith('src/')));
        fs.chmodSync(w.abs('locked.txt'), 0o644);
    });

    it('stat: size/mtime, optional hash, missing paths and directories', async () => {
        const w = workspace({ 'a.js': 'hello', 'dir/b.js': 'world!' });
        fs.symlinkSync('a.js', w.abs('link.js'));
        fs.symlinkSync('nowhere.js', w.abs('broken.js'));
        const f = make(w.root);
        const plain = await f.stat(['a.js', 'dir/b.js', 'nope.js', 'dir', 'broken.js', 'link.js']);
        assert.deepEqual([...plain.keys()], ['a.js', 'dir/b.js', 'nope.js', 'dir', 'broken.js', 'link.js']);
        assert.equal(plain.get('a.js').size, 5);
        assert.equal(typeof plain.get('a.js').mtime, 'number');
        assert.ok(!('hash' in plain.get('a.js')));
        assert.deepEqual(plain.get('nope.js'), { missing: true });
        assert.deepEqual(plain.get('dir'), { missing: true });
        assert.deepEqual(plain.get('broken.js'), { missing: true });
        assert.equal(plain.get('link.js').size, 5, 'a link to a regular file is followed');

        const hashed = await f.stat(['a.js', 'dir/b.js', 'nope.js'], { hash: true });
        assert.equal(hashed.get('a.js').hash, sha1('hello'));
        assert.equal(hashed.get('dir/b.js').hash, sha1('world!'));
        assert.deepEqual(hashed.get('nope.js'), { missing: true });

        // Parity: the same file hashes identically through the reference facade.
        const ref = await localFs(w.root).stat(['a.js'], { hash: true });
        assert.equal(hashed.get('a.js').hash, ref.get('a.js').hash);
        assert.equal(await f.stat([]).then((m) => m.size), 0);
    });

    it('stat: an unreadable file cannot be hashed and reports missing', { skip: isRoot }, async () => {
        const w = workspace({ 'locked.txt': 'secret' });
        fs.chmodSync(w.abs('locked.txt'), 0o000);
        const f = make(w.root);
        const got = await f.stat(['locked.txt'], { hash: true });
        assert.deepEqual(got.get('locked.txt'), { missing: true });
        fs.chmodSync(w.abs('locked.txt'), 0o644);
    });

    it('mtime: stable across calls, equal between list and stat, moves when the file does', async () => {
        const w = workspace({ 'a.js': 'x', 'src/b.js': 'y' });
        const f = make(w.root);
        const s1 = await f.stat(['a.js', 'src/b.js']);
        const s2 = await f.stat(['a.js', 'src/b.js'], { hash: true });
        const l1 = new Map((await f.list()).map((r) => [r.path, r]));
        const l2 = new Map((await f.list()).map((r) => [r.path, r]));
        for (const p of ['a.js', 'src/b.js']) {
            assert.equal(s1.get(p).mtime, s2.get(p).mtime);
            assert.equal(l1.get(p).mtime, l2.get(p).mtime);
            assert.equal(l1.get(p).mtime, s1.get(p).mtime, 'list and stat agree for an unchanged file');
        }
        fs.utimesSync(w.abs('a.js'), new Date(2001, 1, 1), new Date(2001, 1, 1, 0, 0, 1, 500));
        const s3 = await f.stat(['a.js']);
        assert.notEqual(s3.get('a.js').mtime, s1.get('a.js').mtime);
    });

    it('read: exact bytes (binary, non-UTF-8, em dash), missing/dirs/unreadable omitted', async () => {
        const all = Buffer.from(Array.from({ length: 256 }, (_, i) => i));
        const binary = Buffer.concat([all, Buffer.from([0xff, 0xfe, 0x00, 0x0a, 0x0d, 0x0a, 0x80]), all]);
        const text = Buffer.from('const dash = "\u2014"; // em dash\r\nnext line\n', 'utf8');
        const w = workspace({ 'bin.dat': binary, 'text.js': text, 'empty.txt': '', 'dir/x.js': 'x' });
        if (!isRoot) {
            w.write('locked.txt', 'secret');
            fs.chmodSync(w.abs('locked.txt'), 0o000);
        }
        const f = make(w.root);
        const got = await f.read([
            'text.js',
            'nope.js',
            'bin.dat',
            'dir',
            'empty.txt',
            ...(isRoot ? [] : ['locked.txt']),
        ]);
        assert.deepEqual([...got.keys()], ['text.js', 'bin.dat', 'empty.txt']);
        assert.ok(got.get('bin.dat').equals(binary));
        assert.ok(got.get('text.js').equals(text));
        assert.equal(got.get('empty.txt').length, 0);
        assert.equal((await f.read([])).size, 0);
        if (!isRoot) fs.chmodSync(w.abs('locked.txt'), 0o644);
    });

    it('commit create: ok with hash, parent dirs created, refuses an existing path', async () => {
        const w = workspace({ 'old.js': 'incumbent' });
        const f = make(w.root);
        const bytes = Buffer.from('fresh \u2014 content\n');
        const r = await f.commit('new/deeper/file.js', bytes, { create: true });
        assert.deepEqual(r, { outcome: 'ok', hash: sha1(bytes) });
        assert.ok(fs.readFileSync(w.abs('new/deeper/file.js')).equals(bytes));

        const again = await f.commit('new/deeper/file.js', Buffer.from('loser'), { create: true });
        assert.equal(again.outcome, 'exists');
        assert.ok(fs.readFileSync(w.abs('new/deeper/file.js')).equals(bytes), 'the winner is untouched');
        assert.equal((await f.commit('old.js', Buffer.from('x'), { create: true })).outcome, 'exists');
        assert.equal(fs.readFileSync(w.abs('old.js'), 'utf8'), 'incumbent');

        const empty = await f.commit('empty.txt', Buffer.alloc(0), { create: true });
        assert.deepEqual(empty, { outcome: 'ok', hash: sha1(Buffer.alloc(0)) });
        assert.equal(fs.statSync(w.abs('empty.txt')).size, 0);
    });

    it('commit replace: ok, stale with current, missing, symlink refused, mode preserved', async () => {
        const w = workspace({ 'a.js': 'v1', 'tool.sh': '#!/bin/sh\necho 1\n', 'real.js': 'real' });
        fs.chmodSync(w.abs('tool.sh'), 0o754);
        fs.symlinkSync('real.js', w.abs('link.js'));
        const f = make(w.root);

        const ok = await f.commit('a.js', Buffer.from('v2'), { expectedHash: sha1('v1') });
        assert.deepEqual(ok, { outcome: 'ok', hash: sha1('v2') });
        assert.equal(fs.readFileSync(w.abs('a.js'), 'utf8'), 'v2');

        const stale = await f.commit('a.js', Buffer.from('v3'), { expectedHash: sha1('v1') });
        assert.deepEqual(stale, { outcome: 'stale', current: sha1('v2') });
        assert.equal(fs.readFileSync(w.abs('a.js'), 'utf8'), 'v2');

        const lower = await f.commit('a.js', Buffer.from('v3'), { expectedHash: sha1('v2').toLowerCase() });
        assert.equal(lower.outcome, 'ok', 'expectedHash is case-insensitive');

        const blind = await f.commit('a.js', Buffer.from('v4'));
        assert.deepEqual(blind, { outcome: 'ok', hash: sha1('v4') }, 'no expectedHash = unconditional replace');

        assert.deepEqual(await f.commit('gone.js', Buffer.from('x'), { expectedHash: sha1('x') }), {
            outcome: 'missing',
        });
        assert.equal(fs.existsSync(w.abs('gone.js')), false);
        assert.equal(
            (await f.commit('.', Buffer.from('x')).catch(() => ({ outcome: 'rejected' }))).outcome,
            'rejected',
        );

        const viaLink = await f.commit('link.js', Buffer.from('hijack'), { expectedHash: sha1('real') });
        assert.deepEqual(viaLink, { outcome: 'symlink' });
        assert.equal((await f.commit('link.js', Buffer.from('hijack'), { create: true })).outcome, 'symlink');
        assert.equal(fs.readFileSync(w.abs('real.js'), 'utf8'), 'real');
        assert.ok(fs.lstatSync(w.abs('link.js')).isSymbolicLink());

        const script = Buffer.from('#!/bin/sh\necho 2\n');
        assert.equal((await f.commit('tool.sh', script, { expectedHash: sha1('#!/bin/sh\necho 1\n') })).outcome, 'ok');
        assert.equal(fs.statSync(w.abs('tool.sh')).mode & 0o7777, 0o754);
        assert.ok(fs.readFileSync(w.abs('tool.sh')).equals(script));

        // No temp file survives any of the paths above.
        const leftovers = walkFiles(w.root).filter((p) => path.basename(p).startsWith('.okcode-'));
        assert.deepEqual(leftovers, []);
    });

    it('rejects paths that escape the root', async () => {
        const w = workspace({ 'a.js': 'a' });
        fs.writeFileSync(path.join(path.dirname(w.root), 'outside.txt'), 'outside');
        const f = make(w.root);
        for (const bad of ['../outside.txt', '/etc/passwd', 'a/../../outside.txt', '..', '', 'C:/x', '..\\x']) {
            await assert.rejects(() => f.stat([bad]), undefined, `stat ${JSON.stringify(bad)}`);
            await assert.rejects(() => f.read(['a.js', bad]), undefined, `read ${JSON.stringify(bad)}`);
            await assert.rejects(() => f.commit(bad, Buffer.from('x'), { create: true }), undefined, `commit ${bad}`);
            await assert.rejects(() => f.remove([bad]), undefined, `remove ${JSON.stringify(bad)}`);
        }
        await assert.rejects(() => f.lock('../x.lock', 'op'));
        assert.equal(fs.readFileSync(path.join(path.dirname(w.root), 'outside.txt'), 'utf8'), 'outside');
    });

    it('paths with spaces, quotes, shell metacharacters, unicode, a leading dash, a newline', async () => {
        const names = [
            'sp ace/it\'s "q" $HOME `id` ;x.js',
            'ünï — cödé/файл.js',
            '-dash.js',
            'new\nline.js',
            'glob*?[ab].js',
        ];
        const w = workspace();
        const f = make(w.root);
        for (const n of names) {
            const r = await f.commit(n, Buffer.from(`content of ${n}`), { create: true });
            assert.equal(r.outcome, 'ok', `${JSON.stringify(n)}: ${JSON.stringify(r)}`);
            assert.equal(fs.readFileSync(w.abs(n), 'utf8'), `content of ${n}`);
        }
        const listed = (await f.list()).map((r) => r.path).sort();
        assert.deepEqual(listed, [...names].sort());
        const st = await f.stat(names, { hash: true });
        const rd = await f.read(names);
        for (const n of names) {
            assert.equal(st.get(n).hash, sha1(`content of ${n}`), n);
            assert.equal(rd.get(n).toString('utf8'), `content of ${n}`, n);
        }
        const rep = await f.commit(names[0], Buffer.from('v2'), { expectedHash: sha1(`content of ${names[0]}`) });
        assert.equal(rep.outcome, 'ok');
        await f.remove(names);
        for (const n of names) assert.equal(fs.existsSync(w.abs(n)), false, n);
    });

    it('a large file (3 MB) round-trips with no command-line limits', async () => {
        const w = workspace();
        const f = make(w.root);
        const big = crypto.randomBytes(3 * 1024 * 1024);
        const r = await f.commit('big/blob.bin', big, { create: true });
        assert.deepEqual(r, { outcome: 'ok', hash: sha1(big) });
        const got = await f.read(['big/blob.bin']);
        assert.ok(got.get('big/blob.bin').equals(big));
        assert.equal((await f.stat(['big/blob.bin'], { hash: true })).get('big/blob.bin').hash, sha1(big));
        const big2 = crypto.randomBytes(2 * 1024 * 1024 + 7);
        const r2 = await f.commit('big/blob.bin', big2, { expectedHash: sha1(big) });
        assert.deepEqual(r2, { outcome: 'ok', hash: sha1(big2) });
        assert.ok(fs.readFileSync(w.abs('big/blob.bin')).equals(big2));
    });

    it('lock: acquire, contend, release, break only a provably dead same-host owner', async () => {
        const w = workspace();
        const f = make(w.root);
        const held = await f.lock('.okcode/edit.lock', 'op-1');
        assert.equal(held.ok, true);
        const meta = JSON.parse(fs.readFileSync(w.abs('.okcode/edit.lock'), 'utf8'));
        assert.equal(meta.pid, process.pid);
        assert.equal(meta.host, os.hostname());
        assert.equal(meta.id, 'op-1');

        assert.deepEqual(await f.lock('.okcode/edit.lock', 'op-2'), { ok: false, reason: 'locked' });
        await held.release();
        assert.equal(fs.existsSync(w.abs('.okcode/edit.lock')), false);
        const again = await f.lock('.okcode/edit.lock', 'op-3');
        assert.equal(again.ok, true);
        await again.release();

        const lockAbs = w.abs('.okcode/edit.lock');
        // Dead pid, same host: broken and re-acquired.
        fs.writeFileSync(lockAbs, JSON.stringify({ pid: deadPid(), host: os.hostname(), startTime: null, id: 'x' }));
        const broke = await f.lock('.okcode/edit.lock', 'op-4');
        assert.equal(broke.ok, true);
        assert.equal(JSON.parse(fs.readFileSync(lockAbs, 'utf8')).id, 'op-4');
        await broke.release();

        // Live pid whose start time no longer matches: a reused pid, broken.
        if (fs.existsSync('/proc/self/stat')) {
            fs.writeFileSync(
                lockAbs,
                JSON.stringify({ pid: process.pid, host: os.hostname(), startTime: '1', id: 'x' }),
            );
            const reused = await f.lock('.okcode/edit.lock', 'op-5');
            assert.equal(reused.ok, true);
            await reused.release();
        }

        // Different host: never broken, whatever the pid.
        fs.writeFileSync(lockAbs, JSON.stringify({ pid: deadPid(), host: 'another-machine.invalid', id: 'x' }));
        assert.deepEqual(await f.lock('.okcode/edit.lock', 'op-6'), { ok: false, reason: 'locked' });
        // Unparseable: cannot prove it is safe to break.
        fs.writeFileSync(lockAbs, 'not json');
        assert.deepEqual(await f.lock('.okcode/edit.lock', 'op-7'), { ok: false, reason: 'locked' });
        assert.equal(fs.readFileSync(lockAbs, 'utf8'), 'not json');
    });

    it('exec: runs in the root; ok → stdout, failure → stdout+stderr', async () => {
        const w = workspace({ 'marker.txt': 'here' });
        const f = make(w.root);
        assert.deepEqual(await f.exec('cat marker.txt; echo " ok"'), { ok: true, out: 'here ok\n' });
        const bad = await f.exec('echo to-out; echo to-err >&2; exit 3');
        assert.equal(bad.ok, false);
        assert.match(bad.out, /to-out/);
        assert.match(bad.out, /to-err/);
        const quoted = await f.exec(`printf '%s\\n' "it's \u2014 fine"`);
        assert.deepEqual(quoted, { ok: true, out: "it's \u2014 fine\n" });
    });

    it('remove: deletes files, ignores missing ones and directories', async () => {
        const w = workspace({ 'a.js': 'a', 'd/b.js': 'b', 'keep.js': 'k' });
        const f = make(w.root);
        await f.remove(['a.js', 'd/b.js', 'nope.js', 'd']);
        assert.equal(fs.existsSync(w.abs('a.js')), false);
        assert.equal(fs.existsSync(w.abs('d/b.js')), false);
        assert.equal(fs.existsSync(w.abs('d')), true);
        assert.equal(fs.existsSync(w.abs('keep.js')), true);
        await f.remove([]);
    });
}

describe('localFs', () => {
    contract((root) => localFs(root));
});

describe('shell (bash, local runner)', () => {
    const run = shell.localRunner('bash');
    contract((root) => shell({ root, run }));
});

describe('shell (bash over ssh)', { skip: sshdAvailable() ? false : '/usr/sbin/sshd not available' }, () => {
    let sshd = null;
    before(async () => {
        sshd = await startSshd();
    });
    after(async () => {
        if (sshd) await sshd.stop();
    });
    contract((root) => shell({ root, run: (script, stdin) => sshd.run(script, stdin) }));
});

describe('shell (powershell, local runner)', { skip: pwshBin ? false : 'pwsh not installed' }, () => {
    // Only reached where PowerShell exists. Some POSIX-specific assertions
    // (mode bits, newline filenames) may not hold for .NET on every platform.
    const run = pwshBin ? shell.localRunner('powershell') : null;
    contract((root) => shell({ root, run, dialect: 'powershell' }));
});
