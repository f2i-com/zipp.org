//! ZIPP **release tier**: lower the IR to textual LLVM IR and compile it with
//! `clang -O3 -march=native` (optionally `-ffast-math`). No `llvm-sys`/`inkwell`
//! linkage — we emit `.ll` and drive the toolchain as a subprocess, the way an
//! AOT release compiler should (emit IR, let LLVM optimize + codegen).
//!
//! Strategy: every register becomes an `alloca` with load/store at each use;
//! LLVM's `mem2reg`/SROA (run by `-O3`) promotes those to SSA + phi nodes
//! optimally. So the frontend stays trivial and LLVM does FMA contraction,
//! reassociation, vectorization and scheduling for free — the things Cranelift
//! (the tier-0 JIT) does not.
//!
//! Scope: the **whole language** — scalars (`i64`/`f64`, casts), 1-D arrays,
//! strings, structs, and math builtins (same coverage as the JIT). The runtime
//! (allocator + conservative GC, strings, pow, print) is the shared `zipp-rt`
//! crate, linked into the exe as a static library — so the LLVM tier is GC'd
//! too. This module just `declare`s those entry points and calls them.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use zippc::ast::{BinOp, Elem, Type, UnOp};
use zippc::ir::{BuiltinOp, FuncMeta, Instr, Program};

/// LLVM type of a register. `I32`/`U32` are LLVM `i32`; everything else (i64/u64,
/// bool, array/string/struct pointers) is i64-width. Signedness picks sdiv/udiv,
/// slt/ult, sext/zext, sitofp/uitofp, etc.
#[derive(Clone, Copy, PartialEq)]
enum LTy {
    I64,
    I32,
    U32,
    U64,
    F64,
    Arr(bool),
    Str,
    Struct(u32),
}

fn llname(t: LTy) -> &'static str {
    match t {
        LTy::F64 => "double",
        LTy::I32 | LTy::U32 => "i32",
        LTy::Arr(_) | LTy::Str | LTy::Struct(_) => "ptr",
        _ => "i64", // i64, u64, bool
    }
}

fn arr_f64(t: LTy) -> bool {
    matches!(t, LTy::Arr(true))
}

fn struct_id(t: LTy) -> Option<u32> {
    match t {
        LTy::Struct(id) => Some(id),
        _ => None,
    }
}

fn is_int(t: LTy) -> bool {
    matches!(t, LTy::I64 | LTy::I32 | LTy::U32 | LTy::U64)
}

fn signed(t: LTy) -> bool {
    matches!(t, LTy::I64 | LTy::I32)
}

fn lty_of(t: Type) -> LTy {
    match t {
        Type::F64 => LTy::F64,
        Type::Array(e) => LTy::Arr(matches!(e, Elem::F64)),
        Type::Str => LTy::Str,
        Type::Struct(id) => LTy::Struct(id),
        // a nullable struct is a struct pointer that may be null
        Type::OptStruct(id) => LTy::Struct(id),
        Type::I32 => LTy::I32,
        Type::U32 => LTy::U32,
        Type::U64 => LTy::U64,
        _ => LTy::I64, // i64, bool, null
    }
}

/// (slot index, field type) of a struct field by name.
fn struct_field(prog: &Program, id: u32, field: &str) -> Option<(usize, LTy)> {
    let layout = &prog.structs[id as usize];
    let slot = layout.fields.iter().position(|f| f == field)?;
    Some((slot, lty_of(layout.types[slot])))
}

/// Convert a typed value to the raw i64 stored in a struct slot.
fn to_slot(body: &mut String, tmp: &mut usize, lty: LTy, val: &str) -> String {
    match lty {
        LTy::I64 => val.to_string(),
        LTy::F64 => {
            let t = fresh(tmp);
            body.push_str(&format!("  {t} = bitcast double {val} to i64\n"));
            t
        }
        _ => {
            let t = fresh(tmp); // array/string/struct pointer
            body.push_str(&format!("  {t} = ptrtoint ptr {val} to i64\n"));
            t
        }
    }
}

/// Coerce a value between `i64` and `ptr` (a null/struct pointer crossing a
/// boundary — e.g. an `i64` null passed where a nullable-struct `ptr` is wanted).
fn coerce(body: &mut String, tmp: &mut usize, from: LTy, to: LTy, val: &str) -> String {
    match (llname(from), llname(to)) {
        ("i64", "ptr") => {
            let x = fresh(tmp);
            body.push_str(&format!("  {x} = inttoptr i64 {val} to ptr\n"));
            x
        }
        ("ptr", "i64") => {
            let x = fresh(tmp);
            body.push_str(&format!("  {x} = ptrtoint ptr {val} to i64\n"));
            x
        }
        _ => val.to_string(),
    }
}

/// Convert a raw i64 slot back to its typed value.
fn from_slot(body: &mut String, tmp: &mut usize, lty: LTy, raw: &str) -> String {
    match lty {
        LTy::I64 => raw.to_string(),
        LTy::F64 => {
            let t = fresh(tmp);
            body.push_str(&format!("  {t} = bitcast i64 {raw} to double\n"));
            t
        }
        _ => {
            let t = fresh(tmp);
            body.push_str(&format!("  {t} = inttoptr i64 {raw} to ptr\n"));
            t
        }
    }
}

/// Reason a program can't use the LLVM tier, or `None`. The tier now covers the
/// whole language, so this is always `None` (kept for the CLI's fallback path).
pub fn ineligible_reason(prog: &Program) -> Option<&'static str> {
    // Nullable structs run natively; nullable *scalars* (no null sentinel) don't.
    if prog.uses_opt_scalar {
        return Some("nullable scalars (i64 | null)");
    }
    None
}

/// Per-register LLVM type (registers are monotonic ⇒ one static type each).
fn infer(prog: &Program, f: &FuncMeta, end: u32) -> Vec<LTy> {
    let mut t = vec![LTy::I64; f.nregs as usize];
    for (i, p) in f.params.iter().enumerate() {
        t[i] = lty_of(*p);
    }
    for pc in f.entry..end {
        match &prog.code[pc as usize] {
            Instr::Const { dst, .. } => t[*dst as usize] = LTy::I64,
            Instr::FConst { dst, .. } => t[*dst as usize] = LTy::F64,
            Instr::SConst { dst, .. } => t[*dst as usize] = LTy::Str,
            Instr::Cast { dst, to, .. } => t[*dst as usize] = lty_of(*to),
            Instr::Mov { dst, src } => {
                // a null (i64 0) moved into a heap/struct register keeps that type
                let s = t[*src as usize];
                let heap = matches!(t[*dst as usize], LTy::Struct(_) | LTy::Arr(_) | LTy::Str);
                if !(s == LTy::I64 && heap) {
                    t[*dst as usize] = s;
                }
            }
            Instr::ConstNull { dst, id } => {
                t[*dst as usize] = match id {
                    Some(s) => LTy::Struct(*s),
                    None => LTy::I64,
                };
            }
            Instr::Bin { op, dst, a, .. } => {
                use BinOp::*;
                t[*dst as usize] = match op {
                    Eq | Ne | Lt | Le | Gt | Ge | And | Or => LTy::I64, // bool
                    _ => t[*a as usize], // arithmetic / bitwise / shift keep operand type
                };
            }
            Instr::Unary { op, dst, a } => {
                t[*dst as usize] = match op {
                    UnOp::Neg => t[*a as usize],
                    _ => LTy::I64,
                };
            }
            Instr::Call { func, dst, .. } => {
                t[*dst as usize] = lty_of(prog.funcs[*func as usize].ret);
            }
            Instr::ArrayLit { dst, elems } => {
                t[*dst as usize] =
                    LTy::Arr(elems.first().is_some_and(|e| t[*e as usize] == LTy::F64));
            }
            Instr::ArrayRepeat { dst, value, .. } => {
                t[*dst as usize] = LTy::Arr(t[*value as usize] == LTy::F64);
            }
            Instr::Index { dst, arr, .. } => {
                t[*dst as usize] = if arr_f64(t[*arr as usize]) {
                    LTy::F64
                } else {
                    LTy::I64
                };
            }
            Instr::Len { dst, .. } => t[*dst as usize] = LTy::I64,
            Instr::NewStruct { id, dst, .. } => t[*dst as usize] = LTy::Struct(*id),
            Instr::GetField { dst, base, field } => {
                t[*dst as usize] = struct_id(t[*base as usize])
                    .and_then(|id| struct_field(prog, id, field))
                    .map_or(LTy::I64, |(_, fty)| fty);
            }
            Instr::Builtin { op, dst, args } => {
                use BuiltinOp::*;
                t[*dst as usize] = match op {
                    Abs | Min | Max => t[args[0] as usize],
                    Pow => LTy::I64,
                    Sqrt | Floor | Ceil => LTy::F64,
                };
            }
            _ => {}
        }
    }
    t
}

/// Emit a bounds-checked element-address GEP (`base[idx+1]`), aborting via
/// `@zipp_oob` if out of range. Leaves emission in the in-bounds block; returns
/// the element-slot `ptr` SSA name.
fn checked_slot(body: &mut String, tmp: &mut usize, base: &str, idx: &str) -> String {
    let n = *tmp;
    *tmp += 1;
    let len = fresh(tmp);
    body.push_str(&format!("  {len} = load i64, ptr {base}\n"));
    let inb = fresh(tmp);
    body.push_str(&format!("  {inb} = icmp ult i64 {idx}, {len}\n"));
    let (ok, bad) = (format!("chk{n}.ok"), format!("chk{n}.bad"));
    body.push_str(&format!("  br i1 {inb}, label %{ok}, label %{bad}\n"));
    body.push_str(&format!("{bad}:\n"));
    body.push_str(&format!("  call void @zipp_oob(i64 {idx}, i64 {len})\n"));
    body.push_str("  unreachable\n");
    body.push_str(&format!("{ok}:\n"));
    let i1 = fresh(tmp);
    body.push_str(&format!("  {i1} = add i64 {idx}, 1\n"));
    let slot = fresh(tmp);
    body.push_str(&format!("  {slot} = getelementptr inbounds i64, ptr {base}, i64 {i1}\n"));
    slot
}

fn leaders_of(prog: &Program, entry: u32, end: u32) -> std::collections::BTreeSet<u32> {
    let mut s = std::collections::BTreeSet::new();
    s.insert(entry);
    for pc in entry..end {
        match &prog.code[pc as usize] {
            Instr::Jmp { target } => {
                s.insert(*target);
                if pc + 1 < end {
                    s.insert(pc + 1);
                }
            }
            Instr::JmpIfZero { target, .. } | Instr::JmpIfNonZero { target, .. } => {
                s.insert(*target);
                if pc + 1 < end {
                    s.insert(pc + 1);
                }
            }
            _ => {}
        }
    }
    s
}

/// A unique SSA temp name.
fn fresh(tmp: &mut usize) -> String {
    let t = format!("%t{tmp}");
    *tmp += 1;
    t
}

fn load(body: &mut String, tmp: &mut usize, rty: &[LTy], r: u32) -> String {
    let t = fresh(tmp);
    body.push_str(&format!("  {t} = load {}, ptr %r{r}\n", llname(rty[r as usize])));
    t
}

fn store(body: &mut String, rty: &[LTy], dst: u32, val: &str) {
    body.push_str(&format!("  store {} {val}, ptr %r{dst}\n", llname(rty[dst as usize])));
}

fn cmp(body: &mut String, tmp: &mut usize, kind: &str, cc: &str, ty: &str, a: &str, b: &str) -> String {
    let c = fresh(tmp);
    body.push_str(&format!("  {c} = {kind} {cc} {ty} {a}, {b}\n"));
    let r = fresh(tmp);
    body.push_str(&format!("  {r} = zext i1 {c} to i64\n"));
    r
}

/// Numeric cast with Rust `as` semantics (truncate / sign- or zero-extend /
/// saturating float↔int), mirroring the interpreter.
fn emit_cast(body: &mut String, tmp: &mut usize, from: LTy, to: LTy, v: &str) -> String {
    let (fw, tw) = (llname(from), llname(to));
    if from == LTy::F64 && is_int(to) {
        let intr = if signed(to) { "fptosi" } else { "fptoui" };
        let r = fresh(tmp);
        body.push_str(&format!("  {r} = call {tw} @llvm.{intr}.sat.{tw}.f64(double {v})\n"));
        return r;
    }
    if is_int(from) && to == LTy::F64 {
        let op = if signed(from) { "sitofp" } else { "uitofp" };
        let r = fresh(tmp);
        body.push_str(&format!("  {r} = {op} {fw} {v} to double\n"));
        return r;
    }
    if fw == tw {
        return v.to_string(); // same width: signed↔unsigned reinterpret, or f64↔f64
    }
    let r = fresh(tmp);
    if fw == "i32" && tw == "i64" {
        let op = if signed(from) { "sext" } else { "zext" };
        body.push_str(&format!("  {r} = {op} i32 {v} to i64\n"));
    } else {
        body.push_str(&format!("  {r} = trunc i64 {v} to i32\n")); // i64 → i32
    }
    r
}

fn emit_bin(body: &mut String, tmp: &mut usize, op: BinOp, ty: LTy, a: &str, b: &str) -> String {
    use BinOp::*;
    if ty == LTy::F64 {
        let bin = |body: &mut String, tmp: &mut usize, mn: &str| {
            let r = fresh(tmp);
            body.push_str(&format!("  {r} = {mn} double {a}, {b}\n"));
            r
        };
        return match op {
            Add => bin(body, tmp, "fadd"),
            Sub => bin(body, tmp, "fsub"),
            Mul => bin(body, tmp, "fmul"),
            Div => bin(body, tmp, "fdiv"),
            Eq => cmp(body, tmp, "fcmp", "oeq", "double", a, b),
            Ne => cmp(body, tmp, "fcmp", "one", "double", a, b),
            Lt => cmp(body, tmp, "fcmp", "olt", "double", a, b),
            Le => cmp(body, tmp, "fcmp", "ole", "double", a, b),
            Gt => cmp(body, tmp, "fcmp", "ogt", "double", a, b),
            Ge => cmp(body, tmp, "fcmp", "oge", "double", a, b),
            _ => {
                let r = fresh(tmp);
                body.push_str(&format!("  {r} = fadd double 0.0, 0.0 ; unreachable f64 op\n"));
                r
            }
        };
    }
    // Integer path. `w` is the operand width (i32 or i64); signedness picks
    // sdiv/udiv, ashr/lshr, and slt/ult comparison codes.
    let w = llname(ty);
    let sgned = signed(ty);
    let mask = if w == "i32" { 31 } else { 63 };
    let bin = |body: &mut String, tmp: &mut usize, mn: &str| {
        let r = fresh(tmp);
        body.push_str(&format!("  {r} = {mn} {w} {a}, {b}\n"));
        r
    };
    // Shift amount must be masked to the width (LLVM shl/shr are poison if the
    // amount is >= the bit width; the VM and Cranelift mask it the same way).
    let shift = |body: &mut String, tmp: &mut usize, mn: &str| {
        let bm = fresh(tmp);
        body.push_str(&format!("  {bm} = and {w} {b}, {mask}\n"));
        let r = fresh(tmp);
        body.push_str(&format!("  {r} = {mn} {w} {a}, {bm}\n"));
        r
    };
    let logical = |body: &mut String, tmp: &mut usize, mn: &str| {
        let ca = fresh(tmp);
        body.push_str(&format!("  {ca} = icmp ne i64 {a}, 0\n"));
        let cb = fresh(tmp);
        body.push_str(&format!("  {cb} = icmp ne i64 {b}, 0\n"));
        let cc = fresh(tmp);
        body.push_str(&format!("  {cc} = {mn} i1 {ca}, {cb}\n"));
        let r = fresh(tmp);
        body.push_str(&format!("  {r} = zext i1 {cc} to i64\n"));
        r
    };
    match op {
        Add => bin(body, tmp, "add"),
        Sub => bin(body, tmp, "sub"),
        Mul => bin(body, tmp, "mul"),
        Div => bin(body, tmp, if sgned { "sdiv" } else { "udiv" }),
        Mod => bin(body, tmp, if sgned { "srem" } else { "urem" }),
        BitAnd => bin(body, tmp, "and"),
        BitOr => bin(body, tmp, "or"),
        BitXor => bin(body, tmp, "xor"),
        Shl => shift(body, tmp, "shl"),
        Shr => shift(body, tmp, if sgned { "ashr" } else { "lshr" }),
        Eq => cmp(body, tmp, "icmp", "eq", w, a, b),
        Ne => cmp(body, tmp, "icmp", "ne", w, a, b),
        Lt => cmp(body, tmp, "icmp", if sgned { "slt" } else { "ult" }, w, a, b),
        Le => cmp(body, tmp, "icmp", if sgned { "sle" } else { "ule" }, w, a, b),
        Gt => cmp(body, tmp, "icmp", if sgned { "sgt" } else { "ugt" }, w, a, b),
        Ge => cmp(body, tmp, "icmp", if sgned { "sge" } else { "uge" }, w, a, b),
        And => logical(body, tmp, "and"),
        Or => logical(body, tmp, "or"),
    }
}

fn emit_fn(prog: &Program, fi: usize, end: u32) -> Result<String, String> {
    let f = &prog.funcs[fi];
    let rty = infer(prog, f, end);
    let ret = lty_of(f.ret);
    let params: Vec<String> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{} %a{i}", llname(lty_of(*p))))
        .collect();
    let mut s = format!("define {} @zfn{fi}({}) {{\n", llname(ret), params.join(", "));
    s.push_str("entry:\n");
    for r in 0..f.nregs {
        s.push_str(&format!("  %r{r} = alloca {}\n", llname(rty[r as usize])));
    }
    for i in 0..f.nparams {
        s.push_str(&format!("  store {} %a{i}, ptr %r{i}\n", llname(rty[i as usize])));
    }
    s.push_str(&format!("  br label %L{}\n", f.entry));

    let leaders = leaders_of(prog, f.entry, end);
    let mut tmp = 0usize;
    let mut term = true; // entry block ended with the br above
    for pc in f.entry..end {
        if leaders.contains(&pc) {
            if !term {
                s.push_str(&format!("  br label %L{pc}\n"));
            }
            s.push_str(&format!("L{pc}:\n"));
            term = false;
        }
        if term {
            continue; // unreachable code between a terminator and the next leader
        }
        match &prog.code[pc as usize] {
            Instr::Const { dst, imm } => store(&mut s, &rty, *dst, &imm.to_string()),
            Instr::FConst { dst, imm } => {
                store(&mut s, &rty, *dst, &format!("0x{:016X}", imm.to_bits()))
            }
            Instr::Cast { dst, src, to } => {
                let sv = load(&mut s, &mut tmp, &rty, *src);
                let r = emit_cast(&mut s, &mut tmp, rty[*src as usize], lty_of(*to), &sv);
                store(&mut s, &rty, *dst, &r);
            }
            Instr::Mov { dst, src } => {
                let sv = load(&mut s, &mut tmp, &rty, *src);
                store(&mut s, &rty, *dst, &sv);
            }
            Instr::SConst { dst, .. } => {
                // The literal lives in a module-level constant @.sconst_{pc};
                // the string handle is just its address.
                store(&mut s, &rty, *dst, &format!("@.sconst_{pc}"));
            }
            Instr::Bin { op, dst, a, b } if rty[*a as usize] == LTy::Str => {
                // String concat (+) / equality (==, !=) via the runtime.
                let av = load(&mut s, &mut tmp, &rty, *a);
                let bv = load(&mut s, &mut tmp, &rty, *b);
                let r = match op {
                    BinOp::Add => {
                        let t = fresh(&mut tmp);
                        s.push_str(&format!("  {t} = call ptr @zipp_str_concat(ptr {av}, ptr {bv})\n"));
                        t
                    }
                    _ => {
                        let e = fresh(&mut tmp);
                        s.push_str(&format!("  {e} = call i64 @zipp_str_eq(ptr {av}, ptr {bv})\n"));
                        if matches!(op, BinOp::Ne) {
                            let t = fresh(&mut tmp);
                            s.push_str(&format!("  {t} = xor i64 {e}, 1\n")); // !eq
                            t
                        } else {
                            e
                        }
                    }
                };
                store(&mut s, &rty, *dst, &r);
            }
            Instr::Bin { op, dst, a, b } => {
                let mut av = load(&mut s, &mut tmp, &rty, *a);
                let mut bv = load(&mut s, &mut tmp, &rty, *b);
                let mut op_ty = rty[*a as usize];
                // `x === null` / pointer equality: compare as integers, since one
                // side may be an i64 null and LLVM `icmp` needs matching types.
                let ptr_eq = matches!(op, BinOp::Eq | BinOp::Ne)
                    && (llname(rty[*a as usize]) == "ptr" || llname(rty[*b as usize]) == "ptr");
                if ptr_eq {
                    if llname(rty[*a as usize]) == "ptr" {
                        let t = fresh(&mut tmp);
                        s.push_str(&format!("  {t} = ptrtoint ptr {av} to i64\n"));
                        av = t;
                    }
                    if llname(rty[*b as usize]) == "ptr" {
                        let t = fresh(&mut tmp);
                        s.push_str(&format!("  {t} = ptrtoint ptr {bv} to i64\n"));
                        bv = t;
                    }
                    op_ty = LTy::I64;
                }
                let r = emit_bin(&mut s, &mut tmp, *op, op_ty, &av, &bv);
                store(&mut s, &rty, *dst, &r);
            }
            Instr::Unary { op, dst, a } => {
                let av = load(&mut s, &mut tmp, &rty, *a);
                let r = match op {
                    UnOp::Neg if rty[*a as usize] == LTy::F64 => {
                        let t = fresh(&mut tmp);
                        s.push_str(&format!("  {t} = fneg double {av}\n"));
                        t
                    }
                    UnOp::Neg => {
                        let t = fresh(&mut tmp);
                        s.push_str(&format!("  {t} = sub i64 0, {av}\n"));
                        t
                    }
                    UnOp::BitNot => {
                        let t = fresh(&mut tmp);
                        s.push_str(&format!("  {t} = xor i64 {av}, -1\n"));
                        t
                    }
                    UnOp::Not => cmp(&mut s, &mut tmp, "icmp", "eq", "i64", &av, "0"),
                };
                store(&mut s, &rty, *dst, &r);
            }
            Instr::Jmp { target } => {
                s.push_str(&format!("  br label %L{target}\n"));
                term = true;
            }
            Instr::JmpIfZero { cond, target } => {
                let cv = load(&mut s, &mut tmp, &rty, *cond);
                let nz = fresh(&mut tmp);
                s.push_str(&format!("  {nz} = icmp ne i64 {cv}, 0\n"));
                s.push_str(&format!("  br i1 {nz}, label %L{}, label %L{target}\n", pc + 1));
                term = true;
            }
            Instr::JmpIfNonZero { cond, target } => {
                let cv = load(&mut s, &mut tmp, &rty, *cond);
                let nz = fresh(&mut tmp);
                s.push_str(&format!("  {nz} = icmp ne i64 {cv}, 0\n"));
                s.push_str(&format!("  br i1 {nz}, label %L{target}, label %L{}\n", pc + 1));
                term = true;
            }
            Instr::Call { func, arg_base, argc, dst } => {
                let args: Vec<String> = (0..*argc)
                    .map(|k| {
                        let aty = rty[(*arg_base + k) as usize];
                        let pty = lty_of(prog.funcs[*func as usize].params[k as usize]);
                        let v = load(&mut s, &mut tmp, &rty, *arg_base + k);
                        // coerce a null/pointer arg to the parameter's type
                        let v = coerce(&mut s, &mut tmp, aty, pty, &v);
                        format!("{} {v}", llname(pty))
                    })
                    .collect();
                let rt = llname(lty_of(prog.funcs[*func as usize].ret));
                let t = fresh(&mut tmp);
                s.push_str(&format!("  {t} = call {rt} @zfn{func}({})\n", args.join(", ")));
                store(&mut s, &rty, *dst, &t);
            }
            Instr::Ret { src } => {
                let retty = lty_of(f.ret);
                let v = load(&mut s, &mut tmp, &rty, *src);
                // coerce the returned value to the function's declared return type
                let v = coerce(&mut s, &mut tmp, rty[*src as usize], retty, &v);
                s.push_str(&format!("  ret {} {v}\n", llname(retty)));
                term = true;
            }
            Instr::Print { a } => {
                let aty = rty[*a as usize];
                let v = load(&mut s, &mut tmp, &rty, *a);
                match aty {
                    LTy::Str => s.push_str(&format!("  call void @zipp_print_str(ptr {v})\n")),
                    LTy::F64 => s.push_str(&format!("  call void @zipp_print_f64(double {v})\n")),
                    LTy::U64 => s.push_str(&format!("  call void @zipp_print_u64(i64 {v})\n")),
                    LTy::I32 | LTy::U32 => {
                        // widen to i64 (sext signed / zext unsigned) for the printer
                        let w = emit_cast(&mut s, &mut tmp, aty, LTy::I64, &v);
                        s.push_str(&format!("  call void @zipp_print_i64(i64 {w})\n"));
                    }
                    _ => s.push_str(&format!("  call void @zipp_print_i64(i64 {v})\n")),
                }
            }
            Instr::ArrayLit { dst, elems } => {
                let p = fresh(&mut tmp);
                s.push_str(&format!("  {p} = call ptr @zipp_alloc(i64 {})\n", elems.len()));
                let elem_f64 = arr_f64(rty[*dst as usize]);
                for (i, e) in elems.iter().enumerate() {
                    let ev = load(&mut s, &mut tmp, &rty, *e);
                    let raw = if elem_f64 {
                        let b = fresh(&mut tmp);
                        s.push_str(&format!("  {b} = bitcast double {ev} to i64\n"));
                        b
                    } else {
                        ev
                    };
                    let slot = fresh(&mut tmp);
                    s.push_str(&format!(
                        "  {slot} = getelementptr inbounds i64, ptr {p}, i64 {}\n",
                        i + 1
                    ));
                    s.push_str(&format!("  store i64 {raw}, ptr {slot}\n"));
                }
                store(&mut s, &rty, *dst, &p);
            }
            Instr::ArrayRepeat { dst, value, count } => {
                let cv = load(&mut s, &mut tmp, &rty, *count);
                let vv = load(&mut s, &mut tmp, &rty, *value);
                let raw = if arr_f64(rty[*dst as usize]) {
                    let b = fresh(&mut tmp);
                    s.push_str(&format!("  {b} = bitcast double {vv} to i64\n"));
                    b
                } else {
                    vv
                };
                let p = fresh(&mut tmp);
                s.push_str(&format!("  {p} = call ptr @zipp_array_repeat(i64 {cv}, i64 {raw})\n"));
                store(&mut s, &rty, *dst, &p);
            }
            Instr::Index { dst, arr, idx } => {
                let base = load(&mut s, &mut tmp, &rty, *arr);
                let i = load(&mut s, &mut tmp, &rty, *idx);
                let slot = checked_slot(&mut s, &mut tmp, &base, &i);
                let raw = fresh(&mut tmp);
                s.push_str(&format!("  {raw} = load i64, ptr {slot}\n"));
                let val = if arr_f64(rty[*arr as usize]) {
                    let d = fresh(&mut tmp);
                    s.push_str(&format!("  {d} = bitcast i64 {raw} to double\n"));
                    d
                } else {
                    raw
                };
                store(&mut s, &rty, *dst, &val);
            }
            Instr::SetIndex { arr, idx, value } => {
                let base = load(&mut s, &mut tmp, &rty, *arr);
                let i = load(&mut s, &mut tmp, &rty, *idx);
                let vv = load(&mut s, &mut tmp, &rty, *value);
                let raw = if arr_f64(rty[*arr as usize]) {
                    let b = fresh(&mut tmp);
                    s.push_str(&format!("  {b} = bitcast double {vv} to i64\n"));
                    b
                } else {
                    vv
                };
                let slot = checked_slot(&mut s, &mut tmp, &base, &i);
                s.push_str(&format!("  store i64 {raw}, ptr {slot}\n"));
            }
            Instr::Len { dst, arr } => {
                let base = load(&mut s, &mut tmp, &rty, *arr);
                let v = fresh(&mut tmp);
                s.push_str(&format!("  {v} = load i64, ptr {base}\n"));
                store(&mut s, &rty, *dst, &v);
            }
            Instr::NewStruct { id, dst, fields } => {
                // Reuse zipp_alloc(nfields): fields go in the element slots.
                let nfields = prog.structs[*id as usize].fields.len();
                let p = fresh(&mut tmp);
                s.push_str(&format!("  {p} = call ptr @zipp_alloc(i64 {nfields})\n"));
                for (i, fr) in fields.iter().enumerate() {
                    let fv = load(&mut s, &mut tmp, &rty, *fr);
                    let raw = to_slot(&mut s, &mut tmp, rty[*fr as usize], &fv);
                    let slot = fresh(&mut tmp);
                    s.push_str(&format!(
                        "  {slot} = getelementptr inbounds i64, ptr {p}, i64 {}\n",
                        i + 1
                    ));
                    s.push_str(&format!("  store i64 {raw}, ptr {slot}\n"));
                }
                store(&mut s, &rty, *dst, &p);
            }
            Instr::GetField { dst, base, field } => {
                let id = struct_id(rty[*base as usize])
                    .ok_or("internal: GetField on a non-struct register")?;
                let (slot, fty) =
                    struct_field(prog, id, field).ok_or("internal: unknown struct field")?;
                let basev = load(&mut s, &mut tmp, &rty, *base);
                let sp = fresh(&mut tmp);
                s.push_str(&format!(
                    "  {sp} = getelementptr inbounds i64, ptr {basev}, i64 {}\n",
                    slot + 1
                ));
                let raw = fresh(&mut tmp);
                s.push_str(&format!("  {raw} = load i64, ptr {sp}\n"));
                let val = from_slot(&mut s, &mut tmp, fty, &raw);
                store(&mut s, &rty, *dst, &val);
            }
            Instr::SetField { base, field, value } => {
                let id = struct_id(rty[*base as usize])
                    .ok_or("internal: SetField on a non-struct register")?;
                let (slot, fty) =
                    struct_field(prog, id, field).ok_or("internal: unknown struct field")?;
                let basev = load(&mut s, &mut tmp, &rty, *base);
                let vv = load(&mut s, &mut tmp, &rty, *value);
                let raw = to_slot(&mut s, &mut tmp, fty, &vv);
                let sp = fresh(&mut tmp);
                s.push_str(&format!(
                    "  {sp} = getelementptr inbounds i64, ptr {basev}, i64 {}\n",
                    slot + 1
                ));
                s.push_str(&format!("  store i64 {raw}, ptr {sp}\n"));
            }
            Instr::ConstNull { dst, .. } => {
                // a struct-typed register gets a `ptr null`; a comparison-operand
                // null is `i64 0` (both round-trip to 0).
                let v = if llname(rty[*dst as usize]) == "ptr" { "null" } else { "0" };
                store(&mut s, &rty, *dst, v);
            }
            Instr::Builtin { op, dst, args } => {
                use BuiltinOp::*;
                let f64a = rty[args[0] as usize] == LTy::F64;
                let r = fresh(&mut tmp);
                match op {
                    Abs => {
                        let x = load(&mut s, &mut tmp, &rty, args[0]);
                        if f64a {
                            s.push_str(&format!("  {r} = call double @llvm.fabs.f64(double {x})\n"));
                        } else {
                            s.push_str(&format!("  {r} = call i64 @llvm.abs.i64(i64 {x}, i1 false)\n"));
                        }
                    }
                    Min | Max => {
                        let a = load(&mut s, &mut tmp, &rty, args[0]);
                        let b = load(&mut s, &mut tmp, &rty, args[1]);
                        let intr = match (op, f64a) {
                            (Min, true) => "call double @llvm.minnum.f64(double",
                            (Max, true) => "call double @llvm.maxnum.f64(double",
                            (Min, _) => "call i64 @llvm.smin.i64(i64",
                            (_, _) => "call i64 @llvm.smax.i64(i64",
                        };
                        let ty = if f64a { "double" } else { "i64" };
                        s.push_str(&format!("  {r} = {intr} {a}, {ty} {b})\n"));
                    }
                    Pow => {
                        let a = load(&mut s, &mut tmp, &rty, args[0]);
                        let b = load(&mut s, &mut tmp, &rty, args[1]);
                        s.push_str(&format!("  {r} = call i64 @zipp_ipow(i64 {a}, i64 {b})\n"));
                    }
                    Sqrt | Floor | Ceil => {
                        let x = load(&mut s, &mut tmp, &rty, args[0]);
                        let intr = match op {
                            Sqrt => "llvm.sqrt.f64",
                            Floor => "llvm.floor.f64",
                            _ => "llvm.ceil.f64",
                        };
                        s.push_str(&format!("  {r} = call double @{intr}(double {x})\n"));
                    }
                }
                store(&mut s, &rty, *dst, &r);
            }
        }
    }
    if !term {
        let z = if ret == LTy::F64 { "0.0" } else { "0" };
        s.push_str(&format!("  ret {} {z}\n", llname(ret)));
    }
    s.push_str("}\n\n");
    Ok(s)
}

/// LLVM byte-escape (no trailing nul — strings are length-prefixed, not C
/// strings). Printable ASCII except `"` and `\` stays literal; else `\XX`.
fn escape_bytes(s: &str) -> String {
    let mut esc = String::new();
    for &b in s.as_bytes() {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            esc.push(b as char);
        } else {
            esc.push_str(&format!("\\{b:02X}"));
        }
    }
    esc
}

/// Lower a whole program to textual LLVM IR (a self-contained module with a C
/// `main` that times the entry call with `clock()` and prints the result).
pub fn emit_ir(prog: &Program) -> Result<String, String> {
    if let Some(bad) = ineligible_reason(prog) {
        return Err(format!("--llvm supports scalars + arrays + strings + structs only (program uses {bad})"));
    }
    let mut out = String::new();
    out.push_str("; ZIPP → LLVM IR (release tier; runtime linked from zipp-rt)\n");
    out.push_str("declare i32 @clock()\n"); // Windows: clock_t = long = i32, CLOCKS_PER_SEC = 1000
    out.push_str("declare i64 @llvm.fptosi.sat.i64.f64(double)\n");
    out.push_str("declare i32 @llvm.fptosi.sat.i32.f64(double)\n");
    out.push_str("declare i64 @llvm.fptoui.sat.i64.f64(double)\n");
    out.push_str("declare i32 @llvm.fptoui.sat.i32.f64(double)\n");
    // Math-builtin intrinsics (LLVM lowers these to the right instruction or a
    // libm call; minnum/maxnum match Rust's f64::min/max NaN handling).
    out.push_str("declare i64 @llvm.abs.i64(i64, i1)\n");
    out.push_str("declare double @llvm.fabs.f64(double)\n");
    out.push_str("declare i64 @llvm.smin.i64(i64, i64)\n");
    out.push_str("declare i64 @llvm.smax.i64(i64, i64)\n");
    out.push_str("declare double @llvm.minnum.f64(double, double)\n");
    out.push_str("declare double @llvm.maxnum.f64(double, double)\n");
    out.push_str("declare double @llvm.sqrt.f64(double)\n");
    out.push_str("declare double @llvm.floor.f64(double)\n");
    out.push_str("declare double @llvm.ceil.f64(double)\n");
    // The ZIPP runtime — allocator (GC-backed), strings, pow, print, and the
    // CLI result/time markers — is provided by the zipp-rt static library.
    out.push_str("declare void @zipp_set_stack_bottom(i64)\n");
    out.push_str("declare ptr @zipp_alloc(i64)\n");
    out.push_str("declare ptr @zipp_array_repeat(i64, i64)\n");
    out.push_str("declare void @zipp_oob(i64, i64)\n");
    out.push_str("declare ptr @zipp_str_concat(ptr, ptr)\n");
    out.push_str("declare i64 @zipp_str_eq(ptr, ptr)\n");
    out.push_str("declare i64 @zipp_ipow(i64, i64)\n");
    out.push_str("declare void @zipp_print_i64(i64)\n");
    out.push_str("declare void @zipp_print_u64(i64)\n");
    out.push_str("declare void @zipp_print_f64(double)\n");
    out.push_str("declare void @zipp_print_str(ptr)\n");
    out.push_str("declare void @zipp_emit_result_i64(i64)\n");
    out.push_str("declare void @zipp_emit_result_u64(i64)\n");
    out.push_str("declare void @zipp_emit_result_f64(double)\n");
    out.push_str("declare void @zipp_emit_time_ms(i64)\n\n");

    // String literals: one module-level constant each, laid out as the runtime
    // expects — a packed `{ i64 len, [len x i8] bytes }`.
    for (pc, ins) in prog.code.iter().enumerate() {
        if let Instr::SConst { imm, .. } = ins {
            let n = imm.as_bytes().len();
            out.push_str(&format!(
                "@.sconst_{pc} = private unnamed_addr constant <{{ i64, [{n} x i8] }}> <{{ i64 {n}, [{n} x i8] c\"{}\" }}>\n",
                escape_bytes(imm)
            ));
        }
    }
    out.push('\n');

    for i in 0..prog.funcs.len() {
        let end = if i + 1 < prog.funcs.len() {
            prog.funcs[i + 1].entry
        } else {
            prog.code.len() as u32
        };
        out.push_str(&emit_fn(prog, i, end)?);
    }

    // C entry: anchor the GC stack scan, time the kernel with clock() (excludes
    // process startup), then emit the time + result markers via the runtime.
    let mi = prog.main as usize;
    let ret = lty_of(prog.funcs[mi].ret);
    let rt_ty = llname(ret);
    out.push_str("define i32 @main() {\nentry:\n");
    out.push_str("  %anchor = alloca i64\n");
    out.push_str("  %ab = ptrtoint ptr %anchor to i64\n");
    out.push_str("  call void @zipp_set_stack_bottom(i64 %ab)\n");
    out.push_str("  %t0 = call i32 @clock()\n");
    out.push_str(&format!("  %r = call {rt_ty} @zfn{mi}()\n"));
    out.push_str("  %t1 = call i32 @clock()\n");
    out.push_str("  %dt = sub i32 %t1, %t0\n");
    out.push_str("  %dt64 = sext i32 %dt to i64\n");
    out.push_str("  call void @zipp_emit_time_ms(i64 %dt64)\n");
    match ret {
        LTy::F64 => out.push_str("  call void @zipp_emit_result_f64(double %r)\n"),
        LTy::I64 => out.push_str("  call void @zipp_emit_result_i64(i64 %r)\n"),
        LTy::U64 => out.push_str("  call void @zipp_emit_result_u64(i64 %r)\n"),
        LTy::I32 => {
            out.push_str("  %rw = sext i32 %r to i64\n");
            out.push_str("  call void @zipp_emit_result_i64(i64 %rw)\n");
        }
        LTy::U32 => {
            out.push_str("  %rw = zext i32 %r to i64\n");
            out.push_str("  call void @zipp_emit_result_i64(i64 %rw)\n");
        }
        _ => {
            // array/string/struct return: report the handle as an integer
            out.push_str("  %ri = ptrtoint ptr %r to i64\n");
            out.push_str("  call void @zipp_emit_result_i64(i64 %ri)\n");
        }
    }
    out.push_str("  ret i32 0\n}\n");
    Ok(out)
}

/// Result of compiling + running a program through the LLVM release tier.
pub struct LlvmRun {
    pub result: String,
    pub program_output: String,
    pub compile: Duration,
    pub execute: Duration,
}

fn find_clang() -> String {
    if let Ok(p) = std::env::var("ZIPP_CLANG") {
        return p;
    }
    let known = PathBuf::from(r"C:\Program Files\LLVM\bin\clang.exe");
    if known.exists() {
        return known.to_string_lossy().into_owned();
    }
    "clang".to_string() // hope it's on PATH; spawn error guides if not
}

/// Locate the `zipp_rt` static library (the shared runtime to link into the
/// exe). It's built next to the `zipp` binary by `cargo build`.
fn find_zipp_rt() -> Result<String, String> {
    if let Ok(p) = std::env::var("ZIPP_RT_LIB") {
        return Ok(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("zipp_rt.lib");
            if cand.exists() {
                return Ok(cand.to_string_lossy().into_owned());
            }
        }
    }
    Err("zipp_rt.lib not found — build the workspace (`cargo build --release`), \
         or set ZIPP_RT_LIB to its path"
        .into())
}

/// System libraries the Rust std static lib needs on Windows (from
/// `rustc --print native-static-libs`). The CRT is pulled in by clang/the lib's
/// own linker directives.
const RT_SYS_LIBS: &[&str] = &["kernel32", "ntdll", "userenv", "ws2_32", "dbghelp"];

/// Emit IR, compile it with `clang -O3 -march=native [-ffast-math]`, run the
/// exe, and return the parsed result + timings.
pub fn build_and_run(prog: &Program, fast_math: bool) -> Result<LlvmRun, String> {
    let ir = emit_ir(prog)?;
    let clang = find_clang();
    let rt_lib = find_zipp_rt()?;
    let dir = std::env::temp_dir().join(format!("zipp_llvm_{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let ll = dir.join("zmod.ll");
    let exe = dir.join("zmod.exe");
    std::fs::write(&ll, &ir).map_err(|e| format!("write {}: {e}", ll.display()))?;

    // clang compiles the .ll and links the shared runtime (zipp_rt.lib) + the
    // system libs std needs. -Wno-override-module silences the no-triple note.
    let mut args: Vec<String> =
        vec!["-O3".into(), "-march=native".into(), "-Wno-override-module".into()];
    if fast_math {
        args.push("-ffast-math".into());
    }
    args.push("-o".into());
    args.push(exe.to_string_lossy().into_owned());
    args.push(ll.to_string_lossy().into_owned());
    args.push(rt_lib);
    for l in RT_SYS_LIBS {
        args.push(format!("-l{l}"));
    }

    let c0 = Instant::now();
    let out = Command::new(&clang)
        .args(&args)
        .output()
        .map_err(|e| format!("could not run clang ('{clang}'): {e}\nSet ZIPP_CLANG or add clang to PATH."))?;
    let compile = c0.elapsed();
    if !out.status.success() {
        return Err(format!(
            "clang failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let run = Command::new(&exe)
        .output()
        .map_err(|e| format!("could not run compiled exe: {e}"))?;
    if !run.status.success() {
        // A runtime abort (e.g. an out-of-bounds index via zipp_oob) printed its
        // message to stderr before aborting — surface it.
        let msg = String::from_utf8_lossy(&run.stderr);
        let msg = msg.trim();
        if msg.is_empty() {
            return Err(format!("compiled program exited with {}", run.status));
        }
        // The CLI re-prefixes errors with "zipp: "; the runtime already did.
        return Err(msg.strip_prefix("zipp: ").unwrap_or(msg).to_string());
    }
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let mut result = String::new();
    let mut execute = Duration::ZERO;
    let mut prog_out = String::new();
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("__ZRESULT__:") {
            result = v.to_string();
        } else if let Some(ms) = line.strip_prefix("__ZTIME_MS__:") {
            if let Ok(n) = ms.trim().parse::<u64>() {
                execute = Duration::from_millis(n);
            }
        } else {
            prog_out.push_str(line);
            prog_out.push('\n');
        }
    }
    Ok(LlvmRun { result, program_output: prog_out, compile, execute })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_ir_for_scalar_programs() {
        let prog = zippc::compile("fn main(): i64 { let s = 0; let i = 0; while (i < 10) { s = s + i; i = i + 1; } return s; }").unwrap();
        let ir = emit_ir(&prog).unwrap();
        assert!(ir.contains("define i64 @zfn"));
        assert!(ir.contains("@main"));
        assert!(ir.contains("alloca"));
    }

    #[test]
    fn emits_ir_for_nullable_structs() {
        // nullable param + narrowing + `??` — LLVM now compiles this natively
        // (a null is a `ptr null` / `i64 0`, struct equality via `ptrtoint`).
        let ts = "interface User { age: i64; } \
                  function ageOr(u: User | null, fb: i64): i64 { \
                    if (u !== null) { return u.age; } return fb; \
                  } \
                  function main(): i64 { \
                    let some: User | null = { age: 5 } as User; \
                    return ageOr(some, -1) + ageOr(null, -1); }";
        let module = zipp_ts::compile_ts(ts).expect("lower");
        let prog = zippc::compile_module(&module).expect("compile");
        assert!(ineligible_reason(&prog).is_none()); // no interpreter fallback
        let ir = emit_ir(&prog).unwrap();
        assert!(ir.contains("define i64 @zfn"));
        assert!(ir.contains("ptrtoint") || ir.contains("inttoptr")); // null/ptr crossing
    }

    #[test]
    fn emits_ir_for_array_programs() {
        // arrays now emit (literal, index, len, repeat)
        let prog = zippc::compile(
            "fn main(): i64 { let a = [1, 2, 3]; a[0] = 9; let b = [0; 4]; return a[0] + len(b); }",
        )
        .unwrap();
        let ir = emit_ir(&prog).unwrap();
        assert!(ir.contains("@zipp_alloc"));
        assert!(ir.contains("@zipp_oob"));
        assert!(ir.contains("getelementptr"));
    }

    #[test]
    fn emits_ir_for_string_programs() {
        let prog = zippc::compile(
            "fn main(): i64 { let s = \"foo\" + \"bar\"; print(s); return len(s); }",
        )
        .unwrap();
        let ir = emit_ir(&prog).unwrap();
        assert!(ir.contains("@.sconst_"));
        assert!(ir.contains("@zipp_str_concat"));
        assert!(ir.contains("@zipp_print_str"));
    }

    #[test]
    fn emits_ir_for_struct_programs() {
        let prog = zippc::compile(
            "struct P { x: i64, y: f64 } \
             fn main(): f64 { let p = P { x: 3, y: 1.5 }; p.x = p.x + 1; return p.y; }",
        )
        .unwrap();
        let ir = emit_ir(&prog).unwrap();
        assert!(ir.contains("@zipp_alloc")); // structs reuse the array allocator
        assert!(ir.contains("getelementptr"));
    }

    #[test]
    fn emits_ir_for_builtins() {
        // the whole language emits now — math builtins included
        let prog = zippc::compile("fn main(): i64 { return abs(0 - 5) + pow(2, 3); }").unwrap();
        let ir = emit_ir(&prog).unwrap();
        assert!(ir.contains("@llvm.abs.i64"));
        assert!(ir.contains("@zipp_ipow"));
    }
}
