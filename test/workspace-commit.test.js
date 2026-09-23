'use strict';
// What an edit's receipt claims after the commit point, under injected faults.
// The facade is the real localFs with ONE step wrapped — the commit, or the
// readback that follows it — so a fault lands in exactly one step of a real
// editBatch() on a real fixture, rather than the thing being proven being
// mocked away. (Ported from the brain's fakeIndex() cases; on the async facade
// there is no script-sniffing needed to find the commit.)

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { openWorkspace, classifyCommit } = require('../src/workspace');
const { localFs } = require('../src/access');
const { writeFixture, tmpRoot } = require('./fixtures/code-fixture');

const bases = [];
after(() => bases.forEach((b) => fs.rmSync(b, { recursive: true, force: true })));

async function setup({ onCommit = null, onReadback = null, drop = [] } = {}) {
    const { base, root } = tmpRoot('commit');
    bases.push(base);
    writeFixture(root);
    const real = localFs(root);
    let committed = false;
    const access = { ...real };
    for (const m of drop) delete access[m];
    access.commit = async (p, bytes, o) => {
        if (p.endsWith('.okcode-lock')) return real.commit(p, bytes, o);
        committed = true;
        return onCommit ? onCommit(() => real.commit(p, bytes, o), p) : real.commit(p, bytes, o);
    };
    access.read = async (paths) =>
        committed && onReadback ? onReadback(() => real.read(paths), paths) : real.read(paths);
    const ws = await openWorkspace({ id: 'commit', access });
    const disk = (rel) => fs.readFileSync(path.join(root, rel), 'utf8');
    const write = (rel, t) => fs.writeFileSync(path.join(root, rel), t);
    return { ws, disk, write, real };
}

describe('commit outcomes', () => {
    it('a lost response where nothing was published is not_committed', async () => {
        const { ws, disk } = await setup({
            onCommit: () => {
                throw new Error('simulated: crashed before publishing');
            },
        });
        const before = disk('lib/math.js');
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.40;', { at: (await ws.read('TAX_RATE')).at });
        assert.equal(r.outcome, 'not_committed', JSON.stringify(r));
        assert.equal(r.ok, false);
        assert.equal(disk('lib/math.js'), before);
    });

    it('a lost response where the publish happened is committed_verified_with_warning', async () => {
        const { ws, disk } = await setup({
            onCommit: async (commit) => {
                await commit();
                throw new Error('simulated: crashed after publishing');
            },
        });
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.41;', { at: (await ws.read('TAX_RATE')).at });
        assert.equal(r.outcome, 'committed_verified_with_warning', JSON.stringify(r));
        assert.ok(disk('lib/math.js').includes('TAX_RATE = 0.41'));
        assert.ok((await ws.read('TAX_RATE')).body.includes('0.41'), 'the index follows what is on disk');
    });

    it('a lost response with unrelated content on disk is commit_outcome_unknown', async () => {
        const { ws, write } = await setup({
            onCommit: () => {
                write('lib/math.js', 'const NOBODY_AUTHORED_THIS = 1;\n');
                throw new Error('simulated: lost response');
            },
        });
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.42;', { at: (await ws.read('TAX_RATE')).at });
        assert.equal(r.outcome, 'commit_outcome_unknown', JSON.stringify(r));
    });

    it('a wrong self-reported hash with a matching readback is a warning, hashes kept separate', async () => {
        const { ws } = await setup({
            onCommit: async (commit) => ({ ...(await commit()), hash: '0'.repeat(40) }),
        });
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.43;', { at: (await ws.read('TAX_RATE')).at });
        assert.equal(r.outcome, 'committed_verified_with_warning');
        assert.equal(r.commitReportedHash, '0'.repeat(40));
        assert.equal(r.candidateHash, r.readbackHash);
    });

    it('a readback that disagrees after a real ok is diverged_after_commit', async () => {
        const { ws } = await setup({
            onReadback: async (_real, paths) => new Map(paths.map((p) => [p, Buffer.from('const ELSE = 1;\n')])),
        });
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.44;', { at: (await ws.read('TAX_RATE')).at });
        assert.equal(r.outcome, 'diverged_after_commit', JSON.stringify(r));
    });

    it('a readback that cannot be performed is committed_unverified', async () => {
        const { ws, disk } = await setup({
            onReadback: () => {
                throw new Error('simulated: readback unreachable');
            },
        });
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.45;', { at: (await ws.read('TAX_RATE')).at });
        assert.equal(r.outcome, 'committed_unverified', JSON.stringify(r));
        assert.ok(disk('lib/math.js').includes('0.45'));
    });

    it('a write racing in between verify and commit is refused as stale by the facade', async () => {
        const { ws, write, disk } = await setup({
            onCommit: (commit) => {
                write('lib/math.js', disk('lib/math.js').replace('0.17', '0.99'));
                return commit();
            },
        });
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.46;', { at: (await ws.read('TAX_RATE')).at });
        assert.ok(!r.ok && r.stale === true && /^[0-9A-F]{40}$/.test(r.at), JSON.stringify(r));
        assert.ok(disk('lib/math.js').includes('0.99'), "the racer's write survives");
    });
});

describe('optional capabilities', () => {
    it('a held lock refuses the edit and touches nothing', async () => {
        const { ws, disk, real } = await setup();
        const held = await real.lock('lib/math.js.okcode-lock', 'someone-else');
        assert.ok(held.ok);
        const before = disk('lib/math.js');
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.5;', { at: (await ws.read('TAX_RATE')).at });
        assert.ok(!r.ok && r.locked === true, JSON.stringify(r));
        assert.equal(disk('lib/math.js'), before);
        await held.release();
        const again = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.5;', { at: (await ws.read('TAX_RATE')).at });
        assert.ok(again.ok, JSON.stringify(again));
    });

    it('without lock the compare-and-swap alone guards; without exec syntaxCheck is unsupported', async () => {
        const { ws, disk } = await setup({ drop: ['lock', 'exec'] });
        const r = await ws.edit('TAX_RATE', 'const TAX_RATE = 0.6;', { at: (await ws.read('TAX_RATE')).at });
        assert.ok(r.ok, JSON.stringify(r));
        assert.ok(disk('lib/math.js').includes('0.6'));
        const chk = await ws.syntaxCheck('lib/math.js');
        assert.equal(chk.ok, false);
        assert.equal(chk.unsupported, true);
    });

    it('no lock file is left behind, and lock files never enter the graph', async () => {
        const { ws } = await setup();
        await ws.edit('TAX_RATE', 'const TAX_RATE = 0.7;', { at: (await ws.read('TAX_RATE')).at });
        await ws.sync();
        const s = await ws.structure();
        assert.ok(!s.dirs.some((d) => d.dir.includes('okcode')));
        assert.equal((await ws.readFile('lib/math.js.okcode-lock')).ok, false);
    });
});

describe('classifyCommit', () => {
    it('is a pure function of the report and the readback', () => {
        const ok = { outcome: 'ok', hash: 'C' };
        const lost = { outcome: 'response-lost' };
        const base = { candidateHash: 'C', beforeHash: 'B' };
        assert.equal(classifyCommit({ ...base, commit: ok, readbackHash: 'C', readAvailable: true }), 'ok');
        assert.equal(
            classifyCommit({ ...base, commit: ok, readbackHash: null, readAvailable: false }),
            'committed_unverified',
        );
        assert.equal(
            classifyCommit({ ...base, commit: lost, readbackHash: null, readAvailable: false }),
            'commit_outcome_unknown',
        );
        assert.equal(
            classifyCommit({ ...base, commit: lost, readbackHash: 'B', readAvailable: true }),
            'not_committed',
        );
        assert.equal(
            classifyCommit({ ...base, commit: ok, readbackHash: 'X', readAvailable: true }),
            'diverged_after_commit',
        );
    });
});
