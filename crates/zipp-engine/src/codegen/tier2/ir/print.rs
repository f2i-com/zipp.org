//! Human-readable IR dump.
//!
//! Renders an [`IrFunction`] in a Cranelift-like textual form. Purely
//! for debugging — not parsed anywhere, not meant to survive through
//! tools. Format is deliberately simple so `println!`-debugging an
//! SSA graph doesn't require a separate viewer.
//!
//! Example output for `function add(a, b) { return a + b; }`:
//!
//! ```text
//! function(2 params, 3 regs) {
//!   bb0(v0: val, v1: val, v2: val):
//!     v3 = AddGeneric v0, v1
//!     return v3
//! }
//! ```
//!
//! Entry-block parameters are the bytecode register slots (one per
//! register, not per parameter — the translator conservatively makes
//! the whole register window available at entry).

use std::fmt::Write;

use super::types::{IrFunction, IrOp, Terminator};

/// Format an [`IrFunction`] as multi-line text.
pub fn dump(func: &IrFunction) -> String {
    let mut out = String::with_capacity(512);
    let _ = writeln!(
        out,
        "function({} params, {} regs, {} bytes bytecode) {{",
        func.num_parameters, func.num_bytecode_regs, func.bytecode_len
    );
    for block in &func.blocks {
        // Header: `bb0(v0: val, v1: val):`
        write!(out, "  {}(", block.id).unwrap();
        for (i, (vid, ty)) in block.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{vid}: {ty}");
        }
        out.push_str("):\n");

        for (vid, op) in &block.ops {
            if op.is_void() {
                let _ = writeln!(out, "    {}", fmt_op(op));
            } else {
                let _ = writeln!(out, "    {vid} = {}", fmt_op(op));
            }
        }

        let _ = writeln!(out, "    {}", fmt_term(&block.term));
    }
    out.push('}');
    out
}

fn fmt_op(op: &IrOp) -> String {
    match op {
        IrOp::ConstI32(v) => format!("const.i32 {v}"),
        IrOp::ConstF64(bits) => format!("const.f64 {}", f64::from_bits(*bits)),
        IrOp::ConstBool(v) => format!("const.bool {v}"),
        IrOp::ConstNull => "const.null".to_string(),
        IrOp::ConstUndef => "const.undef".to_string(),
        IrOp::ConstValue(bits) => format!("const.value {bits:#018x}"),
        IrOp::LoadReg(r) => format!("load_reg r{r}"),

        IrOp::Copy(v) => format!("copy {v}"),
        IrOp::NegI32(v) => format!("neg.i32 {v}"),
        IrOp::NegF64(v) => format!("neg.f64 {v}"),
        IrOp::NotBool(v) => format!("not.bool {v}"),

        IrOp::AddI32(a, b) => format!("add.i32 {a}, {b}"),
        IrOp::SubI32(a, b) => format!("sub.i32 {a}, {b}"),
        IrOp::MulI32(a, b) => format!("mul.i32 {a}, {b}"),
        IrOp::CheckedAddI32(a, b, d) => format!("cadd.i32 {a}, {b} (deopt {d})"),
        IrOp::CheckedSubI32(a, b, d) => format!("csub.i32 {a}, {b} (deopt {d})"),
        IrOp::CheckedMulI32(a, b, d) => format!("cmul.i32 {a}, {b} (deopt {d})"),
        IrOp::AddF64(a, b) => format!("add.f64 {a}, {b}"),
        IrOp::SubF64(a, b) => format!("sub.f64 {a}, {b}"),
        IrOp::MulF64(a, b) => format!("mul.f64 {a}, {b}"),
        IrOp::DivF64(a, b) => format!("div.f64 {a}, {b}"),
        IrOp::AddGeneric(a, b) => format!("add.val {a}, {b}"),
        IrOp::SubGeneric(a, b) => format!("sub.val {a}, {b}"),
        IrOp::MulGeneric(a, b) => format!("mul.val {a}, {b}"),
        IrOp::DivGeneric(a, b) => format!("div.val {a}, {b}"),
        IrOp::ModGeneric(a, b) => format!("mod.val {a}, {b}"),

        IrOp::EqI32(a, b) => format!("eq.i32 {a}, {b}"),
        IrOp::NeI32(a, b) => format!("ne.i32 {a}, {b}"),
        IrOp::LtI32(a, b) => format!("lt.i32 {a}, {b}"),
        IrOp::LeI32(a, b) => format!("le.i32 {a}, {b}"),
        IrOp::GtI32(a, b) => format!("gt.i32 {a}, {b}"),
        IrOp::GeI32(a, b) => format!("ge.i32 {a}, {b}"),
        IrOp::EqValue(a, b) => format!("eq.val {a}, {b}"),
        IrOp::NeValue(a, b) => format!("ne.val {a}, {b}"),
        IrOp::LooseEqValue(a, b) => format!("looseeq.val {a}, {b}"),
        IrOp::LtValue(a, b) => format!("lt.val {a}, {b}"),
        IrOp::LeValue(a, b) => format!("le.val {a}, {b}"),

        IrOp::CheckI32(v, d) => format!("check.i32 {v} (deopt {d})"),
        IrOp::CheckF64(v, d) => format!("check.f64 {v} (deopt {d})"),
        IrOp::CheckHeap(v, d) => format!("check.heap {v} (deopt {d})"),
        IrOp::CheckHeapShape(v, s, d) => format!("check.shape {v} @ {s} (deopt {d})"),
        IrOp::CheckFunctionIs(v, f, d) => format!("check.fn {v} is {f:#018x} (deopt {d})"),

        IrOp::BoxI32(v) => format!("box.i32 {v}"),
        IrOp::BoxF64(v) => format!("box.f64 {v}"),
        IrOp::BoxBool(v) => format!("box.bool {v}"),
        IrOp::UnboxI32(v) => format!("unbox.i32 {v}"),
        IrOp::UnboxF64(v) => format!("unbox.f64 {v}"),
        IrOp::UnboxBool(v) => format!("unbox.bool {v}"),

        IrOp::LoadSlot(obj, off, ty) => format!("load_slot {obj}.+{off} : {ty}"),
        IrOp::StoreSlot(obj, off, val) => format!("store_slot {obj}.+{off} = {val}"),
        IrOp::LoadGlobal(idx) => format!("load_global g{idx}"),
        IrOp::StoreGlobal(idx, v) => format!("store_global g{idx} = {v}"),

        IrOp::CallRuntime(helper, args) => {
            let args_str: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            format!("call_runtime {helper:?}({})", args_str.join(", "))
        }
        IrOp::CallValue(callee, args) => {
            let args_str: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            format!("call {callee}({})", args_str.join(", "))
        }
        IrOp::MakeClosureNoCapture(idx) => format!("make_closure const#{idx}"),
    }
}

fn fmt_term(term: &Terminator) -> String {
    match term {
        Terminator::Return(None) => "return".to_string(),
        Terminator::Return(Some(v)) => format!("return {v}"),
        Terminator::Jump(b, args) => {
            let args_str: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            format!("jump {b}({})", args_str.join(", "))
        }
        Terminator::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            let ta: Vec<String> = then_args.iter().map(|a| a.to_string()).collect();
            let ea: Vec<String> = else_args.iter().map(|a| a.to_string()).collect();
            format!(
                "branch {cond} ? {then_block}({}) : {else_block}({})",
                ta.join(", "),
                ea.join(", ")
            )
        }
        Terminator::Deopt(d) => format!("deopt {d}"),
        Terminator::Unreachable => "unreachable".to_string(),
    }
}
