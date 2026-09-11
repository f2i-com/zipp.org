//! The 11 September 2026 audit's ZIPP-05: state write-back must not do
//! quadratic key matching. The probe counter is exact and deterministic, so
//! the bound below is a fact about the algorithm, not about a clock.
use super::MERGE_KEY_PROBES;
use crate::embed::{compile_script, HostValue, JsValue, ScriptState};

fn prepare(src: &str) -> ScriptState {
    let mut st = compile_script(src).expect("compiles");
    st.run_init().expect("runs");
    st
}

fn slot_of(st: &ScriptState, name: &str) -> u32 {
    st.symbols()
        .into_iter()
        .find(|s| s.name == name)
        .expect("slot")
        .index
}

fn probes<T>(f: impl FnOnce() -> T) -> (T, usize) {
    MERGE_KEY_PROBES.with(|c| c.set(0));
    let out = f();
    (out, MERGE_KEY_PROBES.with(|c| c.get()))
}

fn keys(n: usize, prefix: &str) -> Vec<String> {
    (0..n).map(|i| format!("{prefix}{i}")).collect()
}

/// A deterministic shuffle (LCG), so the incoming order differs from the
/// stored order without pulling in a random source.
fn shuffled(mut v: Vec<String>) -> Vec<String> {
    let mut x: u64 = 0x9e37_79b9;
    for i in (1..v.len()).rev() {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (x >> 33) as usize % (i + 1);
        v.swap(i, j);
    }
    v
}

#[test]
fn write_back_key_matching_is_linear_at_every_size() {
    for n in [256usize, 512, 1024, 4096] {
        let mut st = prepare(&format!(
            "var state = {{}}; for (var i = 0; i < {n}; i++) state['k' + i] = i;"
        ));
        let slot = slot_of(&st, "state");
        let old = keys(n, "k");

        // Equal key sets, shuffled order: the shape a host mirror sends back
        // every tick.
        let incoming: Vec<(String, HostValue)> = shuffled(old.clone())
            .into_iter()
            .map(|k| {
                let i: f64 = k[1..].parse().unwrap();
                (k, HostValue::Number(i + 1.0))
            })
            .collect();
        let (ok, count) = probes(|| st.set_slot(slot, &HostValue::Object(incoming)));
        assert!(ok);
        assert!(
            count <= 2 * n + 16,
            "{n} equal keys took {count} probes (quadratic would be {})",
            n * (n + 1)
        );
        assert_eq!(
            st.eval_in_context(&format!(
                "state.k0 + ',' + state.k{} + ',' + Object.keys(state).length",
                n - 1
            )),
            Ok(JsValue::String(format!("1,{n},{n}")))
        );

        // Disjoint sets: every old key is a deletion, every new key an
        // insertion — still one probe per incoming key.
        let fresh: Vec<(String, HostValue)> = keys(n, "j")
            .into_iter()
            .map(|k| (k, HostValue::Bool(true)))
            .collect();
        let (ok, count) = probes(|| st.set_slot(slot, &HostValue::Object(fresh)));
        assert!(ok);
        assert!(count <= 2 * n + 16, "{n} disjoint keys took {count} probes");
        assert_eq!(
            st.eval_in_context(
                "Object.keys(state).length + ':' + ('k0' in state) + ':' + state.j7"
            ),
            Ok(JsValue::String(format!("{n}:false:true")))
        );

        // Overlapping halves.
        let half: Vec<(String, HostValue)> = keys(n / 2, "j")
            .into_iter()
            .chain(keys(n / 2, "m"))
            .map(|k| (k, HostValue::Number(2.0)))
            .collect();
        let (ok, count) = probes(|| st.set_slot(slot, &HostValue::Object(half)));
        assert!(ok);
        assert!(
            count <= 2 * n + 16,
            "{n} overlapping keys took {count} probes"
        );
        assert_eq!(
            st.eval_in_context("Object.keys(state).length"),
            Ok(JsValue::Number(n as f64))
        );
    }
}

/// The indexed path keeps every rule the linear one had: accessors are never
/// replaced by their snapshot, non-enumerables the host could not see
/// survive, functions echoed back as null survive, a class instance stays an
/// instance, and nested objects merge the same way.
#[test]
fn write_back_semantics_survive_at_scale() {
    let n = 1024;
    let mut st = prepare(&format!(
        "class Counter {{ constructor() {{ this.n = 0; }} next() {{ return ++this.n; }} }} \
         var state = new Counter(); \
         for (let i = 0; i < {n}; i++) state['k' + i] = {{ v: i, fn: function () {{ return i; }} }}; \
         Object.defineProperty(state, 'hidden', {{ value: 'h', enumerable: false, writable: true, configurable: true }}); \
         Object.defineProperty(state, 'acc', {{ get: function () {{ return 'getter'; }}, set: function (v) {{ this.n = v; }}, enumerable: true, configurable: true }}); \
         state.method = function () {{ return 'm'; }};"
    ));
    let slot = slot_of(&st, "state");
    let read = st.try_get_slot(slot).expect("reads");
    let HostValue::Object(mut pairs) = read else {
        panic!("object")
    };
    // What a host does: spread, edit one field, echo the rest (methods came
    // back as Opaque; send them as Null the way a JSON host would).
    for (k, v) in pairs.iter_mut() {
        if let HostValue::Object(inner) = v {
            for (ik, iv) in inner.iter_mut() {
                if ik == "fn" {
                    *iv = HostValue::Null;
                }
                if ik == "v" && k == "k5" {
                    *iv = HostValue::Number(500.0);
                }
            }
        }
        if k == "method" {
            *v = HostValue::Null;
        }
    }
    pairs.push(("added".into(), HostValue::String("x".into())));
    let (ok, count) = probes(|| st.set_slot(slot, &HostValue::Object(pairs)));
    assert!(ok);
    // Each nested two-key object merges by the small-object scan (one probe
    // per compared key), so the nested level contributes a constant per
    // key; the whole merge stays linear in n.
    assert!(count <= 10 * n + 64, "{count} probes for {n} nested keys");
    assert_eq!(
        st.eval_in_context(
            "[state.k5.v, state.k5.fn(), state.k9.fn(), state.hidden, state.acc, \
              Object.getOwnPropertyDescriptor(state, 'acc').get !== undefined, \
              Object.getOwnPropertyDescriptor(state, 'hidden').enumerable, \
              state.method(), state.next(), state instanceof Counter, state.added].join('|')"
        ),
        Ok(JsValue::String(
            "500|5|9|h|getter|true|false|m|1|true|x".into()
        ))
    );
}
