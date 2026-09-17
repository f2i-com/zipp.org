//! GGUF reading and block dequantization, for a browser.
//!
//! The `gguf` and `ggml-quants` crates are pure computation and already compile
//! to `wasm32-unknown-unknown` unchanged; this is only the boundary that lets
//! JavaScript hand over bytes and take back floats.
//!
//! It deliberately never holds the file. A quantized checkpoint is gigabytes --
//! the two on the machine this was written on are 6.8 GB and 15.7 GB -- so the
//! header is parsed from the first few megabytes and everything after that is a
//! byte range the caller reads for itself, from a `File`, a `Blob` or a ranged
//! request, and hands back one tensor at a time. That also means a row of an
//! embedding table costs a row: `row_range` locates it, because every block
//! format here packs a whole number of blocks into each row.
//!
//! Nothing here executes a model. It turns a file into tensors; what runs them
//! is the caller's business.

use zipp_gguf::{GgmlType, GgufFile};
use wasm_bindgen::prelude::*;
use zipp_tokenizer::Pattern;

fn fail(error: impl core::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

/// JSON-escape into an existing buffer. Metadata is arbitrary text out of a
/// file the caller did not write, so it is escaped rather than trusted.
fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A finite number, or null: JSON cannot say infinity, and a metadata field is
/// not worth failing a load over.
fn push_json_number(out: &mut String, value: f64) {
    if value.is_finite() {
        out.push_str(&format!("{value}"));
    } else {
        out.push_str("null");
    }
}

fn push_value(out: &mut String, value: &zipp_gguf::Value) {
    use zipp_gguf::Value;
    match value {
        Value::U8(v) => out.push_str(&v.to_string()),
        Value::I8(v) => out.push_str(&v.to_string()),
        Value::U16(v) => out.push_str(&v.to_string()),
        Value::I16(v) => out.push_str(&v.to_string()),
        Value::U32(v) => out.push_str(&v.to_string()),
        Value::I32(v) => out.push_str(&v.to_string()),
        Value::U64(v) => out.push_str(&v.to_string()),
        Value::I64(v) => out.push_str(&v.to_string()),
        Value::F32(v) => push_json_number(out, *v as f64),
        Value::F64(v) => push_json_number(out, *v),
        Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        Value::String(v) => push_json_string(out, v),
        Value::Array(array) => push_array(out, array),
    }
}

/// How many entries an array may have before the metadata names its length
/// instead of listing it. A vocabulary and its per-token types run to hundreds
/// of thousands of entries; inlining those would make every reader of the
/// metadata pay for tables most of them never look at, and would push a modest
/// bounded JSON parser past its item budget on a perfectly ordinary file.
const INLINE_LIMIT: usize = 64;

fn array_len(array: &zipp_gguf::Array) -> usize {
    use zipp_gguf::Array;
    match array {
        Array::U8(v) => v.len(),
        Array::I8(v) => v.len(),
        Array::U16(v) => v.len(),
        Array::I16(v) => v.len(),
        Array::U32(v) => v.len(),
        Array::I32(v) => v.len(),
        Array::U64(v) => v.len(),
        Array::I64(v) => v.len(),
        Array::F32(v) => v.len(),
        Array::F64(v) => v.len(),
        Array::Bool(v) => v.len(),
        Array::String(v) => v.len(),
    }
}

fn push_array(out: &mut String, array: &zipp_gguf::Array) {
    use zipp_gguf::Array;
    let count = array_len(array);
    if count > INLINE_LIMIT {
        let kind = if matches!(array, Array::String(_)) { "strings" } else { "numbers" };
        out.push_str(&format!("{{\"{kind}\":{count}}}"));
        return;
    }
    out.push('[');
    macro_rules! numbers {
        ($items:expr) => {{
            for (index, item) in $items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&item.to_string());
            }
        }};
    }
    macro_rules! floats {
        ($items:expr) => {{
            for (index, item) in $items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_json_number(out, *item as f64);
            }
        }};
    }
    match array {
        Array::U8(items) => numbers!(items),
        Array::I8(items) => numbers!(items),
        Array::U16(items) => numbers!(items),
        Array::I16(items) => numbers!(items),
        Array::U32(items) => numbers!(items),
        Array::I32(items) => numbers!(items),
        Array::U64(items) => numbers!(items),
        Array::I64(items) => numbers!(items),
        Array::F32(items) => floats!(items),
        Array::F64(items) => floats!(items),
        Array::Bool(items) => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(if *item { "true" } else { "false" });
            }
        }
        Array::String(items) => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_json_string(out, item);
            }
        }
    }
    out.push(']');
}

/// The parts that are arithmetic rather than boundary, as plain Rust. The
/// wasm_bindgen layer below wraps these; they are what the tests exercise,
/// because a `JsValue` cannot be constructed off the wasm target at all.
pub mod plain {
    use super::{push_json_string, GgmlType, GgufFile};

    pub fn tensors_json(file: &GgufFile) -> String {
        let start = file.tensor_data_start();
        let mut out = String::from("[");
        for (index, tensor) in file.tensors().iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"name\":");
            push_json_string(&mut out, &tensor.name);
            out.push_str(",\"shape\":[");
            for (axis, extent) in tensor.shape.iter().enumerate() {
                if axis > 0 {
                    out.push(',');
                }
                out.push_str(&extent.to_string());
            }
            out.push_str("],\"dtype\":");
            push_json_string(&mut out, &format!("{:?}", tensor.dtype));
            out.push_str(&format!(
                ",\"elements\":{},\"offset\":{},\"bytes\":{},\"readable\":{}}}",
                tensor.numel(),
                start + tensor.offset,
                tensor.nbytes(),
                zipp_quants::is_supported(tensor.dtype)
            ));
        }
        out.push(']');
        out
    }

    pub fn row_range_json(file: &GgufFile, name: &str, start: u32, count: u32) -> Result<String, String> {
        let info = file
            .tensor_by_name(name)
            .ok_or_else(|| format!("no tensor named {name}"))?;
        if info.shape.len() < 2 {
            return Err("row reads need a tensor of rank two or more".into());
        }
        let width = info.shape[0];
        let rows: u64 = info.shape[1..].iter().product();
        let (start, count) = (start as u64, count as u64);
        if start.saturating_add(count) > rows {
            return Err(format!(
                "rows {start}..{} are outside {name}, which has {rows}",
                start + count
            ));
        }
        let block = info.dtype.block_size() as u64;
        if width % block != 0 {
            return Err(format!(
                "{name} has rows of {width}, not a whole number of {block}-element blocks"
            ));
        }
        let row_bytes = (width / block) * info.dtype.type_size() as u64;
        Ok(format!(
            "{{\"offset\":{},\"bytes\":{},\"elements\":{},\"dtype\":\"{:?}\"}}",
            file.tensor_data_start() + info.offset + start * row_bytes,
            count * row_bytes,
            count * width,
            info.dtype
        ))
    }

    pub fn dequantize_bytes(dtype: GgmlType, bytes: &[u8], elements: u32) -> Result<Vec<f32>, String> {
        let mut out = vec![0f32; elements as usize];
        zipp_quants::dequantize(dtype, bytes, &mut out).map_err(|e| e.to_string())?;
        Ok(out)
    }
}

pub fn dtype_by_name(name: &str) -> Result<GgmlType, String> {
    // Matched by the name this module prints, so a caller round-trips what it
    // was told rather than guessing at ggml's numbering.
    Ok(match name {
        "F32" => GgmlType::F32,
        "F16" => GgmlType::F16,
        "BF16" => GgmlType::BF16,
        "Q4_0" => GgmlType::Q4_0,
        "Q4_1" => GgmlType::Q4_1,
        "Q5_0" => GgmlType::Q5_0,
        "Q5_1" => GgmlType::Q5_1,
        "Q8_0" => GgmlType::Q8_0,
        "Q8_1" => GgmlType::Q8_1,
        "Q2_K" => GgmlType::Q2_K,
        "Q3_K" => GgmlType::Q3_K,
        "Q4_K" => GgmlType::Q4_K,
        "Q5_K" => GgmlType::Q5_K,
        "Q6_K" => GgmlType::Q6_K,
        "Q8_K" => GgmlType::Q8_K,
        "IQ4_NL" => GgmlType::IQ4_NL,
        "IQ4_XS" => GgmlType::IQ4_XS,
        other => return Err(format!("unknown dtype {other}")),
    })
}

fn dtype_of(name: &str) -> Result<GgmlType, JsValue> {
    // Matched by the name this module prints, so a caller round-trips what it
    // was told rather than guessing at ggml's numbering.
    dtype_by_name(name).map_err(|e| JsValue::from_str(&e))
}

/// A GGUF file's header: its metadata and where every tensor lives.
#[wasm_bindgen]
pub struct GgufHeader {
    file: GgufFile,
}

#[wasm_bindgen]
impl GgufHeader {
    /// Parse from the beginning of a file. `head` needs only to reach the end
    /// of the tensor table; nothing after that is read here, and a buffer that
    /// stops short fails with an end-of-input error the caller can answer by
    /// reading more.
    #[wasm_bindgen(constructor)]
    pub fn new(head: Vec<u8>) -> Result<GgufHeader, JsValue> {
        Ok(Self {
            file: GgufFile::from_bytes(head).map_err(fail)?,
        })
    }

    pub fn version(&self) -> u32 {
        self.file.version()
    }

    /// Where tensor data begins in the file.
    pub fn data_start(&self) -> f64 {
        self.file.tensor_data_start() as f64
    }

    /// The metadata as JSON. An array longer than 64 entries appears as
    /// `{"strings": count}` or `{"numbers": count}`; fetch those by name with
    /// `strings` or `numbers`.
    pub fn metadata(&self) -> String {
        let mut out = String::from("{");
        for (index, (key, value)) in self.file.metadata().iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_string(&mut out, key);
            out.push(':');
            push_value(&mut out, value);
        }
        out.push('}');
        out
    }

    /// One metadata numeric array by key, as float64: per-token types, scores,
    /// or any other table the metadata summarised rather than inlined.
    pub fn numbers(&self, key: &str) -> Result<Vec<f64>, JsValue> {
        use zipp_gguf::{Array, Value};
        let Some(Value::Array(array)) = self.file.metadata().get(key) else {
            return Err(JsValue::from_str(&format!("no numeric array at {key}")));
        };
        macro_rules! cast {
            ($items:expr) => {
                Ok($items.iter().map(|v| *v as f64).collect())
            };
        }
        match array {
            Array::U8(v) => cast!(v),
            Array::I8(v) => cast!(v),
            Array::U16(v) => cast!(v),
            Array::I16(v) => cast!(v),
            Array::U32(v) => cast!(v),
            Array::I32(v) => cast!(v),
            Array::U64(v) => cast!(v),
            Array::I64(v) => cast!(v),
            Array::F32(v) => cast!(v),
            Array::F64(v) => Ok(v.clone()),
            Array::Bool(v) => Ok(v.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect()),
            Array::String(_) => Err(JsValue::from_str(&format!("{key} is strings; use strings()"))),
        }
    }

    /// One metadata string array by key: a tokenizer's vocabulary or its merges.
    pub fn strings(&self, key: &str) -> Result<Vec<JsValue>, JsValue> {
        match self.file.metadata().get(key) {
            Some(zipp_gguf::Value::Array(zipp_gguf::Array::String(items))) => {
                Ok(items.iter().map(|s| JsValue::from_str(s)).collect())
            }
            Some(_) => Err(JsValue::from_str(&format!("{key} is not a string array"))),
            None => Err(JsValue::from_str(&format!("no metadata key {key}"))),
        }
    }

    /// The tokenizer this checkpoint was trained with.
    ///
    /// Built here, from the file's own metadata, and never handed out as data:
    /// a vocabulary is 151,936 strings and a merge table 151,387, and moving
    /// those across the boundary costs more than tokenizing with them.
    pub fn tokenizer(&self) -> Result<Tokenizer, JsValue> {
        let strings = |key: &str| -> Result<Vec<String>, JsValue> {
            match self.file.metadata().get(key) {
                Some(zipp_gguf::Value::Array(zipp_gguf::Array::String(items))) => Ok(items.clone()),
                Some(_) => Err(JsValue::from_str(&format!("{key} is not a string array"))),
                None => Err(JsValue::from_str(&format!("this checkpoint has no {key}"))),
            }
        };
        let number = |key: &str| -> Option<u32> {
            self.file.metadata().get(key).and_then(|v| match v {
                zipp_gguf::Value::U32(n) => Some(*n),
                zipp_gguf::Value::I32(n) => u32::try_from(*n).ok(),
                zipp_gguf::Value::U64(n) => u32::try_from(*n).ok(),
                _ => None,
            })
        };
        // The pre-tokenizer is refused rather than guessed: the wrong pattern
        // gives token ids that are individually valid and collectively wrong.
        let named = match self.file.metadata().get("tokenizer.ggml.pre") {
            Some(zipp_gguf::Value::String(name)) => name.clone(),
            _ => String::from("default"),
        };
        let pattern = Pattern::from_name(&named).ok_or_else(|| JsValue::from_str(
            &format!("tokenizer.ggml.pre is {named:?}, which this build does not scan; \
                      refusing to guess a pre-tokenizer")))?;

        let tokens = strings("tokenizer.ggml.tokens")?;
        // A model with no merge list is not byte-level BPE; say so by name.
        let merges = strings("tokenizer.ggml.merges")?;
        // Control and user-defined tokens are matched literally before BPE, so
        // a chat marker stays one token instead of six.
        let mut specials = Vec::new();
        if let Some(zipp_gguf::Value::Array(zipp_gguf::Array::I32(kinds))) =
            self.file.metadata().get("tokenizer.ggml.token_type") {
            for (id, kind) in kinds.iter().enumerate() {
                if *kind == 3 || *kind == 4 { specials.push(id as u32); }
            }
        }
        Ok(Tokenizer {
            inner: zipp_tokenizer::Tokenizer::new(tokens, merges, pattern, specials,
                                                  number("tokenizer.ggml.unknown_token_id")),
            bos: number("tokenizer.ggml.bos_token_id"),
            eos: number("tokenizer.ggml.eos_token_id"),
            pre: named,
        })
    }

    /// Every tensor as JSON: name, shape, dtype, and the byte range holding it.
    /// GGUF writes a shape fastest-varying first and it is reported as stored,
    /// so a caller reading the format's own documentation sees these numbers.
    pub fn tensors(&self) -> String {
        plain::tensors_json(&self.file)
    }

    /// Where `count` rows of a tensor live, as JSON `{offset, bytes, elements,
    /// dtype}`. A row is the fastest-varying axis, which for an embedding table
    /// is one token's vector: reading those rows costs a few kilobytes where
    /// the whole table would cost a gigabyte.
    pub fn row_range(&self, name: &str, start: u32, count: u32) -> Result<String, JsValue> {
        plain::row_range_json(&self.file, name, start, count)
            .map_err(|e| JsValue::from_str(&e))
    }
}

/// Turn one tensor's bytes into float32. `bytes` is exactly the range the
/// header described, read by the caller; this module never sees the file.
/// A checkpoint's own tokenizer. Text in, ids out.
#[wasm_bindgen]
pub struct Tokenizer {
    inner: zipp_tokenizer::Tokenizer,
    bos: Option<u32>,
    eos: Option<u32>,
    pre: String,
}

#[wasm_bindgen]
impl Tokenizer {
    pub fn encode(&self, text: &str) -> Result<Vec<u32>, JsValue> {
        self.inner.encode(text).map_err(fail)
    }

    pub fn decode(&self, ids: &[u32]) -> String {
        self.inner.decode(ids)
    }

    #[wasm_bindgen(getter)]
    pub fn vocab_size(&self) -> usize { self.inner.vocab_size() }

    /// Which pre-tokenizer pattern this vocabulary was trained with.
    #[wasm_bindgen(getter)]
    pub fn pre(&self) -> String { self.pre.clone() }

    #[wasm_bindgen(getter)]
    pub fn bos(&self) -> Option<u32> { self.bos }

    #[wasm_bindgen(getter)]
    pub fn eos(&self) -> Option<u32> { self.eos }

    /// One token's text, in the byte-level alphabet the vocabulary uses.
    pub fn token(&self, id: u32) -> Option<String> {
        self.inner.token(id).map(String::from)
    }

    pub fn id_of(&self, piece: &str) -> Option<u32> { self.inner.id_of(piece) }
}

#[wasm_bindgen]
pub fn dequantize(dtype: &str, bytes: &[u8], elements: u32) -> Result<Vec<f32>, JsValue> {
    plain::dequantize_bytes(dtype_of(dtype)?, bytes, elements).map_err(|e| JsValue::from_str(&e))
}

/// The dtypes this build decodes, so a host can say so before it starts.
#[wasm_bindgen]
pub fn supported_dtypes() -> Vec<JsValue> {
    const ALL: [GgmlType; 17] = [
        GgmlType::F32,
        GgmlType::F16,
        GgmlType::BF16,
        GgmlType::Q4_0,
        GgmlType::Q4_1,
        GgmlType::Q5_0,
        GgmlType::Q5_1,
        GgmlType::Q8_0,
        GgmlType::Q8_1,
        GgmlType::Q2_K,
        GgmlType::Q3_K,
        GgmlType::Q4_K,
        GgmlType::Q5_K,
        GgmlType::Q6_K,
        GgmlType::Q8_K,
        GgmlType::IQ4_NL,
        GgmlType::IQ4_XS,
    ];
    ALL.iter()
        .filter(|dtype| zipp_quants::is_supported(**dtype))
        .map(|dtype| JsValue::from_str(&format!("{dtype:?}")))
        .collect()
}
