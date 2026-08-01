#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, ReactionPair, Reactions,
};
use crate::value::Value;
use crate::vm::{cldr_en, dtf_pattern};
use crate::vm::*;

impl<'p> Vm<'p> {

    /// CreateSegmentsObject / CreateSegmentIterator — the two branded objects
    /// `Intl.Segmenter.prototype.segment` hands out. Both hold the same three
    /// slots ([[SegmentsString]] / [[IteratedString]], the granularity, and the
    /// iterator's cursor), so one constructor covers both; `kind` is what
    /// `containing`/`next` brand-check against.
    ///
    /// `input` is ToString'd here, which is `segment`'s step 3 — a Symbol
    /// argument is a TypeError, and an object's `toString` runs exactly once
    /// (segment-tostring.js).
    pub(crate) fn make_segments(
        &mut self,
        kind: u8,
        input: Value,
        granularity: &str,
        index: usize,
    ) -> Result<Value, Thrown> {
        // A string argument is kept as-is (flattened) rather than round-tripped
        // through `to_js_string`, whose lossy view would turn a lone surrogate
        // into U+FFFD; ToString on a String is the identity anyway.
        let sv = if input.is_heap() && self.heap.is_str_like(input.heap_index()) {
            self.heap.flatten(input.heap_index());
            input
        } else {
            let s = self.to_js_string(input)?;
            self.alloc_str(s)
        };
        let mut r = ObjMap::new();
        r.set("input", sv);
        let g = self.alloc_str(granularity.to_string());
        r.set("granularity", g);
        r.set("index", Value::num(index as f64));
        let resolved = self.heap.alloc(HeapObj::Object(Box::new(r)));
        let idx = self.heap.alloc(HeapObj::Intl { kind, resolved });
        if self.intl_protos[kind as usize] != 0 {
            self.proto_of.insert(idx, Value::heap(self.intl_protos[kind as usize]));
        }
        Ok(Value::heap(idx))
    }


    /// CreateSegmentDataObject: an ordinary object whose own property NAMES are
    /// exactly `segment`, `index`, `input` — plus `isWordLike` at
    /// `granularity: "word"` and at no other granularity, which
    /// `segment-{grapheme,word,sentence}-iterable.js` each assert directly.
    pub(crate) fn segment_data_object(
        &mut self,
        input: Value,
        granularity: &str,
        start: usize,
        end: usize,
    ) -> Value {
        let piece = match self.heap.get(input.heap_index()) {
            HeapObj::Str(js) => js.slice_units(start, end),
            _ => return Value::UNDEFINED,
        };
        let seg = Value::heap(self.heap.alloc_js(piece));
        let mut o = ObjMap::new();
        o.set("segment", seg);
        o.set("index", Value::num(start as f64));
        o.set("input", input);
        if granularity == "word" {
            let text = self.display(input);
            let wl = crate::vm::segmenter::is_word_like(&text, start);
            o.set("isWordLike", Value::bool(wl));
        }
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))
    }

}
