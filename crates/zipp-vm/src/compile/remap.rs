//! Register renumbering over one instruction (GENERATED from `bytecode::Instr`
//! by the compiler's class-register finaliser; see `FnCompiler::check_regs`).
//!
//! Exhaustive on purpose: adding an `Instr` variant without listing it here is
//! a compile error, so a register field can never silently escape the remap.
//! Contiguous argument windows (`arg_base` + `argc`) are remapped by their base
//! only -- the compiler allocates windows from the ordinary register stack,
//! never from a class range, so a window can neither start in nor cross into
//! the renumbered range. The mapping itself leaves the `NO_REG` and
//! `BARE_MATH_BY_NAME` sentinels alone (`FnCompiler::check_regs`).

use crate::bytecode::{Instr, Reg};

pub(crate) fn remap_regs(i: &mut Instr, m: &dyn Fn(Reg) -> Reg) {
    match i {
        Instr::LoadConst { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadInt { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadUndefined { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadNewTarget { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadCallee { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadClassValue { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadHole { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadNull { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadBool { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::Move { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::LoadGlobal { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::TypeOfIs { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::TypeOfSame { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::LoadGlobalOrUndefined { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadGlobalDyn { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadGlobalOrUndefinedDyn { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::StoreGlobalDyn { src, .. } => {
            *src = m(*src);
        }
        Instr::EvalScopeHas { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::EvalScopeSet { src, .. } => {
            *src = m(*src);
        }
        Instr::StoreGlobal { src, .. } => {
            *src = m(*src);
        }
        Instr::StoreGlobalStrict { src, .. } => {
            *src = m(*src);
        }
        Instr::StoreGlobalResolved { src, .. } => {
            *src = m(*src);
        }
        Instr::Now { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::Add { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Sub { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Mul { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Div { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Mod { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Neg { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::ToNum { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::Bitwise { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Pow { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::BitNot { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::AddInt { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::StrConcat { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::AddRightPair { dst, a, b, c, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
            *c = m(*c);
        }
        Instr::Pad2Concat { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::Pad2Conditional { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::StrAppendInPlace { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::StrAppendIndex { dst, a, obj, key, scratch, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *obj = m(*obj);
            *key = m(*key);
            *scratch = m(*scratch);
        }
        Instr::StrConcatChain { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Lt { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Le { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Gt { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Ge { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Eq { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Ne { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::LooseEq { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::LooseNe { dst, a, b, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *b = m(*b);
        }
        Instr::Not { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::ToStr { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::TypeOf { dst, a, .. } => {
            *dst = m(*dst);
            *a = m(*a);
        }
        Instr::IsArray { dst, a, callee, this_v, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *callee = m(*callee);
            *this_v = m(*this_v);
        }
        Instr::JsonStringify { dst, val, space, callee, this_v, .. } => {
            *dst = m(*dst);
            *val = m(*val);
            *space = m(*space);
            *callee = m(*callee);
            *this_v = m(*this_v);
        }
        Instr::JsonParse { dst, a, callee, this_v, .. } => {
            *dst = m(*dst);
            *a = m(*a);
            *callee = m(*callee);
            *this_v = m(*this_v);
        }
        Instr::ArrayAppend { arr, val, .. } => {
            *arr = m(*arr);
            *val = m(*val);
        }
        Instr::ArrayRest { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::ObjectSpread { target, src, .. } => {
            *target = m(*target);
            *src = m(*src);
        }
        Instr::ObjectRest { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::ObjectRestDyn { dst, src, keys_base, .. } => {
            *dst = m(*dst);
            *src = m(*src);
            *keys_base = m(*keys_base);
        }
        Instr::DecKey { class, key, .. } => {
            *class = m(*class);
            *key = m(*key);
        }
        Instr::DecElem { class, arg_base, .. } => {
            *class = m(*class);
            *arg_base = m(*arg_base);
        }
        Instr::DecClass { class, arg_base, .. } => {
            *class = m(*class);
            *arg_base = m(*arg_base);
        }
        Instr::DecInits { recv, .. } => {
            *recv = m(*recv);
        }
        Instr::DecField { val, recv, .. } => {
            *val = m(*val);
            *recv = m(*recv);
        }
        Instr::MakeClass { dst, parent, .. } => {
            *dst = m(*dst);
            if let Some(r) = parent.as_mut() { *r = m(*r); }
        }
        Instr::ThisCheck { src, .. } => {
            *src = m(*src);
        }
        Instr::Yield { dst, val, .. } => {
            *dst = m(*dst);
            *val = m(*val);
        }
        Instr::AsyncYieldDelegate { mode_dst, val_dst, val, .. } => {
            *mode_dst = m(*mode_dst);
            *val_dst = m(*val_dst);
            *val = m(*val);
        }
        Instr::RequireObject { val, .. } => {
            *val = m(*val);
        }
        Instr::AsyncIterThrowStep { dst, iter, exc, .. } => {
            *dst = m(*dst);
            *iter = m(*iter);
            *exc = m(*exc);
        }
        Instr::AsyncIterNextStep { dst, iter, idx, sent, next_fn, .. } => {
            *dst = m(*dst);
            *iter = m(*iter);
            *idx = m(*idx);
            *sent = m(*sent);
            *next_fn = m(*next_fn);
        }
        Instr::AsyncIterReturnStep { dst, has_dst, iter, ret, .. } => {
            *dst = m(*dst);
            *has_dst = m(*has_dst);
            *iter = m(*iter);
            *ret = m(*ret);
        }
        Instr::YieldDelegate { mode_dst, val_dst, val, .. } => {
            *mode_dst = m(*mode_dst);
            *val_dst = m(*val_dst);
            *val = m(*val);
        }
        Instr::IterDelegate { value_dst, done_dst, ret_dst, iter, mode, sent, .. } => {
            *value_dst = m(*value_dst);
            *done_dst = m(*done_dst);
            *ret_dst = m(*ret_dst);
            *iter = m(*iter);
            *mode = m(*mode);
            *sent = m(*sent);
        }
        Instr::GenStart => {}
        Instr::Await { dst, val, .. } => {
            *dst = m(*dst);
            *val = m(*val);
        }
        Instr::IterNext { value_dst, done_dst, iter, idx, next, .. } => {
            *value_dst = m(*value_dst);
            *done_dst = m(*done_dst);
            *iter = m(*iter);
            *idx = m(*idx);
            *next = m(*next);
        }
        Instr::IterPrime { dst, iter, .. } => {
            *dst = m(*dst);
            *iter = m(*iter);
        }
        Instr::IterClose { iter, .. } => {
            *iter = m(*iter);
        }
        Instr::IterCloseQuiet { iter, .. } => {
            *iter = m(*iter);
        }
        Instr::IterCloseFinally { iter, kind_reg, .. } => {
            *iter = m(*iter);
            *kind_reg = m(*kind_reg);
        }
        Instr::GetAsyncIterator { dst, src, sync_dst, .. } => {
            *dst = m(*dst);
            *src = m(*src);
            *sync_dst = m(*sync_dst);
        }
        Instr::ForAwaitNext { dst, iter, idx, .. } => {
            *dst = m(*dst);
            *iter = m(*iter);
            *idx = m(*idx);
        }
        Instr::SuperCtorFetch { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::SuperCtor { ctor, arg_base, .. } => {
            *ctor = m(*ctor);
            *arg_base = m(*arg_base);
        }
        Instr::SuperCtorSpread { ctor, args, .. } => {
            *ctor = m(*ctor);
            *args = m(*args);
        }
        Instr::SuperBase { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::SuperMethod { dst, base, arg_base, .. } => {
            *dst = m(*dst);
            *base = m(*base);
            *arg_base = m(*arg_base);
        }
        Instr::SuperGet { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::SuperGetComputed { dst, key, .. } => {
            *dst = m(*dst);
            *key = m(*key);
        }
        Instr::SuperGetRef { dst, receiver, .. } => {
            *dst = m(*dst);
            *receiver = m(*receiver);
        }
        Instr::SuperGetRefComputed { dst, key, receiver, .. } => {
            *dst = m(*dst);
            *key = m(*key);
            *receiver = m(*receiver);
        }
        Instr::SuperMethodComputed { dst, base, key, arg_base, .. } => {
            *dst = m(*dst);
            *base = m(*base);
            *key = m(*key);
            *arg_base = m(*arg_base);
        }
        Instr::SuperSet { base, val, .. } => {
            *base = m(*base);
            *val = m(*val);
        }
        Instr::SuperSetComputed { base, key, val, .. } => {
            *base = m(*base);
            *key = m(*key);
            *val = m(*val);
        }
        Instr::SetHomeObject { method, home, .. } => {
            *method = m(*method);
            *home = m(*home);
        }
        Instr::SuperGetObj { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::SuperGetObjComputed { dst, key, .. } => {
            *dst = m(*dst);
            *key = m(*key);
        }
        Instr::SuperSetObj { val, .. } => {
            *val = m(*val);
        }
        Instr::SuperSetObjComputed { key, val, .. } => {
            *key = m(*key);
            *val = m(*val);
        }
        Instr::SuperMethodObj { dst, arg_base, .. } => {
            *dst = m(*dst);
            *arg_base = m(*arg_base);
        }
        Instr::SuperMethodObjComputed { dst, key, arg_base, .. } => {
            *dst = m(*dst);
            *key = m(*key);
            *arg_base = m(*arg_base);
        }
        Instr::New { dst, callee, arg_base, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *arg_base = m(*arg_base);
        }
        Instr::PushFieldKey { class, key, .. } => {
            *class = m(*class);
            *key = m(*key);
        }
        Instr::FieldInit { val, .. } => {
            *val = m(*val);
        }
        Instr::AsyncFromSyncStep { dst, step, iter, .. } => {
            *dst = m(*dst);
            *step = m(*step);
            *iter = m(*iter);
        }
        Instr::ArrayCtor { dst, arg_base, callee, .. } => {
            *dst = m(*dst);
            *arg_base = m(*arg_base);
            if let Some(r) = callee.as_mut() { *r = m(*r); }
        }
        Instr::NewMap { dst, src, .. } => {
            *dst = m(*dst);
            if let Some(r) = src.as_mut() { *r = m(*r); }
        }
        Instr::NewSet { dst, src, .. } => {
            *dst = m(*dst);
            if let Some(r) = src.as_mut() { *r = m(*r); }
        }
        Instr::NewWeakMap { dst, src, .. } => {
            *dst = m(*dst);
            if let Some(r) = src.as_mut() { *r = m(*r); }
        }
        Instr::NewWeakSet { dst, src, .. } => {
            *dst = m(*dst);
            if let Some(r) = src.as_mut() { *r = m(*r); }
        }
        Instr::NewWeakRef { dst, target, .. } => {
            *dst = m(*dst);
            *target = m(*target);
        }
        Instr::NewBox { dst, arg, .. } => {
            *dst = m(*dst);
            if let Some(r) = arg.as_mut() { *r = m(*r); }
        }
        Instr::NewFinalizationRegistry { dst, cleanup, .. } => {
            *dst = m(*dst);
            *cleanup = m(*cleanup);
        }
        Instr::NewPromise { dst, executor, .. } => {
            *dst = m(*dst);
            *executor = m(*executor);
        }
        Instr::CallSpread { dst, callee, args, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *args = m(*args);
        }
        Instr::CallWithThisSpread { dst, callee, this_v, args, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *this_v = m(*this_v);
            *args = m(*args);
        }
        Instr::CallMethodSpread { dst, obj, args, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *args = m(*args);
        }
        Instr::CallMethodComputedSpread { dst, obj, key, args, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *key = m(*key);
            *args = m(*args);
        }
        Instr::SuperMethodSpread { dst, args, .. } => {
            *dst = m(*dst);
            *args = m(*args);
        }
        Instr::SuperMethodComputedSpread { dst, key, args, .. } => {
            *dst = m(*dst);
            *key = m(*key);
            *args = m(*args);
        }
        Instr::NewSpread { dst, callee, args, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *args = m(*args);
        }
        Instr::MathOp { dst, callee, this_v, arg_base, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *this_v = m(*this_v);
            *arg_base = m(*arg_base);
        }
        Instr::GlobalFn { dst, callee, arg_base, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *arg_base = m(*arg_base);
        }
        Instr::StaticFn { dst, callee, this_v, arg_base, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *this_v = m(*this_v);
            *arg_base = m(*arg_base);
        }
        Instr::ArrayFrom { dst, src, mapfn, callee, this_v, .. } => {
            *dst = m(*dst);
            *src = m(*src);
            *mapfn = m(*mapfn);
            *callee = m(*callee);
            *this_v = m(*this_v);
        }
        Instr::MathSpread { dst, callee, this_v, args, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *this_v = m(*this_v);
            *args = m(*args);
        }
        Instr::InstanceOfDyn { dst, val, ctor, .. } => {
            *dst = m(*dst);
            *val = m(*val);
            *ctor = m(*ctor);
        }
        Instr::HasProp { dst, key, obj, .. } => {
            *dst = m(*dst);
            *key = m(*key);
            *obj = m(*obj);
        }
        Instr::WithHas { dst, obj, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
        }
        Instr::WithGet { dst, obj, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
        }
        Instr::WithSet { obj, val, .. } => {
            *obj = m(*obj);
            *val = m(*val);
        }
        Instr::Jump { .. } => {}
        Instr::JumpIfFalse { cond, .. } => {
            *cond = m(*cond);
        }
        Instr::JumpIfTrue { cond, .. } => {
            *cond = m(*cond);
        }
        Instr::JumpIfNotLt { a, b, .. } => {
            *a = m(*a);
            *b = m(*b);
        }
        Instr::JumpIfNotLe { a, b, .. } => {
            *a = m(*a);
            *b = m(*b);
        }
        Instr::MakeFunc { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::MakeClosure { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::MakeArrow { dst, this_reg, .. } => {
            *dst = m(*dst);
            *this_reg = m(*this_reg);
        }
        Instr::MakeCell { reg, .. } => {
            *reg = m(*reg);
        }
        Instr::MakeCellTdz { reg, .. } => {
            *reg = m(*reg);
        }
        Instr::MakeCellFnName { reg, .. } => {
            *reg = m(*reg);
        }
        Instr::MarkCellConst { reg, .. } => {
            *reg = m(*reg);
        }
        Instr::CellGet { dst, cell, .. } => {
            *dst = m(*dst);
            *cell = m(*cell);
        }
        Instr::CellSet { cell, src, .. } => {
            *cell = m(*cell);
            *src = m(*src);
        }
        Instr::CellSetChecked { cell, src, .. } => {
            *cell = m(*cell);
            *src = m(*src);
        }
        Instr::UpvalGet { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::UpvalSet { src, .. } => {
            *src = m(*src);
        }
        Instr::LoadUpvalDyn { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::StoreUpvalDyn { src, .. } => {
            *src = m(*src);
        }
        Instr::NewArray { dst, arg_base, .. } => {
            *dst = m(*dst);
            *arg_base = m(*arg_base);
        }
        Instr::NewObject { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::NewPlannedObject { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::ToObject { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::CheckCoercible { src, .. } => {
            *src = m(*src);
        }
        Instr::NewError { dst, arg, opts, errors, .. } => {
            *dst = m(*dst);
            if let Some(r) = arg.as_mut() { *r = m(*r); }
            if let Some(r) = opts.as_mut() { *r = m(*r); }
            if let Some(r) = errors.as_mut() { *r = m(*r); }
        }
        Instr::MakeSymbol { dst, desc, .. } => {
            *dst = m(*dst);
            if let Some(r) = desc.as_mut() { *r = m(*r); }
        }
        Instr::LoadBigInt { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::LoadBigIntBig { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::BigIntFrom { dst, arg, .. } => {
            *dst = m(*dst);
            *arg = m(*arg);
        }
        Instr::NewRegExp { dst, pattern, flags, .. } => {
            *dst = m(*dst);
            *pattern = m(*pattern);
            *flags = m(*flags);
        }
        Instr::ObjectKeys { dst, obj, callee, this_v, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *callee = m(*callee);
            *this_v = m(*this_v);
        }
        Instr::ForInKeys { dst, obj, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
        }
        Instr::ForInLive { dst, obj, key, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *key = m(*key);
        }
        Instr::ObjectValues { dst, obj, callee, this_v, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *callee = m(*callee);
            *this_v = m(*this_v);
        }
        Instr::ObjectEntries { dst, obj, callee, this_v, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *callee = m(*callee);
            *this_v = m(*this_v);
        }
        Instr::LenOf { dst, obj, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
        }
        Instr::GetIndex { dst, obj, key, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *key = m(*key);
        }
        Instr::SetIndex { obj, key, val, .. } => {
            *obj = m(*obj);
            *key = m(*key);
            *val = m(*val);
        }
        Instr::GetIndexConcat { dst, obj, key, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *key = m(*key);
        }
        Instr::SetIndexConcat { obj, key, val, .. } => {
            *obj = m(*obj);
            *key = m(*key);
            *val = m(*val);
        }
        Instr::ToConcatKey { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::DeleteIndexConcat { dst, obj, key, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *key = m(*key);
        }
        Instr::ImportCall { dst, spec, opts, .. } => {
            *dst = m(*dst);
            *spec = m(*spec);
            if let Some(r) = opts.as_mut() { *r = m(*r); }
        }
        Instr::ClassStaticField { class, key, val, .. } => {
            *class = m(*class);
            *key = m(*key);
            *val = m(*val);
        }
        Instr::ToPropKey { dst, obj, src, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *src = m(*src);
        }
        Instr::DefineAccessor { obj, key, func, .. } => {
            *obj = m(*obj);
            *key = m(*key);
            *func = m(*func);
        }
        Instr::SetFnNameFromKey { func, key, .. } => {
            *func = m(*func);
            *key = m(*key);
        }
        Instr::GetProp { dst, obj, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
        }
        Instr::SetProp { obj, val, .. } => {
            *obj = m(*obj);
            *val = m(*val);
        }
        Instr::SetPrivate { obj, val, .. } => {
            *obj = m(*obj);
            *val = m(*val);
        }
        Instr::InitDataProp { obj, val, .. } => {
            *obj = m(*obj);
            *val = m(*val);
        }
        Instr::SetLiteralProto { obj, val, .. } => {
            *obj = m(*obj);
            *val = m(*val);
        }
        Instr::AppendDataProp { obj, val, .. } => {
            *obj = m(*obj);
            *val = m(*val);
        }
        Instr::FinalizeObject { dst, val_base, .. } => {
            *dst = m(*dst);
            *val_base = m(*val_base);
        }
        Instr::InitDataPropDyn { obj, key, val, .. } => {
            *obj = m(*obj);
            *key = m(*key);
            *val = m(*val);
        }
        Instr::DeleteProp { dst, obj, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
        }
        Instr::DeleteIndex { dst, obj, key, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *key = m(*key);
        }
        Instr::DeleteGlobal { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::TailCall { callee, arg_base, .. } => {
            *callee = m(*callee);
            *arg_base = m(*arg_base);
        }
        Instr::TailCallWithThis { callee, this_v, arg_base, .. } => {
            *callee = m(*callee);
            *this_v = m(*this_v);
            *arg_base = m(*arg_base);
        }
        Instr::Call { dst, callee, arg_base, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *arg_base = m(*arg_base);
        }
        Instr::CallWithThis { dst, callee, this_v, arg_base, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *this_v = m(*this_v);
            *arg_base = m(*arg_base);
        }
        Instr::RegExpMethod { dst, callee, this_v, arg_base, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *this_v = m(*this_v);
            *arg_base = m(*arg_base);
        }
        Instr::ImportMeta { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::DirectEval { dst, callee, this_v, arg_base, this_reg, .. } => {
            *dst = m(*dst);
            *callee = m(*callee);
            *this_v = m(*this_v);
            *arg_base = m(*arg_base);
            *this_reg = m(*this_reg);
        }
        Instr::CheckGlobalResolvable { .. } => {}
        Instr::DefineField { obj, val, .. } => {
            *obj = m(*obj);
            *val = m(*val);
        }
        Instr::CallMethod { dst, obj, arg_base, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *arg_base = m(*arg_base);
        }
        Instr::CallMethodComputed { dst, obj, key, arg_base, .. } => {
            *dst = m(*dst);
            *obj = m(*obj);
            *key = m(*key);
            *arg_base = m(*arg_base);
        }
        Instr::Throw { src, .. } => {
            *src = m(*src);
        }
        Instr::PushHandler { catch_reg, .. } => {
            *catch_reg = m(*catch_reg);
        }
        Instr::PopHandler => {}
        Instr::PushFinally { kind_reg, val_reg, .. } => {
            *kind_reg = m(*kind_reg);
            *val_reg = m(*val_reg);
        }
        Instr::PopFinally => {}
        Instr::EndFinally { kind_reg, val_reg, .. } => {
            *kind_reg = m(*kind_reg);
            *val_reg = m(*val_reg);
        }
        Instr::OpenUsingScope { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::RegisterDisposable { scope, val, .. } => {
            *scope = m(*scope);
            *val = m(*val);
        }
        Instr::DisposeScope { scope, kind_reg, val_reg, .. } => {
            *scope = m(*scope);
            *kind_reg = m(*kind_reg);
            *val_reg = m(*val_reg);
        }
        Instr::RegisterAsyncDisposable { scope, val, .. } => {
            *scope = m(*scope);
            *val = m(*val);
        }
        Instr::AsyncDisposeNext { scope, res, done, .. } => {
            *scope = m(*scope);
            *res = m(*res);
            *done = m(*done);
        }
        Instr::MergeDispose { kind_reg, val_reg, err, .. } => {
            *kind_reg = m(*kind_reg);
            *val_reg = m(*val_reg);
            *err = m(*err);
        }
        Instr::JumpFinally { .. } => {}
        Instr::SetRaw { arr, raw, .. } => {
            *arr = m(*arr);
            *raw = m(*raw);
        }
        Instr::TemplateGetCached { dst, .. } => {
            *dst = m(*dst);
        }
        Instr::TemplateSetCached { src, .. } => {
            *src = m(*src);
        }
        Instr::ClassAddMember { class, key, .. } => {
            *class = m(*class);
            *key = m(*key);
        }
        Instr::DateNew { dst, arg_base, .. } => {
            *dst = m(*dst);
            *arg_base = m(*arg_base);
        }
        Instr::DateUTC { dst, arg_base, .. } => {
            *dst = m(*dst);
            *arg_base = m(*arg_base);
        }
        Instr::DateParse { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::GetIterator { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::GetIteratorObj { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::IterToArray { dst, src, .. } => {
            *dst = m(*dst);
            *src = m(*src);
        }
        Instr::Return { src, .. } => {
            *src = m(*src);
        }
        Instr::ReturnUndefined => {}
        Instr::Print { arg_base, .. } => {
            *arg_base = m(*arg_base);
        }
    }
}
