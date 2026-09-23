'use strict';
// Validate a CANDIDATE — source text that has not touched any target — never
// a path already overwritten on disk. This is what makes editBatch's
// validate-before-write possible: the file is judged before it is touched,
// not repaired after. It runs on the HOST, over text — never on the target
// machine, which needs no toolchain.
//
// Two JavaScript validators run, deliberately, not one:
//   - acorn is the index's own parser. It gives exact character offsets,
//     which node --check does not, and those offsets are what a diagnostic
//     needs to be addressed the same way a symbol is.
//   - node --check is the syntax the RUNTIME actually accepts. The two
//     disagree — top-level `await` is valid module syntax and invalid
//     script syntax, and which one governs a bare `.js` file depends on a
//     package.json neither parser reads on its own. Acorn accepting a
//     candidate is not proof node will run it.
//
// Both are FAIL-FAST: one syntax error ends the parse. A file with two
// independent syntax errors reports only the first one it hits, on either
// side. `coverage: 'fail-fast'` says so on every JS result, and nothing here
// ever calls a result "valid" — only "no new diagnostic against the baseline
// that was visible".

const { spawnSync, spawn } = require('child_process');
const acorn = require('acorn');
const { langOf } = require('./parse');
const { extensionFor } = require('./extensions');
const templateBalance = require('./template-balance');
const { colorLiterals } = require('./convention-color');
const { lineColAt, offsetAtLineCol } = require('./text');

// moduleKind is resolved by the CALLER (the workspace), independently of parsing,
// because invalid source has no successful parse to read a sourceType off:
// .mjs -> 'module', .cjs -> 'commonjs', .js -> the nearest ancestor
// package.json's "type", or 'ambiguous' when no ancestor manifest declares
// one. Node 24 measured: with NO --input-type, `node --check` on stdin
// treats input as commonjs and does NOT run syntax detection — that only
// happens for the FILE form. So 'ambiguous' is not "omit the flag"; it is
// "accept either", checked as both goals and passing if either does — the
// same verdict syntax detection would reach, with no temp file and no
// possibility of a false rejection on a project with no declared type.
function acornGoalsFor(moduleKind) {
    if (moduleKind === 'module') return ['module'];
    if (moduleKind === 'commonjs') return ['script'];
    return ['script', 'module'];
}

function nodeCheckGoalsFor(moduleKind) {
    if (moduleKind === 'module') return ['module'];
    if (moduleKind === 'commonjs') return ['commonjs'];
    return ['commonjs', 'module'];
}

function tryAcorn(text, moduleKind) {
    let lastErr = null;
    for (const sourceType of acornGoalsFor(moduleKind)) {
        try {
            acorn.parse(text, { ecmaVersion: 'latest', locations: true, sourceType, allowHashBang: true });
            return { ok: true };
        } catch (err) {
            lastErr = err;
        }
    }
    return { ok: false, err: lastErr };
}

function runNodeCheck(text, inputType) {
    return spawnSync(process.execPath, ['--check', `--input-type=${inputType}`], {
        input: text,
        encoding: 'utf8',
        maxBuffer: 8 * 1024 * 1024,
        timeout: 10_000,
    });
}

function nodeCheck(text, moduleKind) {
    let last = null;
    for (const inputType of nodeCheckGoalsFor(moduleKind)) {
        const res = runNodeCheck(text, inputType);
        if (res.status === 0) return { ok: true };
        last = res;
    }
    return { ok: false, res: last };
}

// The ASYNC twin of runNodeCheck, for validateSourceAsync below. `spawn`
// (not spawnSync) so the child process is running in the background the
// INSTANT this returns — before anything awaits it — which is what lets
// editBatch start a baseline check and a candidate check at (very nearly)
// the same moment instead of paying two full `node --check` startups
// back to back.
function runNodeCheckAsync(text, inputType) {
    return new Promise((resolve) => {
        let child;
        try {
            child = spawn(process.execPath, ['--check', `--input-type=${inputType}`], {
                stdio: ['pipe', 'pipe', 'pipe'],
            });
        } catch (err) {
            resolve({ status: 1, stderr: String((err && err.message) || err) });
            return;
        }
        let stderr = '';
        let settled = false;
        const finish = (status) => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            resolve({ status, stderr });
        };
        const timer = setTimeout(() => {
            try {
                child.kill('SIGKILL');
            } catch {
                /* already gone */
            }
            finish(1);
        }, 10_000);
        child.stderr.on('data', (d) => {
            stderr += d;
        });
        child.stdout.on('data', () => {}); // --check prints nothing on success; drained so a full pipe never blocks the child
        child.on('error', (err) => {
            stderr = String((err && err.message) || err);
            finish(1);
        });
        child.on('close', (code) => finish(code == null ? 1 : code));
        child.stdin.on('error', () => {
            /* EPIPE if the child already exited — the close handler still fires */
        });
        child.stdin.write(text);
        child.stdin.end();
    });
}

async function nodeCheckAsync(text, moduleKind) {
    let last = null;
    for (const inputType of nodeCheckGoalsFor(moduleKind)) {
        const res = await runNodeCheckAsync(text, inputType);
        if (res.status === 0) return { ok: true };
        last = res;
    }
    return { ok: false, res: last };
}

// node --check's stderr on a syntax error looks like:
//   [stdin]:1
//   await 1;
//   ^^^^^
//
//   SyntaxError: await is only valid in async functions and the top level bodies of modules
// The caret line's indentation is the best column estimate node offers; it is
// approximate (tabs, multi-byte characters), which is fine here — it is used
// only to place the diagnostic, never to decide whether it matches another.
function parseNodeCheckOutput(stderr) {
    const text = String(stderr || '');
    const lines = text.split('\n');
    const lm = /^\[stdin\]:(\d+)/.exec(text);
    const line = lm ? Number(lm[1]) : 1;
    const caretIdx = lines.findIndex((l) => /^\s*\^+\s*$/.test(l));
    const column = caretIdx > 0 ? (lines[caretIdx].match(/^\s*/) || [''])[0].length + 1 : 1;
    const em = /^(\w*Error): (.+)$/m.exec(text);
    return {
        line,
        column,
        code: em ? em[1] : 'SyntaxError',
        message: em ? em[2] : (lines[0] || 'syntax error').trim(),
    };
}

// Validate CANDIDATE TEXT — never a path, never the target. rel is used to
// pick a language and, separately, to ask the extension registry whether
// some dialect owns this path; moduleKind only matters for JavaScript.
// `palette` is supplied by the caller (the workspace derives it from its own files) —
// a convention this gate can only enforce if it is told what the project's own
// colours are. Absent, the check simply does not run, which is the correct
// behaviour for a repository that has no palette to violate.
function validateSource({ rel, text, moduleKind = 'ambiguous', palette = [] }) {
    const isJS = langOf(rel) === 'javascript';
    const ext = extensionFor(rel);
    if (!isJS && !ext) {
        return {
            validators: [{ validator: 'none', status: 'skipped', covers: [] }],
            diagnostics: [],
            coverage: 'complete',
        };
    }

    const fullSpan = [{ start: 0, end: text.length }];
    const validators = [];
    const diagnostics = [];

    // THE CONVENTION CHECK. Not a syntax claim — the file is perfectly valid
    // with a colour literal in it — but a claim about how THIS project writes
    // colour, derived from the project itself rather than remembered. It is
    // safe to run bluntly because editBatch compares candidate against
    // baseline: a literal already in the tree is not new, and only what this
    // edit introduces can fail. See convention-color.js.
    if (palette.length) {
        const found = colorLiterals(text, palette);
        validators.push({
            validator: 'convention-color',
            status: found.length ? 'failed' : 'passed',
            covers: fullSpan,
        });
        for (const d of found) {
            const { line, column } = lineColAt(text, d.start);
            diagnostics.push({ ...d, line, column });
        }
    }

    if (isJS) {
        const acornResult = tryAcorn(text, moduleKind);
        validators.push({
            validator: 'javascript-acorn',
            status: acornResult.ok ? 'passed' : 'failed',
            covers: fullSpan,
        });
        if (!acornResult.ok && acornResult.err) {
            const pos = Number.isInteger(acornResult.err.pos) ? acornResult.err.pos : 0;
            const { line, column } = lineColAt(text, pos);
            diagnostics.push({
                source: 'javascript-acorn',
                code: 'SyntaxError',
                severity: 'error',
                message: String(acornResult.err.message || 'parse error'),
                start: pos,
                end: pos,
                line,
                column,
            });
        }

        const nc = nodeCheck(text, moduleKind);
        validators.push({ validator: 'javascript-node-check', status: nc.ok ? 'passed' : 'failed', covers: fullSpan });
        if (!nc.ok && nc.res) {
            const parsed = parseNodeCheckOutput(nc.res.stderr);
            const offset = offsetAtLineCol(text, parsed.line, parsed.column);
            diagnostics.push({
                source: 'javascript-node-check',
                code: parsed.code,
                severity: 'error',
                message: parsed.message,
                start: offset,
                end: offset,
                line: parsed.line,
                column: parsed.column,
            });
        }

        // Tag balance inside template literals — the one structural claim the
        // two JS validators cannot make, because markup lives in a STRING and
        // a template that has lost a closing tag is still valid JavaScript.
        // Measured live: an off-by-one range replace duplicated a <div> and
        // dropped an </if>, both validators passed, the file published, four
        // screenshot passes called it done. Not fail-fast: every imbalance in
        // the body is reported, and compareDiagnostics decides which are NEW.
        try {
            for (const d of templateBalance.check(text)) {
                diagnostics.push({
                    source: 'template-balance',
                    code: d.code,
                    severity: 'error',
                    message: d.message,
                    start: d.start,
                    end: d.end,
                    line: d.line,
                    column: d.column,
                });
            }
            validators.push({
                validator: 'template-balance',
                status: diagnostics.some((d) => d.source === 'template-balance') ? 'failed' : 'passed',
                covers: fullSpan,
            });
        } catch (err) {
            // A checker that throws must never make a file uneditable.
            validators.push({
                validator: 'template-balance',
                status: 'skipped',
                covers: [],
                note: String(err.message).slice(0, 120),
            });
        }
    }

    // An extension's own structural analysis, independent of whether this
    // path is JavaScript at all — this is what actually validates a
    // `.ok.html` candidate, which has no top-level JS for acorn/node-check to
    // even run against and previously got NO validation whatsoever. Every
    // diagnostic the analyser reports already carries its own offsets and its
    // own namespaced `source` (okjs.definition, okjs.imports, …), so it is
    // taken through unmodified rather than collapsed into one generic label.
    if (ext) {
        try {
            const result = ext.analyze({ path: rel, source: text });
            // The extension's tooling is unavailable here (e.g. okjs not installed):
            // nothing was checked, so the validator is "skipped", never "passed".
            if (result.fallback) {
                validators.push({
                    validator: `${ext.id}-analyze`,
                    status: 'skipped',
                    covers: [],
                    note: String(result.reason || 'analyser unavailable').slice(0, 160),
                });
            } else {
                for (const d of result.diagnostics || []) {
                    diagnostics.push({
                        source: d.source,
                        code: d.code,
                        severity: d.severity,
                        message: d.message,
                        start: d.range.start,
                        end: d.range.end,
                        line: d.range.loc.start.line,
                        column: d.range.loc.start.column,
                    });
                }
                validators.push({
                    validator: `${ext.id}-analyze`,
                    status: (result.diagnostics || []).some((d) => d.severity === 'error') ? 'failed' : 'passed',
                    covers: fullSpan,
                });
            }
        } catch (err) {
            // Same contract as template-balance above: a checker that throws
            // must never make a file uneditable.
            validators.push({
                validator: `${ext.id}-analyze`,
                status: 'skipped',
                covers: [],
                note: String(err.message).slice(0, 120),
            });
        }
    }

    return { validators, diagnostics, coverage: isJS ? 'fail-fast' : 'structural' };
}

// The ASYNC twin of validateSource, structurally identical (same checks, in
// the same order, over the same fields) except that `node --check` runs via
// `nodeCheckAsync` instead of the synchronous `nodeCheck`. Kept as its own
// function rather than sharing a body with validateSource — validateSource
// is a synchronous API with real callers outside editBatch (writeWholeFile,
// several unit tests) that must not become
// Promise-returning as a side effect of this — so a structural change to
// one of these two must be made to the other too; they are meant to answer
// identically for the same input, just at different speeds.
//
// Existing solely so editBatch can run the BASELINE and CANDIDATE checks
// CONCURRENTLY: `Promise.all([validateSourceAsync(baseline),
// validateSourceAsync(candidate)])` starts both `node --check` children at
// (very nearly) the same moment — each call's synchronous prefix (acorn,
// template-balance, extension analysis) runs, then `spawn` fires before the
// `await` on it suspends, so the SECOND call's own spawn goes out while the
// first child is already running, rather than after it exits.
async function validateSourceAsync({ rel, text, moduleKind = 'ambiguous', palette = [] }) {
    const isJS = langOf(rel) === 'javascript';
    const ext = extensionFor(rel);
    if (!isJS && !ext) {
        return {
            validators: [{ validator: 'none', status: 'skipped', covers: [] }],
            diagnostics: [],
            coverage: 'complete',
        };
    }

    const fullSpan = [{ start: 0, end: text.length }];
    const validators = [];
    const diagnostics = [];

    if (palette.length) {
        const found = colorLiterals(text, palette);
        validators.push({
            validator: 'convention-color',
            status: found.length ? 'failed' : 'passed',
            covers: fullSpan,
        });
        for (const d of found) {
            const { line, column } = lineColAt(text, d.start);
            diagnostics.push({ ...d, line, column });
        }
    }

    if (isJS) {
        const acornResult = tryAcorn(text, moduleKind);
        validators.push({
            validator: 'javascript-acorn',
            status: acornResult.ok ? 'passed' : 'failed',
            covers: fullSpan,
        });
        if (!acornResult.ok && acornResult.err) {
            const pos = Number.isInteger(acornResult.err.pos) ? acornResult.err.pos : 0;
            const { line, column } = lineColAt(text, pos);
            diagnostics.push({
                source: 'javascript-acorn',
                code: 'SyntaxError',
                severity: 'error',
                message: String(acornResult.err.message || 'parse error'),
                start: pos,
                end: pos,
                line,
                column,
            });
        }

        const nc = await nodeCheckAsync(text, moduleKind);
        validators.push({ validator: 'javascript-node-check', status: nc.ok ? 'passed' : 'failed', covers: fullSpan });
        if (!nc.ok && nc.res) {
            const parsed = parseNodeCheckOutput(nc.res.stderr);
            const offset = offsetAtLineCol(text, parsed.line, parsed.column);
            diagnostics.push({
                source: 'javascript-node-check',
                code: parsed.code,
                severity: 'error',
                message: parsed.message,
                start: offset,
                end: offset,
                line: parsed.line,
                column: parsed.column,
            });
        }

        try {
            for (const d of templateBalance.check(text)) {
                diagnostics.push({
                    source: 'template-balance',
                    code: d.code,
                    severity: 'error',
                    message: d.message,
                    start: d.start,
                    end: d.end,
                    line: d.line,
                    column: d.column,
                });
            }
            validators.push({
                validator: 'template-balance',
                status: diagnostics.some((d) => d.source === 'template-balance') ? 'failed' : 'passed',
                covers: fullSpan,
            });
        } catch (err) {
            validators.push({
                validator: 'template-balance',
                status: 'skipped',
                covers: [],
                note: String(err.message).slice(0, 120),
            });
        }
    }

    if (ext) {
        try {
            const result = ext.analyze({ path: rel, source: text });
            // The extension's tooling is unavailable here (e.g. okjs not installed):
            // nothing was checked, so the validator is "skipped", never "passed".
            if (result.fallback) {
                validators.push({
                    validator: `${ext.id}-analyze`,
                    status: 'skipped',
                    covers: [],
                    note: String(result.reason || 'analyser unavailable').slice(0, 160),
                });
            } else {
                for (const d of result.diagnostics || []) {
                    diagnostics.push({
                        source: d.source,
                        code: d.code,
                        severity: d.severity,
                        message: d.message,
                        start: d.range.start,
                        end: d.range.end,
                        line: d.range.loc.start.line,
                        column: d.range.loc.start.column,
                    });
                }
                validators.push({
                    validator: `${ext.id}-analyze`,
                    status: (result.diagnostics || []).some((d) => d.severity === 'error') ? 'failed' : 'passed',
                    covers: fullSpan,
                });
            }
        } catch (err) {
            validators.push({
                validator: `${ext.id}-analyze`,
                status: 'skipped',
                covers: [],
                note: String(err.message).slice(0, 120),
            });
        }
    }

    return { validators, diagnostics, coverage: isJS ? 'fail-fast' : 'structural' };
}

const normaliseMessage = (msg) =>
    String(msg || '')
        .toLowerCase()
        .replace(/\s+/g, ' ')
        .trim();

// Which edited target (if any) a diagnostic falls inside, on the named side
// ('baseline' or 'candidate') of a target's two coordinate spaces. `targets`
// entries carry both, because a diagnostic's offset means different things
// in the two snapshots being compared.
function targetAt(targets, offset, side) {
    return targets.find((t) => offset >= t[`${side}Start`] && offset < t[`${side}End`]) || null;
}

// Compare a baseline validation against a candidate one. Structural matching,
// never a count: a diagnostic survives (is `remaining`, not `newDiagnostics`)
// only if an equivalent one already existed in the baseline.
//
// Two tiers, because "every diagnostic inside an edited region is new" would
// make an UNCHANGED baseline error inside the very region being repaired
// permanently uncommittable — the opposite of the repair path the brief asks
// for.
//   - OUTSIDE every edited region: matched by (validator, code, severity,
//     normalised message, offset mapped baseline -> candidate via the
//     caller's `mapOffset`). The content there did not change, so its offset
//     should map exactly.
//   - INSIDE an edited region: matched by (edited-target identity, validator,
//     code, severity, normalised message). Position is not compared — the
//     region's content changed by design, so its diagnostic's position
//     inside that region is expected to move.
// A severity change is a different key by construction, so it is always a
// new diagnostic. Swapping one error for a structurally different one is a
// new diagnostic too, by the same construction.
function compareDiagnostics(baseline, candidate, { mapOffset, targets = [] } = {}) {
    const baselineBroken = baseline.some((d) => d.severity === 'error');

    const insideBaseline = new Map();
    const outsideBaseline = new Map();
    for (const d of baseline) {
        const t = targetAt(targets, d.start, 'baseline');
        if (t) {
            insideBaseline.set(`${t.name}::${d.source}::${d.code}::${d.severity}::${normaliseMessage(d.message)}`, d);
            continue;
        }
        const mapped = mapOffset ? mapOffset(d.start) : d.start;
        if (mapped == null) continue; // unmappable: content there is gone, not "unchanged"
        outsideBaseline.set(`${mapped}::${d.source}::${d.code}::${d.severity}::${normaliseMessage(d.message)}`, d);
    }

    const newDiagnostics = [];
    const remaining = [];
    const matched = new Set();

    for (const d of candidate) {
        const t = targetAt(targets, d.start, 'candidate');
        const [key, bucket] = t
            ? [`${t.name}::${d.source}::${d.code}::${d.severity}::${normaliseMessage(d.message)}`, insideBaseline]
            : [`${d.start}::${d.source}::${d.code}::${d.severity}::${normaliseMessage(d.message)}`, outsideBaseline];
        if (bucket.has(key)) {
            matched.add(key);
            remaining.push(d);
        } else newDiagnostics.push(d);
    }

    const resolved = [];
    for (const [key, d] of [...insideBaseline, ...outsideBaseline]) {
        if (!matched.has(key)) resolved.push(d);
    }

    return { newDiagnostics, resolved, remaining, baselineBroken };
}

module.exports = { validateSource, validateSourceAsync, compareDiagnostics, acornGoalsFor, nodeCheckGoalsFor };
