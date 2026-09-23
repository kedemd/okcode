'use strict';
// End to end on real parts: okcode on the working okdb, a REAL embedder (local
// Ollama), a local workspace and a "remote" one reached through the shell
// facade over real ssh (throwaway sshd — the test is the host supplying run).
// Only the public API and the model-facing tools are used.
//
// Skipped unless Ollama answers at OLLAMA_URL (default http://localhost:11434)
// with OKCODE_E2E_MODEL (default qwen3-embedding:latest). Slow by design
// (minutes): it embeds a few hundred chunks.
//
// The corpus is a COPY of okcode's own src/ (the "remote" workspace is a copy
// of just src/access) — self-contained, so the test runs the same against a
// linked, built or published okdb. Edits land on the copy, never on a real
// repo. The name/text/meaning targets below are okcode code with a stable
// purpose (identity hashing, the base64 shell transfer, the stale-edit check);
// if one moves, re-anchor the assertion rather than loosen it.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const okcode = require('../src/okcode');
const { createTools } = require('../src/tools');
const { startSshd } = require('./helpers/sshd');

const OLLAMA = process.env.OLLAMA_URL || 'http://localhost:11434';
const MODEL = process.env.OKCODE_E2E_MODEL || 'qwen3-embedding:latest';
const CORPUS = path.join(__dirname, '..', 'src');

async function ollamaReady() {
    try {
        const r = await fetch(`${OLLAMA}/api/tags`, { signal: AbortSignal.timeout(3000) });
        const { models = [] } = await r.json();
        return models.some((m) => m.name === MODEL || m.model === MODEL);
    } catch {
        return false;
    }
}

async function until(what, fn, ms) {
    const deadline = Date.now() + ms;
    let last;
    for (;;) {
        last = await fn();
        if (last.ok) return last;
        if (Date.now() > deadline) assert.fail(`${what} did not converge in ${ms / 1000}s: ${JSON.stringify(last)}`);
        await new Promise((r) => setTimeout(r, 1000));
    }
}

const embedderOf = async (oc, id) => (await oc.status(id)).workspaces[0].embedders[0];
const converged = (oc, id) => async () => {
    const e = await embedderOf(oc, id);
    return { ok: e.state === 'ready' && e.pending === 0 && e.done > 0, ...e };
};

describe('e2e: okcode + okdb + Ollama, local and over ssh', async () => {
    const skip = !(await ollamaReady()) && `Ollama with ${MODEL} not reachable at ${OLLAMA}`;
    let base, localRoot, remoteRoot, oc, sshd, tools;

    before(async () => {
        if (skip) return;
        base = fs.mkdtempSync(path.join(os.tmpdir(), 'okcode-e2e-'));
        localRoot = path.join(base, 'local');
        remoteRoot = path.join(base, 'remote');
        fs.cpSync(CORPUS, localRoot, { recursive: true });
        fs.cpSync(path.join(CORPUS, 'access'), remoteRoot, { recursive: true });
        sshd = await startSshd();
        oc = await okcode.open({
            path: path.join(base, 'store'),
            embedders: { qwen: { type: 'ollama', model: MODEL, url: OLLAMA } },
            active: 'qwen',
        });
        await oc.addWorkspace('local', { access: okcode.access.localFs(localRoot) });
        await oc.addWorkspace('remote', { access: okcode.access.shell({ root: remoteRoot, run: sshd.run }) });
        tools = createTools({
            workspace: async (hint) => oc.workspace(hint || 'local'),
            workspaces: () => oc.workspaces(),
        });
    });

    after(async () => {
        if (oc) await oc.close().catch(() => {});
        if (sshd) await sshd.stop().catch(() => {});
        if (base) fs.rmSync(base, { recursive: true, force: true });
    });

    it('indexes both workspaces with real embeddings', { skip, timeout: 15 * 60_000 }, async () => {
        for (const id of ['local', 'remote']) {
            const e = await until(`embeddings of ${id}`, converged(oc, id), 12 * 60_000);
            assert.equal(e.failed, 0, JSON.stringify(e));
            assert.equal(e.dims, 4096);
        }
        const st = (await oc.status()).workspaces;
        for (const w of st) assert.ok(w.files > 0 && w.symbols > 0, JSON.stringify(w));
    });

    it('finds by name, by text, and by meaning', { skip, timeout: 120_000 }, async (t) => {
        const local = oc.workspace('local');
        const byName = await local.find('findPipeline');
        assert.ok(
            byName.some((h) => /(^|\/)identity\.js$/.test(h.file || h.rel || '')),
            JSON.stringify(byName.slice(0, 3)),
        );

        const grep = await tools.render('code_grep', { workspace: 'local', text: 'restale' });
        assert.match(grep, /workspace\.js/);

        // Semantic: phrased the way a person asks, sharing few tokens with the code.
        const ask = async (q) => (await local.ask(q, { limit: 6 })).map((h) => h.file || h.rel);
        const expect = async (q, re, name) => {
            const files = await ask(q);
            t.diagnostic(`${name}: ${JSON.stringify(files)}`);
            assert.ok(
                files.some((f) => re.test(f)),
                `${name} among ${JSON.stringify(files)}`,
            );
        };
        await expect(
            'how do we know stored vectors came from the same model on the same server so they are never mixed',
            /(^|\/)identity\.js$/,
            'identity.js',
        );
        await expect(
            'how does file content survive being piped through a remote terminal without its characters getting mangled',
            /access\/shell\.js$/,
            'access/shell.js',
        );
        await expect(
            'refuse to overwrite a file that somebody changed after I last looked at it',
            /(^|\/)workspace\.js$/,
            'workspace.js',
        );
    });

    it('edits a remote file through the tools; a stale at is refused', { skip, timeout: 120_000 }, async () => {
        const read = await tools.render('code_read', { workspace: 'remote', symbol: 'shell.js' });
        const at = (read.match(/at=([0-9A-F]{40})/) || [])[1];
        assert.ok(at, read.slice(0, 400));
        const file = path.join(remoteRoot, 'shell.js');
        const before = fs.readFileSync(file, 'utf8');

        const edited = await tools.render('code_edit', {
            workspace: 'remote',
            file: 'shell.js',
            find: "'use strict';",
            body: "'use strict'; // e2e edit over ssh",
        });
        assert.match(edited, /applied/, edited);
        const after = fs.readFileSync(file, 'utf8');
        assert.notEqual(after, before);
        assert.match(after, /e2e edit over ssh/);

        // The at from before the edit is now stale.
        const stale = await tools.render('code_edit', {
            workspace: 'remote',
            edits: [{ target: 'shell.js:1-1', body: "'use strict'; // stale" }],
            at,
        });
        assert.match(stale, /stale|changed since/i, stale);
        assert.equal(fs.readFileSync(file, 'utf8'), after, 'a stale edit wrote nothing');
    });

    it('reset vectors re-embeds the remote workspace', { skip, timeout: 15 * 60_000 }, async () => {
        await oc.reset('remote', { scope: 'vectors' });
        const e = await until('re-embed of remote', converged(oc, 'remote'), 12 * 60_000);
        assert.equal(e.failed, 0);
        const hits = await oc
            .workspace('remote')
            .ask('send new file contents to the other machine and write them', { limit: 5 });
        assert.ok(hits.length > 0);
    });
});
