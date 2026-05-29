//! ZIPP **contract profile**: emit deterministic, **gas-metered** WebAssembly
//! for the scalar subset (`i64`/`f64`/`bool` + sized ints, casts, arithmetic,
//! control flow, functions, recursion, `print`). This is ZIPP.md §7's "contract"
//! lane — the one that competes with the EVM.
//!
//! Why WASM is a good contract target: it's deterministic and sandboxed, and we
//! can make execution *gas-metered* by instrumenting the code itself — a global
//! `$gas` counter is charged per basic block and traps (`unreachable`) when it
//! runs out, exactly like an on-chain VM. No runtime cooperation needed, so the
//! module runs anywhere (we test it under Node).
//!
//! Control flow: ZIPP's IR uses arbitrary jumps, but WASM control flow is
//! structured. We use the standard `br_table` dispatch loop — each basic block
//! is a case selected by a `$pc` local; blocks fall through in order (matching
//! the IR's leaders) or set `$pc` and re-dispatch. No relooper needed.
//!
//! Heap types (arrays/strings/structs) and math builtins fall back to the
//! interpreter for now (a contract heap + gas-priced runtime is future work).

use std::collections::{BTreeSet, HashMap};

use zippc::ast::{BinOp, Type, UnOp};
use zippc::ir::{FuncMeta, Instr, Program};

/// Assemble WAT text into a wasm binary (pure Rust; no external toolchain).
pub fn assemble(wat_text: &str) -> Result<Vec<u8>, String> {
    wat::parse_str(wat_text).map_err(|e| format!("wat assembly failed: {e}"))
}

/// The contract profile's value kind (drives wasm valtype + signedness).
#[derive(Clone, Copy, PartialEq)]
enum WTy {
    I64,
    I32,
    U32,
    U64,
    F64,
}

impl WTy {
    fn valtype(self) -> &'static str {
        match self {
            WTy::F64 => "f64",
            WTy::I32 | WTy::U32 => "i32",
            _ => "i64", // i64, u64, bool
        }
    }
    fn is_int(self) -> bool {
        self != WTy::F64
    }
    fn signed(self) -> bool {
        matches!(self, WTy::I64 | WTy::I32)
    }
}

fn wty_of(t: Type) -> WTy {
    match t {
        Type::F64 => WTy::F64,
        Type::I32 => WTy::I32,
        Type::U32 => WTy::U32,
        Type::U64 => WTy::U64,
        _ => WTy::I64, // i64, bool
    }
}

/// Reason a program can't use the contract profile (scalar subset only), or `None`.
pub fn ineligible_reason(prog: &Program) -> Option<&'static str> {
    if prog.uses_opt_scalar {
        return Some("nullable scalars (i64 | null)");
    }
    for ins in &prog.code {
        let bad = match ins {
            Instr::SConst { .. } => "strings",
            Instr::ArrayLit { .. }
            | Instr::ArrayRepeat { .. }
            | Instr::Index { .. }
            | Instr::SetIndex { .. }
            | Instr::Len { .. } => "arrays",
            Instr::Builtin { .. } => "builtins",
            Instr::NewStruct { .. } | Instr::GetField { .. } | Instr::SetField { .. } => "structs",
            Instr::ConstNull { .. } => "nullable types (T | null)",
            _ => continue,
        };
        return Some(bad);
    }
    for f in &prog.funcs {
        let scalar = |t: Type| matches!(t, Type::I64 | Type::F64 | Type::Bool | Type::I32 | Type::U32 | Type::U64);
        if !scalar(f.ret) || f.params.iter().any(|t| !scalar(*t)) {
            return Some("non-scalar function signatures");
        }
    }
    None
}

/// Per-register [`WTy`] (same forward pass as the other backends).
fn infer(prog: &Program, f: &FuncMeta, end: u32) -> Vec<WTy> {
    let mut t = vec![WTy::I64; f.nregs as usize];
    for (i, p) in f.params.iter().enumerate() {
        t[i] = wty_of(*p);
    }
    for pc in f.entry..end {
        match &prog.code[pc as usize] {
            Instr::Const { dst, .. } => t[*dst as usize] = WTy::I64,
            Instr::FConst { dst, .. } => t[*dst as usize] = WTy::F64,
            Instr::Cast { dst, to, .. } => t[*dst as usize] = wty_of(*to),
            Instr::Mov { dst, src } => t[*dst as usize] = t[*src as usize],
            Instr::Bin { op, dst, a, .. } => {
                use BinOp::*;
                t[*dst as usize] = match op {
                    Eq | Ne | Lt | Le | Gt | Ge | And | Or => WTy::I64, // bool
                    _ => t[*a as usize],
                };
            }
            Instr::Unary { op, dst, a } => {
                t[*dst as usize] = match op {
                    UnOp::Neg => t[*a as usize],
                    _ => WTy::I64,
                };
            }
            Instr::Call { func, dst, .. } => t[*dst as usize] = wty_of(prog.funcs[*func as usize].ret),
            _ => {}
        }
    }
    t
}

fn leaders_of(prog: &Program, entry: u32, end: u32) -> BTreeSet<u32> {
    let mut s = BTreeSet::new();
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

/// f64 constant as a round-trippable literal (Debug always includes a `.`).
fn fconst(x: f64) -> String {
    format!("{x:?}")
}

/// Cast expression (Rust `as` semantics) wrapping `x`.
fn cast_expr(from: WTy, to: WTy, x: &str) -> String {
    let (fv, tv) = (from.valtype(), to.valtype());
    if from == WTy::F64 && to.is_int() {
        let s = if to.signed() { "s" } else { "u" };
        return format!("({tv}.trunc_sat_f64_{s} {x})"); // saturating, non-trapping
    }
    if from.is_int() && to == WTy::F64 {
        let s = if from.signed() { "s" } else { "u" };
        return format!("(f64.convert_{fv}_{s} {x})");
    }
    if fv == tv {
        return x.to_string(); // same valtype: reinterpret, or f64->f64
    }
    if fv == "i32" && tv == "i64" {
        let s = if from.signed() { "s" } else { "u" };
        return format!("(i64.extend_i32_{s} {x})");
    }
    format!("(i32.wrap_i64 {x})") // i64 -> i32
}

/// Binary-op expression yielding the result at its ZIPP type (comparisons
/// produce a bool == i64 0/1).
fn bin_expr(op: BinOp, ty: WTy, a: &str, b: &str) -> String {
    use BinOp::*;
    if ty == WTy::F64 {
        let f = |m: &str| format!("(f64.{m} {a} {b})");
        let c = |m: &str| format!("(i64.extend_i32_u (f64.{m} {a} {b}))");
        return match op {
            Add => f("add"),
            Sub => f("sub"),
            Mul => f("mul"),
            Div => f("div"),
            Eq => c("eq"),
            Ne => c("ne"),
            Lt => c("lt"),
            Le => c("le"),
            Gt => c("gt"),
            Ge => c("ge"),
            _ => "(i64.const 0)".into(),
        };
    }
    let w = ty.valtype();
    let s = ty.signed();
    let f = |m: &str| format!("({w}.{m} {a} {b})");
    let c = |m: &str| format!("(i64.extend_i32_u ({w}.{m} {a} {b}))");
    match op {
        Add => f("add"),
        Sub => f("sub"),
        Mul => f("mul"),
        Div => f(if s { "div_s" } else { "div_u" }),
        Mod => f(if s { "rem_s" } else { "rem_u" }),
        BitAnd => f("and"),
        BitOr => f("or"),
        BitXor => f("xor"),
        Shl => f("shl"), // wasm shifts mask the amount mod width, like ZIPP
        Shr => f(if s { "shr_s" } else { "shr_u" }),
        Eq => c("eq"),
        Ne => c("ne"),
        Lt => c(if s { "lt_s" } else { "lt_u" }),
        Le => c(if s { "le_s" } else { "le_u" }),
        Gt => c(if s { "gt_s" } else { "gt_u" }),
        Ge => c(if s { "ge_s" } else { "ge_u" }),
        // And/Or operate on bool (i64 0/1) operands.
        And => format!("(i64.extend_i32_u (i32.and (i64.ne {a} (i64.const 0)) (i64.ne {b} (i64.const 0))))"),
        Or => format!("(i64.extend_i32_u (i32.or (i64.ne {a} (i64.const 0)) (i64.ne {b} (i64.const 0))))"),
    }
}

/// Emit one straight-line instruction (no terminators) as WAT, or `None` if it
/// is a control-flow terminator (handled by the caller).
fn emit_value_instr(prog: &Program, rty: &[WTy], pc: u32) -> Option<String> {
    let g = |r: u32| format!("(local.get $r{r})");
    let set = |dst: u32, e: String| format!("    (local.set $r{dst} {e})\n");
    Some(match &prog.code[pc as usize] {
        Instr::Const { dst, imm } => set(*dst, format!("(i64.const {imm})")),
        Instr::FConst { dst, imm } => set(*dst, format!("(f64.const {})", fconst(*imm))),
        Instr::Cast { dst, src, to } => {
            set(*dst, cast_expr(rty[*src as usize], wty_of(*to), &g(*src)))
        }
        Instr::Mov { dst, src } => set(*dst, g(*src)),
        Instr::Bin { op, dst, a, b } => {
            set(*dst, bin_expr(*op, rty[*a as usize], &g(*a), &g(*b)))
        }
        Instr::Unary { op, dst, a } => {
            let ty = rty[*a as usize];
            let w = ty.valtype();
            let e = match op {
                UnOp::Neg if ty == WTy::F64 => format!("(f64.neg {})", g(*a)),
                UnOp::Neg => format!("({w}.sub ({w}.const 0) {})", g(*a)),
                UnOp::BitNot => format!("({w}.xor {} ({w}.const -1))", g(*a)),
                UnOp::Not => format!("(i64.extend_i32_u (i64.eqz {}))", g(*a)),
            };
            set(*dst, e)
        }
        Instr::Call { func, arg_base, argc, dst } => {
            let args: String = (0..*argc).map(|k| format!(" {}", g(*arg_base + k))).collect();
            set(*dst, format!("(call $zfn{func}{args})"))
        }
        Instr::Print { a } => {
            let ty = rty[*a as usize];
            match ty {
                WTy::F64 => format!("    (call $print_f64 {})\n", g(*a)),
                WTy::U64 => format!("    (call $print_u64 {})\n", g(*a)),
                WTy::I32 => format!("    (call $print_i64 (i64.extend_i32_s {}))\n", g(*a)),
                WTy::U32 => format!("    (call $print_i64 (i64.extend_i32_u {}))\n", g(*a)),
                _ => format!("    (call $print_i64 {})\n", g(*a)),
            }
        }
        // Terminators are handled by the block emitter.
        Instr::Jmp { .. } | Instr::JmpIfZero { .. } | Instr::JmpIfNonZero { .. } | Instr::Ret { .. } => {
            return None
        }
        other => return Some(format!("    ;; unsupported (ineligible) instr: {other:?}\n")),
    })
}

fn emit_fn(prog: &Program, fi: usize, end: u32) -> String {
    let f = &prog.funcs[fi];
    let rty = infer(prog, f, end);

    // Basic blocks = spans between leaders; assign dense indices.
    let leaders: Vec<u32> = leaders_of(prog, f.entry, end).into_iter().collect();
    let dense: HashMap<u32, usize> = leaders.iter().enumerate().map(|(i, &pc)| (pc, i)).collect();
    let n = leaders.len();
    let block_end = |k: usize| if k + 1 < n { leaders[k + 1] } else { end };

    // signature + locals
    let params: String = (0..f.nparams)
        .map(|i| format!(" (param $r{i} {})", rty[i as usize].valtype()))
        .collect();
    let mut s = format!(
        "  (func $zfn{fi}{params} (result {})\n",
        wty_of(f.ret).valtype()
    );
    for r in f.nparams..f.nregs {
        s.push_str(&format!("    (local $r{r} {})\n", rty[r as usize].valtype()));
    }
    s.push_str("    (local $pc i32)\n");
    s.push_str(&format!("    (local.set $pc (i32.const {}))\n", dense[&f.entry]));

    // br_table dispatch loop: n nested case-blocks + a default, inside a loop.
    s.push_str("    (loop $loop\n");
    s.push_str("    (block $bdef\n");
    for k in (0..n).rev() {
        s.push_str(&format!("    (block $b{k}\n"));
    }
    let labels: String = (0..n).map(|k| format!("$b{k} ")).collect();
    s.push_str(&format!("      (br_table {labels}$bdef (local.get $pc))\n"));
    s.push_str("    )\n"); // close $b0

    for k in 0..n {
        // Landing point for block k. Charge gas for the block, then its instrs.
        let (start, stop) = (leaders[k], block_end(k));
        let cost = (stop - start).max(1);
        s.push_str(&format!(
            "    (global.set $gas (i64.sub (global.get $gas) (i64.const {cost})))\n\
             \x20   (if (i64.lt_s (global.get $gas) (i64.const 0)) (then (unreachable)))\n"
        ));
        let mut terminated = false;
        for pc in start..stop {
            if let Some(code) = emit_value_instr(prog, &rty, pc) {
                s.push_str(&code);
            } else {
                // a terminator
                match &prog.code[pc as usize] {
                    Instr::Jmp { target } => {
                        s.push_str(&format!(
                            "    (local.set $pc (i32.const {})) (br $loop)\n",
                            dense[target]
                        ));
                        terminated = true;
                    }
                    Instr::Ret { src } => {
                        s.push_str(&format!("    (return (local.get $r{src}))\n"));
                        terminated = true;
                    }
                    Instr::JmpIfZero { cond, target } => {
                        let cw = rty[*cond as usize].valtype();
                        s.push_str(&format!(
                            "    (if ({cw}.eqz (local.get $r{cond})) (then (local.set $pc (i32.const {})) (br $loop)))\n",
                            dense[target]
                        ));
                        // false case falls through to block k+1 (== pc+1 leader)
                    }
                    Instr::JmpIfNonZero { cond, target } => {
                        s.push_str(&format!(
                            "    (if (i64.ne (local.get $r{cond}) (i64.const 0)) (then (local.set $pc (i32.const {})) (br $loop)))\n",
                            dense[target]
                        ));
                    }
                    _ => unreachable!(),
                }
            }
            if terminated {
                break;
            }
        }
        // close the enclosing case-block ($b{k+1}) or $bdef after the last block
        s.push_str("    )\n");
    }
    s.push_str("    (unreachable)\n"); // default pc (never taken) inside the loop
    s.push_str("    )\n"); // close loop
    s.push_str("    (unreachable)\n"); // loop never falls through; satisfy the validator
    s.push_str("  )\n");
    s
}

/// Lower a whole program to a gas-metered WASM module (WAT text).
pub fn emit_wat(prog: &Program) -> Result<String, String> {
    if let Some(bad) = ineligible_reason(prog) {
        return Err(format!(
            "--wasm supports the scalar subset only (program uses {bad})"
        ));
    }
    let mut w = String::new();
    w.push_str("(module\n");
    // Host-provided print (a contract wouldn't print, but it's handy for demos).
    w.push_str("  (import \"zipp\" \"print_i64\" (func $print_i64 (param i64)))\n");
    w.push_str("  (import \"zipp\" \"print_u64\" (func $print_u64 (param i64)))\n");
    w.push_str("  (import \"zipp\" \"print_f64\" (func $print_f64 (param f64)))\n");
    // Gas: a mutable, exported counter charged per basic block. The host sets the
    // budget before calling `main`; running out traps via `unreachable`.
    w.push_str("  (global $gas (export \"gas\") (mut i64) (i64.const 1000000000))\n");
    for i in 0..prog.funcs.len() {
        let end = if i + 1 < prog.funcs.len() {
            prog.funcs[i + 1].entry
        } else {
            prog.code.len() as u32
        };
        w.push_str(&emit_fn(prog, i, end));
    }
    // Exported `main` wrapper: widen a sized-int result to i64 so the host reads
    // one uniform width (f64 results stay f64).
    let mi = prog.main as usize;
    let mret = wty_of(prog.funcs[mi].ret);
    let rvt = if mret == WTy::F64 { "f64" } else { "i64" };
    w.push_str(&format!("  (func (export \"main\") (result {rvt})\n"));
    let call = format!("(call $zfn{mi})");
    let body = match mret {
        WTy::I32 => format!("(i64.extend_i32_s {call})"),
        WTy::U32 => format!("(i64.extend_i32_u {call})"),
        _ => call, // f64 / i64 / u64
    };
    w.push_str(&format!("    {body})\n"));
    w.push_str(")\n");
    Ok(w)
}

// A tiny Node harness: provides the `print` imports, sets the gas budget,
// calls `main`, and prints program output + the result / gas-used markers.
// A gas-exhaustion `unreachable` trap surfaces as a non-zero exit + stderr.
const HARNESS_JS: &str = r#"const fs = require('fs');
const [,, wasmPath, gasStr, retkind] = process.argv;
const gas = BigInt(gasStr);
const out = [];
const imports = { zipp: {
  print_i64: (x) => out.push(x.toString()),
  print_u64: (x) => out.push(BigInt.asUintN(64, x).toString()),
  print_f64: (x) => out.push(x.toString()),
}};
WebAssembly.instantiate(fs.readFileSync(wasmPath), imports).then(({ instance }) => {
  const ex = instance.exports;
  ex.gas.value = gas;
  let res;
  try { res = ex.main(); }
  catch (e) {
    if (out.length) process.stdout.write(out.join('\n') + '\n');
    process.stderr.write('out of gas / trap: ' + (e && e.message ? e.message : e) + '\n');
    process.exit(7);
  }
  const used = gas - ex.gas.value;
  if (out.length) process.stdout.write(out.join('\n') + '\n');
  const r = retkind === 'u64' ? BigInt.asUintN(64, res).toString() : res.toString();
  process.stdout.write('__ZRESULT__:' + r + '\n');
  process.stdout.write('__ZGAS__:' + used.toString() + '\n');
}).catch((e) => { process.stderr.write('wasm error: ' + (e && e.message ? e.message : e) + '\n'); process.exit(1); });
"#;

/// Result of compiling + running a program through the contract profile.
pub struct WasmRun {
    pub result: String,
    pub program_output: String,
    pub gas_used: u64,
    pub wasm_bytes: usize,
}

fn find_node() -> String {
    std::env::var("ZIPP_NODE").unwrap_or_else(|_| "node".to_string())
}

/// Emit gas-metered WASM, assemble it, and run it under Node with a `gas` budget.
pub fn build_and_run(prog: &Program, gas: u64) -> Result<WasmRun, String> {
    let wat = emit_wat(prog)?;
    let bytes = assemble(&wat)?;
    let dir = std::env::temp_dir().join(format!("zipp_wasm_{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let wasm_path = dir.join("zmod.wasm");
    let harness = dir.join("run.js");
    std::fs::write(&wasm_path, &bytes).map_err(|e| format!("write wasm: {e}"))?;
    std::fs::write(&harness, HARNESS_JS).map_err(|e| format!("write harness: {e}"))?;

    let retkind = match wty_of(prog.funcs[prog.main as usize].ret) {
        WTy::U64 => "u64",
        WTy::F64 => "f64",
        _ => "int",
    };
    let run = std::process::Command::new(find_node())
        .arg(&harness)
        .arg(&wasm_path)
        .arg(gas.to_string())
        .arg(retkind)
        .output()
        .map_err(|e| format!("could not run node: {e}\nInstall Node, or set ZIPP_NODE."))?;
    if !run.status.success() {
        let err = String::from_utf8_lossy(&run.stderr);
        let err = err.trim();
        return Err(if err.is_empty() {
            format!("wasm run exited with {}", run.status)
        } else {
            err.to_string()
        });
    }
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let (mut result, mut gas_used, mut prog_out) = (String::new(), 0u64, String::new());
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("__ZRESULT__:") {
            result = v.to_string();
        } else if let Some(g) = line.strip_prefix("__ZGAS__:") {
            gas_used = g.trim().parse().unwrap_or(0);
        } else {
            prog_out.push_str(line);
            prog_out.push('\n');
        }
    }
    Ok(WasmRun { result, program_output: prog_out, gas_used, wasm_bytes: bytes.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wat(src: &str) -> String {
        emit_wat(&zippc::compile(src).expect("compile")).expect("emit")
    }

    #[test]
    fn assembles_scalar_programs() {
        // fib(20) — recursion, branches, arithmetic — must assemble to valid wasm.
        let fib = "fn fib(n: i64): i64 { if (n < 2) { return n; } return fib(n-1)+fib(n-2); } \
                   fn main(): i64 { return fib(20); }";
        let bytes = assemble(&wat(fib)).expect("assemble");
        assert!(bytes.starts_with(&[0x00, 0x61, 0x73, 0x6d]));
        // sized ints + a loop also assemble
        let loop_src = "fn main(): i64 { let s = u32(0); let i = 0; while (i < 10) { s = s + u32(i); i = i + 1; } return i64(s); }";
        assert!(assemble(&wat(loop_src)).is_ok());
    }

    #[test]
    fn rejects_heap_programs() {
        let p = zippc::compile("fn main(): i64 { let a = [1,2,3]; return a[0]; }").unwrap();
        assert!(emit_wat(&p).is_err());
    }
}
