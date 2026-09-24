'use strict';
// The pure half of grep and glob: path filters, the line matcher, and the
// rules for when a literal may be pre-filtered by an index. No I/O here — the
// workspace (src/workspace.js) decides which files to read and through what.

// ── globs ───────────────────────────────────────────────────────────────
// rg-flavoured globs over workspace-relative paths:
//   *      anything but '/'          **    any number of directories
//   ?      one character but '/'     [..]  a class ([!..] negates)
//   {a,b}  alternatives (nestable)   !pat  (in a list) exclude
// A glob with no '/' matches a NAME at any depth (`*.js`, `test`); one with a
// '/' is anchored at the workspace root (`src/**/*.ts`; a leading '/' or './'
// is ignored). Like rg, a glob that matches a DIRECTORY takes everything under
// it: `-g test` includes files below any `test/` directory, `!node_modules`
// excludes them.
function globBody(glob) {
    let out = '';
    let depth = 0;
    for (let i = 0; i < glob.length; i++) {
        const c = glob[i];
        if (c === '*') {
            if (glob[i + 1] === '*') {
                const atStart = i === 0 || glob[i - 1] === '/';
                const next = glob[i + 2];
                i++;
                if (atStart && next === '/') {
                    out += '(?:[^/]*/)*';
                    i++;
                } else if (atStart && next === undefined) out += '.*';
                else out += '[^/]*';
            } else out += '[^/]*';
        } else if (c === '?') out += '[^/]';
        else if (c === '[') {
            const end = glob.indexOf(']', i + 2);
            if (end < 0) out += '\\[';
            else {
                let cls = glob.slice(i + 1, end);
                if (cls[0] === '!') cls = `^${cls.slice(1)}`;
                out += `[${cls.replace(/\\/g, '\\\\')}]`;
                i = end;
            }
        } else if (c === '{') {
            depth++;
            out += '(?:';
        } else if (c === '}' && depth > 0) {
            depth--;
            out += ')';
        } else if (c === ',' && depth > 0) out += '|';
        else if (c === '\\' && i + 1 < glob.length) out += escapeRe(glob[++i]);
        else out += escapeRe(c);
    }
    while (depth-- > 0) out += ')';
    return out;
}

function globToRegExp(glob) {
    let g = String(glob).trim().replace(/\\/g, '/');
    g = g.replace(/^\.\//, '').replace(/^\/+/, '');
    const trailingDir = g.endsWith('/');
    g = g.replace(/\/+$/, '');
    if (!g) return /^.*$/;
    const anchored = g.includes('/');
    // Unanchored: the name may sit at any depth; a match on a directory takes
    // its subtree (the caller tests every ancestor too — see pathMatcher).
    const head = anchored ? '^' : '^(?:.*/)?';
    return new RegExp(`${head}${globBody(g)}${trailingDir ? '(?=/)' : ''}$`);
}

// A path and each of its ancestor directories, deepest last: a glob that
// matches a directory takes everything under it.
function prefixesOf(rel) {
    const parts = rel.split('/');
    const out = [];
    for (let i = 1; i <= parts.length; i++) out.push(parts.slice(0, i).join('/'));
    return out;
}

// (rel) => boolean for a list of globs (strings, or one comma-free string)
// and a list of directory/file `paths`. Includes are OR'ed; `!` globs
// exclude; with only excludes, everything else is in. Returns null when
// nothing filters, so the caller can skip the work.
function pathMatcher({ glob = null, paths = null } = {}) {
    const globs = [].concat(glob == null ? [] : glob).flatMap((g) => (typeof g === 'string' ? [g] : []));
    const inc = [];
    const exc = [];
    for (const raw of globs) {
        const g = raw.trim();
        if (!g) continue;
        if (g.startsWith('!')) {
            // A trailing '/' marks a directory-only glob; an exclusion of a
            // directory takes its subtree either way.
            exc.push(globToRegExp(g.slice(1).replace(/\/+$/, '')));
        } else inc.push(globToRegExp(g.replace(/\/+$/, '')));
    }
    const roots = []
        .concat(paths == null ? [] : paths)
        .map((p) =>
            String(p || '')
                .replace(/\\/g, '/')
                .replace(/^\.(\/|$)/, '')
                .replace(/^\/+|\/+$/g, ''),
        )
        .filter((p, i, a) => a.indexOf(p) === i);
    const anyRoot = roots.includes('');
    if (!inc.length && !exc.length && (!roots.length || anyRoot)) return null;
    const hits = (re, rel) => prefixesOf(rel).some((p) => re.test(p));
    return (rel) => {
        if (roots.length && !anyRoot && !roots.some((r) => rel === r || rel.startsWith(`${r}/`))) return false;
        if (inc.length && !inc.some((re) => hits(re, rel))) return false;
        if (exc.some((re) => hits(re, rel))) return false;
        return true;
    };
}

// ── the line matcher ────────────────────────────────────────────────────
const REGEX_META = /[\\^$.|?*+()[\]{}]/;
const escapeRe = (s) => s.replace(/[\\^$.|?*+()[\]{}\/-]/g, '\\$&');

// `caseSensitive`: true | false | 'smart' (sensitive only when the pattern
// has an uppercase letter, as rg's --smart-case).
function resolveCase(pattern, caseSensitive) {
    if (caseSensitive === 'smart') return /\p{Lu}/u.test(pattern);
    return !!caseSensitive;
}

// { test(line) → column (0-based) | -1, literal, ignoreCase, source }.
// Throws a GREP_BAD_PATTERN error for an invalid regex or an empty pattern.
function compileMatcher(pattern, { regex = false, caseSensitive = false } = {}) {
    const p = String(pattern ?? '');
    const bad = (msg) => {
        const err = new Error(msg);
        err.code = 'GREP_BAD_PATTERN';
        return err;
    };
    if (!p) throw bad('an empty pattern matches every line — give the text to look for');
    if (/[\n]/.test(p)) throw bad('a pattern matches within ONE line — search for one line of it');
    const ignoreCase = !resolveCase(p, caseSensitive);
    // A "regex" with no metacharacter in it is a literal, and gets the
    // literal's accelerators.
    const literal = !regex ? p : REGEX_META.test(p) ? null : p;
    let re;
    if (literal !== null) {
        // No `u` flag: an ASCII pattern then folds case over ASCII only —
        // the same bytes a C-locale `grep -i` matches, which is what makes a
        // remote pre-filter sound.
        re = new RegExp(escapeRe(literal), ignoreCase ? 'i' : '');
    } else {
        try {
            re = new RegExp(p, ignoreCase ? 'iu' : 'u');
        } catch {
            try {
                re = new RegExp(p, ignoreCase ? 'i' : '');
            } catch (err) {
                throw bad(`invalid regular expression: ${err.message}`);
            }
        }
    }
    const lower = literal !== null && ignoreCase ? literal.toLowerCase() : null;
    const test =
        literal !== null && !ignoreCase
            ? (line) => line.indexOf(literal)
            : literal !== null && /^[\x00-\x7f]*$/.test(literal)
              ? (line) => (/^[\x00-\x7f]*$/.test(line) ? line.toLowerCase().indexOf(lower) : searchRe(re, line))
              : (line) => searchRe(re, line);
    // Every non-empty matched string of a line, left to right (rg -o).
    const g = new RegExp(re.source, `${re.flags}g`);
    const all = (line) => {
        const out = [];
        g.lastIndex = 0;
        let m;
        while ((m = g.exec(line)) !== null) {
            if (m[0] === '') {
                g.lastIndex++;
                continue;
            }
            out.push(m[0]);
        }
        return out;
    };
    return { test, all, literal, ignoreCase, regex: literal === null, source: p };
}

function searchRe(re, line) {
    const m = re.exec(line);
    return m ? m.index : -1;
}

// A line as shown: CR dropped, and a minified 20 kB line cut to a window
// around the match rather than quoted whole.
const LINE_CAP = 300;
function clip(line, col = 0) {
    if (line.length <= LINE_CAP) return line;
    const from = Math.max(0, Math.min(col - 80, line.length - LINE_CAP));
    return `${from > 0 ? '…' : ''}${line.slice(from, from + LINE_CAP)}…`;
}

// Every matching line of one text, with context. Returns
// { matches: [{ line, col, text, before, after, n }], count } where `count` is
// the number of matching lines (it keeps counting past `maxPerFile`) and `n`
// a match's 0-based ordinal among them. `skip` passes over the first matches
// (counted, not returned) — a page that starts mid-file. `maxPerFile: 0`
// only counts.
function matchText(
    text,
    matcher,
    { context = 0, before = context, after = context, maxPerFile = Infinity, skip = 0 } = {},
) {
    const lines = String(text).split('\n');
    // A trailing newline ends the last line; it does not start another.
    if (lines.length > 1 && lines[lines.length - 1] === '') lines.pop();
    const strip = (l) => (l.endsWith('\r') ? l.slice(0, -1) : l);
    const out = [];
    let count = 0;
    for (let i = 0; i < lines.length; i++) {
        const line = strip(lines[i]);
        const col = matcher.test(line);
        if (col < 0) continue;
        const n = count++;
        if (n < skip || out.length >= maxPerFile) continue;
        const b = [];
        for (let j = Math.max(0, i - before); j < i; j++) b.push(clip(strip(lines[j])));
        const a = [];
        for (let j = i + 1; j <= Math.min(lines.length - 1, i + after); j++) a.push(clip(strip(lines[j])));
        out.push({ line: i + 1, col: col + 1, text: clip(line, col), before: b, after: a, n });
    }
    return { matches: out, count };
}

// rg -o | sort | uniq -c over one text: adds every matched string to `tally`
// (Map<string, { count, lines, files, first }>) and returns the number of
// matching lines. `file` names the text, for the per-string file count.
function tallyText(text, matcher, tally, file = null) {
    const lines = String(text).split('\n');
    if (lines.length > 1 && lines[lines.length - 1] === '') lines.pop();
    let count = 0;
    for (let raw of lines) {
        if (raw.endsWith('\r')) raw = raw.slice(0, -1);
        if (matcher.test(raw) < 0) continue;
        const found = matcher.all(raw);
        if (!found.length) continue;
        count++;
        const seenOnLine = new Set();
        for (const m of found) {
            let t = tally.get(m);
            if (!t) {
                t = { count: 0, lines: 0, files: 0, first: file, lastFile: null };
                tally.set(m, t);
            }
            t.count++;
            if (!seenOnLine.has(m)) {
                seenOnLine.add(m);
                t.lines++;
            }
            if (t.lastFile !== file) {
                t.lastFile = file;
                t.files++;
            }
        }
    }
    return count;
}

// ── index pre-filter rules ──────────────────────────────────────────────
// Which full-text terms EVERY file containing `literal` must have, given the
// content index's tokenizer (split on non-[letter|digit|_], lowercased, tokens
// of <2 chars dropped, >maxLen truncated, stopwords dropped) queried in
// PREFIX mode. A term is only returned when missing it would be impossible:
//   - a word run bounded on BOTH sides by non-word characters inside the
//     literal is a whole token in the file;
//   - a run bounded on the LEFT only (the literal ends mid-word) is a token
//     PREFIX in the file — unless a stopword (never indexed) starts with it;
//   - a run open on the left (the literal starts mid-word) may be the tail of
//     a longer token, which no prefix query can find, so it gives no term.
// Only ASCII runs are used: case folding of anything else is not guaranteed
// to agree between the tokenizer and the matcher. [] = the index cannot help.
const WORD = /[\p{L}\p{N}_]+/gu;
function indexTerms(literal, { stopwords = [], minLen = 2, maxLen = 64 } = {}) {
    if (typeof literal !== 'string' || !literal) return [];
    const stop = stopwords.map((s) => String(s).toLowerCase());
    const out = [];
    for (const m of literal.matchAll(WORD)) {
        const run = m[0];
        const leftBounded = m.index > 0;
        const rightBounded = m.index + run.length < literal.length;
        if (!leftBounded) continue;
        if (!/^[A-Za-z0-9_]+$/.test(run)) continue;
        const t = run.toLowerCase().slice(0, maxLen);
        if (t.length < minLen || stop.includes(t)) continue;
        if (!rightBounded && stop.some((s) => s !== t && s.startsWith(t))) continue;
        out.push(t);
    }
    return [...new Set(out)];
}

// A list argument as a caller may send it: an array, or one string with
// commas — split on commas OUTSIDE braces, so `*.{js,ts}` stays whole. null
// when nothing is left.
function splitList(v) {
    if (v == null || v === '' || v === true) return null;
    const out = [];
    for (const s of [].concat(v).map((x) => String(x))) {
        let depth = 0;
        let cur = '';
        for (const c of s) {
            if (c === '{') depth++;
            else if (c === '}' && depth > 0) depth--;
            if (c === ',' && depth === 0) {
                out.push(cur);
                cur = '';
            } else cur += c;
        }
        out.push(cur);
    }
    const clean = out.map((x) => x.trim()).filter(Boolean);
    return clean.length ? clean : null;
}

module.exports = { globToRegExp, pathMatcher, compileMatcher, matchText, tallyText, indexTerms, splitList, clip };
