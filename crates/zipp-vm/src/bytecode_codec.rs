//! A compiled [`Program`] as bytes and back: what lets the Python runtime
//! (about 0.7 MB of JavaScript every Python program starts from) be compiled
//! once and then loaded, instead of compiled, by every later process.
//!
//! The encoding is private to one build of the engine: [`FORMAT`] and the
//! caller's key (see `frontend::python::runtime_cache`) decide whether bytes
//! may be decoded at all. Decoding reproduces the program exactly —
//! `decode(encode(p))` prints (`{:?}`) the same as `p` — and a truncated or
//! mangled input is an error, never a panic. `Instr` is encoded by code
//! generated from `bytecode.rs` itself (build/instr_codec.rs).

use crate::bytecode::*;
use crate::value::Value;

/// Bumped when this file's layout changes.
pub(crate) const FORMAT: u32 = 1;

pub(crate) struct Writer {
    pub(crate) out: Vec<u8>,
}

impl Writer {
    #[inline]
    fn u64(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.out.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.out.push(v as u8);
    }
    #[inline]
    fn u128(&mut self, mut v: u128) {
        while v >= 0x80 {
            self.out.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.out.push(v as u8);
    }
    #[inline]
    fn u32(&mut self, v: u32) {
        self.u64(v as u64)
    }
    #[inline]
    fn u16(&mut self, v: u16) {
        self.u64(v as u64)
    }
    #[inline]
    fn u8(&mut self, v: u8) {
        self.out.push(v)
    }
    #[inline]
    fn bool(&mut self, v: bool) {
        self.out.push(v as u8)
    }
    #[inline]
    fn i64(&mut self, v: i64) {
        self.u64(((v << 1) ^ (v >> 63)) as u64)
    }
    #[inline]
    fn i128(&mut self, v: i128) {
        self.u128(((v << 1) ^ (v >> 127)) as u128)
    }
    #[allow(dead_code)]
    #[inline]
    fn f64(&mut self, v: f64) {
        self.out.extend_from_slice(&v.to_bits().to_le_bytes())
    }
    #[inline]
    fn opt_u16(&mut self, v: Option<u16>) {
        self.u64(v.map_or(0, |v| v as u64 + 1))
    }
    #[inline]
    fn opt_u32(&mut self, v: Option<u32>) {
        self.u64(v.map_or(0, |v| v as u64 + 1))
    }
    fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.out.extend_from_slice(s.as_bytes());
    }
    fn opt_str(&mut self, s: &Option<String>) {
        match s {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
        }
    }
    fn len(&mut self, n: usize) {
        self.u64(n as u64)
    }
    fn strs(&mut self, v: &[String]) {
        self.len(v.len());
        for s in v {
            self.str(s);
        }
    }
    fn u32s(&mut self, v: &[u32]) {
        self.len(v.len());
        for &x in v {
            self.u32(x);
        }
    }
    fn named(&mut self, v: &[(String, u32)]) {
        self.len(v.len());
        for (s, x) in v {
            self.str(s);
            self.u32(*x);
        }
    }
}

pub(crate) struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

type R<T> = Result<T, String>;

impl<'a> Reader<'a> {
    #[inline]
    fn byte(&mut self) -> R<u8> {
        let b = *self
            .data
            .get(self.at)
            .ok_or_else(|| "truncated bytecode".to_owned())?;
        self.at += 1;
        Ok(b)
    }
    #[inline]
    fn u64(&mut self) -> R<u64> {
        // Most numbers (registers, small indices) take one byte.
        if let Some(&b) = self.data.get(self.at) {
            if b < 0x80 {
                self.at += 1;
                return Ok(b as u64);
            }
        }
        self.u64_slow()
    }
    #[cold]
    fn u64_slow(&mut self) -> R<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            if shift >= 64 {
                return Err("bad varint".into());
            }
            v |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    #[inline]
    fn u128(&mut self) -> R<u128> {
        let mut v = 0u128;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            if shift >= 128 {
                return Err("bad varint".into());
            }
            v |= ((b & 0x7F) as u128) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    #[inline]
    fn u32(&mut self) -> R<u32> {
        u32::try_from(self.u64()?).map_err(|_| "u32 out of range".into())
    }
    #[inline]
    fn u16(&mut self) -> R<u16> {
        u16::try_from(self.u64()?).map_err(|_| "u16 out of range".into())
    }
    #[inline]
    fn u8(&mut self) -> R<u8> {
        self.byte()
    }
    #[inline]
    fn bool(&mut self) -> R<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(format!("bad bool {b}")),
        }
    }
    #[inline]
    fn i64(&mut self) -> R<i64> {
        let v = self.u64()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    #[inline]
    fn i128(&mut self) -> R<i128> {
        let v = self.u128()?;
        Ok(((v >> 1) as i128) ^ -((v & 1) as i128))
    }
    #[allow(dead_code)]
    #[inline]
    fn f64(&mut self) -> R<f64> {
        let bytes = self
            .data
            .get(self.at..self.at + 8)
            .ok_or_else(|| "truncated bytecode".to_owned())?;
        self.at += 8;
        Ok(f64::from_bits(u64::from_le_bytes(bytes.try_into().unwrap())))
    }
    #[inline]
    fn opt_u16(&mut self) -> R<Option<u16>> {
        match self.u64()? {
            0 => Ok(None),
            v => u16::try_from(v - 1)
                .map(Some)
                .map_err(|_| "u16 out of range".into()),
        }
    }
    #[inline]
    fn opt_u32(&mut self) -> R<Option<u32>> {
        match self.u64()? {
            0 => Ok(None),
            v => u32::try_from(v - 1)
                .map(Some)
                .map_err(|_| "u32 out of range".into()),
        }
    }
    fn len(&mut self) -> R<usize> {
        let n = self.u64()? as usize;
        // No list is longer than the bytes left (every element takes one).
        if n > self.data.len() - self.at {
            return Err("bad length".into());
        }
        Ok(n)
    }
    fn str(&mut self) -> R<String> {
        let n = self.len()?;
        let bytes = &self.data[self.at..self.at + n];
        self.at += n;
        String::from_utf8(bytes.to_vec()).map_err(|_| "bad UTF-8".into())
    }
    fn opt_str(&mut self) -> R<Option<String>> {
        Ok(match self.u8()? {
            0 => None,
            1 => Some(self.str()?),
            b => return Err(format!("bad option {b}")),
        })
    }
    fn strs(&mut self) -> R<Vec<String>> {
        let n = self.len()?;
        (0..n).map(|_| self.str()).collect()
    }
    fn u32s(&mut self) -> R<Vec<u32>> {
        let n = self.len()?;
        (0..n).map(|_| self.u32()).collect()
    }
    fn named(&mut self) -> R<Vec<(String, u32)>> {
        let n = self.len()?;
        (0..n).map(|_| Ok((self.str()?, self.u32()?))).collect()
    }
}

include!(concat!(env!("OUT_DIR"), "/instr_codec.rs"));

/// A function's source text: a slice of the program's source when found
/// there (the compiler slices it from there), so it is stored as a range.
struct Sources<'a> {
    base: Option<&'a str>,
    /// Where the last function's text started: functions come in source
    /// order, so the next one is usually just after it.
    cursor: usize,
}

impl Sources<'_> {
    fn encode(&mut self, w: &mut Writer, text: &str) {
        if let Some(base) = self.base.filter(|_| !text.is_empty()) {
            // Any occurrence will do (the text is what is stored). Look near
            // the last function's text first: a nested function's text is
            // usually just after its predecessor's, or (functions listed
            // children first) just before.
            const NEAR: usize = 1 << 14;
            let find_in = |lo: usize, hi: usize| {
                let (lo, hi) = (base.floor_char_boundary(lo), base.floor_char_boundary(hi));
                base[lo..hi].find(text).map(|i| i + lo)
            };
            let cursor = self.cursor.min(base.len());
            let at = find_in(cursor, cursor.saturating_add(text.len() + NEAR).min(base.len()))
                .or_else(|| {
                    find_in(
                        cursor.saturating_sub(NEAR),
                        cursor.saturating_add(text.len()).min(base.len()),
                    )
                })
                .or_else(|| base.find(text));
            if let Some(at) = at {
                self.cursor = at;
                w.u8(1);
                w.len(at);
                w.len(text.len());
                return;
            }
        }
        w.u8(0);
        w.str(text);
    }

    fn decode(&self, r: &mut Reader) -> R<String> {
        match r.u8()? {
            0 => r.str(),
            1 => {
                let at = r.u64()? as usize;
                let len = r.u64()? as usize;
                self.base
                    .and_then(|base| base.get(at..at.checked_add(len)?))
                    .map(str::to_owned)
                    .ok_or_else(|| "source range outside the program's source".into())
            }
            b => Err(format!("bad source kind {b}")),
        }
    }
}

fn encode_func(w: &mut Writer, f: &FuncProto, sources: &mut Sources) {
    // Every field, so a new one is a compile error here.
    let FuncProto {
        name,
        code,
        reg_count,
        param_count,
        length,
        rest_reg,
        arguments_reg,
        is_generator,
        is_async,
        non_constructable,
        lexical_this,
        super_static,
        is_strict,
        simple_params,
        constants,
        string_constants,
        static_key_plans,
        bigint_consts,
        wtf8_consts,
        name_global,
        upvalues,
        eval_sites,
        source,
    } = f;
    w.str(name);
    w.len(code.len());
    for i in code {
        encode_instr(w, i);
    }
    w.u16(*reg_count);
    w.u16(*param_count);
    w.u16(*length);
    w.opt_u16(*rest_reg);
    w.opt_u16(*arguments_reg);
    for b in [
        *is_generator,
        *is_async,
        *non_constructable,
        *lexical_this,
        *super_static,
        *is_strict,
        *simple_params,
    ] {
        w.bool(b);
    }
    w.len(constants.len());
    for c in constants {
        w.u64(c.bits());
    }
    w.strs(string_constants);
    w.len(static_key_plans.len());
    for p in static_key_plans {
        w.strs(p.keys());
    }
    w.len(bigint_consts.len());
    for b in bigint_consts {
        let bytes = b.to_signed_bytes_le();
        w.len(bytes.len());
        w.out.extend_from_slice(&bytes);
    }
    w.u32s(wtf8_consts);
    w.opt_u32(*name_global);
    w.len(upvalues.len());
    for u in upvalues {
        match u {
            UpvalSource::ParentLocal(r) => {
                w.u8(0);
                w.u16(*r);
            }
            UpvalSource::ParentUpval(i) => {
                w.u8(1);
                w.u16(*i);
            }
        }
    }
    w.len(eval_sites.len());
    for (bindings, collisions, lexical) in eval_sites {
        w.len(bindings.len());
        for (name, kind, idx) in bindings {
            w.str(name);
            w.u8(*kind);
            w.u16(*idx);
        }
        match collisions {
            None => w.u8(0),
            Some(v) => {
                w.u8(1);
                w.strs(v);
            }
        }
        w.strs(lexical);
    }
    sources.encode(w, source);
}

fn decode_func(r: &mut Reader, sources: &Sources) -> R<FuncProto> {
    let name = r.str()?;
    let n = r.len()?;
    let mut code = Vec::with_capacity(n);
    for _ in 0..n {
        code.push(decode_instr(r)?);
    }
    let reg_count = r.u16()?;
    let param_count = r.u16()?;
    let length = r.u16()?;
    let rest_reg = r.opt_u16()?;
    let arguments_reg = r.opt_u16()?;
    let is_generator = r.bool()?;
    let is_async = r.bool()?;
    let non_constructable = r.bool()?;
    let lexical_this = r.bool()?;
    let super_static = r.bool()?;
    let is_strict = r.bool()?;
    let simple_params = r.bool()?;
    let n = r.len()?;
    let mut constants = Vec::with_capacity(n);
    for _ in 0..n {
        constants.push(Value::from_bits(r.u64()?));
    }
    let string_constants = r.strs()?;
    let n = r.len()?;
    let mut static_key_plans = Vec::with_capacity(n);
    for _ in 0..n {
        static_key_plans.push(StaticKeyPlan::new(r.strs()?));
    }
    let n = r.len()?;
    let mut bigint_consts = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.len()?;
        let bytes = &r.data[r.at..r.at + len];
        r.at += len;
        bigint_consts.push(num_bigint::BigInt::from_signed_bytes_le(bytes));
    }
    let wtf8_consts = r.u32s()?;
    let name_global = r.opt_u32()?;
    let n = r.len()?;
    let mut upvalues = Vec::with_capacity(n);
    for _ in 0..n {
        upvalues.push(match r.u8()? {
            0 => UpvalSource::ParentLocal(r.u16()?),
            1 => UpvalSource::ParentUpval(r.u16()?),
            b => return Err(format!("bad upvalue kind {b}")),
        });
    }
    let n = r.len()?;
    let mut eval_sites = Vec::with_capacity(n);
    for _ in 0..n {
        let m = r.len()?;
        let mut bindings = Vec::with_capacity(m);
        for _ in 0..m {
            bindings.push((r.str()?, r.u8()?, r.u16()?));
        }
        let collisions = match r.u8()? {
            0 => None,
            1 => Some(r.strs()?),
            b => return Err(format!("bad option {b}")),
        };
        eval_sites.push((bindings, collisions, r.strs()?));
    }
    let source = sources.decode(r)?;
    Ok(FuncProto {
        name,
        code,
        reg_count,
        param_count,
        length,
        rest_reg,
        arguments_reg,
        is_generator,
        is_async,
        non_constructable,
        lexical_this,
        super_static,
        is_strict,
        simple_params,
        constants,
        string_constants,
        static_key_plans,
        bigint_consts,
        wtf8_consts,
        name_global,
        upvalues,
        eval_sites,
        source,
    })
}

fn encode_class(w: &mut Writer, c: &ClassDef) {
    let ClassDef {
        name,
        ctor,
        has_explicit_ctor,
        field_thunk,
        methods,
        getters,
        setters,
        proto_order,
        statics,
        static_getters,
        static_setters,
        source,
        instance_field_names,
        static_field_names,
        dec_plan,
    } = c;
    w.str(name);
    w.opt_u32(*ctor);
    w.bool(*has_explicit_ctor);
    w.opt_u32(*field_thunk);
    w.named(methods);
    w.named(getters);
    w.named(setters);
    w.strs(proto_order);
    w.named(statics);
    w.named(static_getters);
    w.named(static_setters);
    w.str(source);
    w.strs(instance_field_names);
    w.strs(static_field_names);
    match dec_plan {
        None => w.u8(0),
        Some(plan) => {
            w.u8(1);
            w.u32(plan.class_decorators);
            w.len(plan.elements.len());
            for e in &plan.elements {
                let DecElemDef {
                    kind,
                    is_static,
                    is_private,
                    name,
                    computed,
                    sym_key,
                    storage,
                } = e;
                w.u8(*kind);
                w.bool(*is_static);
                w.bool(*is_private);
                w.str(name);
                w.bool(*computed);
                w.bool(*sym_key);
                w.str(storage);
            }
        }
    }
}

fn decode_class(r: &mut Reader) -> R<ClassDef> {
    Ok(ClassDef {
        name: r.str()?,
        ctor: r.opt_u32()?,
        has_explicit_ctor: r.bool()?,
        field_thunk: r.opt_u32()?,
        methods: r.named()?,
        getters: r.named()?,
        setters: r.named()?,
        proto_order: r.strs()?,
        statics: r.named()?,
        static_getters: r.named()?,
        static_setters: r.named()?,
        source: r.str()?,
        instance_field_names: r.strs()?,
        static_field_names: r.strs()?,
        dec_plan: match r.u8()? {
            0 => None,
            1 => {
                let class_decorators = r.u32()?;
                let n = r.len()?;
                let mut elements = Vec::with_capacity(n);
                for _ in 0..n {
                    elements.push(DecElemDef {
                        kind: r.u8()?,
                        is_static: r.bool()?,
                        is_private: r.bool()?,
                        name: r.str()?,
                        computed: r.bool()?,
                        sym_key: r.bool()?,
                        storage: r.str()?,
                    });
                }
                Some(Box::new(DecPlan {
                    class_decorators,
                    elements,
                }))
            }
            b => return Err(format!("bad option {b}")),
        },
    })
}

fn encode_import(w: &mut Writer, i: &ImportEntry) {
    let ImportEntry {
        local_slot,
        import,
        specifier,
        mtype,
    } = i;
    w.u32(*local_slot);
    match import {
        ImportName::Named(s) => {
            w.u8(0);
            w.str(s);
        }
        ImportName::Default => w.u8(1),
        ImportName::Namespace => w.u8(2),
        ImportName::SideEffect => w.u8(3),
        ImportName::LoadOnly => w.u8(4),
        ImportName::Source => w.u8(5),
        ImportName::DeferNamespace => w.u8(6),
    }
    w.str(specifier);
    w.opt_str(mtype);
}

fn decode_import(r: &mut Reader) -> R<ImportEntry> {
    Ok(ImportEntry {
        local_slot: r.u32()?,
        import: match r.u8()? {
            0 => ImportName::Named(r.str()?),
            1 => ImportName::Default,
            2 => ImportName::Namespace,
            3 => ImportName::SideEffect,
            4 => ImportName::LoadOnly,
            5 => ImportName::Source,
            6 => ImportName::DeferNamespace,
            b => return Err(format!("bad import kind {b}")),
        },
        specifier: r.str()?,
        mtype: r.opt_str()?,
    })
}

/// A list of functions (a Python library module compiled for installation)
/// as bytes.
pub(crate) fn encode_functions(functions: &[FuncProto]) -> Vec<u8> {
    let mut w = Writer {
        out: Vec::with_capacity(1 << 16),
    };
    w.u32(FORMAT);
    w.u32(INSTR_VARIANTS as u32);
    let mut sources = Sources {
        base: None,
        cursor: 0,
    };
    w.len(functions.len());
    for f in functions {
        encode_func(&mut w, f, &mut sources);
    }
    w.out
}

/// The functions `bytes` encode (from [`encode_functions`] of this build).
pub(crate) fn decode_functions(bytes: &[u8]) -> R<Vec<FuncProto>> {
    let mut r = Reader { data: bytes, at: 0 };
    if r.u32()? != FORMAT || r.u32()? != INSTR_VARIANTS as u32 {
        return Err("bytecode from another format".into());
    }
    let sources = Sources {
        base: None,
        cursor: 0,
    };
    let n = r.len()?;
    let mut functions = Vec::with_capacity(n);
    for _ in 0..n {
        functions.push(decode_func(&mut r, &sources)?);
    }
    if r.at != bytes.len() {
        return Err("trailing bytes after the functions".into());
    }
    Ok(functions)
}

/// `program` as bytes. `None` for a program that cannot be stored (one with
/// lazily compiled Python modules, which hold compile callbacks). `source`
/// is the text the program was compiled from, if the decoder will have it:
/// functions' source texts are then stored as ranges of it.
pub(crate) fn encode(program: &Program, source: Option<&str>) -> Option<Vec<u8>> {
    let Program {
        functions,
        global_count,
        classes,
        global_names,
        hoisted_globals,
        decl_globals,
        lexical_globals,
        const_globals,
        eval_dynamic_names,
        module_exports,
        module_reexports,
        module_star_reexports,
        module_ns_reexports,
        module_imports,
        module_decl_globals,
        python_natives,
        python_lazy,
    } = program;
    if python_lazy.is_some() {
        return None;
    }
    let mut w = Writer {
        out: Vec::with_capacity(1 << 20),
    };
    w.u32(FORMAT);
    w.u32(INSTR_VARIANTS as u32);
    let mut sources = Sources {
        base: source,
        cursor: 0,
    };
    w.len(functions.len());
    for f in functions {
        encode_func(&mut w, f, &mut sources);
    }
    w.u32(*global_count);
    w.len(classes.len());
    for c in classes {
        encode_class(&mut w, c);
    }
    w.strs(global_names);
    w.u32s(hoisted_globals);
    w.u32s(decl_globals);
    w.u32s(lexical_globals);
    w.u32s(const_globals);
    w.strs(eval_dynamic_names);
    w.len(module_exports.len());
    for (a, b) in module_exports {
        w.str(a);
        w.str(b);
    }
    w.len(module_reexports.len());
    for (a, b, c) in module_reexports {
        w.str(a);
        w.str(b);
        w.str(c);
    }
    w.strs(module_star_reexports);
    w.len(module_ns_reexports.len());
    for (a, b) in module_ns_reexports {
        w.str(a);
        w.str(b);
    }
    w.len(module_imports.len());
    for i in module_imports {
        encode_import(&mut w, i);
    }
    w.u32s(module_decl_globals);
    w.bool(*python_natives);
    Some(w.out)
}

/// The program `bytes` encode (from [`encode`] of this same build, given the
/// same `source`).
pub(crate) fn decode(bytes: &[u8], source: Option<&str>) -> R<Program> {
    let sources = Sources {
        base: source,
        cursor: 0,
    };
    let mut r = Reader { data: bytes, at: 0 };
    if r.u32()? != FORMAT || r.u32()? != INSTR_VARIANTS as u32 {
        return Err("bytecode from another format".into());
    }
    let n = r.len()?;
    let mut functions = Vec::with_capacity(n);
    for _ in 0..n {
        functions.push(decode_func(&mut r, &sources)?);
    }
    let global_count = r.u32()?;
    let n = r.len()?;
    let mut classes = Vec::with_capacity(n);
    for _ in 0..n {
        classes.push(decode_class(&mut r)?);
    }
    let global_names = r.strs()?;
    let hoisted_globals = r.u32s()?;
    let decl_globals = r.u32s()?;
    let lexical_globals = r.u32s()?;
    let const_globals = r.u32s()?;
    let eval_dynamic_names = r.strs()?;
    let n = r.len()?;
    let mut module_exports = Vec::with_capacity(n);
    for _ in 0..n {
        module_exports.push((r.str()?, r.str()?));
    }
    let n = r.len()?;
    let mut module_reexports = Vec::with_capacity(n);
    for _ in 0..n {
        module_reexports.push((r.str()?, r.str()?, r.str()?));
    }
    let module_star_reexports = r.strs()?;
    let n = r.len()?;
    let mut module_ns_reexports = Vec::with_capacity(n);
    for _ in 0..n {
        module_ns_reexports.push((r.str()?, r.str()?));
    }
    let n = r.len()?;
    let mut module_imports = Vec::with_capacity(n);
    for _ in 0..n {
        module_imports.push(decode_import(&mut r)?);
    }
    let module_decl_globals = r.u32s()?;
    let python_natives = r.bool()?;
    if r.at != bytes.len() {
        return Err("trailing bytes after the program".into());
    }
    Ok(Program {
        functions,
        global_count,
        classes,
        global_names,
        hoisted_globals,
        decl_globals,
        lexical_globals,
        const_globals,
        eval_dynamic_names,
        module_exports,
        module_reexports,
        module_star_reexports,
        module_ns_reexports,
        module_imports,
        module_decl_globals,
        python_natives,
        python_lazy: None,
    })
}
