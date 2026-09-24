'use strict';
// Model-facing tools: the text a model sees when it looks up, reads or edits
// code in a workspace. Host-agnostic — this module takes WORKSPACE OBJECTS
// (src/workspace.js), never okcode.js, so any host (the brain, an MCP server,
// a CLI) can offer the same verbs:
//
//   const tools = createTools({
//       workspace: async (hint) => ws | null,          // resolve an id / path hint
//       workspaces: () => [{ id, root?, description? }], // for code_map and errors
//   });
//   tools.schemas();                 // JSON-schema definitions for function calling
//   await tools.render(name, args);  // → text; NEVER throws
//
// Why one renderer: a model reaches these from more than one place (an
// in-cycle lookup, a late action, a native tool call), and the answer has to be
// the same text wherever it was asked. A host that wants side effects (record
// the answer, stamp stats) wraps render(); nothing here knows about memory.
//
// Never throws: a lookup that fails is a sentence, not an exception — the
// conversation around it is still valid, and the sentence says what to do next.

const { pathMatcher, splitList } = require('./grep');

const LOOKUPS = [
    'code_map',
    'code_find',
    'code_grep',
    'code_glob',
    'code_outline',
    'code_read',
    'code_package',
    'code_ask',
];
const EDITS = ['code_edit', 'code_write'];
const TOOLS = [...LOOKUPS, ...EDITS];

// A diff beyond this is summarised, not dumped: the receipt is what the model
// reads next, and a whole-file patch in front of it is a whole-file read.
const DIFF_CAP = 4000;
// The CURRENT region re-read after a refused edit — enough to fix the
// candidate in one step, not a whole file.
const REGION_CAP = 3000;

const isTool = (name) => TOOLS.includes(name);
const isLookup = (name) => LOOKUPS.includes(name);
const isEdit = (name) => EDITS.includes(name);

// What a call was ABOUT — a label for a host's log or observation.
const subjectOf = (name, args = {}) =>
    String(
        (args &&
            (args.symbol ||
                args.target ||
                args.path ||
                args.file ||
                args.query ||
                args.text ||
                args.pattern ||
                args.glob ||
                args.question ||
                args.name ||
                args.workspace)) ||
            '',
    );

// Workspace-layer reasons name library verbs ("use structure()"); a model
// knows the tools. Translate the handful that appear in refusals.
const toToolWords = (s) =>
    String(s || '')
        .replace(/\bstructure\(\)/g, 'code_map')
        .replace(/\bfind\(\)/g, 'code_find')
        .replace(/\bask\(\)/g, 'code_ask')
        .replace(/\bexpectedHash\b/g, '`at`')
        .replace(/\bwriteWholeFile\b/g, 'code_write');

// ── content-addressed edit helpers (from the brain's code_edit) ──────────
// `within` picks between exact duplicates of `find`: the first occurrence at
// or after a landmark that itself occurs exactly once.
function hitWithin(content, hits, within) {
    const lineOf = (idx) => content.slice(0, idx).split('\n').length;
    const marks = [];
    for (let i = content.indexOf(within); i !== -1 && marks.length < 2; i = content.indexOf(within, i + 1))
        marks.push(i);
    if (!marks.length)
        return { error: 'the `within` landmark matches nothing in the file — copy it verbatim from a fresh read' };
    if (marks.length > 1)
        return { error: 'the `within` landmark itself occurs more than once — pick a more distinctive one' };
    const after = hits.filter((h) => h >= marks[0]);
    if (!after.length) {
        return {
            error: `every occurrence of \`find\` sits ABOVE the landmark (landmark at line ${lineOf(marks[0])}, occurrences at lines ${hits.map(lineOf).join(', ')}) — \`within\` must be text above the edit site`,
        };
    }
    const hit = Math.min(...after);
    return { hit, line: lineOf(hit) };
}

// For each duplicate occurrence, the smallest surrounding block of whole lines
// that occurs exactly once — a ready-made unique `find` for the retry.
const ANCHOR_CAP = 1500;
function uniqueAnchors(content, findText, hits) {
    const lines = content.split('\n');
    const lineOf = (idx) => content.slice(0, idx).split('\n').length;
    return hits.map((at) => {
        let from = lineOf(at) - 1;
        let to = lineOf(at + Math.max(findText.length, 1) - 1) - 1;
        for (;;) {
            const probe = lines.slice(from, to + 1).join('\n');
            if (probe.length > ANCHOR_CAP) return { line: lineOf(at), probe: null };
            let n = 0;
            for (let j = content.indexOf(probe); j !== -1 && n < 2; j = content.indexOf(probe, j + 1)) n++;
            if (n === 1) return { line: lineOf(at), probe };
            const canUp = from > 0;
            const canDown = to < lines.length - 1;
            if (!canUp && !canDown) return { line: lineOf(at), probe: null };
            if (canUp) from--;
            if (canDown) to++;
        }
    });
}

// Resolve `find` edits against a fresh whole-file read into line-range
// targets. Returns { edits, at, file } or { error }.
//
// CONTENT-ADDRESSED TARGETS. `find` carries the exact current text being
// replaced; it is located in the file and the edit lands wherever that text
// IS. Line numbers are the most fragile address a file has — both observed
// edit failures (in the brain) were a model pasting a correct body over a
// confabulated line number, caught only three layers down by the template
// validator. The address being the content makes a wrong address impossible:
// zero matches or two-plus matches refuse HERE, cheaply, with the information
// that fixes the retry.
async function resolveFindEdits(ws, file, edits) {
    const g = await ws.readFile(file, { maxLines: Number.MAX_SAFE_INTEGER });
    if (!g || !g.ok) return { error: `cannot read ${file}: ${toToolWords((g && g.reason) || 'unknown')}` };
    const content = String(g.body);
    const out = [];
    for (const e0 of edits) {
        const e = { ...e0 };
        if (e.find === undefined) {
            out.push(e);
            continue;
        }
        let hits = [];
        for (let i = content.indexOf(e.find); i !== -1; i = content.indexOf(e.find, i + 1)) hits.push(i);
        let withinNote = '';
        if (hits.length > 1 && e.within) {
            const sel = hitWithin(content, hits, e.within);
            if (sel.error) withinNote = ` (\`within\` did not settle it — ${sel.error}.)`;
            else hits = [sel.hit];
        }
        delete e.within;
        if (hits.length !== 1) {
            // Equip the retry: where the nearest similar text actually lives,
            // or where every occurrence is.
            let hint = '';
            if (!hits.length) {
                const probe = e.find
                    .split('\n')
                    .map((l) => l.trim())
                    .filter((l) => l.length > 8)[0];
                if (probe) {
                    const j = content.indexOf(probe);
                    if (j !== -1) {
                        const ln = content.slice(0, j).split('\n').length;
                        hint = ` The nearest similar text ("${probe.slice(0, 60)}") is at line ${ln} — copy the CURRENT text from there verbatim, whitespace included.`;
                    } else hint = ' No line of it appears in the file at all — re-read before editing.';
                }
            } else {
                const shown = hits.slice(0, 5);
                const more = hits.length - shown.length;
                const uniq = uniqueAnchors(content, e.find, shown);
                const where = uniq.map((u) => u.line).join(', ') + (more > 0 ? ` (+${more} more)` : '');
                hint = uniq.some((u) => u.probe)
                    ? ` It occurs at lines ${where}.${withinNote} Fastest retry: the SAME find plus within:<a distinctive line from ABOVE the one you mean, verbatim>. Or copy ONE anchor below as \`find\`, EXACTLY as printed — the body must then reproduce the extra surrounding lines unchanged:${uniq
                          .map((u) =>
                              u.probe
                                  ? `\n--- unique anchor for the line ${u.line} occurrence ---\n${u.probe}`
                                  : `\n--- line ${u.line}: no compact unique anchor exists for this one — use the whole-file path ---`,
                          )
                          .join('')}\n--- end anchors ---`
                    : ` It occurs at lines ${where}.${withinNote} Retry the SAME find with within:<a distinctive line from ABOVE the one you mean, verbatim> — the duplicated region is too large for any compact anchor to tell the copies apart.`;
            }
            // The refusal teaches the way out, not just the miss: anchor-hunting
            // on a file with near-duplicate regions burned four live cycles
            // (two refusals, two false "edited" claims) before a human
            // suggested the anchor-free path. The tool knows that path; say it.
            const wayOut =
                ' If the anchor keeps missing or the region is duplicated, skip anchors entirely: code_read the whole file, then land the complete new content with ONE code_write using the `at` from that same read — whole-file replace needs no anchor and either lands or conflicts honestly.';
            return {
                error: hits.length
                    ? `\`find\` matches ${hits.length} places in ${g.file} — it must match exactly one.${hint}${wayOut}`
                    : `\`find\` matches NOTHING in ${g.file} — the text is not there as written.${hint}${wayOut}`,
                at: g.at,
            };
        }
        // Expand to whole lines: the range replaces full lines, so a mid-line
        // match keeps its line's prefix and suffix.
        const start = hits[0];
        const end = start + e.find.length;
        const lineStartIdx = content.lastIndexOf('\n', start - 1) + 1;
        let lineEndIdx = content.indexOf('\n', end);
        if (lineEndIdx === -1) lineEndIdx = content.length;
        e.body = content.slice(lineStartIdx, start) + e.body + content.slice(end, lineEndIdx);
        const fromLine = content.slice(0, lineStartIdx).split('\n').length;
        const toLine = content.slice(0, lineEndIdx).split('\n').length;
        e.target = `${g.file}:${fromLine}-${toLine}`;
        delete e.find;
        out.push(e);
    }
    return { edits: out, at: g.at, file: g.file };
}

// Validators that reported 'failed' on a commit that still landed. Not
// blocking (only a NEW diagnostic blocks, and editBatch already refused on
// those) — but the receipt is the ONLY thing the model sees back for its
// write, and "applied (ok)" alone reads as unqualified success even when
// something real was flagged.
const failedValidators = (res) => (res.validation || []).filter((v) => v.status === 'failed').map((v) => v.validator);

const diagLines = (list, n = 5) =>
    (list || [])
        .slice(0, n)
        .map((d) => `  - ${d.source ? `[${d.source}] ` : ''}${d.line ? `line ${d.line}: ` : ''}${d.message}`)
        .join('\n');

function renderDiff(res) {
    const text = (res.diff || []).map((h) => h.text).join('\n\n');
    if (!text) return '';
    return text.length > DIFF_CAP
        ? `${text.slice(0, DIFF_CAP)}\n…[diff truncated at ${DIFF_CAP} of ${text.length} chars — code_read the file to see the rest]`
        : text;
}

// The receipt for any write that got as far as a commit attempt. Each outcome
// other than 'ok' is a distinct, honest claim about what is and is not known —
// never collapsed into "it worked" or "nothing happened".
function renderReceipt(verb, res) {
    const newAt = res.readbackHash || null;
    const warn = failedValidators(res);
    const head =
        res.outcome === 'ok'
            ? `applied: ${res.file} (ok) — validated before commit, published atomically, verified by readback.  at=${newAt}`
            : `${verb} on ${res.file} finished as ${res.outcome} — ${toToolWords(res.reason || 'read the file again before trying anything further')}${newAt ? `  at=${newAt}` : ''}`;
    const lines = [head];
    if (res.baselineBroken) {
        lines.push(
            `  note: the file already had ${(res.remaining || []).length} diagnostic(s) before this edit; none were added.`,
        );
    }
    if (warn.length) {
        lines.push(
            `  WARNING: ${warn.join(', ')} validator(s) reported issues — not blocking this commit, but worth a look before calling it done.`,
        );
    }
    const diff = renderDiff(res);
    if (diff) lines.push(diff);
    if (res.outcome === 'ok') lines.push(`  [the new at is ${newAt} — pass it to the next code_edit on ${res.file}]`);
    return lines.join('\n');
}

// `grepMaxChars`: the size code_grep keeps its answer under (the `max_chars`
// argument overrides it per call). A host that clips tool output clips the
// END — the continuation line — so the renderer stops first, at a line
// boundary, and says exactly where to resume.
function createTools({ workspace, workspaces = () => [], grepMaxChars = 12000 } = {}) {
    if (typeof workspace !== 'function') throw new Error('createTools needs workspace: async (hint) => ws | null');

    const known = () => {
        try {
            return [].concat(workspaces() || []).filter(Boolean);
        } catch {
            return [];
        }
    };
    const knownLine = () => {
        const list = known();
        return list.length
            ? `known workspaces: ${list.map((w) => `${w.id}${w.root ? ` (${w.root})` : ''}`).join(', ')}`
            : 'no workspaces are registered';
    };

    // Resolve the hint. With no hint and exactly one workspace, that one is
    // meant — asking a model to spell out the only choice buys nothing.
    async function wsFor(hint) {
        let ws = await workspace(hint === undefined || hint === null || hint === '' ? undefined : String(hint));
        if (!ws && (hint === undefined || hint === null || hint === '')) {
            const list = known();
            if (list.length === 1) ws = await workspace(list[0].id);
            if (!ws) return { error: `which workspace? pass \`workspace\` — ${knownLine()}` };
        }
        if (!ws) return { error: `no workspace "${hint}" — ${knownLine()}` };
        return { ws };
    }

    const rootOf = (ws) => {
        const entry = known().find((w) => w.id === ws.id);
        return (entry && entry.root) || (ws.access && ws.access.root) || null;
    };

    // A model often pastes an ABSOLUTE path it saw elsewhere; inside this
    // workspace's root, that is the relative path it meant.
    const relOf = (ws, p) => {
        const s = String(p || '').replace(/\\/g, '/');
        const root = rootOf(ws);
        if (!root) return s;
        const r = String(root).replace(/\\/g, '/').replace(/\/+$/, '');
        return s.startsWith(`${r}/`) ? s.slice(r.length + 1) : s;
    };

    async function codeMap(ws) {
        const s = await ws.stats();
        const st = await ws.structure();
        // The workspace's shape includes what it STANDS ON. A dependency's
        // package.json description is the author's own one-line answer to
        // "what is this" — without it here, that question costs a whole
        // research cycle of grepping call sites (measured: "what is okdb").
        let deps = null;
        try {
            deps = await ws.packages();
        } catch {
            deps = null;
        }
        const depLines =
            !deps || deps.error || !deps.length
                ? null
                : deps
                      .filter((p) => p.source !== 'transitive')
                      .map(
                          (p) =>
                              `  ${p.name}@${p.version || '?'}${p.description ? ` — ${String(p.description).slice(0, 120)}` : ''}`,
                      );
        const root = rootOf(ws);
        return [
            `${ws.id}${root ? ` (${root})` : ''}`,
            `${s.files} files, ${s.symbols} symbols — ${s.withSymbols} with structure, ${s.searchable} searchable, ${s.opaque} opaque`,
            `languages: ${Object.entries(s.byLang || {})
                .map(([l, n]) => `${l} ${n}`)
                .join(', ')}`,
            depLines && depLines.length ? `dependencies (code_package for exports):\n${depLines.join('\n')}` : '',
            st.largest && st.largest.length
                ? `largest symbols:\n${st.largest
                      .slice(0, 10)
                      .map((x) => `  ${String(x.lines).padStart(5)} lines  ${x.name}  ${x.file}`)
                      .join('\n')}`
                : '',
        ]
            .filter(Boolean)
            .join('\n');
    }

    async function codeAsk(ws, a) {
        const q = String(a.question || a.query || '');
        // A HOST (never the model — it is not in the schema) may hand over
        // the question already embedded: `vector` (+ `identity`, the space
        // it came from). It is searched as-is, with no embed call; the
        // question text still drives the name/text eyes.
        const vector = a.vector != null ? a.vector : null;
        if (!q && !vector)
            return '(code_ask) — needs `question`: what the code you are looking for DOES, in your own words.';
        const profile = a.embedder || a.profile || null;
        const query = vector ? { text: q || undefined, vector, identity: a.identity || undefined } : q;
        let hits;
        try {
            hits = await ws.ask(query, { limit: Number(a.limit) || 10, profile, embedder: profile });
        } catch (err) {
            if (err && err.code === 'OKCODE_NO_EMBEDDINGS') {
                return `(code_ask "${q}") — ${ws.id} has no semantic index (no embedder configured). Use code_find for names and doc prose, or code_grep for exact text.`;
            }
            throw err;
        }
        if (!hits.length) return `(code_ask "${q}") — nothing in ${ws.id} matched.`;
        // The fused answer: names, file text and meaning. Each hit says which
        // eye found it, because "the name matches" and "this reads like what
        // you asked" are different claims and the caller should be able to
        // tell them apart.
        return (
            `Answering ${q ? `"${q}"` : 'a query vector'} from ${ws.id}:\n` +
            hits
                .map((h) => {
                    const where = `${h.file}:${h.lines}`;
                    const how =
                        h.via === 'vector'
                            ? 'meaning'
                            : h.via === 'content'
                              ? 'file text'
                              : h.via === 'content~' || h.via === 'fts~'
                                ? 'loose match'
                                : 'name/doc';
                    // The REGION, not one line of it. A hit that located the
                    // right place and then showed a single line
                    // ("flex-direction: column; gap: .15rem; }") sent a live
                    // task to the shell to read around it.
                    const body = h.window
                        ? `\n      ${h.file}:${h.window.lineStart}-${h.window.lineEnd}  at=${h.at}\n` +
                          h.window.text
                              .split('\n')
                              .map((l) => `      | ${l}`)
                              .join('\n')
                        : h.quote
                          ? `\n      ${h.quote}`
                          : h.doc
                            ? `\n      ${h.doc}`
                            : '';
                    return `  [${how}] ${h.path || h.name}  ${where}${body}`;
                })
                .join('\n')
        );
    }

    async function codePackage(ws, a) {
        // The surface of a dependency, not its source. The question this
        // answers — "does it expose X" — was costing whole cycles of reading
        // library files.
        if (a.name) {
            const p = await ws.package(String(a.name));
            return p.error
                ? `(code_package ${a.name}) — ${p.error}`
                : [
                      `${p.name}@${p.version || '?'} (${p.source})${p.description ? ` — ${p.description}` : ''}`,
                      `entry: ${p.main || '(none)'}${p.types ? `   types: ${p.types}` : ''}`,
                      `exports (${p.surfaceFrom || 'not found'}): ${(p.surface || []).join(', ') || '(none)'}`,
                      (p.members || []).length ? `members: ${p.members.join(', ')}` : '',
                  ]
                      .filter(Boolean)
                      .join('\n');
        }
        const list = await ws.packages();
        return list.error
            ? `(code_package) — ${list.error}`
            : `${list.length} dependencies of ${ws.id}:\n` +
                  list
                      .map(
                          (p) =>
                              `  ${p.name}@${p.version || '?'} (${p.source}) — ${(p.surface || []).length} exported name(s)${p.description ? ` — ${String(p.description).slice(0, 120)}` : ''}`,
                      )
                      .join('\n');
    }

    const listArg = splitList;
    const num = (v) => (v == null || v === '' || !Number.isFinite(Number(v)) ? undefined : Number(v));
    const caseArg = (a) => {
        const v = a.case_sensitive ?? a.caseSensitive ?? a.case;
        if (v === 'smart') return 'smart';
        if (v === true || v === 'true' || v === 'sensitive') return true;
        return false;
    };

    // rg-style: `file:line: text` for a match and `file-line- text` for
    // context (the address is copyable as-is into code_read), grouped under
    // one heading per file, `--` between separated stretches. Returns the
    // heading and the rows, each hit row carrying its match ordinal `n`, so a
    // caller cutting for size knows where the cut fell.
    function renderGrepFile(fileRow, ms, withContext) {
        const rows = new Map();
        for (const m of ms) {
            m.before.forEach((t, j) => {
                const k = m.line - m.before.length + j;
                if (!rows.has(k)) rows.set(k, { text: t, hit: false, n: m.n });
            });
            rows.set(m.line, { text: m.text, hit: true, n: m.n });
            m.after.forEach((t, j) => {
                const k = m.line + 1 + j;
                if (!rows.has(k)) rows.set(k, { text: t, hit: false, n: null });
            });
        }
        const nums = [...rows.keys()].sort((x, y) => x - y);
        const lines = [];
        let prev = null;
        for (const k of nums) {
            const r = rows.get(k);
            if (withContext && prev !== null && k > prev + 1) lines.push({ text: '--', n: null });
            // A context row before a hit belongs to that hit: cutting there
            // resumes at the hit.
            lines.push({ text: r.hit ? `${fileRow.file}:${k}: ${r.text}` : `${fileRow.file}-${k}- ${r.text}`, n: r.n });
            prev = k;
        }
        const plural = fileRow.count === 1 ? '' : 's';
        let more = '';
        if (fileRow.shown < fileRow.count) {
            const from = fileRow.from || 0;
            const range = !from
                ? ''
                : fileRow.shown === 1
                  ? ` (match ${from + 1})`
                  : ` (matches ${from + 1}-${from + fileRow.shown})`;
            more = `, showing ${fileRow.shown}${range}`;
            if (fileRow.passed)
                more += ` — raise max_per_file for the rest (with paths:["${fileRow.file}"] for this file alone)`;
        }
        return { head: `── ${fileRow.file} (${fileRow.count} matching line${plural}${more}; at=${fileRow.at})`, lines };
    }

    const GREP_OUTPUTS = ['lines', 'files', 'matches'];

    async function codeGrep(ws, a) {
        // THE LITERAL EYE. code_find and code_ask both search by MEANING, and
        // meaning is exactly what fails on the questions a UI task asks.
        // Measured (brain): `code_find("CLAUDE accordion title side rail")`
        // returned three files, none of which render it — while the literal
        // text `toggleSection('claude')` lands on the right file:line, first
        // hit, every time. A live task burned 8 lookups over 7 phrasings
        // failing to make that jump, then shelled out for a grep. The index
        // could always answer it; nothing exposed the answer.
        //
        // And it has to be at least as good as the grep it replaces: the
        // first version returned ONE line number per file and no text, so
        // the model learned to sweep the repo with shell grep/sed instead —
        // minutes per investigation. Every matching line, its text, its
        // context, regex, case and path filters: rg, over the workspace.
        //
        // Enumeration ("every OKDB_* name") is a different question from
        // "where": measured, a lines answer hit the host's output cap after
        // 173 lines and the model reported 68 of 77 names. So: output:
        // 'matches' (distinct matched strings with counts — rg -o | sort |
        // uniq -c) and 'files' (rg -c) answer it in one call; counts are
        // always exact; and a capped answer ENDS with how to get the rest
        // (page / offset), inside the size budget rather than cut by it.
        const q = String(a.pattern ?? a.text ?? a.query ?? '');
        if (!q) return '(code_grep) — needs `pattern`: the text (or, with regex: true, the expression) to look for.';
        if (typeof ws.grep !== 'function') return legacyGrep(ws, q, a);
        const rawOut = a.output ?? a.mode;
        const output = rawOut == null || rawOut === '' ? 'lines' : String(rawOut).toLowerCase();
        if (!GREP_OUTPUTS.includes(output)) {
            return `(code_grep "${q}") — output must be one of ${GREP_OUTPUTS.map((o) => `'${o}'`).join(', ')} (got '${rawOut}').`;
        }
        const glob = listArg(a.glob ?? a.globs ?? a.include);
        const paths = listArg(a.paths ?? a.path ?? a.dir);
        const maxChars = Math.max(1000, num(a.max_chars ?? a.maxChars) ?? grepMaxChars);
        const page = num(a.page);
        const offset = num(a.offset);
        let r;
        try {
            r = await ws.grep(q, {
                regex: a.regex === true || a.regex === 'true',
                caseSensitive: caseArg(a),
                glob,
                paths,
                output,
                page,
                offset,
                context: num(a.context),
                before: num(a.before),
                after: num(a.after),
                maxMatches: num(a.max_matches ?? a.maxMatches) ?? (output === 'matches' ? 500 : 100),
                maxPerFile: num(a.max_per_file ?? a.maxPerFile) ?? 10,
                // `limit` is the historical "max files".
                maxFiles: num(a.max_files ?? a.maxFiles ?? a.limit) ?? (output === 'files' ? 500 : undefined),
            });
        } catch (err) {
            if (err && err.code === 'GREP_BAD_PATTERN') return `(code_grep "${q}") — ${err.message}`;
            throw err;
        }
        const scope = [
            glob ? `glob ${glob.join(' ')}` : '',
            paths ? `under ${paths.join(', ')}` : '',
            r.regex ? 'regex' : '',
            r.ignoreCase ? 'ignoring case' : 'case-sensitive',
        ]
            .filter(Boolean)
            .join(', ');
        // A result from a workspace object that predates paging/modes.
        const counts = r.counts || r.files;
        const total = r.total || { lines: r.files.reduce((n, f) => n + f.count, 0), files: r.files.length };
        if (!total.lines) {
            return (
                `(code_grep "${q}") — ${r.regex ? 'no line matches that expression' : 'that text appears nowhere'} in ${ws.id} (${r.searched} files searched; ${scope}).` +
                (glob || paths ? ' Check the glob/paths filter, or drop it.' : '') +
                ' If you expected it to exist, it does not as written: try a shorter distinctive fragment,' +
                ' the other case, or code_ask to search by meaning.'
            );
        }
        const nLines = `${total.lines} matching line${total.lines === 1 ? '' : 's'}`;
        const nFiles = `${total.files} file${total.files === 1 ? '' : 's'}`;
        const pageNote = r.page > 1 || r.offset > 0 ? ` — page ${r.page}${r.offset ? `, from ${r.offset}` : ''}` : '';
        const readHint =
            '  [code_read "<file>:<from>-<to>" for more around a line; code_edit {file, find, body} to change one]';
        const other = (o) =>
            GREP_OUTPUTS.filter((x) => x !== o)
                .map((x) => `output:'${x}'`)
                .join(' or ');

        // Fill the budget with rows, whole rows only; `cut` = the index of
        // the first row that did not fit.
        const fit = (headText, rows, reserve) => {
            let used = headText.length + reserve;
            for (let i = 0; i < rows.length; i++) {
                used += rows[i].length + 1;
                if (used > maxChars) return i;
            }
            return rows.length;
        };
        const RESERVE = 400; // the continuation + hint lines

        if (output === 'files') {
            const head = `code_grep "${q}" in ${ws.id} — ${nLines} in ${nFiles} (${r.searched} searched; ${scope})${pageNote}; matching lines per file:`;
            const rows = r.files.map((f) => `  ${f.file}: ${f.count}`);
            const cut = fit(head, rows, RESERVE);
            const shownTo = r.offset + cut;
            const tail = [];
            if (shownTo < total.files) {
                const restFiles = total.files - shownTo;
                const restLines = counts.slice(shownTo).reduce((n, f) => n + f.count, 0);
                const how = cut < rows.length ? `offset=${shownTo}` : `page=${r.page + 1}`;
                tail.push(
                    `  [${restFiles} more file(s) with ${restLines} matching line(s) not shown — narrow with glob/paths, or continue with ${how}]`,
                );
            }
            tail.push('  [output:\'lines\' shows the lines themselves; paths:["<file>"] for one file]');
            return [head, ...rows.slice(0, cut), ...tail].join('\n');
        }

        if (output === 'matches') {
            const head = `code_grep "${q}" in ${ws.id} — ${total.distinct} distinct match${total.distinct === 1 ? '' : 'es'} (${total.occurrences} occurrence${total.occurrences === 1 ? '' : 's'} on ${nLines} in ${nFiles}; ${r.searched} searched; ${scope})${pageNote}; count, match, where:`;
            const w = String(r.distinct.length ? r.distinct[0].count : 1).length;
            const rows = r.distinct.map(
                (d) => `  ${String(d.count).padStart(w)}  ${d.text}  (${d.files === 1 ? d.first : `${d.files} files`})`,
            );
            const cut = fit(head, rows, RESERVE);
            const shownTo = r.offset + cut;
            const tail = [];
            if (shownTo < total.distinct) {
                const how = cut < rows.length ? `offset=${shownTo}` : `page=${r.page + 1}`;
                tail.push(
                    `  [${total.distinct - shownTo} more distinct match(es) not shown — narrow the pattern or glob/paths, or continue with ${how}]`,
                );
            }
            tail.push(
                `  [output:'lines' (with paths/glob) shows where a match is; ${"output:'files'"} counts per file]`,
            );
            return [head, ...rows.slice(0, cut), ...tail].join('\n');
        }

        const head = `code_grep "${q}" in ${ws.id} — ${nLines} in ${nFiles} (${r.searched} searched; ${scope})${pageNote}:`;
        const byFile = new Map();
        for (const m of r.matches) {
            if (!byFile.has(m.file)) byFile.set(m.file, []);
            byFile.get(m.file).push(m);
        }
        const startOf = new Map(counts.map((f) => [f.file, f.start]));
        const withContext = r.matches.some((m) => m.before.length || m.after.length);
        // Flatten to rows; a hit row (and the context leading into it)
        // carries the stream index a resume would start at.
        const rows = [];
        r.files.forEach((f, i) => {
            const { head: h, lines } = renderGrepFile(f, byFile.get(f.file) || [], withContext);
            const s0 = startOf.get(f.file);
            const at = (n) => (n == null || s0 == null ? null : s0 + n);
            const firstN = (byFile.get(f.file) || [])[0];
            if (i > 0) rows.push({ text: '', s: at(firstN && firstN.n) });
            rows.push({ text: h, s: at(firstN && firstN.n) });
            for (const l of lines) rows.push({ text: l.text, s: at(l.n) });
        });
        let cut = fit(
            head,
            rows.map((x) => x.text),
            RESERVE,
        );
        // Where the next answer should start: the first hit not rendered.
        let resume = null;
        if (cut < rows.length) {
            for (let i = cut; i < rows.length; i++) {
                if (rows[i].s != null) {
                    resume = rows[i].s;
                    break;
                }
            }
            // A cut inside trailing context: the hits are all out; resume
            // where the page itself ends.
            if (resume === null && r.next) resume = r.next.offset;
            // Leave no dangling heading or leading context for the hit the
            // next answer starts at: it repeats them.
            else while (cut > 0 && rows[cut - 1].s === resume) cut--;
            while (cut > 0 && (rows[cut - 1].text === '--' || rows[cut - 1].text === '')) cut--;
        }
        const body = rows
            .slice(0, cut)
            .map((x) => x.text)
            .join('\n');
        const tail = [];
        const restFrom = resume !== null ? resume : r.next ? r.next.offset : null;
        const paged = restFrom !== null && counts.length && counts[0].start != null;
        if (paged) {
            const restLines = total.lines - restFrom;
            const restFiles = counts.filter((f) => f.start + f.count > restFrom).length;
            const how = resume !== null ? `offset=${resume}` : `page=${r.page + 1}`;
            tail.push(
                `  [${restLines} more matching line(s) in ${restFiles} file(s) not shown — narrow with glob/paths, use ${other('lines')} for the whole picture in one answer, or continue with ${how}]`,
            );
        }
        const passed = r.files.reduce((n, f) => n + (f.passed || 0), 0);
        if (passed && resume === null) {
            tail.push(
                `  [${passed} matching line(s) passed over by max_per_file (see the file headings) — raise max_per_file, narrow with glob/paths, or use ${other('lines')}]`,
            );
        }
        if (!paged && r.unsearched) {
            // A workspace object that predates paging.
            tail.push(
                `  [stopped at ${r.matches.length} lines — ${r.unsearched} more candidate file(s) not searched; narrow with glob/paths or raise max_matches]`,
            );
        } else if (!r.matches.length) {
            tail.push(
                `  [page ${r.page}${r.offset ? ` from ${r.offset}` : ''} is past the end — ${r.pages} page(s) in all]`,
            );
        }
        tail.push(readHint);
        return `${head}\n${body}${body ? '\n' : ''}${tail.join('\n')}`;
    }

    // A host whose workspace object predates grep() (only mentions()).
    async function legacyGrep(ws, q, a) {
        const limit = Number(a.limit) || 20;
        const hits = await ws.mentions(q, { limit });
        if (!hits.length) {
            return (
                `(code_grep "${q}") — that exact text appears nowhere in ${ws.id}.` +
                ' If you expected it to exist, it does not: check the spelling, try a shorter' +
                ' distinctive fragment, or use code_ask to search by meaning instead.'
            );
        }
        return (
            `Lines containing "${q}" in ${ws.id} (${hits.length}${hits.length >= limit ? '+' : ''}):\n` +
            hits.map((h) => `  ${h.rel}:${h.line}`).join('\n') +
            `\n  [exact text only — read one with code_read "<file>:<from>-<to>" to see it in context]`
        );
    }

    async function codeGlob(ws, a) {
        const pats = listArg(a.pattern ?? a.glob ?? a.query ?? a.path);
        if (!pats) return '(code_glob) — needs `pattern`: a glob such as "src/**/*.js", "*.test.js" or "components".';
        const limit = num(a.limit) ?? 200;
        let r;
        if (typeof ws.glob === 'function') r = await ws.glob(pats, { limit });
        else {
            // A host whose workspace object predates glob(): the same match
            // over the listing it already has.
            const listed = await ws.files({});
            const m = pathMatcher({ glob: pats });
            const all = listed.files.filter((f) => !m || m(f.file));
            r = { ok: true, total: all.length, truncated: all.length > limit, files: all.slice(0, limit) };
        }
        if (!r.ok) return `(code_glob) — ${r.reason}`;
        const label = pats.join(' ');
        if (!r.total) return `(code_glob "${label}") — no file in ${ws.id} matches.`;
        return (
            `code_glob "${label}" in ${ws.id} — ${r.total} file${r.total === 1 ? '' : 's'}:\n` +
            r.files
                .map(
                    (f) =>
                        `  ${f.file}${f.lines != null ? `  (${f.lines} lines)` : f.size != null ? `  (${f.size} bytes)` : ''}${f.indexed === false ? '  [not searchable]' : ''}`,
                )
                .join('\n') +
            (r.truncated ? `\n  [showing ${r.files.length} of ${r.total} — narrow the glob or raise limit]` : '')
        );
    }

    async function codeOutline(ws, a) {
        // WHAT SHAPE IS THIS FILE. Measured (brain): with no memory of the
        // file, a task spent its last enrichment round on `code_read` of a
        // whole 456-line file — 18,115 chars — because the only way to ask
        // "what is in here" was to read it. Every piece is addressed by the
        // range syntax code_read already takes, so the pair composes: outline
        // to choose, read to see.
        const f = relOf(ws, a.file || a.path || a.query || '');
        if (!f) return '(code_outline) — needs `file`: the path to break into pieces.';
        const o = await ws.outline(f, { limit: Number(a.limit) || 60 });
        if (!o.ok) return `(code_outline "${f}") — ${toToolWords(o.reason)}`;
        const what =
            o.by === 'region' ? 'structural regions' : o.by === 'symbol' ? 'symbols' : 'chunks (no parser structure)';
        return (
            `${o.file} — ${o.lines} lines in ${o.total} ${what}  at=${o.at}\n` +
            o.symbols
                .map(
                    (s) =>
                        `  ${String(s.span).padStart(4)}L  ${s.path}  ${s.name}${s.signature ? ` ${s.signature}` : ''}${s.doc ? `  — ${s.doc}` : ''}`,
                )
                .join('\n') +
            (o.truncated ? `\n  [${o.total} pieces, showing the first ${o.symbols.length}]` : '') +
            `\n  [read any one with code_read "<file>:<from>-<to>"]`
        );
    }

    async function codeFind(ws, a) {
        const q = String(a.query || a.symbol || a.name || '');
        if (!q) return '(code_find) — needs `query`: a symbol name or a descriptive phrase.';
        const uses = !!a.uses;
        const hits = uses
            ? await ws.refs(q, { limit: Number(a.limit) || 30 })
            : await ws.find(q, { limit: Number(a.limit) || 15, kind: a.kind || null });
        if (!hits.length) return `(code_find "${q}") — no ${uses ? 'uses' : 'definitions'} found in ${ws.id}.`;
        return (
            `${uses ? 'Uses' : 'Definitions'} of "${q}" in ${ws.id}:\n` +
            hits
                .map((h) =>
                    uses
                        ? `  ${h.file}:${h.line}  in ${h.symbol}  — ${h.text}`
                        : `  ${h.kind} ${h.path || h.name}  ${h.file}:${h.lines}  (${h.span} lines)${h.signature ? `\n      ${h.signature}` : ''}${h.doc ? `\n      ${h.doc}` : ''}`,
                )
                .join('\n')
        );
    }

    async function codeRead(ws, a) {
        // `symbol` is the historical name of the argument; it takes a file and
        // a range too, so accept whichever field the caller reached for.
        const target = relOf(ws, a.symbol || a.target || a.path || a.file || '');
        if (!target) return '(code_read) — needs `symbol`: a symbol, a file path, or "file:from-to".';
        const got = await ws.read(target, {
            from: a.from ? Number(a.from) : null,
            to: a.to ? Number(a.to) : null,
        });
        return !got.ok
            ? `(code_read ${target}) — ${toToolWords(got.reason)}${got.candidates && got.candidates.length ? `\n  candidates: ${got.candidates.join(', ')}` : ''}`
            : `${got.file}:${got.lineStart}-${got.lineEnd}` +
                  `${got.kind ? ` (${got.kind} ${got.path || got.name})` : `${got.lines ? ` of ${got.lines} lines` : ''}`}` +
                  `  at=${got.at}\n${got.body}` +
                  (got.note ? `\n\n  [${got.note}]` : '');
    }

    async function codeEdit(ws, a) {
        // An edit changes the world; everything that makes it safe lives in
        // the workspace — the strict snapshot, candidate validation before any
        // write, the atomic publish, the independent readback — and the text
        // here carries the RECEIPT, not a claim that something worked.
        const singleGiven =
            a.symbol !== undefined || a.target !== undefined || a.body !== undefined || a.find !== undefined;
        const batchGiven = Array.isArray(a.edits);
        let edits = null;
        let formError = null;
        if (singleGiven && batchGiven) {
            formError = 'code_edit received BOTH a single edit and {edits} — use exactly one form, not both';
        } else if (!singleGiven && !batchGiven) {
            formError = 'code_edit needs {file, find, body}, or {symbol, body, at}, or {edits: [...]} — none was given';
        } else if (singleGiven) {
            const sym = a.symbol !== undefined ? a.symbol : a.target;
            if (a.body === undefined) formError = 'an edit needs a body';
            else if (a.find !== undefined) {
                edits = [
                    {
                        find: String(a.find),
                        body: String(a.body),
                        ...(a.within !== undefined ? { within: String(a.within) } : {}),
                    },
                ];
            } else if (sym === undefined) formError = 'a single edit needs `find` (preferred) or `symbol`';
            else edits = [{ target: relOf(ws, sym), body: String(a.body) }];
        } else if (!a.edits.length) {
            formError = 'edits must be a non-empty array of {find, body} or {target, body}';
        } else {
            edits = a.edits.map((e) => ({
                ...(e && e.find !== undefined
                    ? { find: String(e.find), ...(e.within !== undefined ? { within: String(e.within) } : {}) }
                    : { target: relOf(ws, (e && (e.target || e.symbol)) || '') }),
                body: String((e && e.body) ?? ''),
            }));
        }
        if (formError) return `(code_edit) REFUSED — ${formError}`;

        let at = a.at || a.expected_hash || null;
        if (edits.some((e) => e.find !== undefined)) {
            const file = relOf(
                ws,
                a.file || a.path || (edits.find((e) => e.target && e.target.includes('.')) || {}).target || '',
            ).replace(/:\d+(-\d+)?$/, '');
            if (!file) return '(code_edit) REFUSED — a `find` edit needs `file`: the file whose text is being replaced';
            const r = await resolveFindEdits(ws, file, edits);
            if (r.error) return `(code_edit) REFUSED — ${r.error}${r.at ? `\n  current at=${r.at}` : ''}`;
            edits = r.edits;
            // The fresh read IS the currency proof for content-addressed
            // edits; a caller-supplied at (from an older read) still wins so a
            // moved file is refused honestly.
            at = at || r.at;
        }

        const res = await ws.editBatch({ at, edits });
        // No `outcome` at all means the target was NEVER touched — a
        // pre-commit refusal, distinct from every outcome below it.
        if (!res.outcome) {
            const lines = [`(code_edit) REFUSED — ${toToolWords(res.reason)}`];
            if (res.stale && res.at) lines.push(`  the file's current at=${res.at}`);
            if (res.candidates && res.candidates.length) lines.push(`  candidates: ${res.candidates.join(', ')}`);
            if (res.newDiagnostics && res.newDiagnostics.length)
                lines.push(`  new diagnostics:\n${diagLines(res.newDiagnostics)}`);
            // EQUIP THE RETRY. A refusal that travels alone is a complaint
            // without material: observed live (brain), the turn after a
            // refused edit held the diagnostic ("</div> closes <span>") but not
            // the region, produced prose instead of a corrected edit, and the
            // task exited on a false done. So each target is re-read HERE: the
            // same answer carries the diagnostic, the CURRENT region body, and
            // the fresh `at` a corrected code_edit must cite.
            if (!res.crossFile) {
                const seen = new Set();
                for (const e of edits.slice(0, 3)) {
                    const t = String(e.target);
                    if (seen.has(t)) continue;
                    seen.add(t);
                    try {
                        const g = await ws.read(t);
                        if (g && g.ok) {
                            const body =
                                g.body.length > REGION_CAP ? `${g.body.slice(0, REGION_CAP)}\n…[truncated]` : g.body;
                            lines.push(
                                `── CURRENT ${g.file}:${g.lineStart}-${g.lineEnd} at=${g.at} (re-read after the refusal — send the corrected edit against THIS at):\n${body}`,
                            );
                        }
                    } catch {
                        /* the diagnostic still travels */
                    }
                }
            }
            return lines.join('\n');
        }
        return renderReceipt('code_edit', res);
    }

    async function codeWrite(ws, a) {
        const p = relOf(ws, a.path || a.file || '');
        if (!p) return '(code_write) — needs `path`: the workspace-relative file to create or replace.';
        if (a.content === undefined || a.content === null)
            return '(code_write) — needs `content`: the complete file text.';
        const at = a.at || a.expected_hash || null;
        // No `at` means CREATE — an existing file can only be replaced by
        // naming the version being replaced.
        const res = await ws.writeWholeFile(p, String(a.content), { expectedHash: at, create: !at });
        if (!res.outcome) {
            const lines = [`(code_write ${p}) REFUSED — ${toToolWords(res.reason)}`];
            if (/already exists/.test(String(res.reason))) {
                lines.push(
                    '  to replace it: code_read the file and pass its `at` here (or use code_edit for part of it).',
                );
            }
            if (res.stale && res.at) lines.push(`  the file's current at=${res.at}`);
            if (res.newDiagnostics && res.newDiagnostics.length)
                lines.push(`  diagnostics:\n${diagLines(res.newDiagnostics)}`);
            return lines.join('\n');
        }
        if (res.outcome === 'ok' && !res.diff) {
            const warn = failedValidators(res);
            return (
                `applied: ${res.file} (${at ? 'replaced' : 'created'}, ok) — validated, published atomically, verified by readback.  at=${res.readbackHash}` +
                (warn.length
                    ? `\n  WARNING: ${warn.join(', ')} validator(s) reported issues — not blocking, worth a look.`
                    : '')
            );
        }
        return renderReceipt('code_write', res);
    }

    const handlers = {
        code_map: codeMap,
        code_find: codeFind,
        code_grep: codeGrep,
        code_glob: codeGlob,
        code_outline: codeOutline,
        code_read: codeRead,
        code_package: codePackage,
        code_ask: codeAsk,
        code_edit: codeEdit,
        code_write: codeWrite,
    };

    // Returns the text to show the model. Never throws.
    async function render(name, args = {}) {
        const a = args && typeof args === 'object' ? args : {};
        if (!isTool(name)) return `(${name}) — not an okcode tool; known: ${TOOLS.join(', ')}`;
        try {
            const { ws, error } = await wsFor(a.workspace);
            if (error) return `(${name}) — ${error}`;
            return await handlers[name](ws, a);
        } catch (err) {
            return `(${name}) failed: ${String((err && err.message) || err).slice(0, 200)}`;
        }
    }

    // JSON-schema tool definitions, written for a model. `workspace` is
    // required only when there is a real choice to make.
    function schemas() {
        const list = known();
        const many = list.length !== 1;
        const wsProp = {
            type: 'string',
            description:
                `Workspace id, its root path, or any path inside it` +
                (list.length
                    ? ` — one of: ${list.map((w) => w.id + (w.description ? ` (${w.description})` : '')).join(', ')}`
                    : '') +
                (many ? '' : '. Optional: there is only one.'),
        };
        const req = (...r) => (many ? ['workspace', ...r] : r);
        const def = (name, description, properties, required) => ({
            name,
            description,
            parameters: { type: 'object', properties: { workspace: wsProp, ...properties }, required },
        });
        return [
            def(
                'code_map',
                'Shape of a workspace: files, languages, dependencies, biggest symbols. Start here when you do not yet know a project.',
                {},
                req(),
            ),
            def(
                'code_find',
                'Where a symbol or phrase is DEFINED (default) or USED (uses: true): name, kind, file, line range, signature per hit. Searches names, doc comments and file text. Use code_ask when you know what the code does but not its name; code_grep when the exact text must appear.',
                {
                    query: { type: 'string', description: 'A symbol name or a descriptive phrase' },
                    uses: {
                        type: 'boolean',
                        description: 'true = every use, by enclosing symbol; default = the definition',
                    },
                    kind: { type: 'string', description: 'Only this symbol kind (function, class, method, const, …)' },
                    limit: { type: 'number', description: 'Max hits (default 15; 30 for uses)' },
                },
                req('query'),
            ),
            def(
                'code_grep',
                'Search file CONTENTS like ripgrep: every matching line as `line: text`, grouped by file, with optional context lines. Literal text by default (case-insensitive); regex: true for a JavaScript regular expression; glob/paths to restrict which files. Use this INSTEAD of shelling out to grep/rg/sed: it is faster (index-accelerated, and on a remote workspace it searches where the files live), covers exactly the project (no node_modules, .gitignored or secret files), and every hit is a file:line code_read and code_edit accept. code_find/code_ask match by meaning. ' +
                    "Pick the output for the question: 'lines' (default) = WHERE, the lines themselves; 'files' = which files and how many matching lines each (rg -c); 'matches' = the DISTINCT matched strings with counts (rg -o | sort | uniq -c) — use it to ENUMERATE (\"every OKDB_[A-Z_]+ env var\", \"all event names\"): one call, the complete list. Counts in the heading are always exact; a capped answer ends with a line saying what was not shown and the page/offset that continues it — never report a partial list as complete.",
                {
                    pattern: {
                        type: 'string',
                        description: 'The text to find (literal), or a JS regex when regex is true. One line.',
                    },
                    regex: { type: 'boolean', description: 'Treat pattern as a regular expression (default false)' },
                    case_sensitive: {
                        type: 'boolean',
                        description: 'true = match case exactly; default false = ignore case',
                    },
                    glob: {
                        type: 'array',
                        items: { type: 'string' },
                        description:
                            'Only files matching these globs: "*.js", "src/**/*.ts", "*.{css,html}"; "!pattern" excludes ("!*.test.js", "!dist"). A glob without "/" matches a name at any depth.',
                    },
                    paths: {
                        type: 'array',
                        items: { type: 'string' },
                        description: 'Only under these workspace-relative directories or files ("src", "lib/util.js")',
                    },
                    context: { type: 'number', description: 'Lines of context before and after each match (default 0)' },
                    output: {
                        type: 'string',
                        enum: ['lines', 'files', 'matches'],
                        description:
                            "'lines' (default): matching lines with text; 'files': files with match counts; 'matches': distinct matched strings with occurrence counts — for enumerating names/values",
                    },
                    page: {
                        type: 'number',
                        description:
                            'Which page of a capped answer (default 1); the answer says when there is a next one',
                    },
                    offset: {
                        type: 'number',
                        description:
                            'Resume at this position (a matching line, file or distinct match, 0-based) — use the offset a cut-off answer names',
                    },
                    max_matches: {
                        type: 'number',
                        description:
                            "Page size: matching lines (default 100), or distinct strings for output:'matches' (default 500)",
                    },
                    max_per_file: {
                        type: 'number',
                        description: 'Max matching lines shown per file per page (default 10)',
                    },
                    max_files: {
                        type: 'number',
                        description: "Max files per page (default unlimited; 500 for output:'files')",
                    },
                },
                req('pattern'),
            ),
            def(
                'code_glob',
                'List workspace files whose PATHS match a glob — "src/**/*.ts", "*.test.js", "components", "!dist" — with line counts, from the index (no file is read). Use instead of find/ls in a shell to see what exists before reading.',
                {
                    pattern: {
                        type: 'array',
                        items: { type: 'string' },
                        description:
                            'One or more globs; "!pattern" excludes. Without "/" a glob matches a name at any depth; a glob matching a directory lists everything under it.',
                    },
                    limit: { type: 'number', description: 'Max files listed (default 200)' },
                },
                req('pattern'),
            ),
            def(
                'code_outline',
                'Split ONE file into named pieces (symbols, structural regions, or chunks) with line ranges and the file\'s "at". Ask before code_read when you do not yet know which part of a file you want.',
                {
                    file: { type: 'string', description: 'Workspace-relative path (as the other tools report it)' },
                    limit: { type: 'number', description: 'Max pieces (default 60)' },
                },
                req('file'),
            ),
            def(
                'code_read',
                'Read source, verified against the file as it is now: a symbol ("store.js#openStore.saveFiles", or a bare name), a whole file, or a range ("public/dashboard.html:269-339"). Every read returns at=<token>: the file\'s version, which code_edit and code_write require.',
                {
                    symbol: { type: 'string', description: 'A symbol, a file path, or file:from-to' },
                    from: { type: 'number', description: 'First line of a range' },
                    to: { type: 'number', description: 'Last line' },
                },
                req('symbol'),
            ),
            def(
                'code_package',
                "A dependency's surface — version, description, entry, exported names — without reading its source. No name = list every declared dependency.",
                { name: { type: 'string', description: 'Package name, e.g. "react" or "@scope/pkg"' } },
                req(),
            ),
            def(
                'code_ask',
                'Ask a workspace a question in your own words and get the symbols and files that answer it, with the region around each hit — when you know what the code DOES but not what it is called. Combines names, file text and semantic (embedding) search; each hit says which one found it.',
                {
                    question: { type: 'string' },
                    embedder: { type: 'string', description: 'Embedding profile to query (default: the active one)' },
                    limit: { type: 'number', description: 'Max hits (default 10)' },
                },
                req('question'),
            ),
            def(
                'code_edit',
                'Change code in ONE file. PREFERRED form: {file, find, body} — `find` is the EXACT current text being replaced, copied verbatim (it is located uniquely; line numbers are never trusted). Also: {symbol, body, at} for a named symbol or a range ("file.js:10-20"), or {edits: [...], at?} for several non-overlapping changes to one file. Symbol/range forms need the "at" of the code_read that supplied the body; a stale at is refused (with the current at), never rebased. The candidate is validated before the write; one that adds a new syntax/structure error is refused. Answers applied (with the new at and a diff) or refused (with why, and the current text to retry against).',
                {
                    at: {
                        type: 'string',
                        description:
                            '"at" from the code_read that supplied the body; stale is refused. Optional with `find` — a fresh read anchors it.',
                    },
                    file: { type: 'string', description: 'With `find`: the file to edit' },
                    find: {
                        type: 'string',
                        description:
                            'The EXACT current text being replaced, verbatim from the file (whitespace included). Include a line or two of surrounding context so a twin of the text elsewhere in the file cannot collide; for repetitive files, add `within` too.',
                    },
                    within: {
                        type: 'string',
                        description:
                            'Disambiguator for repetitive files: a short DISTINCTIVE text from ABOVE the edit site — an enclosing signature, a section header — copied verbatim. It must occur exactly once in the file; if `find` has twins, the first match at/after this landmark is edited. Never a line number.',
                    },
                    symbol: {
                        type: 'string',
                        description: 'A symbol (store.js#openStore.saveFiles) or a file range (file.html:919-931)',
                    },
                    body: {
                        type: 'string',
                        description: 'The complete replacement for what find matched / the symbol / the range',
                    },
                    edits: {
                        type: 'array',
                        description: 'Batch form: several edits to one file — each {find, body} or {target, body}',
                        items: {
                            type: 'object',
                            properties: {
                                find: { type: 'string', description: 'Exact current text being replaced (preferred)' },
                                within: {
                                    type: 'string',
                                    description: 'Unique landmark text above the edit site, verbatim',
                                },
                                target: { type: 'string', description: 'A symbol or a file range' },
                                body: { type: 'string', description: 'Complete replacement' },
                            },
                            required: ['body'],
                        },
                    },
                },
                req(),
            ),
            def(
                'code_write',
                'Create a new file, or replace an existing one ENTIRELY. Without `at` it creates (refused if the file exists); with `at` (from a code_read of that file) it replaces, refused if the file changed since. Validated before writing; published atomically. For part of a file prefer code_edit.',
                {
                    path: { type: 'string', description: 'Workspace-relative path' },
                    content: { type: 'string', description: 'The complete new file text' },
                    at: { type: 'string', description: 'Required to replace an existing file: the at from reading it' },
                },
                req('path', 'content'),
            ),
        ];
    }

    return { TOOLS, isTool, isLookup, isEdit, subjectOf, schemas, render };
}

module.exports = { createTools, TOOLS, LOOKUPS, EDITS, isTool, isLookup, isEdit, subjectOf, hitWithin, uniqueAnchors };
