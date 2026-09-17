//! The byte ranges this bridge reports must be the ones the reader itself
//! would slice. Everything downstream trusts those offsets: the caller reads
//! them out of a file this module never sees, so an error here is silent and
//! produces plausible nonsense rather than a failure.
//!
//! The dequantization is `ggml-quants`' own and is tested there. What is tested
//! here is the arithmetic added on top: where a tensor starts in the file, and
//! where a row starts inside a tensor.

use std::collections::BTreeMap;

use zipp_gguf::{GgmlType, GgufFile, TensorInfo, Value};
use zipp_model_wasm::{dtype_by_name, plain};

/// A file with two tensors: one dense, one Q4_K, both with rows that are a
/// whole number of 256-element blocks.
fn fixture() -> (Vec<u8>, Vec<f32>) {
    const WIDTH: usize = 256;
    const ROWS: usize = 6;
    let dense: Vec<f32> = (0..WIDTH * ROWS).map(|i| (i as f32) * 0.25 - 100.0).collect();
    let mut dense_bytes = Vec::with_capacity(dense.len() * 4);
    for value in &dense {
        dense_bytes.extend_from_slice(&value.to_le_bytes());
    }
    // Q4_K blocks of plausible shape; the values need not be meaningful, only
    // consistent between the two ways of reading them.
    let block = zipp_quants::q4_k::BYTES_PER_BLOCK;
    let quantized: Vec<u8> = (0..block * ROWS).map(|i| ((i * 7 + 3) % 251) as u8).collect();

    let metadata = BTreeMap::from([
        ("general.architecture".to_string(), Value::String("test".into())),
        ("general.alignment".to_string(), Value::U32(32)),
    ]);
    let tensors = vec![
        (
            TensorInfo {
                name: "dense".into(),
                shape: vec![WIDTH as u64, ROWS as u64],
                dtype: GgmlType::F32,
                offset: 0,
            },
            dense_bytes,
        ),
        (
            TensorInfo {
                name: "quantized".into(),
                shape: vec![WIDTH as u64, ROWS as u64],
                dtype: GgmlType::Q4_K,
                offset: 0,
            },
            quantized,
        ),
    ];
    let bytes = zipp_gguf::reader::write_to_vec(&metadata, &tensors, 32).expect("write");
    (bytes, dense)
}

#[test]
fn a_tensor_range_is_where_the_reader_finds_it() {
    let (bytes, _) = fixture();
    let file = GgufFile::from_bytes(bytes.clone()).expect("parse");
    let reported: serde_lite::Value = serde_lite::parse(&plain::tensors_json(&file));

    for tensor in file.tensors() {
        let entry = reported.find(&tensor.name);
        let offset = entry.number("offset") as usize;
        let length = entry.number("bytes") as usize;
        // The reader's own slice of this tensor, and the range we told a caller
        // to read, must be the same bytes.
        assert_eq!(&bytes[offset..offset + length], file.tensor_data(tensor),
                   "{} range disagrees with the reader", tensor.name);
    }
}

#[test]
fn a_row_range_lands_on_that_row() {
    let (bytes, dense) = fixture();
    let file = GgufFile::from_bytes(bytes.clone()).expect("parse");
    const WIDTH: usize = 256;

    for (start, count) in [(0u32, 1u32), (2, 3), (5, 1)] {
        let range: serde_lite::Value = serde_lite::parse(
            &plain::row_range_json(&file, "dense", start, count).expect("range"));
        let offset = range.number("offset") as usize;
        let length = range.number("bytes") as usize;
        let elements = range.number("elements") as u32;
        assert_eq!(elements as usize, count as usize * WIDTH);
        let floats = plain::dequantize_bytes(dtype_by_name("F32").unwrap(),
                                            &bytes[offset..offset + length], elements).expect("floats");
        let first = start as usize * WIDTH;
        assert_eq!(floats, &dense[first..first + elements as usize],
                   "rows {start}..{} are not the rows they claim", start + count);
    }
}

#[test]
fn a_quantized_row_starts_on_a_block_boundary() {
    let (bytes, _) = fixture();
    let file = GgufFile::from_bytes(bytes).expect("parse");
    let whole: serde_lite::Value =
        serde_lite::parse(&plain::row_range_json(&file, "quantized", 0, 6).expect("range"));
    let one: serde_lite::Value =
        serde_lite::parse(&plain::row_range_json(&file, "quantized", 3, 1).expect("range"));
    let block = zipp_quants::q4_k::BYTES_PER_BLOCK as f64;
    assert_eq!(one.number("bytes"), block, "a 256-wide Q4_K row is one block");
    assert_eq!(one.number("offset") - whole.number("offset"), 3.0 * block);
    assert_eq!(whole.number("bytes"), 6.0 * block);
}

#[test]
fn rows_past_the_end_are_refused() {
    let (bytes, _) = fixture();
    let file = GgufFile::from_bytes(bytes).expect("parse");
    assert!(plain::row_range_json(&file, "dense", 5, 2).is_err(), "six rows, not seven");
    assert!(plain::row_range_json(&file, "dense", 99, 1).is_err());
    assert!(plain::row_range_json(&file, "missing", 0, 1).is_err());
}

/// The smallest JSON reader that serves these tests, so the crate keeps its
/// two dependencies.
mod serde_lite {
    #[derive(Debug)]
    pub struct Value(String);

    pub fn parse(text: &str) -> Value {
        Value(text.to_string())
    }

    impl Value {
        /// The object in an array whose "name" is `name`, or this value itself.
        pub fn find(&self, name: &str) -> Value {
            let needle = format!("\"name\":\"{name}\"");
            let start = self.0.find(&needle).unwrap_or_else(|| panic!("no entry {name}"));
            let end = self.0[start..].find('}').expect("unterminated") + start;
            Value(self.0[start..end].to_string())
        }

        pub fn number(&self, key: &str) -> f64 {
            let needle = format!("\"{key}\":");
            let start = self.0.find(&needle).unwrap_or_else(|| panic!("no key {key}")) + needle.len();
            let rest = &self.0[start..];
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.' || c == 'e'))
                .unwrap_or(rest.len());
            rest[..end].parse().unwrap_or_else(|_| panic!("{key} is not a number: {rest:.20}"))
        }
    }
}
