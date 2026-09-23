'use strict';
// THE PALETTE GATE — a convention promoted out of memory and into the write
// gate, where it cannot go stale.
//
// The failure, measured: asked to make a label green, the agent wrote
// `style="color:#22c55e"`. The project defines `--green: #3fb950` in two
// files and uses it everywhere. #22c55e is Tailwind's green-500 and appears
// nowhere in this repository — so the agent did not merely skip the variable,
// it invented a different green. A separate run of the identical task used
// `var(--green)` correctly, which is what makes this a convention failure
// rather than a knowledge one: the information was equally available both
// times.
//
// A reviewer's line, and the reason this is code and not a remembered fact:
//
//   Mechanically testable convention -> promote it into a lint/write-gate rule.
//   Cheaply re-derived convention    -> cache and revalidate.
//   Subjective or contextual         -> advice, never blocking truth.
//
// A colour literal is the first kind. Nothing needs to remember this rule,
// nothing needs to revalidate it, and it cannot outlive the palette it
// describes — it reads the palette from the workspace every time it runs.
//
// WHY IT IS SAFE TO BE STRICT. The gate never judges a file on its own: it
// validates the CANDIDATE and the BASELINE and refuses only what is NEW.
// Every literal already in the tree produces the same diagnostic on both sides
// and is therefore invisible. Only a literal this edit introduces can fail.
// That is what lets the rule be blunt without a suppression list.

// `--name: <value>` — a custom property declaration. Values that are not
// colours are ignored below; matching loosely here keeps the regex honest
// about what it is (a declaration scanner, not a colour parser).
const DECL = /--([a-z0-9-]+)\s*:\s*([^;}\n]+)/gi;

// A colour literal in a position where this project would use a variable.
// Deliberately NOT every hex in the file: a gradient stop, a box-shadow
// rgba(), an SVG attribute and a syntax-highlighting table are all places
// where a literal is the honest thing to write. The properties below are the
// ones the palette exists for.
const COLOR_PROP =
    /(^|[;{\s"'`])(color|background|background-color|border-color|outline-color|fill|stroke|caret-color|text-decoration-color)\s*:\s*([^;}\n"'`]+)/gi;
const HEX = /#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})\b/i;
const RGB = /\brgba?\(\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*[, ]\s*(\d{1,3})/i;

function rgbOf(value) {
    const h = HEX.exec(value);
    if (h) {
        let s = h[1];
        if (s.length === 3 || s.length === 4)
            s = s
                .slice(0, 3)
                .split('')
                .map((c) => c + c)
                .join('');
        else s = s.slice(0, 6);
        return [parseInt(s.slice(0, 2), 16), parseInt(s.slice(2, 4), 16), parseInt(s.slice(4, 6), 16)];
    }
    const r = RGB.exec(value);
    if (r) return [Number(r[1]), Number(r[2]), Number(r[3])];
    return null;
}

// Every custom property in the given sources that resolves to a colour.
// Built from the workspace, so the rule is exactly as current as the code.
function paletteFrom(sources) {
    const out = new Map();
    for (const text of sources) {
        if (!text) continue;
        DECL.lastIndex = 0;
        let m;
        while ((m = DECL.exec(text))) {
            const name = m[1];
            const rgb = rgbOf(m[2].trim());
            if (rgb && !out.has(name)) out.set(name, { name, value: m[2].trim(), rgb });
        }
    }
    return [...out.values()];
}

// Plain squared distance in RGB. Not perceptually uniform, and it does not
// need to be — this only picks which variable to NAME in the message, and
// being approximately right about "you probably meant --green" is the whole
// job. The diagnostic fires on the literal existing, not on the distance.
const dist2 = (a, b) => (a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2 + (a[2] - b[2]) ** 2;

function nearest(rgb, palette) {
    let best = null,
        bestD = Infinity;
    for (const p of palette) {
        const d = dist2(rgb, p.rgb);
        if (d < bestD) {
            bestD = d;
            best = p;
        }
    }
    // ~64 per channel average: close enough that "you meant this one" is a
    // useful sentence, far enough that unrelated colours are not mislabelled.
    return best && bestD <= 3 * 64 * 64 ? { ...best, d: Math.round(Math.sqrt(bestD)) } : null;
}

// Diagnostics, in the shape validate.js already emits: source, code,
// severity, message, start/end offsets.
function colorLiterals(text, palette) {
    if (!palette.length) return [];
    const out = [];
    const src = String(text || '');
    COLOR_PROP.lastIndex = 0;
    let m;
    while ((m = COLOR_PROP.exec(src))) {
        const prop = m[2];
        const value = m[3];
        // A variable, a keyword, `inherit`, `transparent`, `currentColor` —
        // all fine. Only a literal colour is the thing being ruled out.
        if (/var\(\s*--/.test(value)) continue;
        // A gradient or an image is not "the colour of this element", and its
        // stops are a composition rather than a palette choice — the project
        // writes those as literals throughout. Caught by the test: a
        // `linear-gradient(90deg, #111, #222)` was being reported as a
        // background colour literal, which would have made the rule noisy
        // enough to be worth suppressing, and a suppressed rule is no rule.
        if (/\b(gradient|url|image-set|cross-fade)\s*\(/i.test(value)) continue;
        const rgb = rgbOf(value);
        if (!rgb) continue;
        const litMatch = HEX.exec(value) || RGB.exec(value);
        const at = m.index + m[0].indexOf(value) + value.indexOf(litMatch[0]);
        const near = nearest(rgb, palette);
        out.push({
            source: 'convention-color',
            code: 'CONVENTION_COLOR_LITERAL',
            severity: 'warning',
            message: near
                ? `${prop}: ${litMatch[0]} is a colour literal — this project defines --${near.name} (${near.value}) for this. Use var(--${near.name}), or add a new variable beside the others if none fits.`
                : `${prop}: ${litMatch[0]} is a colour literal — this project styles colour through CSS variables (${palette
                      .slice(0, 4)
                      .map((p) => `--${p.name}`)
                      .join(', ')}…). Add a variable beside the others rather than inlining a value.`,
            start: at,
            end: at + litMatch[0].length,
        });
    }
    return out;
}

module.exports = { paletteFrom, colorLiterals, rgbOf, nearest };
