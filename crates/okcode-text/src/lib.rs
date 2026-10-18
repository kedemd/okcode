//! okcode-text: analysis/text.js, template-balance.js, convention-color.js.
pub mod convention_color;
pub mod template_balance;
pub mod text;

use convention_color as cc;
use serde_json::{json, Value};
use text::{u16s, Edit};

fn s_arg(args: &[Value], i: usize) -> Result<String, String> {
    match args.get(i) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Null) | None => Ok(String::new()),
        Some(v) => Err(format!("arg {i}: expected string, got {v}")),
    }
}
fn n_arg(args: &[Value], i: usize) -> Result<i64, String> {
    let v = args.get(i).ok_or(format!("arg {i} missing"))?;
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).ok_or(format!("arg {i}: not a number"))
}
fn palette_arg(v: Option<&Value>) -> Vec<cc::Palette> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|p| cc::Palette {
                    name: p["name"].as_str().unwrap_or("").to_string(),
                    value: p["value"].as_str().unwrap_or("").to_string(),
                    rgb: [0, 1, 2].map(|k| p["rgb"][k].as_i64().unwrap_or(0)),
                })
                .collect()
        })
        .unwrap_or_default()
}
fn pal_json(p: &cc::Palette) -> Value {
    json!({"name": p.name, "value": p.value, "rgb": p.rgb})
}

pub fn dispatch(name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "text.decode" => {
            use base64::Engine;
            let b64 = args.first().and_then(|a| a["$buf"].as_str()).ok_or("expected $buf")?;
            let buf = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| e.to_string())?;
            let (text, lossless) = text::decode(&buf);
            Ok(json!({"text": text, "lossless": lossless}))
        }
        "text.dominantEol" => Ok(json!(text::dominant_eol(&s_arg(args, 0)?))),
        "text.normaliseEol" => Ok(json!(text::normalise_eol(&s_arg(args, 0)?, &s_arg(args, 1)?))),
        "text.lineOffsets" => Ok(json!(text::line_offsets(&u16s(&s_arg(args, 0)?)))),
        "text.lineRangeToOffsets" => {
            let (start, end) = text::line_range_to_offsets(&u16s(&s_arg(args, 0)?), n_arg(args, 1)?, n_arg(args, 2)?);
            Ok(json!({"start": start, "end": end}))
        }
        "text.lineColAt" => {
            let (line, column) = text::line_col_at(&u16s(&s_arg(args, 0)?), n_arg(args, 1)?);
            Ok(json!({"line": line, "column": column}))
        }
        "text.offsetAtLineCol" => Ok(json!(text::offset_at_line_col(
            &u16s(&s_arg(args, 0)?),
            n_arg(args, 1)?,
            args.get(2).and_then(|v| v.as_i64())
        ))),
        "text.hunksFor" => {
            let edits: Vec<Edit> = args
                .get(2)
                .and_then(|v| v.as_array())
                .ok_or("edits")?
                .iter()
                .map(|e| Edit {
                    start: e["start"].as_i64().unwrap_or(0),
                    end: e["end"].as_i64().unwrap_or(0),
                    body: u16s(e["body"].as_str().unwrap_or("")),
                    target: e["target"].as_str().map(str::to_owned),
                })
                .collect();
            let ctx = args.get(3).and_then(|o| o["context"].as_i64()).unwrap_or(3);
            Ok(Value::Array(text::hunks_for(&u16s(&s_arg(args, 0)?), &u16s(&s_arg(args, 1)?), &edits, ctx)))
        }
        "templateBalance.check" => {
            let d = template_balance::check(&u16s(&s_arg(args, 0)?));
            Ok(Value::Array(
                d.iter()
                    .map(|d| {
                        json!({"start": d.start, "end": d.end, "code": d.code, "name": d.name,
                               "message": d.message, "line": d.line, "column": d.column})
                    })
                    .collect(),
            ))
        }
        "templateBalance.templateSpans" => {
            let sp = template_balance::template_spans(&u16s(&s_arg(args, 0)?));
            Ok(Value::Array(
                sp.iter()
                    .map(|s| {
                        json!({"start": s.start, "end": s.end,
                               "interpolations": s.interpolations.iter().map(|i| json!({"start": i.start, "end": i.end})).collect::<Vec<_>>()})
                    })
                    .collect(),
            ))
        }
        "conventionColor.rgbOf" => Ok(json!(cc::rgb_of(&s_arg(args, 0)?))),
        "conventionColor.paletteFrom" => {
            let srcs: Vec<Option<String>> = args
                .first()
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            Ok(Value::Array(cc::palette_from(&srcs).iter().map(pal_json).collect()))
        }
        "conventionColor.nearest" => {
            let rgb = [0, 1, 2].map(|k| args[0][k].as_i64().unwrap_or(0));
            Ok(match cc::nearest(rgb, &palette_arg(args.get(1))) {
                Some(n) => {
                    let mut v = pal_json(&n.p);
                    v["d"] = json!(n.d);
                    v
                }
                None => Value::Null,
            })
        }
        "conventionColor.colorLiterals" => {
            let out = cc::color_literals(&s_arg(args, 0)?, &palette_arg(args.get(1)));
            Ok(Value::Array(
                out.iter()
                    .map(|(_, d)| {
                        json!({"source": "convention-color", "code": "CONVENTION_COLOR_LITERAL", "severity": "warning",
                               "message": d.message, "start": d.start, "end": d.end})
                    })
                    .collect(),
            ))
        }
        _ => Err(format!("unknown fn {name}")),
    }
}
