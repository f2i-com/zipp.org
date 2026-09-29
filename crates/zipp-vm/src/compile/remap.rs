//! Register renumbering over one instruction for `FnCompiler::check_regs`.
//! Variants with the same operand shape share a mapping body.
//!
//! Exhaustive on purpose: adding an `Instr` variant without listing it here is
//! a compile error, so a register field can never silently escape the remap.
//! Contiguous argument windows (`arg_base` + `argc`) are remapped by their base
//! only -- the compiler allocates windows from the ordinary register stack,
//! never from a class range, so a window can neither start in nor cross into
//! the renumbered range. The mapping itself leaves the `NO_REG` and
//! `BARE_MATH_BY_NAME` sentinels alone (`FnCompiler::check_regs`).

use crate::bytecode::{Instr, Reg};

// Full builds retain a separate arm per variant: the grouped WASM candidates
// showed repeatable slowdowns. Lite retains the measured compact grouping.
// Both expansions use the same exhaustive operand list and mapping bodies.
#[cfg(not(feature = "wasm-lite"))]
macro_rules! remap_by_shape {
    ($instr:expr; $(#[$attr:meta] $first:pat_param $(| $rest:pat_param)* => $body:block)*) => {
        match $instr {
            $(#[$attr] $first => $body, $(#[$attr] $rest => $body,)*)*
        }
    };
}

#[cfg(feature = "wasm-lite")]
macro_rules! remap_by_shape {
    ($instr:expr; $(#[$attr:meta] $first:pat_param $(| $rest:pat_param)* => $body:block)*) => {
        match $instr {
            $(#[$attr] $first $(| $rest)* => $body,)*
        }
    };
}

pub(crate) fn remap_regs(i: &mut Instr, m: &dyn Fn(Reg) -> Reg) {
    remap_by_shape!(i;
        #[cfg(all())]
        Instr::LoadConst { dst: r0, .. }
        | Instr::LoadInt { dst: r0, .. }
        | Instr::LoadUndefined { dst: r0, .. }
        | Instr::LoadNewTarget { dst: r0, .. }
        | Instr::LoadCallee { dst: r0, .. }
        | Instr::LoadClassValue { dst: r0, .. }
        | Instr::LoadHole { dst: r0, .. }
        | Instr::LoadNull { dst: r0, .. }
        | Instr::LoadBool { dst: r0, .. }
        | Instr::LoadGlobal { dst: r0, .. }
        | Instr::LoadGlobalOrUndefined { dst: r0, .. }
        | Instr::LoadGlobalDyn { dst: r0, .. }
        | Instr::LoadGlobalOrUndefinedDyn { dst: r0, .. }
        | Instr::StoreGlobalDyn { src: r0, .. }
        | Instr::EvalScopeHas { dst: r0, .. }
        | Instr::EvalScopeSet { src: r0, .. }
        | Instr::StoreGlobal { src: r0, .. }
        | Instr::StoreGlobalStrict { src: r0, .. }
        | Instr::StoreGlobalResolved { src: r0, .. }
        | Instr::Now { dst: r0, .. }
        | Instr::DecInits { recv: r0, .. }
        | Instr::ThisCheck { src: r0, .. }
        | Instr::RequireObject { val: r0, .. }
        | Instr::IterClose { iter: r0, .. }
        | Instr::IterCloseQuiet { iter: r0, .. }
        | Instr::SuperCtorFetch { dst: r0, .. }
        | Instr::SuperBase { dst: r0, .. }
        | Instr::SuperGet { dst: r0, .. }
        | Instr::SuperGetObj { dst: r0, .. }
        | Instr::SuperSetObj { val: r0, .. }
        | Instr::FieldInit { val: r0, .. }
        | Instr::JumpIfFalse { cond: r0, .. }
        | Instr::JumpIfTrue { cond: r0, .. }
        | Instr::MakeFunc { dst: r0, .. }
        | Instr::MakeClosure { dst: r0, .. }
        | Instr::MakeCell { reg: r0, .. }
        | Instr::MakeCellTdz { reg: r0, .. }
        | Instr::MakeCellFnName { reg: r0, .. }
        | Instr::MarkCellConst { reg: r0, .. }
        | Instr::UpvalGet { dst: r0, .. }
        | Instr::UpvalSet { src: r0, .. }
        | Instr::LoadUpvalDyn { dst: r0, .. }
        | Instr::StoreUpvalDyn { src: r0, .. }
        | Instr::NewObject { dst: r0, .. }
        | Instr::NewPlannedObject { dst: r0, .. }
        | Instr::CheckCoercible { src: r0, .. }
        | Instr::LoadBigInt { dst: r0, .. }
        | Instr::LoadBigIntBig { dst: r0, .. }
        | Instr::DeleteGlobal { dst: r0, .. }
        | Instr::ImportMeta { dst: r0, .. }
        | Instr::Throw { src: r0, .. }
        | Instr::PushHandler { catch_reg: r0, .. }
        | Instr::OpenUsingScope { dst: r0, .. }
        | Instr::TemplateGetCached { dst: r0, .. }
        | Instr::TemplateSetCached { src: r0, .. }
        | Instr::Return { src: r0, .. }
        | Instr::Print { arg_base: r0, .. } => {
            *r0 = m(*r0);
        }
        #[cfg(all())]
        Instr::Move { dst: r0, src: r1, .. }
        | Instr::TypeOfIs { dst: r0, a: r1, .. }
        | Instr::Neg { dst: r0, a: r1, .. }
        | Instr::ToNum { dst: r0, a: r1, .. }
        | Instr::BitNot { dst: r0, a: r1, .. }
        | Instr::AddInt { dst: r0, a: r1, .. }
        | Instr::Pad2Concat { dst: r0, src: r1, .. }
        | Instr::Pad2Conditional { dst: r0, src: r1, .. }
        | Instr::Not { dst: r0, a: r1, .. }
        | Instr::ToStr { dst: r0, a: r1, .. }
        | Instr::TypeOf { dst: r0, a: r1, .. }
        | Instr::ArrayAppend { arr: r0, val: r1, .. }
        | Instr::ArrayRest { dst: r0, src: r1, .. }
        | Instr::ObjectSpread { target: r0, src: r1, .. }
        | Instr::ObjectRest { dst: r0, src: r1, .. }
        | Instr::DecKey { class: r0, key: r1, .. }
        | Instr::DecElem { class: r0, arg_base: r1, .. }
        | Instr::DecClass { class: r0, arg_base: r1, .. }
        | Instr::DecField { val: r0, recv: r1, .. }
        | Instr::Yield { dst: r0, val: r1, .. }
        | Instr::Await { dst: r0, val: r1, .. }
        | Instr::IterPrime { dst: r0, iter: r1, .. }
        | Instr::IterCloseFinally { iter: r0, kind_reg: r1, .. }
        | Instr::SuperCtor { ctor: r0, arg_base: r1, .. }
        | Instr::SuperCtorSpread { ctor: r0, args: r1, .. }
        | Instr::SuperGetComputed { dst: r0, key: r1, .. }
        | Instr::SuperGetRef { dst: r0, receiver: r1, .. }
        | Instr::SuperSet { base: r0, val: r1, .. }
        | Instr::SetHomeObject { method: r0, home: r1, .. }
        | Instr::SuperGetObjComputed { dst: r0, key: r1, .. }
        | Instr::SuperSetObjComputed { key: r0, val: r1, .. }
        | Instr::SuperMethodObj { dst: r0, arg_base: r1, .. }
        | Instr::PushFieldKey { class: r0, key: r1, .. }
        | Instr::NewWeakRef { dst: r0, target: r1, .. }
        | Instr::NewFinalizationRegistry { dst: r0, cleanup: r1, .. }
        | Instr::NewPromise { dst: r0, executor: r1, .. }
        | Instr::SuperMethodSpread { dst: r0, args: r1, .. }
        | Instr::WithHas { dst: r0, obj: r1, .. }
        | Instr::WithGet { dst: r0, obj: r1, .. }
        | Instr::WithSet { obj: r0, val: r1, .. }
        | Instr::JumpIfNotLt { a: r0, b: r1, .. }
        | Instr::JumpIfNotLe { a: r0, b: r1, .. }
        | Instr::MakeArrow { dst: r0, this_reg: r1, .. }
        | Instr::CellGet { dst: r0, cell: r1, .. }
        | Instr::CellSet { cell: r0, src: r1, .. }
        | Instr::CellSetChecked { cell: r0, src: r1, .. }
        | Instr::NewArray { dst: r0, arg_base: r1, .. }
        | Instr::ToObject { dst: r0, src: r1, .. }
        | Instr::BigIntFrom { dst: r0, arg: r1, .. }
        | Instr::ForInKeys { dst: r0, obj: r1, .. }
        | Instr::LenOf { dst: r0, obj: r1, .. }
        | Instr::ToConcatKey { dst: r0, src: r1, .. }
        | Instr::SetFnNameFromKey { func: r0, key: r1, .. }
        | Instr::GetProp { dst: r0, obj: r1, .. }
        | Instr::SetProp { obj: r0, val: r1, .. }
        | Instr::SetPrivate { obj: r0, val: r1, .. }
        | Instr::InitDataProp { obj: r0, val: r1, .. }
        | Instr::SetLiteralProto { obj: r0, val: r1, .. }
        | Instr::AppendDataProp { obj: r0, val: r1, .. }
        | Instr::FinalizeObject { dst: r0, val_base: r1, .. }
        | Instr::DeleteProp { dst: r0, obj: r1, .. }
        | Instr::TailCall { callee: r0, arg_base: r1, .. }
        | Instr::DefineField { obj: r0, val: r1, .. }
        | Instr::PushFinally { kind_reg: r0, val_reg: r1, .. }
        | Instr::EndFinally { kind_reg: r0, val_reg: r1, .. }
        | Instr::RegisterDisposable { scope: r0, val: r1, .. }
        | Instr::RegisterAsyncDisposable { scope: r0, val: r1, .. }
        | Instr::SetRaw { arr: r0, raw: r1, .. }
        | Instr::ClassAddMember { class: r0, key: r1, .. }
        | Instr::DateNew { dst: r0, arg_base: r1, .. }
        | Instr::DateUTC { dst: r0, arg_base: r1, .. }
        | Instr::DateParse { dst: r0, src: r1, .. }
        | Instr::GetIterator { dst: r0, src: r1, .. }
        | Instr::GetIteratorObj { dst: r0, src: r1, .. }
        | Instr::IterToArray { dst: r0, src: r1, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
        }
        #[cfg(all())]
        Instr::TypeOfSame { dst: r0, a: r1, b: r2, .. }
        | Instr::Add { dst: r0, a: r1, b: r2, .. }
        | Instr::Sub { dst: r0, a: r1, b: r2, .. }
        | Instr::Mul { dst: r0, a: r1, b: r2, .. }
        | Instr::Div { dst: r0, a: r1, b: r2, .. }
        | Instr::Mod { dst: r0, a: r1, b: r2, .. }
        | Instr::Bitwise { dst: r0, a: r1, b: r2, .. }
        | Instr::Pow { dst: r0, a: r1, b: r2, .. }
        | Instr::StrConcat { dst: r0, a: r1, b: r2, .. }
        | Instr::StrAppendInPlace { dst: r0, a: r1, b: r2, .. }
        | Instr::StrConcatChain { dst: r0, a: r1, b: r2, .. }
        | Instr::Lt { dst: r0, a: r1, b: r2, .. }
        | Instr::Le { dst: r0, a: r1, b: r2, .. }
        | Instr::Gt { dst: r0, a: r1, b: r2, .. }
        | Instr::Ge { dst: r0, a: r1, b: r2, .. }
        | Instr::Eq { dst: r0, a: r1, b: r2, .. }
        | Instr::Ne { dst: r0, a: r1, b: r2, .. }
        | Instr::LooseEq { dst: r0, a: r1, b: r2, .. }
        | Instr::LooseNe { dst: r0, a: r1, b: r2, .. }
        | Instr::ObjectRestDyn { dst: r0, src: r1, keys_base: r2, .. }
        | Instr::AsyncYieldDelegate { mode_dst: r0, val_dst: r1, val: r2, .. }
        | Instr::AsyncIterThrowStep { dst: r0, iter: r1, exc: r2, .. }
        | Instr::YieldDelegate { mode_dst: r0, val_dst: r1, val: r2, .. }
        | Instr::GetAsyncIterator { dst: r0, src: r1, sync_dst: r2, .. }
        | Instr::ForAwaitNext { dst: r0, iter: r1, idx: r2, .. }
        | Instr::SuperMethod { dst: r0, base: r1, arg_base: r2, .. }
        | Instr::SuperGetRefComputed { dst: r0, key: r1, receiver: r2, .. }
        | Instr::SuperSetComputed { base: r0, key: r1, val: r2, .. }
        | Instr::SuperMethodObjComputed { dst: r0, key: r1, arg_base: r2, .. }
        | Instr::New { dst: r0, callee: r1, arg_base: r2, .. }
        | Instr::AsyncFromSyncStep { dst: r0, step: r1, iter: r2, .. }
        | Instr::CallSpread { dst: r0, callee: r1, args: r2, .. }
        | Instr::CallMethodSpread { dst: r0, obj: r1, args: r2, .. }
        | Instr::SuperMethodComputedSpread { dst: r0, key: r1, args: r2, .. }
        | Instr::NewSpread { dst: r0, callee: r1, args: r2, .. }
        | Instr::GlobalFn { dst: r0, callee: r1, arg_base: r2, .. }
        | Instr::InstanceOfDyn { dst: r0, val: r1, ctor: r2, .. }
        | Instr::HasProp { dst: r0, key: r1, obj: r2, .. }
        | Instr::NewRegExp { dst: r0, pattern: r1, flags: r2, .. }
        | Instr::ForInLive { dst: r0, obj: r1, key: r2, .. }
        | Instr::GetIndex { dst: r0, obj: r1, key: r2, .. }
        | Instr::SetIndex { obj: r0, key: r1, val: r2, .. }
        | Instr::GetIndexConcat { dst: r0, obj: r1, key: r2, .. }
        | Instr::SetIndexConcat { obj: r0, key: r1, val: r2, .. }
        | Instr::DeleteIndexConcat { dst: r0, obj: r1, key: r2, .. }
        | Instr::ClassStaticField { class: r0, key: r1, val: r2, .. }
        | Instr::ToPropKey { dst: r0, obj: r1, src: r2, .. }
        | Instr::DefineAccessor { obj: r0, key: r1, func: r2, .. }
        | Instr::InitDataPropDyn { obj: r0, key: r1, val: r2, .. }
        | Instr::DeleteIndex { dst: r0, obj: r1, key: r2, .. }
        | Instr::TailCallWithThis { callee: r0, this_v: r1, arg_base: r2, .. }
        | Instr::Call { dst: r0, callee: r1, arg_base: r2, .. }
        | Instr::CallMethod { dst: r0, obj: r1, arg_base: r2, .. }
        | Instr::DisposeScope { scope: r0, kind_reg: r1, val_reg: r2, .. }
        | Instr::AsyncDisposeNext { scope: r0, res: r1, done: r2, .. }
        | Instr::MergeDispose { kind_reg: r0, val_reg: r1, err: r2, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            *r2 = m(*r2);
        }
        #[cfg(all())]
        Instr::AddRightPair { dst: r0, a: r1, b: r2, c: r3, .. }
        | Instr::IsArray { dst: r0, a: r1, callee: r2, this_v: r3, .. }
        | Instr::JsonParse { dst: r0, a: r1, callee: r2, this_v: r3, .. }
        | Instr::AsyncIterReturnStep { dst: r0, has_dst: r1, iter: r2, ret: r3, .. }
        | Instr::SuperMethodComputed { dst: r0, base: r1, key: r2, arg_base: r3, .. }
        | Instr::CallWithThisSpread { dst: r0, callee: r1, this_v: r2, args: r3, .. }
        | Instr::CallMethodComputedSpread { dst: r0, obj: r1, key: r2, args: r3, .. }
        | Instr::MathOp { dst: r0, callee: r1, this_v: r2, arg_base: r3, .. }
        | Instr::StaticFn { dst: r0, callee: r1, this_v: r2, arg_base: r3, .. }
        | Instr::MathSpread { dst: r0, callee: r1, this_v: r2, args: r3, .. }
        | Instr::ObjectKeys { dst: r0, obj: r1, callee: r2, this_v: r3, .. }
        | Instr::ObjectValues { dst: r0, obj: r1, callee: r2, this_v: r3, .. }
        | Instr::ObjectEntries { dst: r0, obj: r1, callee: r2, this_v: r3, .. }
        | Instr::CallWithThis { dst: r0, callee: r1, this_v: r2, arg_base: r3, .. }
        | Instr::RegExpMethod { dst: r0, callee: r1, this_v: r2, arg_base: r3, .. }
        | Instr::CallMethodComputed { dst: r0, obj: r1, key: r2, arg_base: r3, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            *r2 = m(*r2);
            *r3 = m(*r3);
        }
        #[cfg(all())]
        Instr::StrAppendIndex { dst: r0, a: r1, obj: r2, key: r3, scratch: r4, .. }
        | Instr::JsonStringify { dst: r0, val: r1, space: r2, callee: r3, this_v: r4, .. }
        | Instr::AsyncIterNextStep { dst: r0, iter: r1, idx: r2, sent: r3, next_fn: r4, .. }
        | Instr::IterNext { value_dst: r0, done_dst: r1, iter: r2, idx: r3, next: r4, .. }
        | Instr::ArrayFrom { dst: r0, src: r1, mapfn: r2, callee: r3, this_v: r4, .. }
        | Instr::DirectEval { dst: r0, callee: r1, this_v: r2, arg_base: r3, this_reg: r4, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            *r2 = m(*r2);
            *r3 = m(*r3);
            *r4 = m(*r4);
        }
        #[cfg(all())]
        Instr::MakeClass { dst: r0, parent: r1, .. }
        | Instr::NewMap { dst: r0, src: r1, .. }
        | Instr::NewSet { dst: r0, src: r1, .. }
        | Instr::NewWeakMap { dst: r0, src: r1, .. }
        | Instr::NewWeakSet { dst: r0, src: r1, .. }
        | Instr::NewBox { dst: r0, arg: r1, .. }
        | Instr::MakeSymbol { dst: r0, desc: r1, .. } => {
            *r0 = m(*r0);
            if let Some(r) = r1.as_mut() { *r = m(*r); }
        }
        #[cfg(all())]
        Instr::IterDelegate { value_dst: r0, done_dst: r1, ret_dst: r2, iter: r3, mode: r4, sent: r5, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            *r2 = m(*r2);
            *r3 = m(*r3);
            *r4 = m(*r4);
            *r5 = m(*r5);
        }
        #[cfg(all())]
        Instr::GenStart
        | Instr::Jump { .. }
        | Instr::CheckGlobalResolvable { .. }
        | Instr::PopHandler
        | Instr::PopFinally
        | Instr::JumpFinally { .. }
        | Instr::ReturnUndefined => {
        }
        #[cfg(all())]
        Instr::ArrayCtor { dst: r0, arg_base: r1, callee: r2, .. }
        | Instr::ImportCall { dst: r0, spec: r1, opts: r2, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            if let Some(r) = r2.as_mut() { *r = m(*r); }
        }
        #[cfg(all())]
        Instr::NewError { dst: r0, arg: r1, opts: r2, errors: r3, .. } => {
            *r0 = m(*r0);
            if let Some(r) = r1.as_mut() { *r = m(*r); }
            if let Some(r) = r2.as_mut() { *r = m(*r); }
            if let Some(r) = r3.as_mut() { *r = m(*r); }
        }
        #[cfg(not(feature = "wasm-lite"))]
        Instr::PyArith { dst: r0, a: r1, b: r2, .. }
        | Instr::PyCompare { dst: r0, a: r1, b: r2, .. }
        | Instr::PyGlobal { dst: r0, globals: r1, rt: r2, .. }
        | Instr::PyStrItem { dst: r0, s: r1, k: r2, .. }
        | Instr::PyGenNext { dst: r0, next: r1, this: r2, .. }
        | Instr::PyMethod { dst: r0, obj: r1, rt: r2, .. }
        | Instr::PyModGet { dst: r0, obj: r1, rt: r2, .. }
        | Instr::PyLen { dst: r0, v: r1, rt: r2, .. }
        | Instr::PySeq { dst: r0, items: r1, rt: r2, .. }
        | Instr::PyUnpack { dst: r0, v: r1, rt: r2, .. }
        | Instr::PyCall { dst: r0, f: r1, arg_base: r2, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            *r2 = m(*r2);
        }
        #[cfg(not(feature = "wasm-lite"))]
        Instr::PyAddImm { dst: r0, a: r1, .. }
        | Instr::PyJumpCompare { a: r0, b: r1, .. }
        | Instr::PyClassOf { dst: r0, obj: r1, .. }
        | Instr::PyDictGet { dst: r0, obj: r1, .. }
        | Instr::PyDictSet { obj: r0, val: r1, .. }
        | Instr::PyCallEntry { dst: r0, f: r1, .. }
        | Instr::PyStrLen { dst: r0, s: r1, .. }
        | Instr::PyGetAttr { dst: r0, obj: r1, .. }
        | Instr::PySetAttr { obj: r0, val: r1, .. }
        | Instr::PyAttrFn { dst: r0, obj: r1, .. }
        | Instr::PyRaise { e: r0, rt: r1, .. }
        | Instr::PyClassAttr { dst: r0, obj: r1, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
        }
        #[cfg(not(feature = "wasm-lite"))]
        Instr::PyGetItem { dst: r0, o: r1, k: r2, seq: r3, dict: r4, .. }
        | Instr::PySetItem { o: r0, k: r1, v: r2, seq: r3, dict: r4, .. }
        | Instr::PyNew { dst: r0, entry: r1, this_f: r2, cls: r3, rt: r4, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            *r2 = m(*r2);
            *r3 = m(*r3);
            *r4 = m(*r4);
        }
        #[cfg(not(feature = "wasm-lite"))]
        Instr::PyIsInstance { dst: r0, v: r1, t: r2, rt: r3, .. }
        | Instr::PyCaught { dst: r0, e: r1, line: r2, rt: r3, .. }
        | Instr::PyDictLookup { dst: r0, d: r1, k: r2, rt: r3, .. }
        | Instr::PyMakeExc { dst: r0, cls: r1, args: r2, rt: r3, .. } => {
            *r0 = m(*r0);
            *r1 = m(*r1);
            *r2 = m(*r2);
            *r3 = m(*r3);
        }
        #[cfg(not(feature = "wasm-lite"))]
        Instr::PyExcPop { rt: r0, .. } => {
            *r0 = m(*r0);
        }
    );
}
