//! MessagePack ↔ okcode's JSON value model, with the conventions okcode-core has always used for
//! okdb values (db.rs j2v/v2j): Buffer ⇄ `{"$buf": base64}`, Date ⇄ `{"$date": ms}`, a JS Map ⇄
//! `{"$map": [[k, v]…]}`, `{"$set": […]}` → array, `{"$okdb_undef": …}` → msgpackr's undefined
//! (fixext1 type 0). Replies: nil and undefined → null, undefined members are dropped from maps.
use serde_json::{Map, Value};

pub const UNDEF_ARG: &str = "$okdb_undef";

// ── encode ─────────────────────────────────────────────────────────────────

pub fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    enc(v, &mut out);
    out
}

fn enc(v: &Value, o: &mut Vec<u8>) {
    match v {
        Value::Null => o.push(0xc0),
        Value::Bool(b) => o.push(if *b { 0xc3 } else { 0xc2 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                enc_int(i, o)
            } else if let Some(u) = n.as_u64() {
                o.push(0xcf);
                o.extend_from_slice(&u.to_be_bytes());
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.fract() == 0.0 && f.abs() <= 9007199254740991.0 {
                    enc_int(f as i64, o)
                } else {
                    o.push(0xcb);
                    o.extend_from_slice(&f.to_be_bytes());
                }
            }
        }
        Value::String(s) => enc_str(s, o),
        Value::Array(a) => {
            enc_len(a.len(), 0x90, 0xdc, 0xdd, o);
            for x in a {
                enc(x, o);
            }
        }
        Value::Object(m) => {
            if m.len() == 1 {
                if let Some(Value::String(s)) = m.get("$buf") {
                    return enc_bin(&b64_decode(s), o);
                }
                if m.contains_key(UNDEF_ARG) {
                    return o.extend_from_slice(&[0xd4, 0x00, 0x00]);
                }
                if let Some(Value::Array(items)) = m.get("$set") {
                    return enc(&Value::Array(items.clone()), o);
                }
                if let Some(d) = m.get("$date") {
                    return enc_date(d.as_f64().unwrap_or(f64::NAN), o);
                }
                if let Some(Value::Array(pairs)) = m.get("$map") {
                    if pairs.iter().all(|p| matches!(p, Value::Array(kv) if kv.len() == 2)) {
                        enc_len(pairs.len(), 0x80, 0xde, 0xdf, o);
                        for p in pairs {
                            enc(&p[0], o);
                            enc(&p[1], o);
                        }
                        return;
                    }
                }
            }
            enc_len(m.len(), 0x80, 0xde, 0xdf, o);
            for (k, x) in m {
                enc_str(k, o);
                enc(x, o);
            }
        }
    }
}

fn enc_int(i: i64, o: &mut Vec<u8>) {
    match i {
        0..=127 => o.push(i as u8),
        -32..=-1 => o.push(i as i8 as u8),
        128..=255 => o.extend_from_slice(&[0xcc, i as u8]),
        256..=65535 => {
            o.push(0xcd);
            o.extend_from_slice(&(i as u16).to_be_bytes())
        }
        65536..=4294967295 => {
            o.push(0xce);
            o.extend_from_slice(&(i as u32).to_be_bytes())
        }
        -128..=-33 => o.extend_from_slice(&[0xd0, i as i8 as u8]),
        -32768..=-129 => {
            o.push(0xd1);
            o.extend_from_slice(&(i as i16).to_be_bytes())
        }
        -2147483648..=-32769 => {
            o.push(0xd2);
            o.extend_from_slice(&(i as i32).to_be_bytes())
        }
        _ => {
            o.push(0xd3);
            o.extend_from_slice(&i.to_be_bytes())
        }
    }
}

fn enc_len(n: usize, fix: u8, m16: u8, m32: u8, o: &mut Vec<u8>) {
    if n < 16 {
        o.push(fix | n as u8)
    } else if n <= 0xffff {
        o.push(m16);
        o.extend_from_slice(&(n as u16).to_be_bytes())
    } else {
        o.push(m32);
        o.extend_from_slice(&(n as u32).to_be_bytes())
    }
}

fn enc_str(s: &str, o: &mut Vec<u8>) {
    let n = s.len();
    if n < 32 {
        o.push(0xa0 | n as u8)
    } else if n <= 0xff {
        o.extend_from_slice(&[0xd9, n as u8])
    } else if n <= 0xffff {
        o.push(0xda);
        o.extend_from_slice(&(n as u16).to_be_bytes())
    } else {
        o.push(0xdb);
        o.extend_from_slice(&(n as u32).to_be_bytes())
    }
    o.extend_from_slice(s.as_bytes());
}

fn enc_bin(b: &[u8], o: &mut Vec<u8>) {
    let n = b.len();
    if n <= 0xff {
        o.extend_from_slice(&[0xc4, n as u8])
    } else if n <= 0xffff {
        o.push(0xc5);
        o.extend_from_slice(&(n as u16).to_be_bytes())
    } else {
        o.push(0xc6);
        o.extend_from_slice(&(n as u32).to_be_bytes())
    }
    o.extend_from_slice(b);
}

/// timestamp ext (-1), 96-bit form: nsec u32, sec i64
fn enc_date(ms: f64, o: &mut Vec<u8>) {
    if !ms.is_finite() {
        return o.push(0xc0);
    }
    let sec = (ms / 1000.0).floor();
    let nsec = ((ms - sec * 1000.0) * 1_000_000.0).round().clamp(0.0, 999_999_999.0) as u32;
    o.extend_from_slice(&[0xc7, 12, 0xff]);
    o.extend_from_slice(&nsec.to_be_bytes());
    o.extend_from_slice(&(sec as i64).to_be_bytes());
}

// ── decode ─────────────────────────────────────────────────────────────────

/// One MessagePack value → JSON (the whole buffer must be one value).
pub fn decode(b: &[u8]) -> Result<Value, String> {
    let mut r = Rd { b, i: 0 };
    let v = r.val()?.unwrap_or(Value::Null);
    if r.i != b.len() {
        return Err(format!("msgpack: {} trailing bytes", b.len() - r.i));
    }
    Ok(v)
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        if self.i + n > self.b.len() {
            return Err("msgpack: truncated".into());
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn be<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    /// None = msgpackr's undefined (dropped from maps, null elsewhere)
    fn val(&mut self) -> Result<Option<Value>, String> {
        let t = self.u8()?;
        Ok(Some(match t {
            0x00..=0x7f => Value::from(t),
            0xe0..=0xff => Value::from(t as i8),
            0x80..=0x8f => return self.map((t & 0x0f) as usize),
            0x90..=0x9f => return self.arr((t & 0x0f) as usize),
            0xa0..=0xbf => return self.str((t & 0x1f) as usize),
            0xc0 => Value::Null,
            0xc2 => Value::Bool(false),
            0xc3 => Value::Bool(true),
            0xc4 => {
                let n = self.u8()? as usize;
                return self.bin(n);
            }
            0xc5 => {
                let n = u16::from_be_bytes(self.be()?) as usize;
                return self.bin(n);
            }
            0xc6 => {
                let n = u32::from_be_bytes(self.be()?) as usize;
                return self.bin(n);
            }
            0xc7 => {
                let n = self.u8()? as usize;
                return self.ext(n);
            }
            0xc8 => {
                let n = u16::from_be_bytes(self.be()?) as usize;
                return self.ext(n);
            }
            0xc9 => {
                let n = u32::from_be_bytes(self.be()?) as usize;
                return self.ext(n);
            }
            0xca => num(f32::from_be_bytes(self.be()?) as f64),
            0xcb => num(f64::from_be_bytes(self.be()?)),
            0xcc => Value::from(self.u8()?),
            0xcd => Value::from(u16::from_be_bytes(self.be()?)),
            0xce => Value::from(u32::from_be_bytes(self.be()?)),
            0xcf => Value::from(u64::from_be_bytes(self.be()?)),
            0xd0 => Value::from(self.u8()? as i8),
            0xd1 => Value::from(i16::from_be_bytes(self.be()?)),
            0xd2 => Value::from(i32::from_be_bytes(self.be()?)),
            0xd3 => Value::from(i64::from_be_bytes(self.be()?)),
            0xd4 => return self.ext(1),
            0xd5 => return self.ext(2),
            0xd6 => return self.ext(4),
            0xd7 => return self.ext(8),
            0xd8 => return self.ext(16),
            0xd9 => {
                let n = self.u8()? as usize;
                return self.str(n);
            }
            0xda => {
                let n = u16::from_be_bytes(self.be()?) as usize;
                return self.str(n);
            }
            0xdb => {
                let n = u32::from_be_bytes(self.be()?) as usize;
                return self.str(n);
            }
            0xdc => {
                let n = u16::from_be_bytes(self.be()?) as usize;
                return self.arr(n);
            }
            0xdd => {
                let n = u32::from_be_bytes(self.be()?) as usize;
                return self.arr(n);
            }
            0xde => {
                let n = u16::from_be_bytes(self.be()?) as usize;
                return self.map(n);
            }
            0xdf => {
                let n = u32::from_be_bytes(self.be()?) as usize;
                return self.map(n);
            }
            0xc1 => return Err("msgpack: reserved byte 0xc1".into()),
        }))
    }
    fn str(&mut self, n: usize) -> Result<Option<Value>, String> {
        let s = self.take(n)?;
        Ok(Some(Value::String(String::from_utf8_lossy(s).into_owned())))
    }
    fn bin(&mut self, n: usize) -> Result<Option<Value>, String> {
        let s = b64_encode(self.take(n)?);
        let mut m = Map::new();
        m.insert("$buf".into(), Value::String(s));
        Ok(Some(Value::Object(m)))
    }
    fn arr(&mut self, n: usize) -> Result<Option<Value>, String> {
        let mut a = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            a.push(self.val()?.unwrap_or(Value::Null));
        }
        Ok(Some(Value::Array(a)))
    }
    fn map(&mut self, n: usize) -> Result<Option<Value>, String> {
        let mut pairs: Vec<(Value, Option<Value>)> = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            let k = self.val()?.unwrap_or(Value::Null);
            pairs.push((k, self.val()?));
        }
        if pairs.iter().all(|(k, _)| k.is_string()) {
            let mut m = Map::new();
            for (k, v) in pairs {
                if let (Value::String(k), Some(v)) = (k, v) {
                    m.insert(k, v);
                }
            }
            return Ok(Some(Value::Object(m)));
        }
        // a JS Map with non-string keys
        let mut m = Map::new();
        m.insert("$map".into(), Value::Array(pairs.into_iter().map(|(k, v)| Value::Array(vec![k, v.unwrap_or(Value::Null)])).collect()));
        Ok(Some(Value::Object(m)))
    }
    fn ext(&mut self, n: usize) -> Result<Option<Value>, String> {
        let ty = self.u8()? as i8;
        let d = self.take(n)?;
        if ty == 0 && n == 1 {
            return Ok(None); // msgpackr's undefined
        }
        if ty == -1 {
            let ms = match n {
                4 => u32::from_be_bytes(d.try_into().unwrap()) as f64 * 1000.0,
                8 => {
                    let x = u64::from_be_bytes(d.try_into().unwrap());
                    (x & 0x3_ffff_ffff) as f64 * 1000.0 + (x >> 34) as f64 / 1_000_000.0
                }
                12 => {
                    let ns = u32::from_be_bytes(d[0..4].try_into().unwrap()) as f64;
                    let s = i64::from_be_bytes(d[4..12].try_into().unwrap()) as f64;
                    s * 1000.0 + ns / 1_000_000.0
                }
                _ => return Err(format!("msgpack: timestamp of {n} bytes")),
            };
            let mut m = Map::new();
            m.insert("$date".into(), num(ms));
            return Ok(Some(Value::Object(m)));
        }
        Err(format!("msgpack: unsupported ext type {ty}"))
    }
}

fn num(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() <= 9007199254740991.0 {
        Value::from(f as i64)
    } else {
        serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
    }
}

// ── base64 (the $buf convention) ───────────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(B64[(n >> 18) as usize & 63] as char);
        s.push(B64[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    s
}

pub fn b64_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| B64.iter().position(|&x| x == c).map(|p| p as u32);
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let Some(v) = val(c) else { continue };
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        for v in [
            json!(null),
            json!([true, false, 0, 127, 128, -1, -33, 70000, -70000, 1u64 << 40, -(1i64 << 40), 1.5, "x", "a".repeat(40)]),
            json!({ "a": [1, 2, { "b": "c" }], "d": {} }),
            json!({ "$buf": "aGk=" }),
            json!({ "$date": 1760000000123i64 }),
            json!({ "$map": [[1, "one"], [[2], "two"]] }),
            json!((0..20).collect::<Vec<_>>()),
        ] {
            assert_eq!(decode(&encode(&v)).unwrap(), v, "{v}");
        }
    }

    #[test]
    fn undefined_and_set() {
        assert_eq!(encode(&json!({ "$okdb_undef": true })), vec![0xd4, 0, 0]);
        assert_eq!(decode(&[0x82, 0xa1, b'a', 0x01, 0xa1, b'b', 0xd4, 0, 0]).unwrap(), json!({ "a": 1 }));
        assert_eq!(decode(&encode(&json!({ "$set": [1, 2] }))).unwrap(), json!([1, 2]));
    }
}
