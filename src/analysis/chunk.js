'use strict';
// The symbol-aware chunker: how a file is cut into pieces for embedding (and
// for an outline of a file that has no symbols). Pure — text in, offsets out.
//
// CHUNKING IS THE DESIGN DECISION HERE. okdb's built-in strategies split on
// characters, sentences or blank lines — all of which cut a function in half
// and embed the halves as if they were separate thoughts. A chunk should be a
// unit someone could act on, so this splits on SYMBOL boundaries via the same
// parser the index already uses: one chunk per function where a function fits,
// merged neighbours where they are small, nested boundaries where a function
// is too big. A hit then maps back to something addressable — `createIndex.find`
// — rather than to bytes 4096-5120.
//
// Non-JS files (markdown, json, html) get the best structural rule available
// for their shape; anything else falls back to paragraph boundaries, which is
// the right unit for prose and harmless for the rest.

const acorn = require('acorn');
const { extract } = require('./parse');

// The okdb chunk-strategy name the store registers `headedChunks` under.
const STRATEGY = 'okcode-symbols';
// The name of the preparation step. okdb's preparers and chunkers see only a
// field VALUE — never the row — so the path the chunker needs (language
// seams, chunk headers) is stamped onto the text by `prepareText` before okdb
// ever sees it (the store serves that as its own resolved field).
const PREPARER = 'okcode-content';
// The path stamp, read and stripped by the chunker. A comment, so that
// anything which ever embeds the prepared text verbatim still sees valid
// source rather than a stray header.
const MARK = '//@ ';
// Below this, a file is not worth a vector: no chunks → no vectors.
const MIN_EMBED_CHARS = 120;

// Chunk sizing, in characters. Big enough to hold a whole small function with
// its doc comment; small enough that one chunk is about one idea. It is a
// RETRIEVAL choice, not a capacity one: a chunk the size of a file matches
// everything weakly and nothing well.
const CHUNK = 1400;
const CHUNK_MAX = 2600;

// The readable part of a pipeline name (src/identity.js names pipelines).
const slug = (s) =>
    String(s)
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '_')
        .replace(/^_|_$/g, '');

// ── boundaries ──────────────────────────────────────────────────────────────

// Offsets where a new top-level thing begins. Nested boundaries are added only
// when a top-level thing is too big to be one chunk — a 400-line factory has to
// be cut somewhere, and its methods are the honest seams.
function boundaries(text) {
    let ast;
    for (const sourceType of ['script', 'module']) {
        try {
            ast = acorn.parse(text, { ecmaVersion: 'latest', sourceType, allowHashBang: true });
            break;
        } catch {
            /* try the other, then give up */
        }
    }
    if (!ast) return null;
    const out = new Set([0]);
    for (const node of ast.body) {
        out.add(node.start);
        if (node.end - node.start <= CHUNK_MAX) continue;
        // Too big: add the seams inside it. One level is enough — that is the
        // difference between a factory and its methods.
        walkInner(node, out);
    }
    out.add(text.length);
    return [...out].sort((a, b) => a - b);
}

function walkInner(node, out, depth = 0) {
    if (!node || depth > 2 || typeof node !== 'object') return;
    for (const key of Object.keys(node)) {
        const v = node[key];
        const kids = Array.isArray(v) ? v : v && v.type ? [v] : [];
        for (const kid of kids) {
            if (!kid || !kid.type) continue;
            const named =
                (kid.type === 'FunctionDeclaration' && kid.id) ||
                (kid.type === 'Property' && kid.value && /Function/.test(kid.value.type)) ||
                kid.type === 'MethodDefinition' ||
                (kid.type === 'VariableDeclarator' && kid.init && /Function/.test(kid.init.type));
            if (named) out.add(kid.start);
            walkInner(kid, out, depth + (named ? 1 : 0));
        }
    }
}

// Blank-line boundaries, for everything with no better idea.
function paragraphBoundaries(text) {
    const out = [0];
    const re = /\n\s*\n/g;
    let m;
    while ((m = re.exec(text))) out.push(m.index + m[0].length);
    out.push(text.length);
    return out;
}

// Markdown: headings are the strongest seam a document has, and blank lines
// are the next.
function markdownBoundaries(text) {
    const out = new Set(paragraphBoundaries(text));
    for (const m of text.matchAll(/^#{1,6} .*$/gm)) out.add(m.index);
    return [...out].sort((a, b) => a - b);
}

// HTML: element starts, and — the point of this — the JavaScript inside
// <script>. okdb's own `html` preparer strips script and style blocks
// entirely, which is right for a content site and exactly wrong here: a page
// whose logic IS a script block would lose the only part anyone searches for.
// Keep every byte, cut in better places.
function htmlBoundaries(text) {
    const out = new Set([0, text.length]);
    for (const m of text.matchAll(/\n[ \t]*<(?![/!])[a-zA-Z][^>]*>/g)) out.add(m.index + 1);
    for (const m of text.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)) {
        const openEnd = m.index + m[0].indexOf('>') + 1;
        out.add(m.index);
        out.add(openEnd);
        out.add(m.index + m[0].length);
        const inner = boundaries(m[1]);
        if (inner) for (const b of inner) out.add(openEnd + b);
    }
    return [...out].sort((a, b) => a - b);
}

// JSON: members of the top-level object or array (and one level below).
// Without this a formatted JSON file — almost no blank lines — was nearly all
// arbitrary 2,600-character cuts. A scanner rather than a parse: the file may
// be huge, and only the shallow commas are wanted.
function jsonBoundaries(text) {
    const out = new Set([0, text.length]);
    let depth = 0;
    let inString = false;
    let escaped = false;
    for (let i = 0; i < text.length; i++) {
        const ch = text[i];
        if (inString) {
            if (escaped) escaped = false;
            else if (ch === '\\') escaped = true;
            else if (ch === '"') inString = false;
            continue;
        }
        // Two levels deep, not one. A lockfile is a single depth-1 member
        // holding ninety kilobytes. Extra seams cost nothing: the merge pass
        // joins them back up to CHUNK, so a finer boundary set can only ever
        // improve where the cuts land.
        if (ch === '"') inString = true;
        else if (ch === '{' || ch === '[') {
            depth++;
            if (depth <= 2) out.add(i + 1);
        } else if (ch === '}' || ch === ']') depth--;
        else if (ch === ',' && depth <= 2) out.add(i + 1);
    }
    return [...out].sort((a, b) => a - b);
}

// Which seams to look for, by what the file is. JavaScript keeps the parser;
// everything else gets the best structural rule available for its shape.
function boundariesFor(rel, text) {
    const ext = (String(rel || '')
        .toLowerCase()
        .match(/\.[^./\\]+$/) || [''])[0];
    if (['.js', '.mjs', '.cjs', '.jsx'].includes(ext)) return boundaries(text);
    if (['.html', '.htm', '.ok', '.svg', '.xml'].includes(ext)) return htmlBoundaries(text);
    if (['.json', '.jsonc'].includes(ext)) return jsonBoundaries(text);
    if (['.md', '.markdown'].includes(ext)) return markdownBoundaries(text);
    return null;
}

// Greedily merge adjacent segments up to CHUNK, never exceeding CHUNK_MAX, and
// hard-split anything that is one indivisible oversized segment (a minified
// line, a giant literal).
function fromBoundaries(text, marks) {
    const chunks = [];
    let i = 0;
    while (i < marks.length - 1) {
        const start = marks[i];
        let j = i + 1;
        while (j < marks.length - 1 && marks[j] - start < CHUNK) j++;
        let end = marks[j];
        if (end - start > CHUNK_MAX && j > i + 1) {
            j = i + 1;
            end = marks[j];
        }
        // One indivisible oversized segment. Cut it at the LAST LINE BREAK
        // before the cap rather than at the exact byte: a chunk that begins
        // mid-word embeds as noise and reads as garbage in a hit, while a
        // line-start successor usually begins at a statement or a comment.
        // The byte cap remains the fallback for a single line longer than
        // CHUNK_MAX (minified source). (An `if`, not a loop: the recursion is
        // the loop.)
        if (end - start > CHUNK_MAX) {
            const nl = text.lastIndexOf('\n', start + CHUNK_MAX);
            const cut = nl > start + CHUNK / 2 ? nl + 1 : start + CHUNK_MAX;
            chunks.push({ text: text.slice(start, cut), start, end: cut });
            const rest = marks.slice(i);
            rest[0] = cut;
            return chunks.concat(fromBoundaries(text, rest));
        }
        const slice = text.slice(start, end);
        if (slice.trim()) chunks.push({ text: slice, start, end });
        i = j;
    }
    return chunks;
}

function chunkCode(text, rel = null) {
    if (!text) return [];
    // With a filename, the language decides the seams. Without one (a caller
    // passing raw text), try the parser and fall back to blank lines.
    const marks = (rel ? boundariesFor(rel, text) : null) || boundaries(text) || paragraphBoundaries(text);
    return fromBoundaries(text, marks);
}

// ── regions: the titled outline of a text that has no symbols ───────────────
//
// The same seams, promoted to a MENU a reader chooses from: a markdown or
// HTML document by its headings, a JSON file by its members, extracted pages
// by their form feeds, prose by its paragraphs, a log by line ranges. Each
// region is `{ kind, title, lineStart, lineEnd }` (1-based, inclusive; a
// heading also has `level`) and is addressed the way any range is
// (`file:from-to`), a heading also as `file#Title`. Code is not handled here
// (null): its outline is its symbols, or chunks cut at its parser's seams.
//
// Headings nest: a heading's region runs to the next heading of the same or a
// higher level, so it holds its subsections, and reading `file#Title` reads
// the whole section. Everything else is a flat partition.

// Prose paragraphs are merged up to this many lines; a block longer than it
// (a log with no blank lines) is cut into runs of LOG_LINES.
const REGION_LINES = 80;
const LOG_LINES = 200;

const CODE_EXT = new Set([
    '.js',
    '.mjs',
    '.cjs',
    '.jsx',
    '.ts',
    '.tsx',
    '.css',
    '.scss',
    '.sh',
    '.bash',
    '.ps1',
    '.sql',
]);

const titleOf = (s) =>
    String(s || '')
        .replace(/\s+/g, ' ')
        .trim()
        .slice(0, 100);

// The lines, and the last one that holds text (a trailing newline does not
// open a line worth offering).
function lineTable(text) {
    const lines = text.split('\n');
    let last = lines.length;
    while (last > 1 && !lines[last - 1].trim()) last--;
    return { lines, last };
}

// Trim blank lines off the end of a span (never past its start).
function tight(lines, a, b) {
    while (b > a && !lines[b - 1].trim()) b--;
    return b;
}

function firstLine(lines, a, b) {
    for (let i = a; i <= b; i++) if (lines[i - 1].trim()) return lines[i - 1];
    return '';
}

// Headings → nested sections, plus the preamble before the first one.
function headingRegions(lines, last, heads) {
    if (!heads.length) return null;
    const out = [];
    if (heads[0].line > 1) {
        let a = 1;
        while (a < heads[0].line && !lines[a - 1].trim()) a++;
        if (a < heads[0].line) {
            const b = tight(lines, a, heads[0].line - 1);
            out.push({ kind: 'section', title: titleOf(lines[a - 1]), lineStart: a, lineEnd: b });
        }
    }
    heads.forEach((h, i) => {
        const next = heads.slice(i + 1).find((o) => o.level <= h.level);
        const b = tight(lines, h.line, next ? next.line - 1 : last);
        out.push({ kind: 'heading', title: h.title, level: h.level, lineStart: h.line, lineEnd: b });
    });
    return out;
}

// ATX headings (`## Title`), outside fenced code.
function markdownHeads(lines) {
    const heads = [];
    let fence = null;
    lines.forEach((l, i) => {
        const f = /^\s{0,3}(`{3,}|~{3,})/.exec(l);
        if (f) {
            if (!fence) fence = f[1][0];
            else if (f[1][0] === fence) fence = null;
            return;
        }
        if (fence) return;
        const m = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/.exec(l);
        if (m && m[2]) heads.push({ line: i + 1, level: m[1].length, title: titleOf(m[2]) });
    });
    return heads;
}

function htmlHeads(lines) {
    const heads = [];
    lines.forEach((l, i) => {
        const m = /<h([1-6])\b[^>]*>(.*?)(<\/h\1>|$)/i.exec(l);
        if (!m) return;
        const title = titleOf(m[2].replace(/<[^>]*>/g, ''));
        if (title) heads.push({ line: i + 1, level: Number(m[1]), title });
    });
    return heads;
}

// The members of a JSON document's top-level object (by key) or array (by
// index, small neighbours grouped up to maxLines). A scanner, not a parse:
// the file may be huge and only the shallow structure is wanted. null when
// there is no multi-line top level to cut.
function jsonRegions(text, lines, last, maxLines) {
    const starts = [];
    let depth = 0;
    let inString = false;
    let escaped = false;
    let top = null;
    let line = 1;
    let expect = false;
    for (let i = 0; i < text.length; i++) {
        const ch = text[i];
        if (ch === '\n') line++;
        if (inString) {
            if (escaped) escaped = false;
            else if (ch === '\\') escaped = true;
            else if (ch === '"') inString = false;
            continue;
        }
        if (/\s/.test(ch)) continue;
        if (expect && depth === 1 && ch !== '}' && ch !== ']') {
            let key = null;
            if (top === '{' && ch === '"') {
                const m = /^"((?:[^"\\]|\\.)*)"/.exec(text.slice(i, i + 400));
                key = m ? m[1] : null;
            }
            starts.push({ line, key });
        }
        expect = false;
        if (ch === '"') inString = true;
        else if (ch === '{' || ch === '[') {
            depth++;
            if (depth === 1) {
                top = ch;
                expect = true;
            }
        } else if (ch === '}' || ch === ']') depth--;
        else if (ch === ',' && depth === 1) expect = true;
    }
    if (!top || starts.length < 2 || starts[starts.length - 1].line === starts[0].line) return null;
    const spans = starts.map((s, i) => {
        const end = i + 1 < starts.length ? Math.max(s.line, starts[i + 1].line - 1) : Math.max(s.line, last - 1);
        return { key: s.key, index: i, lineStart: s.line, lineEnd: tight(lines, s.line, end) };
    });
    if (top === '{') {
        return spans.map((s) => ({
            kind: 'member',
            title: titleOf(s.key ?? `[${s.index}]`),
            lineStart: s.lineStart,
            lineEnd: s.lineEnd,
        }));
    }
    const out = [];
    for (const s of spans) {
        const prior = out[out.length - 1];
        if (prior && s.lineEnd - prior.lineStart + 1 <= maxLines) {
            prior.lineEnd = s.lineEnd;
            prior.to = s.index;
        } else out.push({ kind: 'member', from: s.index, to: s.index, lineStart: s.lineStart, lineEnd: s.lineEnd });
    }
    return out.map(({ from, to, ...r }) => ({ ...r, title: from === to ? `[${from}]` : `[${from}]–[${to}]` }));
}

// Pages, where a text extracted from a paged document kept its form feeds.
function pageRegions(lines, last) {
    const out = [];
    let a = 1;
    const push = (b) => {
        let s = a;
        while (s < b && !lines[s - 1].replace(/\f/g, '').trim()) s++;
        b = tight(lines, s, b);
        if (lines.slice(s - 1, b).some((l) => l.replace(/\f/g, '').trim())) {
            out.push({ kind: 'page', title: `page ${out.length + 1}`, lineStart: s, lineEnd: b });
        }
    };
    for (let i = 2; i <= last; i++) {
        if (!lines[i - 1].includes('\f')) continue;
        // the page break opens this line: the page before ends above it
        push(i - 1);
        a = i;
    }
    push(last);
    return out.length > 1 ? out : null;
}

// Prose and logs: blank-line paragraphs merged up to maxLines; a block longer
// than that (a log with no blank lines) cut into runs of logLines.
function paragraphRegions(lines, last, maxLines, logLines) {
    const blocks = [];
    let a = null;
    for (let i = 1; i <= last + 1; i++) {
        const blank = i > last || !lines[i - 1].trim();
        if (!blank && a === null) a = i;
        if (blank && a !== null) {
            blocks.push([a, i - 1]);
            a = null;
        }
    }
    const out = [];
    let run = null;
    const close = () => {
        if (run) {
            const title = titleOf(firstLine(lines, run[0], run[1]));
            out.push({ kind: 'paragraph', title, lineStart: run[0], lineEnd: run[1] });
        }
        run = null;
    };
    for (const [s, e] of blocks) {
        if (e - s + 1 > maxLines) {
            close();
            for (let x = s; x <= e; x += logLines) {
                const y = Math.min(e, x + logLines - 1);
                out.push({ kind: 'lines', title: titleOf(firstLine(lines, x, y)), lineStart: x, lineEnd: y });
            }
            continue;
        }
        if (run && e - run[0] + 1 <= maxLines) run[1] = e;
        else {
            close();
            run = [s, e];
        }
    }
    close();
    return out;
}

function regions(rel, text, { maxLines = REGION_LINES, logLines = LOG_LINES } = {}) {
    if (typeof text !== 'string') return [];
    const ext = (String(rel || '')
        .toLowerCase()
        .match(/\.[^./\\]+$/) || [''])[0];
    if (CODE_EXT.has(ext)) return null;
    if (!text.trim()) return [];
    const { lines, last } = lineTable(text);
    const max = Math.max(1, maxLines);
    if (['.md', '.markdown'].includes(ext)) {
        const r = headingRegions(lines, last, markdownHeads(lines));
        if (r) return r;
    }
    if (['.html', '.htm', '.xml', '.svg', '.ok'].includes(ext)) {
        const r = headingRegions(lines, last, htmlHeads(lines));
        if (r) return r;
    }
    if (['.json', '.jsonc'].includes(ext)) {
        const r = jsonRegions(text, lines, last, max);
        if (r) return r;
    }
    if (text.includes('\f')) {
        const r = pageRegions(lines, last);
        if (r) return r;
    }
    return paragraphRegions(lines, last, max, Math.max(1, logLines));
}

// The section a heading title names, in one text: the heading region |
// { ambiguous: [regions] } | null. Matched on the title as written, then with
// case ignored; leading #s and runs of spaces do not count, so `## Install`
// and `install` name the same section.
function sectionOf(rel, text, title) {
    const all = (regions(rel, text) || []).filter((r) => r.kind === 'heading');
    if (!all.length) return null;
    const want = titleOf(String(title || '').replace(/^\s*#+\s*/, ''));
    let hits = all.filter((r) => r.title === want);
    if (!hits.length) hits = all.filter((r) => r.title.toLowerCase() === want.toLowerCase());
    if (!hits.length) return null;
    if (hits.length > 1) return { ambiguous: hits };
    return hits[0];
}

// ── the embedding text ──────────────────────────────────────────────────────

// What okdb embeds for one file: the path stamped on the front (the chunker is
// handed text and nothing else — no record, no key — and that one line is what
// lets each chunk carry its own address), then the file text. Empty for a file
// that is not indexed as text or is too short to be worth a vector.
function prepareText(rel, text, { indexed = true } = {}) {
    if (!indexed || typeof text !== 'string' || text.length < MIN_EMBED_CHARS) return '';
    return `${MARK}${rel || ''}\n${text}`;
}

// How many characters `prepareText` put in front of the file text — the
// amount to subtract from a chunk offset (which okdb records against the
// PREPARED text) to get an offset into the file.
const markLength = (rel) => MARK.length + String(rel || '').length + 1;

// Each chunk is prefixed with where it came from — `// src/store.js ›
// openStore.saveFiles` and that symbol's doc line.
//
// Measured, not assumed. On the same 1,093 chunks and the same ten questions:
// qwen3-embedding:8b went 0.461 → 0.484 MRR with headers, bge-m3 0.378 →
// 0.433. A raw chunk out of the middle of a file is anonymous — the words
// that say what it IS live in the path and in the comment above the function,
// and both are usually outside the slice. (It does not help every model:
// qwen3-0.6b went 0.339 → 0.318. A model too small to use the context is only
// distracted by more of it.)
function headedChunks(text) {
    let rel = null;
    let offset = 0;
    if (text.startsWith(MARK)) {
        const nl = text.indexOf('\n');
        rel = text.slice(MARK.length, nl);
        offset = nl + 1;
        text = text.slice(offset);
    }
    const chunks = chunkCode(text, rel);
    if (!rel) return chunks;
    let symbols = [];
    try {
        symbols = extract(rel, text).symbols || [];
    } catch {
        symbols = [];
    }
    // char offset -> line, once
    const lineAt = [0];
    for (let i = 0; i < text.length; i++) if (text[i] === '\n') lineAt.push(i + 1);
    const lineOf = (pos) => {
        let lo = 0;
        let hi = lineAt.length - 1;
        while (lo < hi) {
            const mid = (lo + hi + 1) >> 1;
            if (lineAt[mid] <= pos) lo = mid;
            else hi = mid - 1;
        }
        return lo + 1;
    };
    return chunks.map((c) => {
        const line = lineOf(c.start);
        let owner = null;
        for (const s of symbols) {
            if (line < s.lineStart || line > s.lineEnd) continue;
            if (!owner || s.lineEnd - s.lineStart < owner.lineEnd - owner.lineStart) owner = s;
        }
        const head =
            `// ${rel}${owner ? ` › ${owner.path || owner.name}` : ''}\n` +
            (owner && owner.doc ? `// ${owner.doc.slice(0, 200)}\n` : '');
        // Offsets stay relative to the PREPARED text, marker included, or the
        // manifest's positions stop matching what okdb sliced.
        return { text: `${head}\n${c.text}`, start: c.start + offset, end: c.end + offset };
    });
}

module.exports = {
    CHUNK,
    CHUNK_MAX,
    MARK,
    STRATEGY,
    PREPARER,
    MIN_EMBED_CHARS,
    slug,
    boundaries,
    walkInner,
    paragraphBoundaries,
    markdownBoundaries,
    htmlBoundaries,
    jsonBoundaries,
    boundariesFor,
    fromBoundaries,
    chunkCode,
    regions,
    sectionOf,
    REGION_LINES,
    LOG_LINES,
    headedChunks,
    prepareText,
    markLength,
};
