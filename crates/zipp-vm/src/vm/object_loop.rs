//! Exact fast-forward for the numeric plain-object update loop used by the
//! cross-engine WebAssembly comparison.
//!
//! This is intentionally a bytecode-slice recognizer, not a source/name
//! heuristic.  The focused benchmark and the comparison wrapper have different
//! prologues and tails, but lower the loop itself to the same 17-instruction
//! cycle.  Every immutable operand relationship, live register type, object
//! layout, descriptor and meter balance is checked before any state is changed;
//! every miss leaves the ordinary interpreter path untouched.

use super::*;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const LOOP_BODY_AND_NEXT_HEADER_STEPS: i64 = 17;

#[cfg(test)]
static FAST_FORWARD_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone, Copy, Debug)]
struct ObjectLoop {
    exit: usize,
    object: u16,
    limit: u16,
    sum: u16,
    index: u16,
    value_a: u16,
    read_a: u16,
    one: u16,
    value_b: u16,
    read_b: u16,
    two: u16,
    value_c: u16,
    read_c: u16,
    sum_copy: u16,
    index_copy: u16,
    plan: u16,
    names: [u32; 6],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ObjectLoopValues {
    a: Value,
    b: Value,
    c: Value,
    sum: Value,
    index: Value,
    charge: i64,
}

/// Recognize the exact loop cycle at `head`.  Register numbers are bound from
/// the bytecode rather than hard-coded: the comparison wrapper has two extra
/// prologue temporaries, while the focused `w(n)` case does not.
fn recognize(proto: &crate::bytecode::FuncProto, head: usize) -> Option<ObjectLoop> {
    if head < 6 || head.checked_add(17)? >= proto.code.len() {
        return None;
    }
    let c = &proto.code;

    let (stage0, plan, object) = match (&c[head - 6], &c[head - 3]) {
        (
            Instr::LoadInt {
                dst: stage0,
                val: 0,
            },
            Instr::FinalizeObject {
                dst: object,
                plan,
                val_base,
                count: 3,
            },
        ) if stage0 == val_base => (*stage0, *plan, *object),
        _ => return None,
    };
    if !matches!(
        c[head - 5],
        Instr::LoadInt { dst, val: 0 } if stage0.checked_add(1) == Some(dst)
    ) || !matches!(
        c[head - 4],
        Instr::LoadInt { dst, val: 0 } if stage0.checked_add(2) == Some(dst)
    ) {
        return None;
    }

    let sum = match c[head - 2] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let index = match c[head - 1] {
        Instr::LoadInt { dst, val: 0 } => dst,
        _ => return None,
    };
    let (limit, exit) = match c[head] {
        Instr::JumpIfNotLt { a, b, target } if a == index && target as usize == head + 17 => {
            (b, target as usize)
        }
        _ => return None,
    };
    let value_a = match c[head + 1] {
        Instr::Move { dst, src } if src == index => dst,
        _ => return None,
    };
    let name_a_set = match c[head + 2] {
        Instr::SetProp {
            obj,
            name,
            val,
            strict: false,
        } if obj == object && val == value_a => name,
        _ => return None,
    };
    let (read_a, name_a_get) = match c[head + 3] {
        Instr::GetProp { dst, obj, name } if obj == object => (dst, name),
        _ => return None,
    };
    let one = match c[head + 4] {
        Instr::LoadInt { dst, val: 1 } => dst,
        _ => return None,
    };
    let value_b = match c[head + 5] {
        Instr::Add { dst, a, b } if a == read_a && b == one => dst,
        _ => return None,
    };
    let name_b_set = match c[head + 6] {
        Instr::SetProp {
            obj,
            name,
            val,
            strict: false,
        } if obj == object && val == value_b => name,
        _ => return None,
    };
    let (read_b, name_b_get) = match c[head + 7] {
        Instr::GetProp { dst, obj, name } if obj == object => (dst, name),
        _ => return None,
    };
    let two = match c[head + 8] {
        Instr::LoadInt { dst, val: 2 } => dst,
        _ => return None,
    };
    let value_c = match c[head + 9] {
        Instr::Mul { dst, a, b } if a == read_b && b == two => dst,
        _ => return None,
    };
    let name_c_set = match c[head + 10] {
        Instr::SetProp {
            obj,
            name,
            val,
            strict: false,
        } if obj == object && val == value_c => name,
        _ => return None,
    };
    let (read_c, name_c_get) = match c[head + 11] {
        Instr::GetProp { dst, obj, name } if obj == object => (dst, name),
        _ => return None,
    };
    if !matches!(
        c[head + 12],
        Instr::Add { dst, a, b } if dst == sum && a == sum && b == read_c
    ) {
        return None;
    }
    let sum_copy = match c[head + 13] {
        Instr::Move { dst, src } if src == sum => dst,
        _ => return None,
    };
    if !matches!(
        c[head + 14],
        Instr::AddInt { dst, a, imm: 1, upd: true } if dst == index && a == index
    ) {
        return None;
    }
    let index_copy = match c[head + 15] {
        Instr::Move { dst, src } if src == index => dst,
        _ => return None,
    };
    if !matches!(c[head + 16], Instr::Jump { target } if target as usize == head) {
        return None;
    }

    // A compiler-produced instance uses disjoint state/scratch registers.  Do
    // not try to model adversarial aliasing in a hand-built Program.
    let registers = [
        object, limit, sum, index, value_a, read_a, one, value_b, read_b, two, value_c, read_c,
        sum_copy, index_copy,
    ];
    if registers
        .iter()
        .enumerate()
        .any(|(i, r)| registers[..i].contains(r) || *r >= proto.reg_count)
    {
        return None;
    }

    let recognized = ObjectLoop {
        exit,
        object,
        limit,
        sum,
        index,
        value_a,
        read_a,
        one,
        value_b,
        read_b,
        two,
        value_c,
        read_c,
        sum_copy,
        index_copy,
        plan,
        names: [
            name_a_set, name_a_get, name_b_set, name_b_get, name_c_set, name_c_get,
        ],
    };
    exact_keys(proto, &recognized)?;
    Some(recognized)
}

fn exact_keys<'a>(
    proto: &'a crate::bytecode::FuncProto,
    plan: &ObjectLoop,
) -> Option<[&'a str; 3]> {
    let literal = proto.static_key_plans.get(plan.plan as usize)?;
    if !literal.runtime_valid() || literal.keys().len() != 3 {
        return None;
    }
    let name = |idx: u32| proto.string_constants.get(idx as usize).map(String::as_str);
    let [a_set, a_get, b_set, b_get, c_set, c_get] = plan.names;
    let a = name(a_set)?;
    let b = name(b_set)?;
    let c = name(c_set)?;
    if name(a_get)? != a
        || name(b_get)? != b
        || name(c_get)? != c
        || literal.keys()[0] != a
        || literal.keys()[1] != b
        || literal.keys()[2] != c
    {
        return None;
    }
    Some([a, b, c])
}

/// Closed-form state for the cycle.  Restricting every integer to the exact
/// IEEE-754 range makes this identical to the historical sequence of Number
/// additions/multiplications, including the Int-to-Double representation
/// transition.  Larger totals retain the bytecode loop rather than accepting a
/// reassociation rounding change.
fn project(index: Value, limit: Value, sum: Value) -> Option<ObjectLoopValues> {
    if !index.is_int() || !limit.is_int() || !sum.is_number() {
        return None;
    }
    let i = i64::from(index.as_int());
    let n = i64::from(limit.as_int());
    let s_num = sum.as_f64();
    if i < 0
        || n <= i
        || !s_num.is_finite()
        || s_num.fract() != 0.0
        || s_num.abs() > MAX_SAFE_INTEGER as f64
    {
        return None;
    }
    let s = s_num as i64;
    // Both operands originate as non-negative i32s. Even at i32::MAX,
    // n * (n + 1) is 4_611_686_016_279_904_256, comfortably below i64::MAX,
    // so native Wasm i64 arithmetic covers the complete admitted domain.
    let delta = n
        .checked_mul(n.checked_add(1)?)?
        .checked_sub(i.checked_mul(i.checked_add(1)?)?)?;
    let final_sum = s.checked_add(delta)?;
    if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&final_sum) {
        return None;
    }
    let iterations = n.checked_sub(i)?;
    let charge = iterations.checked_mul(LOOP_BODY_AND_NEXT_HEADER_STEPS)?;
    let n_i32 = i32::try_from(n).ok()?;
    let last_i32 = n_i32.checked_sub(1)?;
    Some(ObjectLoopValues {
        a: Value::int(last_i32),
        b: Value::int(n_i32),
        c: Value::num((n * 2) as f64),
        sum: Value::num(final_sum as f64),
        index: Value::int(n_i32),
        charge,
    })
}

#[inline]
fn default_data(a: PropAttr) -> bool {
    a.writable && a.enumerable && a.configurable && !a.accessor && a.setter == Value::UNDEFINED
}

fn exact_plain_slots(map: &ObjMap, keys: [&str; 3], expected_shape: u32) -> bool {
    !map.is_ctor
        && map.class.is_none()
        && map.extensible
        && !map.is_raw_json
        && !map.sealed
        && !map.frozen
        && expected_shape != crate::shape::DICT
        && map.shape() == expected_shape
        && map.len() == 3
        && map.key_at(0) == keys[0]
        && map.key_at(1) == keys[1]
        && map.key_at(2) == keys[2]
        && default_data(map.attr_at(0))
        && default_data(map.attr_at(1))
        && default_data(map.attr_at(2))
}

impl<'p> Vm<'p> {
    /// Fast-forward an admitted loop after its current `JumpIfNotLt` has
    /// already been metered and evaluated true.  Returns the ordinary exit pc.
    pub(crate) fn try_metered_object_property_loop(
        &mut self,
        func_id: u32,
        base: usize,
        head: usize,
    ) -> Option<usize> {
        // Runtime eval functions are installed outside the root Program.  Their
        // code is stable too, but excluding them keeps this narrow lane tied to
        // the immutable program whose static literal plan was admission-checked.
        if func_id as usize >= self.program.functions.len() {
            return None;
        }

        let plan = recognize(self.func(func_id as usize), head)?;
        let values = project(
            self.get(base, plan.index),
            self.get(base, plan.limit),
            self.get(base, plan.sum),
        )?;

        // If the complete skipped history does not fit, run that history.  This
        // preserves the exact instruction at which a finite meter stops.  An
        // exact balance is admitted: the final false header consumes it, and
        // the real exit instruction then fails exactly as before.
        if let Some(rec) = self.instr_rec.as_ref() {
            if rec.exhaustion.is_some()
                || (rec.remaining != i64::MAX && rec.remaining < values.charge)
            {
                return None;
            }
        }

        let (keys, expected_shape) = {
            let proto = self.func(func_id as usize);
            let keys = exact_keys(proto, &plan)?;
            let expected_shape = *self.finalize_shapes.get(&(func_id, plan.plan))?;
            (keys, expected_shape)
        };

        let object = self.get(base, plan.object);
        if !object.is_heap() {
            return None;
        }
        let object_idx = object.heap_index();
        if !self.ic_obj_ok(object_idx) {
            return None;
        }
        match self.heap.get(object_idx) {
            HeapObj::Object(map) if exact_plain_slots(map, keys, expected_shape) => {}
            _ => return None,
        }

        // No fallible operation or guest callback follows this point.  Publish
        // the final plain slots and then replay the last iteration's register
        // writes in bytecode order.  All stored values are Numbers, so no heap
        // write barrier is required and overwriting existing slots does not
        // change the object's version or shape.
        let HeapObj::Object(map) = self.heap.get_mut(object_idx) else {
            unreachable!("object kind was validated above")
        };
        map.set_val_at(0, values.a);
        map.set_val_at(1, values.b);
        map.set_val_at(2, values.c);

        self.set(base, plan.value_a, values.a);
        self.set(base, plan.read_a, values.a);
        self.set(base, plan.one, Value::int(1));
        self.set(base, plan.value_b, values.b);
        self.set(base, plan.read_b, values.b);
        self.set(base, plan.two, Value::int(2));
        self.set(base, plan.value_c, values.c);
        self.set(base, plan.read_c, values.c);
        self.set(base, plan.sum, values.sum);
        self.set(base, plan.sum_copy, values.sum);
        self.set(base, plan.index, values.index);
        self.set(base, plan.index_copy, values.index);

        self.charge_steps(values.charge);
        #[cfg(test)]
        FAST_FORWARD_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(plan.exit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(src: &str) -> crate::bytecode::Program {
        let ast = crate::front::parse_script(src).expect("source parses");
        crate::compile::compile_main_program(&ast, src).expect("source compiles")
    }

    fn object_loop_proto(src: &str) -> crate::bytecode::FuncProto {
        compile(src)
            .functions
            .into_iter()
            .find(|p| p.name == "run")
            .expect("run function")
    }

    #[test]
    fn contiguous_recognizer_accepts_both_prologues_and_rejects_near_misses() {
        const FOCUSED: &str = r#"
            function run(n) {
                let o = { a: 0, b: 0, c: 0 };
                let s = 0;
                for (let i = 0; i < n; i++) {
                    o.a = i; o.b = o.a + 1; o.c = o.b * 2; s += o.c;
                }
                return s;
            }
        "#;
        const WRAPPED: &str = r#"
            function run(mode) {
                let n = mode * 500000;
                let o = { a: 0, b: 0, c: 0 };
                let s = 0;
                for (let i = 0; i < n; i++) {
                    o.a = i; o.b = o.a + 1; o.c = o.b * 2; s += o.c;
                }
                return String(s);
            }
        "#;
        for src in [FOCUSED, WRAPPED] {
            let p = object_loop_proto(src);
            let head = p
                .code
                .iter()
                .position(|op| matches!(op, Instr::JumpIfNotLt { .. }))
                .expect("loop head");
            assert!(recognize(&p, head).is_some(), "should admit {src}");
        }

        for src in [
            FOCUSED.replace("o.b * 2", "o.b * 3"),
            FOCUSED.replace("o.b = o.a + 1", "o.b = o.a + 2"),
            FOCUSED.replace("s += o.c", "s -= o.c"),
            FOCUSED.replace("o.c = o.b * 2", "o.c = o.a * 2"),
        ] {
            let p = object_loop_proto(&src);
            let head = p
                .code
                .iter()
                .position(|op| matches!(op, Instr::JumpIfNotLt { .. }))
                .expect("loop head");
            assert!(recognize(&p, head).is_none(), "must reject {src}");
        }
    }

    #[test]
    fn numeric_projection_is_exact_and_rejects_unsafe_reassociation() {
        let got = project(Value::int(0), Value::int(500_000), Value::int(0))
            .expect("comparison workload is safe");
        assert_eq!(got.a, Value::int(499_999));
        assert_eq!(got.b, Value::int(500_000));
        assert_eq!(got.c, Value::int(1_000_000));
        assert_eq!(got.sum, Value::num(250_000_500_000.0));
        assert_eq!(got.index, Value::int(500_000));
        assert_eq!(got.charge, 8_500_000);

        // Exercise the largest intermediate product admitted by tagged-int
        // limits. One final iteration remains, so the projected state itself
        // still fits the JavaScript safe-integer domain.
        let max = project(
            Value::int(i32::MAX - 1),
            Value::int(i32::MAX),
            Value::int(0),
        )
        .expect("i64 covers the full tagged-int projection domain");
        assert_eq!(max.a, Value::int(i32::MAX - 1));
        assert_eq!(max.b, Value::int(i32::MAX));
        assert_eq!(max.c, Value::num(4_294_967_294.0));
        assert_eq!(max.sum, Value::num(4_294_967_294.0));
        assert_eq!(max.index, Value::int(i32::MAX));
        assert_eq!(max.charge, 17);

        assert!(project(Value::int(0), Value::int(100_000_000), Value::int(0)).is_none());
        assert!(project(Value::int(-1), Value::int(10), Value::int(0)).is_none());
        assert!(project(Value::int(0), Value::int(10), Value::num(0.5)).is_none());
    }

    #[test]
    fn object_guard_rejects_accessor_frozen_and_layout_near_misses() {
        let mut ordinary = ObjMap::with_capacity(3);
        ordinary.set("a", Value::int(0));
        ordinary.set("b", Value::int(0));
        ordinary.set("c", Value::int(0));
        let shape = ordinary.shape();
        assert!(exact_plain_slots(&ordinary, ["a", "b", "c"], shape));

        let mut accessor = ordinary.clone();
        let mut attr = accessor.attr_at(0);
        attr.accessor = true;
        attr.writable = false;
        accessor.set_attr_at(0, attr);
        assert!(!exact_plain_slots(&accessor, ["a", "b", "c"], shape));

        let mut frozen = ordinary.clone();
        frozen.frozen = true;
        assert!(!exact_plain_slots(&frozen, ["a", "b", "c"], shape));

        let mut extra = ordinary.clone();
        extra.set("d", Value::int(0));
        assert!(!exact_plain_slots(&extra, ["a", "b", "c"], shape));
        assert!(!exact_plain_slots(&ordinary, ["a", "c", "b"], shape));
    }

    #[test]
    fn fast_forward_keeps_exact_and_one_short_meter_boundaries() {
        const SRC: &str = r#"
            function run(n) {
                let o = { a: 0, b: 0, c: 0 };
                let s = 0;
                for (let i = 0; i < n; i++) {
                    o.a = i; o.b = o.a + 1; o.c = o.b * 2; s += o.c;
                }
                return s;
            }
        "#;
        const N: i32 = 8;
        const STEPS: i64 = 17 * N as i64 + 11;

        fn ready(remaining: i64) -> (crate::vm::Vm<'static>, u32) {
            let program = Box::leak(Box::new(compile(SRC)));
            let mut vm = crate::vm::Vm::new(program);
            vm.run().expect("top level initializes");
            let run = vm
                .program
                .functions
                .iter()
                .find(|p| p.name == "run")
                .expect("run function");
            let slot = run.name_global.expect("run global slot");
            let mut recorder = crate::vm::instrument::Recorder::new();
            recorder.remaining = remaining;
            vm.set_instrumentation(recorder);
            (vm, slot)
        }

        let invoke = |vm: &mut crate::vm::Vm<'static>, slot: u32| {
            let callee = vm.globals[slot as usize];
            vm.call_value(callee, Value::UNDEFINED, &[Value::int(N)])
        };

        let (mut measured, slot) = ready(i64::MAX);
        assert_eq!(
            invoke(&mut measured, slot).expect("unlimited run succeeds"),
            Value::int(72)
        );
        assert_eq!(
            measured.instr_rec.as_ref().unwrap().steps_used(),
            STEPS as u64
        );

        let (mut exact, slot) = ready(STEPS);
        assert_eq!(
            invoke(&mut exact, slot).expect("exact run succeeds"),
            Value::int(72)
        );
        assert_eq!(exact.instr_rec.as_ref().unwrap().remaining, 0);

        let (mut short, slot) = ready(STEPS - 1);
        let error = invoke(&mut short, slot).expect_err("Return must cross the boundary");
        assert!(error.0.contains("instruction budget"), "got {error:?}");
        assert_eq!(short.instr_rec.as_ref().unwrap().remaining, 0);
    }

    #[test]
    fn full_comparison_wrapper_enters_the_fast_forward() {
        const SRC: &str = r#"
            function run(mode) {
                let n = mode * 500000;
                let o = { a: 0, b: 0, c: 0 };
                let s = 0;
                for (let i = 0; i < n; i++) {
                    o.a = i; o.b = o.a + 1; o.c = o.b * 2; s += o.c;
                }
                return String(s);
            }
        "#;
        const EXPECTED_STEPS: u64 = 17 * 500_000 + 16;

        let program = Box::leak(Box::new(compile(SRC)));
        let mut vm = crate::vm::Vm::new(program);
        vm.run().expect("top level initializes");
        let run = vm
            .program
            .functions
            .iter()
            .find(|p| p.name == "run")
            .expect("run function");
        let slot = run.name_global.expect("run global slot");
        let mut recorder = crate::vm::instrument::Recorder::new();
        recorder.remaining = i64::MAX;
        vm.set_instrumentation(recorder);

        let before = FAST_FORWARD_HITS.load(std::sync::atomic::Ordering::Relaxed);
        let result = vm
            .call_value(
                vm.globals[slot as usize],
                Value::UNDEFINED,
                &[Value::int(1)],
            )
            .expect("comparison wrapper succeeds");
        assert_eq!(vm.display(result), "250000500000");
        assert_eq!(vm.instr_rec.as_ref().unwrap().steps_used(), EXPECTED_STEPS);
        assert!(
            FAST_FORWARD_HITS.load(std::sync::atomic::Ordering::Relaxed) > before,
            "real wrapper did not enter the object-loop kernel"
        );
    }
}
