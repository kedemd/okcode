'use strict';
// Tag balance inside template literals — the gate acorn and `node --check`
// cannot be: markup lives in a STRING, so a template that has lost a closing
// tag is still perfectly valid JavaScript.
//
// Measured, 2026-08-17: an agent's code_edit replaced a five-line range one line
// off, which duplicated `<div class="rail-head">` and dropped `</if>` in
// topics-rail.ok.js. Both JS validators passed, the file was published, and
// four screenshot passes then reported the task done. Nothing mechanical
// disagreed. This is that mechanical disagreement.
//
// Deliberately conservative, and safe by construction rather than by being
// clever: the result feeds validate.js, whose compareDiagnostics matches a
// candidate's diagnostics STRUCTURALLY against the baseline's. So a systematic
// quirk of this checker appears identically on both sides and cancels — only an
// imbalance the edit INTRODUCED is ever a new diagnostic. A file this checker
// misreads is therefore still editable; it just keeps its own misreading.
//
// Offsets are absolute in the file, so a diagnostic here addresses exactly like
// one from acorn.

const { lineColAt } = require('./text');

const BACKTICK = '`';
const BACKSLASH = '\\';

// HTML void elements: no closing tag, and these templates never self-close them.
const VOID = new Set([
    'area',
    'base',
    'br',
    'col',
    'embed',
    'hr',
    'img',
    'input',
    'link',
    'meta',
    'param',
    'source',
    'track',
    'wbr',
]);

// Find template literal bodies, and the interpolation ranges inside each.
//
// The body is ONE span, not one per segment between interpolations: markup pairs
// across an interpolation all the time (`<div>${x}</div>`), so splitting there
// would report the opener and the closer as two unbalanced fragments. Instead
// each `${…}` is recorded as an opaque range the tag scan skips — which also
// means markup written inside an interpolation is neither checked nor mistaken
// for the body's own.
//
// A nested template inside an interpolation is therefore NOT checked. That is a
// deliberate limit: this checker's job is to refuse an edit that BREAKS balance,
// and compareDiagnostics only ever acts on a NEW diagnostic, so missing one
// costs nothing while a false positive would make a valid file uneditable.
function templateSpans(text) {
    const spans = [];
    let i = 0;
    let quote = null; // ' or " — outside a template body only
    while (i < text.length) {
        const c = text[i];
        if (quote) {
            if (c === BACKSLASH) {
                i += 2;
                continue;
            }
            if (c === quote) quote = null;
            i++;
            continue;
        }
        if (c === '/' && text[i + 1] === '/') {
            const nl = text.indexOf('\n', i);
            i = nl === -1 ? text.length : nl;
            continue;
        }
        if (c === '/' && text[i + 1] === '*') {
            const end = text.indexOf('*/', i + 2);
            i = end === -1 ? text.length : end + 2;
            continue;
        }
        if (c === "'" || c === '"') {
            quote = c;
            i++;
            continue;
        }
        if (c !== BACKTICK) {
            i++;
            continue;
        }

        // A template body: walk to its unescaped closing backtick, recording
        // interpolation ranges (brace-balanced, quote- and nested-template-aware).
        const start = i + 1;
        const interpolations = [];
        let j = start;
        while (j < text.length) {
            const ch = text[j];
            if (ch === BACKSLASH) {
                j += 2;
                continue;
            }
            if (ch === BACKTICK) break;
            if (ch === '$' && text[j + 1] === '{') {
                const from = j;
                j += 2;
                let depth = 1;
                let q = null;
                let tmpl = 0;
                while (j < text.length && depth > 0) {
                    const d = text[j];
                    if (d === BACKSLASH) {
                        j += 2;
                        continue;
                    }
                    if (q) {
                        if (d === q) q = null;
                        j++;
                        continue;
                    }
                    if (tmpl > 0) {
                        // Inside a nested template only its own backtick ends it;
                        // its braces are text, not this expression's.
                        if (d === BACKTICK) tmpl--;
                        j++;
                        continue;
                    }
                    if (d === "'" || d === '"') {
                        q = d;
                        j++;
                        continue;
                    }
                    if (d === BACKTICK) {
                        tmpl++;
                        j++;
                        continue;
                    }
                    if (d === '{') depth++;
                    else if (d === '}') depth--;
                    j++;
                }
                interpolations.push({ start: from, end: j });
                continue;
            }
            j++;
        }
        spans.push({ start, end: Math.min(j, text.length), interpolations });
        i = j + 1;
    }
    // Only a body that actually contains an opening tag can be unbalanced.
    return spans.filter((sp) => {
        if (sp.end <= sp.start) return false;
        let body = '';
        let at = sp.start;
        for (const it of sp.interpolations) {
            body += text.slice(at, it.start);
            at = it.end;
        }
        body += text.slice(at, sp.end);
        return /<[A-Za-z][\w-]*[\s/>]/.test(body);
    });
}

// Walk one template body, absolute offsets, and return diagnostics.
function checkSpan(text, span) {
    const out = [];
    const stack = [];
    const interps = span.interpolations || [];
    const skipTo = (pos) => {
        const it = interps.find((x) => pos >= x.start && pos < x.end);
        return it ? it.end : null;
    };
    let i = span.start;
    while (i < span.end) {
        const jump = skipTo(i);
        if (jump != null) {
            i = jump;
            continue;
        }
        const c = text[i];
        if (c !== '<') {
            i++;
            continue;
        }
        // A comment: skip wholesale, markup inside it is not markup.
        if (text.startsWith('<!--', i)) {
            const end = text.indexOf('-->', i + 4);
            i = end === -1 || end > span.end ? span.end : end + 3;
            continue;
        }
        if (text.startsWith('<!', i)) {
            const gt = text.indexOf('>', i);
            i = gt === -1 ? span.end : gt + 1;
            continue;
        }
        const m = /^<(\/?)([A-Za-z][\w-]*)/.exec(text.slice(i, Math.min(i + 200, span.end)));
        if (!m) {
            i++;
            continue;
        } // `a < b` — not a tag
        const closing = m[1] === '/';
        const name = m[2].toLowerCase();
        // Find this tag's `>`, ignoring any inside a quoted attribute value —
        // that is what keeps `:class="a > b"` from ending the tag early and
        // `:class="a < b"` from opening one.
        let j = i + m[0].length;
        let q = null;
        let selfClosing = false;
        while (j < span.end) {
            const ch = text[j];
            if (q) {
                if (ch === q) q = null;
                j++;
                continue;
            }
            if (ch === '"' || ch === "'") {
                q = ch;
                j++;
                continue;
            }
            if (ch === '>') {
                selfClosing = text[j - 1] === '/';
                break;
            }
            j++;
        }
        if (j >= span.end) break; // an unterminated tag at the end of the body
        if (closing) {
            const top = stack[stack.length - 1];
            if (!top) {
                out.push({
                    start: i,
                    end: j + 1,
                    code: 'UnexpectedClosingTag',
                    name,
                    message: `</${name}> has no matching opening tag in this template`,
                });
            } else if (top.name !== name) {
                // Report the mismatch once, at the closer. If it does close
                // something further up the stack, everything between was left
                // open — unwind to it so the rest of the body still reads.
                out.push({
                    start: i,
                    end: j + 1,
                    code: 'UnexpectedClosingTag',
                    name,
                    message: `</${name}> closes <${top.name}> — the innermost open tag is <${top.name}>, opened at line ${lineColAt(text, top.start).line}`,
                });
                const idx = [...stack].reverse().findIndex((t) => t.name === name);
                if (idx !== -1) stack.splice(stack.length - 1 - idx);
                else stack.pop();
            } else {
                stack.pop();
            }
        } else if (!selfClosing && !VOID.has(name)) {
            stack.push({ name, start: i });
        }
        i = j + 1;
    }
    for (const t of stack) {
        out.push({
            start: t.start,
            end: t.start + t.name.length + 1,
            code: 'UnclosedTag',
            name: t.name,
            message: `<${t.name}> is never closed in this template`,
        });
    }
    return out;
}

// check(text) → diagnostics, absolute offsets, line/column attached.
function check(text) {
    const s = String(text || '');
    if (!s.includes(BACKTICK)) return [];
    const out = [];
    for (const span of templateSpans(s)) out.push(...checkSpan(s, span));
    return out.sort((a, b) => a.start - b.start).map((d) => ({ ...d, ...lineColAt(s, d.start) }));
}

module.exports = { check, templateSpans, VOID };
