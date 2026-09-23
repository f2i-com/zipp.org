//! The native CLI's synchronous GPU bridge: the embedder side of
//! `_zipp_gpu.native` (zipp-vm's `runtime/native_gpu.js`, feature
//! `python-native-gpu`).
//!
//! A Python program compiled *unhosted* evaluates its graphs where it posts
//! them, before `submit` returns. With this bridge installed as the Python
//! state's `__zippHostCall` handler, zipp_gpu first offers each request to
//! the GPU: the call below runs it on gpu-lab's runtime (in its own
//! JavaScript state, see [`GpuHost`]) and returns the reply before the
//! program continues, so the program's semantics are the CPU evaluator's
//! whether or not a GPU is present.
//!
//! The GPU starts lazily: the first `zipp.gpu.open` (a program's first
//! graph) finds the adapter and brings the runtime up; a program that never
//! submits a graph never touches the GPU.
//!
//! Host calls:
//! - `zipp.gpu.open` -> `"1"` or `"0"`.
//! - `zipp.gpu.request(kind, json, bytes)`: the payload as JSON whose
//!   `{"$f32": [offset, length]}` objects are float32 arrays in the first
//!   `bytes` bytes of the Python state's `__zipp_ngpu_up`. Returns
//!   `"<n>\n<json>"`: the reply, its arrays likewise out of line in `n` bytes
//!   that `zipp.gpu.fetch` then copies into `__zipp_ngpu_down`.
use zipp_vm::embed::{HostCtx, HostValue};

use crate::{GpuHost, GpuOptions};

enum Opened {
    Unknown,
    Yes(Box<GpuHost>),
    No,
}

/// The state behind one Python program's `__zippHostCall`.
pub struct SyncBridge {
    options: GpuOptions,
    log: bool,
    host: Opened,
    pending: Vec<u8>,
    next_id: f64,
}

impl SyncBridge {
    /// `log`: name the adapter (or its absence) on stderr when the GPU is
    /// first asked for.
    pub fn new(options: GpuOptions, log: bool) -> Self {
        SyncBridge {
            options,
            log,
            host: Opened::Unknown,
            pending: Vec::new(),
            next_id: 1.0,
        }
    }

    /// The adapter, once the GPU has been opened on it.
    pub fn adapter(&self) -> Option<&crate::AdapterSummary> {
        match &self.host {
            Opened::Yes(host) => Some(host.adapter()),
            _ => None,
        }
    }

    fn open(&mut self) -> bool {
        if let Opened::Unknown = self.host {
            self.host = match GpuHost::open(&self.options) {
                Ok(Some(host)) => {
                    if self.log {
                        eprintln!("zipp: GPU: {}", host.adapter().describe());
                    }
                    Opened::Yes(Box::new(host))
                }
                Ok(None) => {
                    if self.log {
                        eprintln!(
                            "zipp: GPU: no hardware adapter; graphs run on the CPU evaluator"
                        );
                    }
                    Opened::No
                }
                Err(error) => {
                    // An adapter exists but the runtime cannot start on it: a
                    // machine problem worth one line.
                    eprintln!("zipp: {error}; graphs run on the CPU evaluator");
                    Opened::No
                }
            };
        }
        matches!(self.host, Opened::Yes(_))
    }

    /// Serve one `__zippHostCall` from the Python state.
    pub fn call(
        &mut self,
        ctx: &mut dyn HostCtx,
        kind: &str,
        args: &[String],
    ) -> Result<String, String> {
        match kind {
            "zipp.gpu.open" => Ok(if self.open() { "1" } else { "0" }.into()),
            "zipp.gpu.request" => {
                if !self.open() {
                    return Err("RuntimeError: no native GPU host".into());
                }
                let request = args.first().ok_or("TypeError: request kind")?.clone();
                let text = args.get(1).ok_or("TypeError: request payload")?;
                let bytes: usize = args
                    .get(2)
                    .and_then(|s| s.parse().ok())
                    .ok_or("TypeError: request byte count")?;
                let upload = crate::device::region(ctx, "__zipp_ngpu_up", bytes)?;
                let json: serde_json::Value = serde_json::from_str(text)
                    .map_err(|e| format!("TypeError: GPU request: {e}"))?;
                let payload = to_host(&json, &upload[..bytes])?;
                let id = self.next_id;
                self.next_id += 1.0;
                let Opened::Yes(host) = &mut self.host else {
                    unreachable!("opened above")
                };
                let reply = host.handle(id, &request, payload);
                let mut blob = Vec::new();
                let mut out = String::new();
                write_json(&reply, &mut out, &mut blob);
                let head = format!("{}\n{out}", blob.len());
                self.pending = blob;
                Ok(head)
            }
            "zipp.gpu.fetch" => {
                let blob = std::mem::take(&mut self.pending);
                let target = crate::device::region(ctx, "__zipp_ngpu_down", blob.len())?;
                target[..blob.len()].copy_from_slice(&blob);
                Ok(String::new())
            }
            _ => Err(format!("TypeError: unknown host call '{kind}'")),
        }
    }
}

/// A request's JSON as a host value, `{"$f32": [offset, length]}` read from `bytes`.
fn to_host(value: &serde_json::Value, bytes: &[u8]) -> Result<HostValue, String> {
    use serde_json::Value;
    Ok(match value {
        Value::Null => HostValue::Null,
        Value::Bool(b) => HostValue::Bool(*b),
        Value::Number(n) => HostValue::Number(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => HostValue::String(s.clone()),
        Value::Array(items) => HostValue::Array(
            items
                .iter()
                .map(|v| to_host(v, bytes))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(Value::Array(at)) = map.get("$f32") {
                    let offset = at
                        .first()
                        .and_then(Value::as_u64)
                        .ok_or("TypeError: $f32")? as usize;
                    let length =
                        at.get(1).and_then(Value::as_u64).ok_or("TypeError: $f32")? as usize;
                    let end = length
                        .checked_mul(4)
                        .and_then(|n| n.checked_add(offset))
                        .filter(|&end| end <= bytes.len())
                        .ok_or("RangeError: GPU request tensor outside its buffer")?;
                    return Ok(HostValue::Float32Array(
                        bytes[offset..end]
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                            .collect(),
                    ));
                }
            }
            HostValue::Object(
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), to_host(v, bytes)?)))
                    .collect::<Result<_, String>>()?,
            )
        }
    })
}

fn json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A reply as JSON (object keys in their order), float32 arrays into `blob`.
fn write_json(value: &HostValue, out: &mut String, blob: &mut Vec<u8>) {
    match value {
        HostValue::Undefined | HostValue::Null | HostValue::Opaque => out.push_str("null"),
        HostValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        // Rust prints the shortest text that reads back as the same f64.
        HostValue::Number(n) if n.is_finite() => out.push_str(&format!("{n}")),
        HostValue::Number(_) => out.push_str("null"),
        HostValue::String(s) => json_string(s, out),
        HostValue::Utf16(units) => json_string(&String::from_utf16_lossy(units), out),
        HostValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out, blob);
            }
            out.push(']');
        }
        HostValue::Object(pairs) => {
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(k, out);
                out.push(':');
                write_json(v, out, blob);
            }
            out.push('}');
        }
        HostValue::Float32Array(values) => {
            out.push_str(&format!("{{\"$f32\":[{},{}]}}", blob.len(), values.len()));
            blob.reserve(values.len() * 4);
            for v in values {
                blob.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_round_trip_keeps_order_numbers_and_tensors() {
        let value = HostValue::Object(vec![
            ("z".into(), HostValue::Number(0.1)),
            ("a".into(), HostValue::Number(-0.0)),
            ("t".into(), HostValue::Float32Array(vec![1.5, -2.0])),
            ("s".into(), HostValue::String("q\"\n".into())),
            (
                "n".into(),
                HostValue::Array(vec![HostValue::Null, HostValue::Bool(true)]),
            ),
        ]);
        let (mut out, mut blob) = (String::new(), Vec::new());
        write_json(&value, &mut out, &mut blob);
        assert_eq!(
            out,
            r#"{"z":0.1,"a":-0,"t":{"$f32":[0,2]},"s":"q\"\u000a","n":[null,true]}"#
        );
        let back = to_host(&serde_json::from_str(&out).unwrap(), &blob).unwrap();
        let HostValue::Object(pairs) = back else {
            panic!()
        };
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).unwrap().1.clone();
        assert_eq!(get("t"), HostValue::Float32Array(vec![1.5, -2.0]));
        assert_eq!(get("z"), HostValue::Number(0.1));
        assert!(to_host(&serde_json::from_str(r#"{"$f32":[4,2]}"#).unwrap(), &blob).is_err());
    }
}
