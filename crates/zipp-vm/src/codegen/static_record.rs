//! Bounded static-record factory recognition for the Tier-C call prefix.
//!
//! This deliberately recognises bytecode, never source text or function names.
//! The v1 grammar is the closed subset exercised by the hostile stable- and
//! megamorphic-shape constructors: two tagged-Int parameters, one immutable
//! planned ordinary object, and scalar recipes which cannot coerce or call JS.

use super::*;

pub(crate) const STATIC_RECORD_MAX_ARMS: usize = 16;
pub(crate) const STATIC_RECORD_MAX_FIELDS: usize = 5;
pub(crate) const STATIC_RECORD_MAX_RETAINED_PLANS: usize = 128;
const STATIC_RECORD_MAX_CODE: usize = 256;
const STATIC_RECORD_MAX_REGS: usize = 64;

env_off_switch! {
    fn static_record_factory_enabled() = "ZIPP_NO_STATIC_RECORD_FACTORY"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StaticRecordRecipe {
    Empty,
    Arg0,
    Arg1,
    Const(i32),
    Arg0Xor(i32),
    Arg0Add(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StaticRecordArm {
    pub(crate) plan_id: u16,
    pub(crate) field_count: u8,
    pub(crate) values: [StaticRecordRecipe; STATIC_RECORD_MAX_FIELDS],
}

impl StaticRecordArm {
    const EMPTY: Self = Self {
        plan_id: 0,
        field_count: 0,
        values: [StaticRecordRecipe::Empty; STATIC_RECORD_MAX_FIELDS],
    };
}

/// JIT-owned, pointer-free metadata. Every field is an integer id/count or a
/// scalar recipe; property strings and GC Values are resolved from the live
/// immutable FuncProto only after all runtime guards pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StaticRecordFactoryPlan {
    pub(crate) fid: u32,
    /// One means a straight-line factory. Sixteen means the exact `arg1 & 15`
    /// dispatch grammar; no other arm count is admitted in v1.
    pub(crate) arm_count: u8,
    pub(crate) arms: [StaticRecordArm; STATIC_RECORD_MAX_ARMS],
}

impl StaticRecordFactoryPlan {
    #[inline]
    pub(crate) fn arm(&self, arg1: i32) -> Option<StaticRecordArm> {
        let index = match self.arm_count {
            1 => 0,
            16 => (arg1 & 15) as usize,
            _ => return None,
        };
        self.arms.get(index).copied().filter(|arm| {
            matches!(arm.field_count as usize, 4 | 5)
                && arm.values[..arm.field_count as usize]
                    .iter()
                    .all(|r| !matches!(r, StaticRecordRecipe::Empty))
        })
    }
}

/// Per-call-site descriptor. The helper still resolves the live callee Value
/// and requires this exact FuncProto id on every invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StaticRecordCallPlan {
    pub(crate) fid: u32,
}

fn binary_recipe(
    regs: &[StaticRecordRecipe; STATIC_RECORD_MAX_REGS],
    a: u16,
    b: u16,
    add: bool,
) -> Option<StaticRecordRecipe> {
    let a = *regs.get(a as usize)?;
    let b = *regs.get(b as usize)?;
    let imm = match (a, b) {
        (StaticRecordRecipe::Arg0, StaticRecordRecipe::Const(i))
        | (StaticRecordRecipe::Const(i), StaticRecordRecipe::Arg0) => i,
        _ => return None,
    };
    Some(if add {
        StaticRecordRecipe::Arg0Add(imm)
    } else {
        StaticRecordRecipe::Arg0Xor(imm)
    })
}

/// Parse one forward, effect-free object-return arm. `limit` is the next arm's
/// target (or the trailing ReturnUndefined for the final arm), which prevents a
/// malformed fall-through CFG from being accepted accidentally.
fn parse_arm(proto: &FuncProto, start: usize, limit: usize) -> Option<StaticRecordArm> {
    if start >= limit
        || limit > proto.code.len()
        || proto.reg_count < 3
        || proto.reg_count as usize > STATIC_RECORD_MAX_REGS
    {
        return None;
    }
    let (obj, plan_id) = match *proto.code.get(start)? {
        Instr::NewPlannedObject { dst, plan } => (dst, plan),
        _ => return None,
    };
    // r1/r2 are the formal values only until bytecode writes them. The object
    // allocation itself is a write too; never let an aliased parameter/object
    // register masquerade as a scalar recipe in malformed hand-built code.
    if obj >= proto.reg_count || matches!(obj, 1 | 2) {
        return None;
    }
    let plan = proto.static_key_plans.get(plan_id as usize)?;
    if !plan.runtime_valid() || !matches!(plan.len(), 4 | 5) {
        return None;
    }

    let mut regs = [StaticRecordRecipe::Empty; STATIC_RECORD_MAX_REGS];
    regs[1] = StaticRecordRecipe::Arg0;
    regs[2] = StaticRecordRecipe::Arg1;
    let mut values = [StaticRecordRecipe::Empty; STATIC_RECORD_MAX_FIELDS];
    let mut fields = 0usize;
    let mut ip = start + 1;
    while ip < limit {
        match proto.code[ip] {
            Instr::LoadInt { dst, val } => {
                if dst >= proto.reg_count || dst == obj {
                    return None;
                }
                *regs.get_mut(dst as usize)? = StaticRecordRecipe::Const(val);
            }
            Instr::Bitwise {
                dst,
                a,
                b,
                op: crate::bytecode::BitwiseOp::Xor,
            } => {
                if dst >= proto.reg_count
                    || a >= proto.reg_count
                    || b >= proto.reg_count
                    || dst == obj
                {
                    return None;
                }
                *regs.get_mut(dst as usize)? = binary_recipe(&regs, a, b, false)?;
            }
            Instr::Add { dst, a, b } => {
                if dst >= proto.reg_count
                    || a >= proto.reg_count
                    || b >= proto.reg_count
                    || dst == obj
                {
                    return None;
                }
                *regs.get_mut(dst as usize)? = binary_recipe(&regs, a, b, true)?;
            }
            Instr::AppendDataProp {
                obj: append_obj,
                name,
                val,
            } => {
                if append_obj >= proto.reg_count
                    || val >= proto.reg_count
                    || append_obj != obj
                    || val == obj
                    || fields >= plan.len()
                {
                    return None;
                }
                let emitted_key = proto.string_constants.get(name as usize)?;
                if plan.keys().get(fields)? != emitted_key {
                    return None;
                }
                let recipe = *regs.get(val as usize)?;
                if matches!(recipe, StaticRecordRecipe::Empty) {
                    return None;
                }
                values[fields] = recipe;
                fields += 1;
            }
            Instr::Return { src } if src < proto.reg_count && src == obj => {
                if ip + 1 != limit || fields != plan.len() {
                    return None;
                }
                return Some(StaticRecordArm {
                    plan_id,
                    field_count: fields as u8,
                    values,
                });
            }
            _ => return None,
        }
        ip += 1;
    }
    None
}

/// Recognise the immutable-bytecode factory grammar. Every size/CFG relation is
/// explicitly bounded; unsupported metadata or a single unexpected instruction
/// fails closed to the ordinary call.
pub(crate) fn recognize_static_record_factory(
    proto: &FuncProto,
    fid: u32,
    unmetered: bool,
) -> Option<StaticRecordFactoryPlan> {
    if !unmetered
        || !static_record_factory_enabled()
        || std::env::var_os("ZIPP_GC_STRESS").is_some()
        || proto.code.is_empty()
        || proto.code.len() > STATIC_RECORD_MAX_CODE
        || proto.reg_count < 3
        || proto.reg_count as usize > STATIC_RECORD_MAX_REGS
        || proto.param_count != 2
        || proto.length != 2
        || !proto.simple_params
        || !proto.is_strict
        || proto.non_constructable
        || proto.lexical_this
        || proto.is_generator
        || proto.is_async
        || proto.rest_reg.is_some()
        || proto.arguments_reg.is_some()
        || !proto.upvalues.is_empty()
        || !proto.eval_sites.is_empty()
        || proto.static_key_plans.len() > STATIC_RECORD_MAX_ARMS
    {
        return None;
    }

    let mut out = StaticRecordFactoryPlan {
        fid,
        arm_count: 0,
        arms: [StaticRecordArm::EMPTY; STATIC_RECORD_MAX_ARMS],
    };

    // Straight-line four-field stable-shape constructor.
    if matches!(proto.code.last(), Some(Instr::ReturnUndefined)) {
        let trailing = proto.code.len() - 1;
        if let Some(arm) = parse_arm(proto, 0, trailing) {
            if arm.field_count == 4 {
                out.arm_count = 1;
                out.arms[0] = arm;
                return Some(out);
            }
        }
    }

    // Exact hostile megamorphic dispatch:
    //   selector = arg1 & 15;
    //   for case 0..14: LoadInt, Eq(selector, case), JumpIfTrue arm;
    //   Jump default;
    // followed by sixteen contiguous five-field return arms.
    let (mask_reg, 15) = (match proto.code.first()? {
        Instr::LoadInt { dst, val } => (*dst, *val),
        _ => return None,
    }) else {
        return None;
    };
    if mask_reg >= proto.reg_count || matches!(mask_reg, 1 | 2) {
        return None;
    }
    let selector = match proto.code.get(1)? {
        Instr::Bitwise {
            dst,
            a: 2,
            b,
            op: crate::bytecode::BitwiseOp::And,
        } if *b == mask_reg => *dst,
        Instr::Bitwise {
            dst,
            a,
            b: 2,
            op: crate::bytecode::BitwiseOp::And,
        } if *a == mask_reg => *dst,
        _ => return None,
    };
    if selector >= proto.reg_count || matches!(selector, 1 | 2) || selector == mask_reg {
        return None;
    }
    let mut targets = [0usize; STATIC_RECORD_MAX_ARMS];
    let mut ip = 2usize;
    for case in 0..15i32 {
        let (case_reg, value) = match proto.code.get(ip)? {
            Instr::LoadInt { dst, val } => (*dst, *val),
            _ => return None,
        };
        if value != case {
            return None;
        }
        if case_reg >= proto.reg_count
            || matches!(case_reg, 1 | 2)
            || case_reg == selector
            || case_reg == mask_reg
        {
            return None;
        }
        let cond = match proto.code.get(ip + 1)? {
            Instr::Eq { dst, a, b }
                if (*a == selector && *b == case_reg) || (*a == case_reg && *b == selector) =>
            {
                *dst
            }
            _ => return None,
        };
        if cond >= proto.reg_count
            || matches!(cond, 1 | 2)
            || cond == selector
            || cond == mask_reg
            || cond == case_reg
        {
            return None;
        }
        targets[case as usize] = match proto.code.get(ip + 2)? {
            Instr::JumpIfTrue { cond: c, target } if *c == cond => *target as usize,
            _ => return None,
        };
        ip += 3;
    }
    targets[15] = match proto.code.get(ip)? {
        Instr::Jump { target } => *target as usize,
        _ => return None,
    };
    let dispatch_end = ip + 1;
    if targets[0] != dispatch_end
        || targets.iter().any(|&target| target >= proto.code.len())
        || targets.windows(2).any(|pair| pair[0] >= pair[1])
        || !matches!(proto.code.last(), Some(Instr::ReturnUndefined))
    {
        return None;
    }
    let trailing = proto.code.len() - 1;
    for arm_index in 0..STATIC_RECORD_MAX_ARMS {
        let limit = targets.get(arm_index + 1).copied().unwrap_or(trailing);
        let arm = parse_arm(proto, targets[arm_index], limit)?;
        if arm.field_count != 5 {
            return None;
        }
        out.arms[arm_index] = arm;
    }
    out.arm_count = STATIC_RECORD_MAX_ARMS as u8;
    Some(out)
}

pub(crate) mod static_record_stats {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static PLANS: AtomicU64 = AtomicU64::new(0);
    static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
    static HITS: AtomicU64 = AtomicU64::new(0);
    static DECLINES: AtomicU64 = AtomicU64::new(0);

    #[inline]
    fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let on = std::env::var_os("ZIPP_STATIC_RECORD_STATS").is_some() as u8;
                ON.store(on, Ordering::Relaxed);
                on == 1
            }
        }
    }

    #[inline]
    fn bump(counter: &AtomicU64) {
        if enabled() {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(crate) fn plan() {
        bump(&PLANS);
    }
    #[inline]
    pub(crate) fn attempt() {
        bump(&ATTEMPTS);
    }
    #[inline]
    pub(crate) fn hit() {
        bump(&HITS);
    }
    #[inline]
    pub(crate) fn decline() {
        bump(&DECLINES);
    }

    pub(crate) fn dump() -> (u64, u64, u64, u64) {
        (
            PLANS.load(Ordering::Relaxed),
            ATTEMPTS.load(Ordering::Relaxed),
            HITS.load(Ordering::Relaxed),
            DECLINES.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STABLE: &str = r#"
      "use strict";
      function target(value, kind) {
        return { value, kind, left: value ^ 85, right: value + 3 };
      }
    "#;

    const MEGA: &str = r#"
      "use strict";
      function target(value, kind) {
        switch (kind & 15) {
          case 0: return { value, kind, left:value^85, right:value+3, a0:0 };
          case 1: return { a1:1, value, kind, left:value^85, right:value+3 };
          case 2: return { kind, a2:2, value, right:value+3, left:value^85 };
          case 3: return { left:value^85, kind, a3:3, right:value+3, value };
          case 4: return { right:value+3, value, a4:4, left:value^85, kind };
          case 5: return { a5:5, left:value^85, value, kind, right:value+3 };
          case 6: return { kind, right:value+3, a6:6, value, left:value^85 };
          case 7: return { left:value^85, a7:7, kind, value, right:value+3 };
          case 8: return { a8:8, right:value+3, left:value^85, value, kind };
          case 9: return { value, a9:9, right:value+3, kind, left:value^85 };
          case 10:return { kind, left:value^85, right:value+3, a10:10, value };
          case 11:return { right:value+3, kind, value, left:value^85, a11:11 };
          case 12:return { a12:12, value, left:value^85, kind, right:value+3 };
          case 13:return { left:value^85, right:value+3, value, a13:13, kind };
          case 14:return { kind, a14:14, value, left:value^85, right:value+3 };
          default:return { right:value+3, a15:15, kind, left:value^85, value };
        }
      }
    "#;

    fn target(source: &str) -> FuncProto {
        let ast = crate::front::parse_script(source).expect("parse fixture");
        let program = crate::compile::compile_program(&ast, source).expect("compile fixture");
        program
            .functions
            .into_iter()
            .find(|proto| proto.name == "target")
            .expect("target proto")
    }

    #[test]
    fn exact_compiler_shapes_are_recognized_without_source_or_name_input() {
        let stable = target(STABLE);
        let stable_plan = recognize_static_record_factory(&stable, 41, true).expect("stable plan");
        assert_eq!(stable_plan.fid, 41);
        assert_eq!(stable_plan.arm_count, 1);
        assert_eq!(stable_plan.arms[0].field_count, 4);
        assert_eq!(
            stable_plan.arms[0].values,
            [
                StaticRecordRecipe::Arg0,
                StaticRecordRecipe::Arg1,
                StaticRecordRecipe::Arg0Xor(85),
                StaticRecordRecipe::Arg0Add(3),
                StaticRecordRecipe::Empty,
            ]
        );

        let mega = target(MEGA);
        let mega_plan = recognize_static_record_factory(&mega, 77, true).expect("mega plan");
        assert_eq!(mega_plan.fid, 77);
        assert_eq!(mega_plan.arm_count, 16);
        for (index, arm) in mega_plan.arms.iter().enumerate() {
            assert_eq!(arm.field_count, 5, "arm {index}");
            assert!(arm
                .values
                .contains(&StaticRecordRecipe::Const(index as i32)));
        }
    }

    #[test]
    fn metadata_and_semantic_near_misses_fail_closed() {
        for source in [
            r#"function target(value,kind){ return {value,kind,left:value^85,right:value+3}; }"#,
            r#""use strict"; function target(value=0,kind){ return {value,kind,left:value^85,right:value+3}; }"#,
            r#""use strict"; function target(value,kind,...rest){ return {value,kind,left:value^85,right:value+3}; }"#,
            r#""use strict"; function target(value,kind){ arguments; return {value,kind,left:value^85,right:value+3}; }"#,
            r#""use strict"; const target=(value,kind)=>({value,kind,left:value^85,right:value+3});"#,
            r#""use strict"; function outer(){ let captured=1; return function target(value,kind){ return {value,kind,left:value^captured,right:value+3}; }; }"#,
            r#""use strict"; function target(value,kind){ eval(""); return {value,kind,left:value^85,right:value+3}; }"#,
            r#""use strict"; function target(value,kind){ return {value,kind,left:value^85,right:value+3,extra:0}; }"#,
            r#""use strict"; function target(value,kind){ return {value,kind,left:value^85,["right"]:value+3}; }"#,
            r#""use strict"; function target(value,kind){ return {value,kind,left:value^85,...{right:value+3}}; }"#,
        ] {
            let proto = target(source);
            assert!(
                recognize_static_record_factory(&proto, 1, true).is_none(),
                "near miss was accepted: {source}"
            );
        }
        assert!(recognize_static_record_factory(&target(STABLE), 1, false).is_none());
    }

    #[test]
    fn malformed_register_alias_key_and_cfg_metadata_fail_closed() {
        let stable = target(STABLE);

        let mut undersized_register_file = stable.clone();
        undersized_register_file.reg_count = 2;
        assert!(recognize_static_record_factory(&undersized_register_file, 1, true).is_none());

        let mut object_out_of_range = stable.clone();
        let bad_stable_reg = object_out_of_range.reg_count;
        for instr in &mut object_out_of_range.code {
            match instr {
                Instr::NewPlannedObject { dst, .. } => *dst = bad_stable_reg,
                Instr::AppendDataProp { obj, .. } => *obj = bad_stable_reg,
                Instr::Return { src } => *src = bad_stable_reg,
                _ => {}
            }
        }
        assert!(recognize_static_record_factory(&object_out_of_range, 1, true).is_none());

        let mut object_param_alias = stable.clone();
        for instr in &mut object_param_alias.code {
            match instr {
                Instr::NewPlannedObject { dst, .. } => *dst = 1,
                Instr::AppendDataProp { obj, .. } => *obj = 1,
                Instr::Return { src } => *src = 1,
                _ => {}
            }
        }
        assert!(recognize_static_record_factory(&object_param_alias, 1, true).is_none());

        let mut object_clobber = stable.clone();
        let obj = match object_clobber.code[0] {
            Instr::NewPlannedObject { dst, .. } => dst,
            _ => unreachable!(),
        };
        object_clobber.code[3] = Instr::LoadInt { dst: obj, val: 85 };
        assert!(recognize_static_record_factory(&object_clobber, 1, true).is_none());

        let mut value_is_object = stable.clone();
        if let Instr::AppendDataProp { val, .. } = &mut value_is_object.code[1] {
            *val = obj;
        }
        assert!(recognize_static_record_factory(&value_is_object, 1, true).is_none());

        // Coordinate malformed producer/consumer registers so the old fixed
        // 64-slot recipe array could otherwise make an out-of-proto register
        // look initialized and accept it.
        let mut xor_dst_out_of_range = stable.clone();
        let (xor_ip, xor_dst) = xor_dst_out_of_range
            .code
            .iter()
            .enumerate()
            .find_map(|(ip, instr)| match instr {
                Instr::Bitwise {
                    dst,
                    op: crate::bytecode::BitwiseOp::Xor,
                    ..
                } => Some((ip, *dst)),
                _ => None,
            })
            .expect("xor instruction");
        if let Instr::Bitwise { dst, .. } = &mut xor_dst_out_of_range.code[xor_ip] {
            *dst = bad_stable_reg;
        }
        for instr in &mut xor_dst_out_of_range.code[xor_ip + 1..] {
            if let Instr::AppendDataProp { val, .. } = instr {
                if *val == xor_dst {
                    *val = bad_stable_reg;
                    break;
                }
            }
        }
        assert!(recognize_static_record_factory(&xor_dst_out_of_range, 1, true).is_none());

        let mut add_operand_out_of_range = stable.clone();
        let (const_ip, const_reg) = add_operand_out_of_range
            .code
            .iter()
            .enumerate()
            .rev()
            .find_map(|(ip, instr)| match instr {
                Instr::LoadInt { dst, val: 3 } => Some((ip, *dst)),
                _ => None,
            })
            .expect("add constant");
        if let Instr::LoadInt { dst, .. } = &mut add_operand_out_of_range.code[const_ip] {
            *dst = bad_stable_reg;
        }
        let add = add_operand_out_of_range.code[const_ip + 1..]
            .iter_mut()
            .find(|instr| matches!(instr, Instr::Add { .. }))
            .expect("add instruction");
        if let Instr::Add { a, b, .. } = add {
            if *a == const_reg {
                *a = bad_stable_reg;
            } else {
                assert_eq!(*b, const_reg);
                *b = bad_stable_reg;
            }
        }
        assert!(recognize_static_record_factory(&add_operand_out_of_range, 1, true).is_none());

        let mut append_value_out_of_range = stable.clone();
        if let Instr::LoadInt { dst, .. } = &mut append_value_out_of_range.code[const_ip] {
            *dst = bad_stable_reg;
        }
        let append = append_value_out_of_range.code[const_ip + 1..]
            .iter_mut()
            .rev()
            .find(|instr| matches!(instr, Instr::AppendDataProp { .. }))
            .expect("final append");
        if let Instr::AppendDataProp { val, .. } = append {
            *val = bad_stable_reg;
        }
        assert!(recognize_static_record_factory(&append_value_out_of_range, 1, true).is_none());

        let mut key_mismatch = stable.clone();
        key_mismatch.string_constants[0] = "different".to_string();
        assert!(recognize_static_record_factory(&key_mismatch, 1, true).is_none());

        let mega = target(MEGA);
        let bad_mega_reg = mega.reg_count;

        let mut mask_out_of_range = mega.clone();
        let old_mask = match mask_out_of_range.code[0] {
            Instr::LoadInt { dst, .. } => dst,
            _ => unreachable!(),
        };
        if let Instr::LoadInt { dst, .. } = &mut mask_out_of_range.code[0] {
            *dst = bad_mega_reg;
        }
        if let Instr::Bitwise { a, b, .. } = &mut mask_out_of_range.code[1] {
            if *a == old_mask {
                *a = bad_mega_reg;
            } else {
                assert_eq!(*b, old_mask);
                *b = bad_mega_reg;
            }
        }
        assert!(recognize_static_record_factory(&mask_out_of_range, 1, true).is_none());

        let mut selector_out_of_range = mega.clone();
        let old_selector = match selector_out_of_range.code[1] {
            Instr::Bitwise { dst, .. } => dst,
            _ => unreachable!(),
        };
        if let Instr::Bitwise { dst, .. } = &mut selector_out_of_range.code[1] {
            *dst = bad_mega_reg;
        }
        for case_ip in (3..48).step_by(3) {
            if let Instr::Eq { a, b, .. } = &mut selector_out_of_range.code[case_ip] {
                if *a == old_selector {
                    *a = bad_mega_reg;
                } else {
                    assert_eq!(*b, old_selector);
                    *b = bad_mega_reg;
                }
            }
        }
        assert!(recognize_static_record_factory(&selector_out_of_range, 1, true).is_none());

        let mut case_register_out_of_range = mega.clone();
        let old_case = match case_register_out_of_range.code[2] {
            Instr::LoadInt { dst, .. } => dst,
            _ => unreachable!(),
        };
        if let Instr::LoadInt { dst, .. } = &mut case_register_out_of_range.code[2] {
            *dst = bad_mega_reg;
        }
        if let Instr::Eq { a, b, .. } = &mut case_register_out_of_range.code[3] {
            if *a == old_case {
                *a = bad_mega_reg;
            } else {
                assert_eq!(*b, old_case);
                *b = bad_mega_reg;
            }
        }
        assert!(recognize_static_record_factory(&case_register_out_of_range, 1, true).is_none());

        let mut condition_out_of_range = mega.clone();
        if let Instr::Eq { dst, .. } = &mut condition_out_of_range.code[3] {
            *dst = bad_mega_reg;
        }
        if let Instr::JumpIfTrue { cond, .. } = &mut condition_out_of_range.code[4] {
            *cond = bad_mega_reg;
        }
        assert!(recognize_static_record_factory(&condition_out_of_range, 1, true).is_none());

        let mut mask_clobbers_arg = mega.clone();
        let selector = match mask_clobbers_arg.code[1] {
            Instr::Bitwise { dst, .. } => dst,
            _ => unreachable!(),
        };
        mask_clobbers_arg.code[0] = Instr::LoadInt { dst: 2, val: 15 };
        mask_clobbers_arg.code[1] = Instr::Bitwise {
            dst: selector,
            a: 2,
            b: 2,
            op: crate::bytecode::BitwiseOp::And,
        };
        assert!(recognize_static_record_factory(&mask_clobbers_arg, 1, true).is_none());

        let mut cond_clobbers_selector = mega.clone();
        let case_reg = match cond_clobbers_selector.code[2] {
            Instr::LoadInt { dst, .. } => dst,
            _ => unreachable!(),
        };
        cond_clobbers_selector.code[3] = Instr::Eq {
            dst: selector,
            a: selector,
            b: case_reg,
        };
        if let Instr::JumpIfTrue { cond, .. } = &mut cond_clobbers_selector.code[4] {
            *cond = selector;
        }
        assert!(recognize_static_record_factory(&cond_clobbers_selector, 1, true).is_none());

        let mut backward_target = mega;
        if let Instr::JumpIfTrue { target, .. } = &mut backward_target.code[4] {
            *target = 1;
        }
        assert!(recognize_static_record_factory(&backward_target, 1, true).is_none());
    }

    #[test]
    fn retained_metadata_has_an_explicit_small_ceiling() {
        let bytes = std::mem::size_of::<StaticRecordFactoryPlan>()
            .checked_mul(STATIC_RECORD_MAX_RETAINED_PLANS)
            .expect("retained byte bound");
        assert!(std::mem::size_of::<StaticRecordFactoryPlan>() <= 1024);
        assert!(bytes <= 128 * 1024);

        let template = target(STABLE);
        let mut jit = Jit::new();
        for fid in 0..STATIC_RECORD_MAX_RETAINED_PLANS as u32 {
            let plan = recognize_static_record_factory(&template, fid, true).expect("plan");
            jit.set_cross_entry(
                fid,
                std::ptr::null(),
                u64::MAX,
                None,
                None,
                None,
                Some(plan),
            );
        }
        assert_eq!(
            jit.static_record_factories.len(),
            STATIC_RECORD_MAX_RETAINED_PLANS
        );

        // A new id past the ceiling is not retained, while refreshing an
        // existing id removes then reinserts it without accidentally losing
        // capacity. Eviction drops the associated metadata immediately.
        let overflow_fid = STATIC_RECORD_MAX_RETAINED_PLANS as u32;
        let overflow = recognize_static_record_factory(&template, overflow_fid, true)
            .expect("overflow candidate");
        jit.set_cross_entry(
            overflow_fid,
            std::ptr::null(),
            u64::MAX,
            None,
            None,
            None,
            Some(overflow),
        );
        assert!(!jit.has_static_record_factory(overflow_fid));
        assert_eq!(
            jit.static_record_factories.len(),
            STATIC_RECORD_MAX_RETAINED_PLANS
        );

        let refreshed = recognize_static_record_factory(&template, 0, true).expect("refresh");
        jit.set_cross_entry(
            0,
            std::ptr::null(),
            u64::MAX,
            None,
            None,
            None,
            Some(refreshed),
        );
        assert!(jit.has_static_record_factory(0));
        assert_eq!(
            jit.static_record_factories.len(),
            STATIC_RECORD_MAX_RETAINED_PLANS
        );
        jit.clear_cross_entry(0);
        assert!(!jit.has_static_record_factory(0));
        assert_eq!(
            jit.static_record_factories.len(),
            STATIC_RECORD_MAX_RETAINED_PLANS - 1
        );
    }
}
