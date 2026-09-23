'use strict';
// Shell-facade specifics the shared contract cannot see: static scripts, one
// round trip per batch, transport failures classified, quoting.

const { it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');
const { shell } = require('../src/access');

const sha1 = (b) => crypto.createHash('sha1').update(b).digest('hex').toUpperCase();
const local = shell.localRunner('bash');

function workspace(files = {}) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-shell-'));
    for (const [p, c] of Object.entries(files)) {
        fs.mkdirSync(path.dirname(path.join(root, p)), { recursive: true });
        fs.writeFileSync(path.join(root, p), c);
    }
    return root;
}

// A run() that records every script and passes it to the real local bash.
function recording() {
    const calls = [];
    const run = (script, stdin) => {
        calls.push({ script, stdin });
        return local(script, stdin);
    };
    return { calls, run };
}

it('scripts are static per method: no root, path or payload is interpolated', async () => {
    const seen = new Map();
    for (const name of ['a b', "it's"]) {
        const root = workspace({ [`${name}/x.js`]: 'x' });
        const { calls, run } = recording();
        const f = shell({ root, run });
        await f.list();
        await f.stat([`${name}/x.js`], { hash: true });
        await f.read([`${name}/x.js`]);
        await f.commit(`${name}/y.js`, Buffer.from('PAYLOAD-MARKER'), { create: true });
        await f.remove([`${name}/y.js`]);
        await f.exec('echo hi');
        const l = await f.lock('z.lock', 'op');
        await l.release();
        for (const c of calls) {
            assert.ok(!c.script.includes(root), 'root never appears in a script');
            assert.ok(!c.script.includes(name), 'paths never appear in a script');
            assert.ok(!c.script.includes('PAYLOAD'), 'payloads never appear in a script');
            const k = c.script;
            seen.set(k, (seen.get(k) || 0) + 1);
        }
        fs.rmSync(root, { recursive: true, force: true });
    }
    // Two workspaces with different roots/paths produced the same set of scripts.
    for (const count of seen.values()) assert.equal(count % 2, 0);
});

it('batched: stat, read and remove cost one run() regardless of path count (1201 paths, past the 500-arg slices)', async () => {
    const files = Object.fromEntries(Array.from({ length: 1201 }, (_, i) => [`d/f${i}.js`, `file ${i}`]));
    const root = workspace(files);
    const { calls, run } = recording();
    const f = shell({ root, run });
    const paths = Object.keys(files);

    const st = await f.stat(paths, { hash: true });
    assert.equal(calls.length, 1);
    assert.equal(st.get('d/f7.js').hash, sha1('file 7'));
    const rd = await f.read(paths);
    assert.equal(calls.length, 2);
    assert.equal(rd.size, 1201);
    assert.equal(rd.get('d/f1200.js').toString(), 'file 1200');
    const ls = await f.list();
    assert.equal(calls.length, 3);
    assert.equal(ls.length, 1201);
    await f.remove(paths);
    assert.equal(calls.length, 4);
    assert.deepEqual(fs.readdirSync(path.join(root, 'd')), []);
    fs.rmSync(root, { recursive: true, force: true });
});

it('read: a batch whose sizes do not add up is re-read per file, never mis-split', async () => {
    const root = workspace({ 'a.js': 'alpha', 'b.js': 'bravo', 'c.bin': Buffer.from([0, 1, 2, 255]) });
    const calls = [];
    // Simulate a file growing between the find and the cat: one extra byte in
    // the concatenated data.
    const run = async (script, stdin) => {
        calls.push(script);
        const out = await local(script, stdin);
        if (!out.includes('@DATA')) return out;
        const [head, data] = out.split('@DATA\n');
        const bytes = Buffer.concat([Buffer.from(data.replace(/\s+/g, ''), 'base64'), Buffer.from('!')]);
        return `${head}@DATA\n${bytes.toString('base64')}\n`;
    };
    const f = shell({ root, run });
    const got = await f.read(['a.js', 'nope', 'b.js', 'c.bin']);
    assert.equal(calls.length, 2);
    assert.deepEqual([...got.keys()], ['a.js', 'b.js', 'c.bin']);
    assert.equal(got.get('a.js').toString(), 'alpha');
    assert.equal(got.get('b.js').toString(), 'bravo');
    assert.deepEqual([...got.get('c.bin')], [0, 1, 2, 255]);
    fs.rmSync(root, { recursive: true, force: true });
});

it('commit: a transport failure is response-lost, never a refusal', async () => {
    const root = workspace({ 'a.js': 'a' });
    const f = shell({
        root,
        run: async () => {
            throw new Error('simulated: connection dropped after send');
        },
    });
    const r = await f.commit('a.js', Buffer.from('b'), { expectedHash: sha1('a') });
    assert.equal(r.outcome, 'response-lost');
    assert.match(r.error, /connection dropped/);

    const garbled = shell({ root, run: async () => 'what?\n' });
    const g = await garbled.commit('a.js', Buffer.from('b'));
    assert.equal(g.outcome, 'response-lost');
    fs.rmSync(root, { recursive: true, force: true });
});

it('commit: a payload mangled in transit is caught on the target and nothing is published', async () => {
    const root = workspace({ 'a.js': 'original' });
    // Drop the last base64 quantum of the payload on the way to the target.
    const clip = (script, stdin) => local(script, script.includes('CORRUPT') ? stdin.slice(0, -4) : stdin);
    const f = shell({ root, run: clip });
    const r = await f.commit('a.js', Buffer.from('a replacement long enough to clip'), {
        expectedHash: sha1('original'),
    });
    assert.equal(r.outcome, 'response-lost');
    assert.match(r.error, /corrupted/);
    assert.equal(fs.readFileSync(path.join(root, 'a.js'), 'utf8'), 'original');
    assert.deepEqual(
        fs.readdirSync(root).filter((n) => n.startsWith('.okcode-')),
        [],
    );
    fs.rmSync(root, { recursive: true, force: true });
});

it('a missing root rejects reads rather than answering "missing"', async () => {
    const f = shell({ root: path.join(os.tmpdir(), 'okcode-no-such-root-' + process.pid), run: local });
    await assert.rejects(() => f.stat(['a.js']), /workspace root not found/);
    await assert.rejects(() => f.list(), /workspace root not found/);
});

it('lock: a lock recorded for another host is never broken, even if the pid is dead there', async () => {
    const root = workspace();
    // A target that reports a different hostname than this caller's.
    const run = (script, stdin) =>
        script.includes('uname -n') ? local(script.replace('$(uname -n)', 'elsewhere'), stdin) : local(script, stdin);
    const f = shell({ root, run });
    fs.writeFileSync(
        path.join(root, 'x.lock'),
        JSON.stringify({ pid: spawnSync(process.execPath, ['-e', '']).pid, host: os.hostname(), id: 'old' }),
    );
    assert.deepEqual(await f.lock('x.lock', 'new'), { ok: false, reason: 'locked' });
    fs.rmSync(root, { recursive: true, force: true });
});

it('quote: POSIX single-quote escaping survives a round trip through sh', () => {
    for (const s of ['plain', "it's", "'", "''", 'a"b$c`d\\e', 'new\nline', '— unicode']) {
        const out = spawnSync('sh', ['-c', `printf '%s' ${shell.quote(s)}`], { encoding: 'utf8' }).stdout;
        assert.equal(out, s);
    }
});

it('rejects an unknown dialect and a missing run', () => {
    assert.throws(() => shell({ root: '/tmp', run: local, dialect: 'fish' }), /unknown shell dialect/);
    assert.throws(() => shell({ root: '/tmp' }), /run/);
});
