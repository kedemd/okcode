#!/usr/bin/env node
'use strict';
// Idle cost of an open okcode: index a repo (or reopen its store), let it settle, then measure a
// window of doing nothing — whole-process CPU (all threads), context switches (≈ wakeups),
// threads, RSS. Run once per side; the native side via OKCODE_NATIVE=1 + the preload, as
// rs/tools/bench-cpu.sh does.
//
//   node rs/tools/idle.js --repo DIR --store DIR [--settle 10] [--window 60] [--bare]
// --bare: measure a bare node process (no okcode) for the baseline. Prints one JSON line.

const fs = require('fs');
const path = require('path');

const a = {};
for (let i = 2; i < process.argv.length; i++) if (process.argv[i].startsWith('--')) a[process.argv[i].slice(2)] = process.argv[i + 1]?.startsWith('--') ? true : process.argv[i + 1] ?? true;
const SETTLE = Number(a.settle || 10) * 1000;
const WINDOW = Number(a.window || 60) * 1000;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// per-thread wakeups (--threads): name → voluntary+involuntary context switches
function perThread() {
    const out = {};
    try {
        for (const t of fs.readdirSync('/proc/self/task')) {
            try {
                const st = fs.readFileSync(`/proc/self/task/${t}/status`, 'utf8');
                const name = /^Name:\s*(.*)$/m.exec(st)[1].trim();
                const n = Number(/^voluntary_ctxt_switches:\s*(\d+)/m.exec(st)[1]) + Number(/^nonvoluntary_ctxt_switches:\s*(\d+)/m.exec(st)[1]);
                out[`${name}#${t}`] = n;
            } catch {}
        }
    } catch {}
    return out;
}

function sample() {
    const u = process.resourceUsage();
    let threads = null;
    try {
        threads = fs.readdirSync('/proc/self/task').length;
    } catch {}
    return { cpuUs: u.userCPUTime + u.systemCPUTime, vol: u.voluntaryContextSwitches, invol: u.involuntaryContextSwitches, threads, rss: process.memoryUsage().rss };
}

(async () => {
    let oc = null;
    let db = null;
    if (a.bare === undefined) {
        const OKDB = require('@kedem/okdb');
        const okcode = require(path.join(__dirname, '..', '..', 'src', 'okcode'));
        db = new OKDB(a.store);
        db.log.detachConsole();
        await db.open();
        oc = await okcode.open({ db });
        await oc.addWorkspace('idle', { access: okcode.access.localFs(a.repo), fts: a.fts || 'full' });
    }
    await sleep(SETTLE);
    const t0 = a.threads ? perThread() : null;
    const s0 = sample();
    // keep the event loop alive without doing work: one timer for the whole window
    await sleep(WINDOW);
    const s1 = sample();
    const t1 = a.threads ? perThread() : null;
    const secs = WINDOW / 1000;
    const out = {
        side: a.bare !== undefined ? 'bare-node' : process.env.OKCODE_NATIVE === '1' ? 'rs' : 'js',
        windowS: secs,
        cpuMsPerMin: Math.round(((s1.cpuUs - s0.cpuUs) / 1000 / secs) * 60),
        wakeupsPerSec: Math.round(((s1.vol - s0.vol + s1.invol - s0.invol) / secs) * 10) / 10,
        threads: s1.threads,
        rssMB: Math.round(s1.rss / 1048576),
    };
    if (t1) {
        const by = {};
        for (const [k, v] of Object.entries(t1)) {
            const d = v - (t0[k] ?? 0);
            const name = k.split('#')[0];
            if (d > 0) by[name] = Math.round(((by[name] || 0) + d / secs) * 10) / 10;
        }
        out.wakeupsPerSecByThread = Object.fromEntries(Object.entries(by).sort((x, y) => y[1] - x[1]));
    }
    console.log(JSON.stringify(out));
    if (oc) await oc.close().catch(() => {});
    if (db) await db.close().catch(() => {});
    process.exit(0);
})().catch((e) => {
    console.error(e);
    process.exit(1);
});
