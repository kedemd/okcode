#!/usr/bin/env node
'use strict';
// One-time quality check for configs D/E (bench/README.md): hit@3 / hit@10
// for ws.ask() (fused lexical + semantic) against a find()+grep-only
// baseline (no embedder at all — the question's own words, lexically), over
// bench/questions-frontend.json. Run against a store an earlier `cold` (or
// `warm`) compare.js run already populated and embedded.
//
// usage: node bench/quality.js --store DIR --questions bench/questions-frontend.json

const path = require('path');
const fs = require('fs');
const OKDB = require('@kedem/okdb');
const okcode = require(path.join(__dirname, '..', 'src', 'okcode'));
const { localFs } = okcode.access;

function parseArgs(argv) {
    const out = {};
    for (let i = 0; i < argv.length; i++) if (argv[i].startsWith('--')) out[argv[i].slice(2)] = argv[i + 1];
    return out;
}

function hitAt(files, answer, k) {
    return files.slice(0, k).includes(answer);
}

async function main() {
    const a = parseArgs(process.argv.slice(2));
    if (!a.store || !a.questions) {
        console.error('usage: quality.js --store DIR --questions FILE.json [--repo DIR]');
        process.exit(1);
    }
    const questions = JSON.parse(fs.readFileSync(a.questions, 'utf8'));

    const db = new OKDB(a.store);
    db.log.detachConsole();
    await db.open();
    const oc = await okcode.open({ db });
    // The store already has the workspace registered (an earlier `cold`
    // run added it); re-add it here with its access facade so this process
    // can query it — a repo path is needed only because the access facade
    // is code, never persisted (DESIGN §9).
    const ws = await oc.addWorkspace('bench', { access: localFs(a.repo || process.cwd()) });

    const rows = [];
    for (const { question, file } of questions) {
        const askHits = await ws.ask(question, { limit: 10 }).catch((err) => {
            console.error(`ask("${question}") failed: ${err.message}`);
            return [];
        });
        const askFiles = askHits.map((h) => h.file);

        // Baseline: find() over the question's own words (names/doc/content
        // — no embedder involved at all), then code_grep-shaped literal
        // fallback isn't meaningful for a natural-language question, so the
        // baseline here is find() alone, the honest "no semantic eye" case.
        const findHits = await ws.find(question, { limit: 10 }).catch(() => []);
        const findFiles = findHits.map((h) => h.file);

        rows.push({
            question,
            file,
            ask: { hit3: hitAt(askFiles, file, 3), hit10: hitAt(askFiles, file, 10) },
            find: { hit3: hitAt(findFiles, file, 3), hit10: hitAt(findFiles, file, 10) },
        });
    }

    const score = (key, metric) => rows.filter((r) => r[key][metric]).length;
    const n = rows.length;
    const summary = {
        n,
        ask: { 'hit@3': `${score('ask', 'hit3')}/${n}`, 'hit@10': `${score('ask', 'hit10')}/${n}` },
        find: { 'hit@3': `${score('find', 'hit3')}/${n}`, 'hit@10': `${score('find', 'hit10')}/${n}` },
        rows,
    };
    console.log(JSON.stringify(summary, null, 2));

    await oc.close();
    await db.close();
}

main().catch((err) => {
    console.error(err.stack || err.message);
    process.exit(1);
});
