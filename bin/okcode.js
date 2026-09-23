#!/usr/bin/env node
'use strict';
// okcode — locate, read and edit code by name, from the terminal.
//
// Driveable with nothing else running. That is the point: the library is
// general purpose, so it has to be usable and testable on its own before any
// host wires it in.
//
// `--at` is mandatory for edits against an existing file — read the symbol or
// file first (its `at=` is printed to stderr) and pass that token back. A
// missing or stale token is refused, never silently rebased.

const fs = require('fs');
const path = require('path');

const USAGE = `okcode — find, read and edit code by name

usage: okcode <command> [args] [--root DIR] [--id NAME] [--store DIR] [--json]

global flags
  --root DIR               the workspace folder (local; default: cwd)
  --store DIR              okcode's okdb store (default: <root>/.okcode — one workspace,
                           created and kept up to date automatically). Point several
                           workspaces at one shared store with --store + --id.
  --id NAME                workspace id (default: "default" in <root>/.okcode;
                           the root's basename in a shared --store)
  --embedder-config FILE   embedder profiles (JSON: { "embedders": {...}, "active": "name" })
  --json                   machine output

look up
  structure [--dir sub/]           directories, sizes, largest symbols
  find <query> [--kind K]          where a symbol or phrase is defined
  refs <query>                     where it is used, by enclosing symbol
  grep <text>                      exact text → file:line
  outline <file>                   one file in named pieces with line ranges
  read <symbol|file#symbol|file|file:a-b>
  read-at <file> <a> <b>           a line range (at= on stderr)
  packages [name] [--refresh]      dependencies and their exported surface
  ask <question> [--embedder NAME] names + file text + meaning (needs an embedder)

edit (every write validates first and commits atomically)
  edit <symbol|file:a-b> --file body.txt --at HASH
  edit-batch --json batch.json     { "at": "HASH", "edits": [{ "target", "body" }] }
  write <file> --file text.txt (--create | --at HASH)
  check <file>                     syntax check on the target (needs exec)

manage
  status                           files, symbols, full-text and embedder state
  sync [--force]                   rescan now (--force re-hashes everything)
  reset --scope vectors|fts|all    re-embed / rebuild text search / drop and rescan
  embedders                        embedder profiles
  workspaces                       registered workspaces
  remove-workspace <id>            drop a workspace's index
  stats                            this workspace's counters

okcode help <command> for one command.`;

const HELP = {
    structure:
        'okcode structure [--dir sub/]\n  Directories by symbol count, total lines, and the largest top-level symbols.',
    find: 'okcode find <query> [--kind function|class|method|const|…] [--limit N]\n  Definitions by name, doc prose and file content.',
    refs: 'okcode refs <query> [--limit N]\n  Every textual use, attributed to the symbol containing it.',
    grep: 'okcode grep <text> [--limit N]\n  Exact, case-insensitive text → file:line.',
    outline:
        'okcode outline <file> [--limit N]\n  The file in named pieces (symbols, regions or chunks) with line ranges and at=.',
    read: 'okcode read <symbol | parent.symbol | file#symbol | file | file:a-b>\n  Body on stdout; location and at= on stderr.',
    'read-at': 'okcode read-at <file> <a> <b> [--at HASH]\n  A line range; with --at, refused if the file moved.',
    packages: "okcode packages [name] [--refresh]\n  Declared dependencies, or one package's surface.",
    ask: 'okcode ask <question> [--embedder NAME] [--limit N]\n  Fused lexical + semantic answer. Needs an embedder profile.',
    edit: 'okcode edit <symbol | file:a-b> --file body.txt --at HASH\n  Replace one symbol or range. --at is the token from a read.',
    'edit-batch':
        'okcode edit-batch --json batch.json   (or --batch batch.json)\n  batch.json: { "at": "HASH", "edits": [{ "target": "...", "body": "..." }] } — one file, non-overlapping targets.',
    write: 'okcode write <file> --file text.txt (--create | --at HASH)\n  Create a new file, or replace an existing one whole.',
    check: "okcode check <file>\n  node --check on the target (needs the facade's exec capability).",
    status: 'okcode status\n  Per workspace: files, symbols, full-text state; per embedder profile: done/pending/failed.',
    sync: 'okcode sync [--force]\n  Rescan changed files now; --force re-hashes everything.',
    reset: 'okcode reset --scope vectors|fts|all\n  vectors: re-embed · fts: rebuild text search · all: drop the index and rescan.',
    embedders: 'okcode embedders\n  Embedder profiles known to the store (and --embedder-config).',
    workspaces: 'okcode workspaces\n  Workspaces registered in the store.',
    'remove-workspace': "okcode remove-workspace <id>\n  Drop a workspace's index (files are untouched).",
    stats: 'okcode stats\n  Counters for this workspace.',
};

// ── argv ────────────────────────────────────────────────────────────────
const BOOL = new Set(['json', 'refresh', 'force', 'create', 'all', 'help', 'h']);

function parseArgs(argv) {
    const cmd = argv[0] && !argv[0].startsWith('-') ? argv[0] : null;
    const rest = cmd ? argv.slice(1) : argv;
    const flags = {};
    const positional = [];
    for (let i = 0; i < rest.length; i++) {
        const a = rest[i];
        if (!a.startsWith('--') && a !== '-h') {
            positional.push(a);
            continue;
        }
        let name = a.replace(/^--?/, '');
        let value;
        const eq = name.indexOf('=');
        if (eq >= 0) {
            value = name.slice(eq + 1);
            name = name.slice(0, eq);
        } else if (name === 'json' && cmd === 'edit-batch' && rest[i + 1] && !rest[i + 1].startsWith('--')) {
            // brain-compatible: `edit-batch --json batch.json` names the batch.
            name = 'batch';
            value = rest[++i];
        } else if (BOOL.has(name)) {
            value = true;
        } else if (rest[i + 1] !== undefined) {
            value = rest[++i];
        } else {
            value = true;
        }
        flags[name] = value;
    }
    return { cmd, flags, positional };
}

const { cmd, flags, positional } = parseArgs(process.argv.slice(2));
const JSON_OUT = flags.json === true;

class Exit extends Error {
    constructor(code, message) {
        super(message || '');
        this.exitCode = code;
    }
}
const fail = (message) => {
    throw new Exit(1, message);
};

const out = (s = '') => process.stdout.write(`${s}\n`);
const err = (s = '') => process.stderr.write(`${s}\n`);
const j = (o) => out(JSON.stringify(o, null, 2));
const pad = (s, n) => String(s === null || s === undefined ? '' : s).padEnd(n);

// A readable view of any plain object — management results whose shape is the
// library's to evolve, printed without the CLI having to know every field.
function tree(o, indent = '') {
    if (o === null || o === undefined || typeof o !== 'object') return `${indent}${o}`;
    const lines = [];
    const entries = Array.isArray(o) ? o.map((v, i) => [`[${i}]`, v]) : Object.entries(o);
    for (const [k, v] of entries) {
        if (v === undefined) continue;
        if (v && typeof v === 'object' && !(Array.isArray(v) && v.every((x) => x === null || typeof x !== 'object'))) {
            lines.push(`${indent}${k}:`);
            lines.push(tree(v, `${indent}  `));
        } else {
            lines.push(`${indent}${k}: ${Array.isArray(v) ? v.join(', ') || '(none)' : v}`);
        }
    }
    return lines.join('\n');
}

// ── boot ────────────────────────────────────────────────────────────────
const root = path.resolve(String(flags.root || process.cwd()));
// Simple local mode: the store lives in the folder (like .git) and holds one
// workspace, "default". A shared store (--store) holds many, one env each.
const sharedStore = !!flags.store;
const storeDir = path.resolve(String(flags.store || path.join(root, '.okcode')));
const wsId = String(flags.id || (sharedStore ? path.basename(root) || 'workspace' : 'default'));

// The in-folder store must never be committed. (Indexing already skips it:
// dot-directories are pruned by every access facade.)
function ignoreLocalStore() {
    if (sharedStore) return;
    const gi = path.join(root, '.gitignore');
    try {
        const text = fs.readFileSync(gi, 'utf8');
        if (/^\/?\.okcode\/?\s*$/m.test(text)) return;
        fs.appendFileSync(gi, `${text.endsWith('\n') || !text ? '' : '\n'}.okcode/\n`);
    } catch {
        /* no .gitignore — nothing to protect */
    }
}

function embedderConfig() {
    if (!flags['embedder-config']) return {};
    const file = String(flags['embedder-config']);
    let cfg;
    try {
        cfg = JSON.parse(fs.readFileSync(file, 'utf8'));
    } catch (e) {
        fail(`--embedder-config ${file}: ${e.message}`);
    }
    // Either { embedders: {...}, active } or the profile map itself.
    const embedders = cfg && cfg.embedders && typeof cfg.embedders === 'object' ? cfg.embedders : cfg;
    const active = (cfg && cfg.active) || flags.embedder || undefined;
    return { embedders, ...(active ? { active } : {}) };
}

let oc = null;
let db = null;
async function openOkcode() {
    const okcode = require('../src/okcode');
    fs.mkdirSync(storeDir, { recursive: true });
    ignoreLocalStore();
    const { embedders, active } = embedderConfig();
    // The CLI opens okdb itself so okdb's console logging (console.info →
    // STDOUT) can be detached before open: stdout is the answer, and --json
    // must stay parseable. OKCODE_VERBOSE=1 routes okdb's log to stderr.
    const OKDB = require('@kedem/okdb');
    db = new OKDB(storeDir);
    db.log.detachConsole();
    if (process.env.OKCODE_VERBOSE) {
        db.log.attach(({ level, msg, meta, context }) =>
            err(
                `[okdb ${level}] ${msg}${meta && Object.keys(meta).length ? ` ${JSON.stringify(meta)}` : ''}` +
                    (context === undefined
                        ? ''
                        : ` ${context && context.stack ? context.stack : JSON.stringify(context)}`),
            ),
        );
    }
    await db.open();
    oc = await okcode.open({
        db,
        ...(embedders ? { embedders } : {}),
        ...(active ? { active } : {}),
        log: process.env.OKCODE_VERBOSE ? (...a) => err(a.join(' ')) : () => {},
    });
    return { okcode, oc };
}

// The workspace this invocation is about: --root through the local facade.
async function openWs() {
    const { okcode } = await openOkcode();
    if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) fail(`--root ${root} is not a directory`);
    const ws = oc.workspace(wsId) || (await oc.addWorkspace(wsId, { access: okcode.access.localFs(root) }));
    return ws;
}

function toolsFor(ws) {
    const { createTools } = require('../src/tools');
    return createTools({ workspace: async () => ws, workspaces: () => [{ id: ws.id, root }] });
}

const need = (v, usage) => {
    if (v === undefined || v === null || v === '' || v === true) fail(usage);
    return v;
};

// ── commands ────────────────────────────────────────────────────────────
async function structure(ws) {
    const s = await ws.structure({ dir: flags.dir || null });
    if (JSON_OUT) return j(s);
    out(`\n${s.id} (${root})`);
    out(`  ${s.files} files · ${s.lines.toLocaleString()} lines · ${s.symbols} symbols\n`);
    out('  DIRECTORY                          FILES   LINES  SYMBOLS');
    for (const d of s.dirs)
        out(`  ${pad(d.dir, 32)} ${pad(d.files, 7)} ${pad(d.lines.toLocaleString(), 7)} ${d.symbols}`);
    out('\n  LARGEST SYMBOLS (the shape a file listing hides)');
    for (const l of s.largest) out(`  ${pad(l.lines + ' lines', 12)} ${pad(l.name, 22)} ${l.file}`);
    if (s.unparsed.length) {
        out('\n  UNPARSED');
        for (const u of s.unparsed) out(`  ${pad(u.file, 34)} ${u.reason}`);
    }
    out();
}

async function find(ws) {
    const q = need(positional[0], 'find needs a query');
    const hits = await ws.find(q, { kind: flags.kind || null, limit: Number(flags.limit) || 20 });
    if (JSON_OUT) return j(hits);
    if (!hits.length) return out(`no match for "${q}"`);
    out();
    for (const h of hits) {
        out(
            `  ${pad(h.kind, 9)} ${pad(h.path || h.name, 30)} ${pad(h.file + ':' + h.lines, 42)} ${h.span} lines${h.exported ? '  [exported]' : ''}`,
        );
        if (h.signature) out(`  ${' '.repeat(9)} ${h.signature}`);
        if (h.doc) out(`  ${' '.repeat(9)} ${h.doc.slice(0, 120)}`);
        out();
    }
}

async function refs(ws) {
    const q = need(positional[0], 'refs needs a query');
    const hits = await ws.refs(q, { limit: Number(flags.limit) || 40 });
    if (JSON_OUT) return j(hits);
    if (!hits.length) return out(`no uses of "${q}"`);
    const byFile = new Map();
    for (const h of hits) {
        if (!byFile.has(h.file)) byFile.set(h.file, []);
        byFile.get(h.file).push(h);
    }
    out(`\n${hits.length} textual use(s) of "${q}"\n`);
    for (const [file, list] of byFile) {
        out(`  ${file}`);
        for (const h of list) out(`    ${pad(h.line, 6)} ${pad('in ' + h.symbol, 26)} ${h.text}`);
        out();
    }
}

async function lookupVia(ws, tool, args, jsonFn) {
    if (JSON_OUT) return j(await jsonFn());
    const text = await toolsFor(ws).render(tool, args);
    out(text);
    // A tool's failure is a sentence starting "(<tool>" — still a failure here.
    if (text.startsWith(`(${tool}`)) throw new Exit(1);
}

async function packages(ws) {
    const name = positional[0];
    if (name) {
        const p = await ws.package(name);
        if (JSON_OUT) return j(p);
        if (p.error) fail(p.error);
        out(`\n${p.name}@${p.version || '?'}  (${p.source})`);
        if (p.description) out(`  ${p.description}`);
        out(`  entry: ${p.main || '(none)'}${p.types ? `   types: ${p.types}` : ''}`);
        if ((p.exportKeys || []).length) out(`  exports map: ${p.exportKeys.join(', ')}`);
        out(`\n  SURFACE (${p.surfaceFrom || 'not found'}${p.surfaceFile ? ` · ${p.surfaceFile}` : ''})`);
        out(`  ${(p.surface || []).join(', ') || '(none)'}`);
        if ((p.members || []).length) {
            out(`\n  MEMBERS (${p.members.length})`);
            out(`  ${p.members.join(', ')}`);
        }
        return out();
    }
    const list = await ws.packages({ refresh: flags.refresh === true });
    if (JSON_OUT) return j(list);
    if (list.error) fail(list.error);
    out(`\n${list.length} declared dependencies\n`);
    out('  PACKAGE                          VERSION       SOURCE       SURFACE  MEMBERS');
    for (const p of list) {
        out(
            `  ${pad(p.name, 32)} ${pad(p.version, 13)} ${pad(p.source, 12)} ${pad((p.surface || []).length, 8)} ${(p.members || []).length}`,
        );
    }
    out();
}

// The FULL token, not a truncated preview — `--at` is mandatory for every
// existing-file edit, so whatever is printed here has to be usable verbatim on
// the next command line, not just readable by a human.
async function read(ws) {
    const q = need(positional[0], 'read needs a symbol, file, or file:a-b');
    const r = await ws.read(q);
    if (JSON_OUT) {
        j(r);
        if (!r.ok) throw new Exit(1);
        return;
    }
    if (!r.ok) {
        err(r.reason);
        for (const c of r.candidates || []) err(`  ${c}`);
        throw new Exit(1);
    }
    err(
        `# ${r.file}:${r.lineStart}-${r.lineEnd}${r.kind ? `  (${r.kind} ${r.path || r.name})` : r.lines ? `  of ${r.lines} lines` : ''}`,
    );
    err(`# at=${r.at}`);
    if (r.note) err(`# ${r.note}`);
    out(r.body);
}

async function readAt(ws) {
    const file = need(positional[0], 'read-at needs <file> <a> <b>');
    const r = await ws.readAt(file, Number(positional[1]), Number(positional[2] || positional[1]), flags.at || null);
    if (JSON_OUT) {
        j(r);
        if (!r.ok) throw new Exit(1);
        return;
    }
    if (!r.ok) {
        err(r.reason);
        if (r.at) err(`# current at=${r.at}`);
        throw new Exit(1);
    }
    err(`# ${r.file}:${r.lineStart}-${r.lineEnd}`);
    err(`# at=${r.at}`);
    out(r.body);
}

// Print the receipt honestly: an outcome other than 'ok' is real evidence
// something was attempted, never collapsed into a bare pass/fail.
function reportEdit(r) {
    if (JSON_OUT) {
        j(r);
        if (r.outcome !== 'ok') throw new Exit(1);
        return;
    }
    if (!r.outcome) {
        err(`REFUSED: ${r.reason}`);
        if (r.stale && r.at) err(`  current at=${r.at}`);
        for (const c of r.candidates || []) err(`  candidate: ${c}`);
        for (const d of (r.newDiagnostics || []).slice(0, 10))
            err(`  ${d.source ? `[${d.source}] ` : ''}${d.line ? `line ${d.line}: ` : ''}${d.message}`);
        throw new Exit(1);
    }
    err(`${r.outcome}  ${r.file}  operation=${r.operationId}`);
    err(
        `  beforeHash=${(r.beforeHash || '').slice(0, 12)} candidateHash=${(r.candidateHash || '').slice(0, 12)}` +
            ` commitReportedHash=${(r.commitReportedHash || '').slice(0, 12)} readbackHash=${(r.readbackHash || '').slice(0, 12)}`,
    );
    if (r.readbackHash) err(`# at=${r.readbackHash}`);
    if (r.baselineBroken)
        err(`  baselineBroken=true (${(r.remaining || []).length} pre-existing diagnostic(s) unchanged)`);
    const failed = (r.validation || []).filter((v) => v.status === 'failed').map((v) => v.validator);
    if (failed.length) err(`  WARNING: ${failed.join(', ')} validator(s) reported issues (not blocking)`);
    if (r.reason && r.outcome !== 'ok') err(`  ${r.reason}`);
    for (const h of r.diff || []) {
        out(h.text);
        out();
    }
    if (r.outcome !== 'ok') throw new Exit(1);
}

const readBody = (file) => fs.readFileSync(String(file), 'utf8');

async function edit(ws) {
    const target = positional[0];
    if (!target || !flags.file || flags.file === true) fail('edit needs <symbol> --file <body.txt> --at HASH');
    if (!flags.at || flags.at === true) {
        fail('edit needs --at — read the symbol or file first (its at= is printed to stderr) and pass that token');
    }
    const body = readBody(flags.file).replace(/\n$/, '');
    reportEdit(await ws.edit(target, body, { at: String(flags.at) }));
}

async function editBatch(ws) {
    const file = flags.batch;
    if (!file || file === true)
        fail('edit-batch needs --json <batch.json>: { "at": "HASH", "edits": [{ "target", "body" }] }');
    let batch;
    try {
        batch = JSON.parse(readBody(file));
    } catch (e) {
        fail(`edit-batch: ${file}: ${e.message}`);
    }
    if (!batch.at) fail('edit-batch: the batch JSON needs "at" — a missing token is always refused');
    reportEdit(await ws.editBatch(batch));
}

async function write(ws) {
    const rel = positional[0];
    if (!rel || !flags.file || flags.file === true) fail('write needs <file> --file <text.txt> (--create | --at HASH)');
    const create = flags.create === true;
    const at = flags.at && flags.at !== true ? String(flags.at) : null;
    if (!create && !at)
        fail('write needs --create for a new file, or --at HASH (from a read) to replace an existing one');
    if (create && at) fail('write takes --create OR --at, not both');
    const r = await ws.writeWholeFile(rel, readBody(flags.file), { create, expectedHash: at });
    reportEdit(r);
}

async function check(ws) {
    const file = need(positional[0], 'check needs <file>');
    const r = await ws.syntaxCheck(file);
    j(r);
    if (!r.ok) throw new Exit(1);
}

async function stats(ws) {
    const s = await ws.stats();
    if (JSON_OUT) return j(s);
    out(tree(s));
}

async function status() {
    await openWs();
    const s = await oc.status(flags.all ? undefined : wsId);
    if (JSON_OUT) return j(s);
    out(tree(s));
}

async function sync() {
    await openWs();
    const r = await oc.sync(wsId, { force: flags.force === true });
    if (JSON_OUT) return j(r === undefined ? { ok: true } : r);
    const rows = Array.isArray(r) ? r : r && typeof r === 'object' ? [r] : [];
    if (!rows.length) return out(`synced ${wsId}`);
    for (const x of rows) {
        out(
            x && x.id !== undefined && x.scanned !== undefined
                ? `synced ${x.id}: ${x.scanned} scanned, ${x.changed || 0} changed, ${x.removed || 0} removed (${x.ms}ms)`
                : tree(x),
        );
    }
}

async function reset() {
    const scope = flags.scope;
    if (!['vectors', 'fts', 'all'].includes(scope)) fail('reset needs --scope vectors|fts|all');
    await openWs();
    const r = await oc.reset(wsId, { scope });
    if (JSON_OUT) return j(r === undefined ? { ok: true } : r);
    out(r && typeof r === 'object' ? tree(r) : `reset ${wsId} (${scope})`);
}

async function embedders() {
    await openOkcode();
    // Whatever the library exposes for profiles; status carries them otherwise.
    const list = oc.embedders();
    if (JSON_OUT) return j(list);
    if (!list.length) return out('no embedder profiles (pass --embedder-config FILE.json)');
    for (const p of list) {
        out(
            `  ${p.active ? '*' : ' '} ${pad(p.name, 16)} ${pad(p.type, 10)} ${pad(p.model, 32)} ${pad(p.dims ? `${p.dims}d` : '?d', 6)} ${p.state}`,
        );
    }
}

async function workspaces() {
    await openOkcode();
    const list = await oc.workspaces();
    if (JSON_OUT) return j(list);
    if (!list || !list.length) return out('no workspaces registered');
    for (const w of list) {
        if (typeof w === 'string') out(`  ${w}`);
        else {
            const where = w.root || (w.access && w.access.root) || '';
            const kind = w.access && w.access.kind ? ` (${w.access.kind})` : '';
            const synced = w.lastSync ? `  synced ${new Date(w.lastSync).toISOString()}` : '';
            out(`  ${pad(w.id, 24)} ${where}${kind}${synced}`);
        }
    }
}

async function removeWorkspace() {
    const id = need(positional[0], 'remove-workspace needs <id>');
    await openOkcode();
    const r = await oc.removeWorkspace(id);
    if (JSON_OUT) return j(r === undefined ? { ok: true, removed: id } : r);
    out(`removed ${id}`);
}

const WS_COMMANDS = {
    structure,
    find,
    refs,
    grep: (ws) =>
        lookupVia(ws, 'code_grep', { text: need(positional[0], 'grep needs text'), limit: flags.limit }, () =>
            ws.mentions(positional[0], { limit: Number(flags.limit) || 20 }),
        ),
    outline: (ws) =>
        lookupVia(ws, 'code_outline', { file: need(positional[0], 'outline needs <file>'), limit: flags.limit }, () =>
            ws.outline(positional[0], { limit: Number(flags.limit) || 60 }),
        ),
    ask: (ws) => {
        const q = need(positional.join(' '), 'ask needs a question');
        const profile = flags.embedder && flags.embedder !== true ? String(flags.embedder) : null;
        return lookupVia(ws, 'code_ask', { question: q, embedder: profile, limit: flags.limit }, () =>
            ws.ask(q, { limit: Number(flags.limit) || 10, profile, embedder: profile }),
        );
    },
    packages,
    read,
    'read-at': readAt,
    edit,
    'edit-batch': editBatch,
    write,
    check,
    stats,
};
const MGMT_COMMANDS = {
    status,
    sync,
    reset,
    embedders,
    workspaces,
    'remove-workspace': removeWorkspace,
};

async function main() {
    if (!cmd || cmd === 'help' || flags.help || flags.h) {
        const topic = cmd === 'help' ? positional[0] : cmd;
        if (topic && HELP[topic]) {
            out(HELP[topic]);
        } else {
            out(USAGE);
            if (topic && topic !== 'help') throw new Exit(1, `unknown command "${topic}"`);
        }
        if (process.argv.length <= 2) throw new Exit(1);
        return;
    }
    if (MGMT_COMMANDS[cmd]) return MGMT_COMMANDS[cmd]();
    if (!WS_COMMANDS[cmd]) throw new Exit(1, `unknown command "${cmd}" — okcode --help`);
    const ws = await openWs();
    await WS_COMMANDS[cmd](ws);
    // Writes happen after the answer, never in front of it.
    await ws.flush();
}

main()
    .then(() => 0)
    .catch((e) => {
        if (e instanceof Exit) {
            if (e.message) err(e.message);
            return e.exitCode;
        }
        err(e && e.code ? `${e.code}: ${e.message}` : String((e && e.message) || e));
        return 1;
    })
    .then(async (code) => {
        if (oc) await oc.close().catch(() => {});
        if (db) await db.close().catch(() => {});
        process.exit(code);
    });
