#!/usr/bin/env node
'use strict';
// One subprocess, one phase, one number. bench/compare.sh drives this as a
// child of `/usr/bin/time -f %M` so each MEASURED phase (a cold index, a
// warm reopen, an embed wait) is its own process with its own peak-RSS
// reading — mixing phases into one process would let a later phase's drop
// in usage hide an earlier phase's peak.
//
// Usage:
//   compare.js cold  --repo DIR --store DIR [--fts full|symbols] [--embedder type:model@url]
//   compare.js warm  --repo DIR --store DIR
//   compare.js embed --store DIR --embedder type:model@url     (poll until done; prints embedMs)
// Every subcommand prints ONE line of JSON to stdout and nothing else there
// (stderr carries okdb/okcode's own logging, same as the CLI).

const path = require('path');
const OKDB = require('@kedem/okdb');
const okcode = require(path.join(__dirname, '..', 'src', 'okcode'));
const { localFs } = okcode.access;

// okdb's own console logging defaults to stdout, same as the CLI — and
// stdout here is the one JSON line a caller parses, so it is detached the
// same way bin/okcode.js does it.
async function openStore(storePath) {
    const db = new OKDB(storePath);
    db.log.detachConsole();
    await db.open();
    return db;
}

function parseArgs(argv) {
    const out = {};
    for (let i = 0; i < argv.length; i++) {
        const a = argv[i];
        if (a.startsWith('--')) out[a.slice(2)] = argv[i + 1];
    }
    return out;
}

function parseEmbedder(spec) {
    // type:model@url — '@' separates the url (which has its OWN ':'s, as in
    // http://host:port) from type:model (a plain split on ':' alone mis-cut
    // "ollama:qwen3-embedding:latest:http://host:11434" because the MODEL
    // name itself contains a ':').
    const at = String(spec).indexOf('@');
    const head = at >= 0 ? spec.slice(0, at) : spec;
    const url = at >= 0 ? spec.slice(at + 1) : undefined;
    const i = head.indexOf(':');
    const type = i >= 0 ? head.slice(0, i) : head;
    const model = i >= 0 ? head.slice(i + 1) : undefined;
    return { bench: { type, model, ...(url ? { url } : {}) } };
}

async function waitEmbedded(oc, wsId, { timeoutMs = 30 * 60 * 1000, pollMs = 1500 } = {}) {
    const t0 = Date.now();
    for (;;) {
        const s = await oc.status(wsId);
        const embedders = (s.workspaces[0] || {}).embedders || [];
        const pending = embedders.reduce((n, e) => n + (e.pending || 0), 0);
        const failed = embedders.reduce((n, e) => n + (e.failed || 0), 0);
        const done = embedders.reduce((n, e) => n + (e.done || 0), 0);
        if (embedders.length && pending === 0 && (done > 0 || failed > 0)) return { ms: Date.now() - t0, done, failed };
        if (Date.now() - t0 > timeoutMs) throw new Error(`embedding did not finish within ${timeoutMs}ms (pending=${pending})`);
        await new Promise((r) => setTimeout(r, pollMs));
    }
}

async function cold(a) {
    const fts = a.fts === 'symbols' ? 'symbols' : 'full';
    const embedders = a.embedder ? parseEmbedder(a.embedder) : undefined;
    const db = await openStore(a.store);
    const oc = await okcode.open({ db, ...(embedders ? { embedders, active: 'bench' } : {}) });
    const t0 = Date.now();
    await oc.addWorkspace('bench', { access: localFs(a.repo), fts });
    const indexMs = Date.now() - t0;
    let embedMs = null;
    if (embedders) {
        const r = await waitEmbedded(oc, 'bench');
        embedMs = r.ms;
    }
    const status = await oc.status('bench');
    await oc.close();
    await db.close();
    console.log(JSON.stringify({ phase: 'cold', fts, indexMs, embedMs, status: status.workspaces[0] }));
}

async function warm(a) {
    // Must match the `cold` run's own fts mode: addWorkspace()'s fts
    // defaults to 'full' when not given, so a warm reopen of a 'symbols'
    // workspace that omits it would silently CONVERGE it back to 'full' —
    // building the whole content index on what was supposed to be a cheap
    // reopen, not just measuring it wrong.
    const fts = a.fts === 'symbols' ? 'symbols' : 'full';
    const db = await openStore(a.store);
    const oc = await okcode.open({ db });
    const t0 = Date.now();
    await oc.addWorkspace('bench', { access: localFs(a.repo), fts });
    const warmMs = Date.now() - t0;
    await oc.close();
    await db.close();
    console.log(JSON.stringify({ phase: 'warm', warmMs }));
}

const [cmd, ...rest] = process.argv.slice(2);
const a = parseArgs(rest);
const fns = { cold, warm };
if (!fns[cmd]) {
    console.error(`usage: compare.js <cold|warm> --repo DIR --store DIR [--fts full|symbols] [--embedder type:model@url]`);
    process.exit(1);
}
fns[cmd](a).catch((err) => {
    console.error(err.stack || err.message);
    process.exit(1);
});
