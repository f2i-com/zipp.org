//! Binary tensor transport and the CPU tensor kernels (the 15 September 2026
//! ML transport track).
//!
//! A guest `Float32Array` crosses the host boundary as
//! `HostValue::Float32Array` — one node, charged by its bytes — instead of
//! `Opaque`, and a host one arrives as a fresh `Float32Array`. The Python
//! GPU path is built on it: a compiled training step sends its tensors to the
//! host as float32 bytes, takes its outputs back straight into tensor
//! storage, and judges staleness by storage identity and version counters
//! rather than by comparing `tolist()` snapshots.
//!
//! Tiers: the host conversions read and build heap objects directly, so no
//! compiled tier is involved in them; Python states always run with the VM
//! JIT disabled (`compile_python_program` calls `disable_vm_jit`), so the
//! Python cases below are interpreter-only by construction.

use zipp_vm::embed::{
    compile_script, FingerprintBudget, HostValue, HostValueBudget, JsValue, ScriptState,
};

fn prepare(src: &str) -> ScriptState {
    let mut st = compile_script(src).expect("source compiles");
    st.run_init().expect("top level runs");
    st
}

fn slot_of(st: &ScriptState, name: &str) -> u32 {
    st.symbols()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no global named {name}"))
        .index
}

/// The elements of a `Float32Array` host value as bit patterns, so NaN
/// payloads and signed zeros compare exactly.
fn bits(v: &HostValue) -> Vec<u32> {
    match v {
        HostValue::Float32Array(values) => values.iter().map(|x| x.to_bits()).collect(),
        other => panic!("expected a Float32Array, got {other:?}"),
    }
}

fn f32s(values: &[f32]) -> Vec<u32> {
    values.iter().map(|x| x.to_bits()).collect()
}

const ARRAYS: &str = r#"
    var f = new Float32Array([1.5, -0, 3e38, 0]);
    new Uint32Array(f.buffer)[3] = 0x7fc00001;
    var whole = new Float32Array([0, 1, 2, 3, 4, 5]);
    var sub = whole.subarray(2, 5);
    class Tensor32 extends Float32Array {}
    var derived = new Tensor32([7, 8]);
    var state = { t: f, n: 1, list: [sub] };
    var doubles = new Float64Array(2), bytes = new Uint8Array(2), ints = new Int32Array(2);
    var view = new DataView(new ArrayBuffer(8));
    var gone = new Float32Array(4); gone.buffer.transfer();
    var rab = new ArrayBuffer(16, { maxByteLength: 32 });
    var fixed = new Float32Array(rab, 8, 2), tracking = new Float32Array(rab);
    var x = 0;
    function shrink() { rab.resize(8); }
    function grow() { rab.resize(32); }
    function poke(i, v) { f[i] = v; }
    function describe(a) { return [a instanceof Float32Array, a.length, a[0], a.buffer.byteLength, a.byteOffset]; }
    function make(n) { var r = new Float32Array(n); for (var i = 0; i < n; i++) r[i] = i / 4; return r; }
    function check() {
        return [x instanceof Float32Array, x.length, x.buffer.byteLength,
                new Uint32Array(x.buffer)[1] === 0x7fc00001, Object.is(x[2], -0),
                state.t === f, state.list[0] === sub, f[0], whole.join()].join();
    }
"#;

#[test]
fn a_float32_array_reads_as_its_elements_bit_for_bit() {
    let mut st = prepare(ARRAYS);
    let f = st.get_slot(slot_of(&st, "f"));
    assert_eq!(
        bits(&f),
        vec![
            1.5f32.to_bits(),
            (-0.0f32).to_bits(),
            3e38f32.to_bits(),
            0x7fc0_0001
        ]
    );
    // A view reads its own window of the buffer, not the whole buffer.
    let sub = st.get_slot(slot_of(&st, "sub"));
    assert_eq!(bits(&sub), f32s(&[2.0, 3.0, 4.0]));
    // An instance of a subclass is still float32 elements.
    let derived = st.get_slot(slot_of(&st, "derived"));
    assert_eq!(bits(&derived), f32s(&[7.0, 8.0]));
    // Nested inside data, too.
    let HostValue::Object(pairs) = st.get_slot(slot_of(&st, "state")) else {
        panic!("state is an object");
    };
    assert_eq!(pairs[0].0, "t");
    assert_eq!(bits(&pairs[0].1), bits(&f));
    assert_eq!(pairs[2].1, HostValue::Array(vec![sub.clone()]));
}

#[test]
fn other_typed_arrays_and_dead_views_stay_opaque() {
    let mut st = prepare(ARRAYS);
    for name in ["doubles", "bytes", "ints", "view", "gone"] {
        assert_eq!(st.get_slot(slot_of(&st, name)), HostValue::Opaque, "{name}");
    }
    let (fixed, tracking) = (slot_of(&st, "fixed"), slot_of(&st, "tracking"));
    assert_eq!(bits(&st.get_slot(fixed)), f32s(&[0.0, 0.0]));
    assert_eq!(bits(&st.get_slot(tracking)).len(), 4);
    // A fixed-length view the buffer shrank out from under is out of bounds:
    // it has no elements to copy, and is opaque rather than empty.
    st.call_global("shrink", &[]).unwrap();
    assert_eq!(st.get_slot(fixed), HostValue::Opaque);
    assert_eq!(bits(&st.get_slot(tracking)).len(), 2);
    st.call_global("grow", &[]).unwrap();
    assert_eq!(bits(&st.get_slot(tracking)).len(), 8);
    assert_eq!(bits(&st.get_slot(fixed)), f32s(&[0.0, 0.0]));
}

#[test]
fn a_float32_array_costs_one_node_and_its_bytes() {
    let mut st = prepare(&format!(
        "{ARRAYS}\nvar big = new Float32Array(3000000); big[2999999] = 2.5;\n\
         var huge = new Float32Array(5000000);"
    ));
    let f = slot_of(&st, "f");
    let mut tight = HostValueBudget::new(10, 15);
    let err = st.try_get_slot_bounded(f, &mut tight).unwrap_err();
    assert!(err.contains("string limit"), "{err}");
    let mut exact = HostValueBudget::new(10, 16);
    assert_eq!(
        st.try_get_slot_bounded(f, &mut exact)
            .map(|v| bits(&v).len()),
        Ok(4)
    );
    assert_eq!((exact.nodes_used(), exact.string_bytes_used()), (1, 16));
    // Three million elements were far past the node ceiling as numbers; as
    // bytes they fit the default budget (12 MB of 16 MB).
    let big = st
        .try_get_slot(slot_of(&st, "big"))
        .expect("fits the byte budget");
    let HostValue::Float32Array(values) = big else {
        panic!("big")
    };
    assert_eq!((values.len(), values[2_999_999]), (3_000_000, 2.5));
    let err = st.try_get_slot(slot_of(&st, "huge")).unwrap_err();
    assert!(err.contains("string limit"), "{err}");
}

#[test]
fn the_digest_follows_the_bytes_and_the_read_budget() {
    let mut st = prepare(ARRAYS);
    let f = slot_of(&st, "f");
    let first = st.fingerprint_slot(f);
    assert!(first.is_some());
    assert_eq!(first, st.fingerprint_slot(f), "no mutation, same digest");
    st.call_global("poke", &[JsValue::Number(1.0), JsValue::Number(0.0)])
        .unwrap();
    let zero = st.fingerprint_slot(f);
    assert_ne!(first, zero, "-0 became +0: the bytes moved");
    st.call_global("poke", &[JsValue::Number(1.0), JsValue::Number(7.0)])
        .unwrap();
    assert_ne!(zero, st.fingerprint_slot(f), "an element changed");
    // Refused exactly where the read is refused.
    assert_eq!(
        st.fingerprint_slot_bounded(f, &mut FingerprintBudget::new(10, 15)),
        None
    );
    assert!(st
        .fingerprint_slot_bounded(f, &mut FingerprintBudget::new(10, 16))
        .is_some());
}

#[test]
fn a_host_float32_array_arrives_as_a_fresh_array() {
    let mut st = prepare(ARRAYS);
    let x = slot_of(&st, "x");
    let nan = f32::from_bits(0x7fc0_0001);
    assert!(st.set_slot(x, &HostValue::Float32Array(vec![1.0, nan, -0.0])));
    assert_eq!(
        st.call_global("check", &[]).unwrap(),
        JsValue::String("true,3,12,true,true,true,true,1.5,0,1,2,3,4,5".into())
    );
    // As an argument, and as a result.
    let describe = slot_of(&st, "describe");
    assert_eq!(
        st.call_slot(describe, &[HostValue::Float32Array(vec![2.5, 3.5])]),
        Ok(HostValue::Array(vec![
            HostValue::Bool(true),
            HostValue::Number(2.0),
            HostValue::Number(2.5),
            HostValue::Number(8.0),
            HostValue::Number(0.0),
        ]))
    );
    let made = st.call_slot(slot_of(&st, "make"), &[HostValue::Number(3.0)]);
    assert_eq!(made.map(|v| bits(&v)), Ok(f32s(&[0.0, 0.25, 0.5])));
}

#[test]
fn an_unchanged_echo_keeps_the_guest_array_and_an_edit_replaces_it() {
    let mut st = prepare(ARRAYS);
    let state = slot_of(&st, "state");
    let read = st.get_slot(state);
    // Written straight back: the guest keeps its own arrays (identity, and
    // the buffer `whole` shares with `sub`).
    assert!(st.set_slot(state, &read));
    assert_eq!(
        st.eval_in_context("[state.t === f, state.list[0] === sub, state.n, whole.join()].join()")
            .unwrap(),
        JsValue::String("true,true,1,0,1,2,3,4,5".into())
    );
    // An edited copy is a write: a new array, and the old one untouched.
    let HostValue::Object(mut pairs) = read else {
        panic!("state")
    };
    let HostValue::Float32Array(values) = &mut pairs[0].1 else {
        panic!("t")
    };
    values[0] = 42.0;
    assert!(st.set_slot(state, &HostValue::Object(pairs)));
    assert_eq!(
        st.eval_in_context("[state.t === f, state.t[0], f[0], state.t.length].join()")
            .unwrap(),
        JsValue::String("false,42,1.5,4".into())
    );
    // A slot that holds a typed array itself still declines a whole write.
    let f = slot_of(&st, "f");
    assert!(!st.set_slot(f, &HostValue::Float32Array(vec![9.0])));
    assert_eq!(
        st.eval_in_context("f.length").unwrap(),
        JsValue::Number(4.0)
    );
}

#[cfg(feature = "python")]
mod python {
    use super::*;
    use zipp_vm::frontend::compile_python_program;

    fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(f)
            .expect("spawn")
            .join()
            .expect("test thread")
    }

    fn run(source: &str) -> Vec<String> {
        let source = source.to_owned();
        big_stack(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &[], &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(400_000_000, None);
            state.run_init()?;
            Ok::<_, String>(state.take_output())
        })
        .unwrap()
    }

    fn s(v: &str) -> HostValue {
        HostValue::String(v.to_string())
    }

    fn field<'a>(v: &'a HostValue, key: &str) -> &'a HostValue {
        let HostValue::Object(pairs) = v else {
            panic!("{key}: not an object: {v:?}")
        };
        &pairs
            .iter()
            .find(|(k, _)| k == key)
            .unwrap_or_else(|| panic!("no {key}"))
            .1
    }

    fn items(v: &HostValue) -> &[HostValue] {
        let HostValue::Array(items) = v else {
            panic!("not an array: {v:?}")
        };
        items
    }

    fn number(v: &HostValue) -> f64 {
        let HostValue::Number(n) = v else {
            panic!("not a number: {v:?}")
        };
        *n
    }

    const TRAINING: &str = r#"
import torch
from torch import nn
torch.manual_seed(0)
model = nn.Sequential(nn.Linear(3, 4), nn.ReLU(), nn.Linear(4, 2))
opt = torch.optim.SGD(model.parameters(), lr=0.1)
x = torch.rand(5, 3)
y = torch.rand(5, 2)
def step(x, y):
    opt.zero_grad()
    loss = ((model(x) - y) ** 2).mean()
    loss.backward()
    opt.step()
    return loss
compiled = torch.compile(step, training=True)
log = []
def request():
    compiled(x, y).submit(lambda loss: log.append(loss.item()), lambda error: log.append(str(error)))
def weights():
    return [p.detach().reshape(-1).tolist() for p in model.parameters()]
def change():
    with torch.no_grad():
        model[0].weight.fill_(0.25)
def taken():
    out = list(log)
    log.clear()
    return out
def draw():
    pass
"#;

    /// A hosted training program and its hook slots.
    struct Hosted {
        state: ScriptState,
        call: u32,
        take: u32,
    }

    impl Hosted {
        fn new() -> Hosted {
            let modules = vec![("main".to_owned(), TRAINING.to_owned())];
            let mut state = compile_python_program("main", &modules, &[], &[], true)
                .expect("compiles")
                .into_state();
            state.set_limits(400_000_000, None);
            state.run_init().expect("runs");
            let slot = |name: &str| {
                state
                    .symbols()
                    .into_iter()
                    .find(|s| s.name == name)
                    .unwrap()
                    .index
            };
            let (call, take) = (slot("__zipp_py_call"), slot("__zipp_py_take_host"));
            Hosted { state, call, take }
        }

        fn call(&mut self, name: &str, args: Vec<HostValue>) -> HostValue {
            self.state
                .call_slot(self.call, &[s(name), HostValue::Array(args)])
                .unwrap_or_else(|e| panic!("{name}: {e}"))
        }

        /// Record one step and take its request: (id, payload).
        fn request(&mut self) -> (f64, HostValue) {
            self.call("request", vec![]);
            let taken = self.state.call_slot(self.take, &[]).unwrap();
            let [request] = items(&taken) else {
                panic!("one request: {taken:?}")
            };
            assert_eq!(field(request, "kind"), &s("gpu.execute"));
            (
                number(field(request, "id")),
                field(request, "payload").clone(),
            )
        }

        /// Answer request `id` with each output filled by `fill(output index,
        /// element count)`.
        fn deliver(
            &mut self,
            id: f64,
            payload: &HostValue,
            fill: impl Fn(usize, usize) -> HostValue,
        ) -> HostValue {
            let shapes = shapes(items(field(payload, "nodes")));
            let mut outputs = Vec::new();
            for (i, output) in items(field(payload, "outputs")).iter().enumerate() {
                let shape = &shapes[number(field(output, "id")) as usize];
                let count = shape.iter().product();
                let HostValue::String(name) = field(output, "name") else {
                    panic!("name")
                };
                outputs.push((
                    name.clone(),
                    HostValue::Object(vec![
                        (
                            "shape".into(),
                            HostValue::Array(
                                shape.iter().map(|&d| HostValue::Number(d as f64)).collect(),
                            ),
                        ),
                        ("dtype".into(), s("float32")),
                        ("data".into(), fill(i, count)),
                    ]),
                ));
            }
            let value = HostValue::Object(vec![
                ("version".into(), HostValue::Number(1.0)),
                ("backend".into(), s("test")),
                ("outputs".into(), HostValue::Object(outputs)),
                ("stats".into(), HostValue::Object(vec![])),
            ]);
            let reply = HostValue::Object(vec![
                ("ok".into(), HostValue::Bool(true)),
                ("value".into(), value),
            ]);
            self.call("__zipp_py_deliver", vec![HostValue::Number(id), reply])
        }
    }

    /// Each node's shape, inferred as a host does it (graph protocol v1).
    fn shapes(nodes: &[HostValue]) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = Vec::new();
        for node in nodes {
            let arg =
                |out: &[Vec<usize>], key: &str| out[number(field(node, key)) as usize].clone();
            let HostValue::String(op) = field(node, "op") else {
                panic!("op")
            };
            let shape = match op.as_str() {
                "input" | "full" => items(field(node, "shape"))
                    .iter()
                    .map(|d| number(d) as usize)
                    .collect(),
                "add" | "sub" | "mul" => {
                    let a = arg(&out, "a");
                    if a.is_empty() {
                        arg(&out, "b")
                    } else {
                        a
                    }
                }
                "relu" | "positive" | "life" => arg(&out, "a"),
                "transpose" => arg(&out, "a").into_iter().rev().collect(),
                "sum" => vec![],
                "matmul" => vec![arg(&out, "a")[0], arg(&out, "b")[1]],
                other => panic!("unexpected op {other}"),
            };
            out.push(shape);
        }
        out
    }

    /// Deterministic, exactly representable output values.
    fn ramp(output: usize, count: usize) -> Vec<f32> {
        (0..count).map(|k| output as f32 + k as f32 / 8.0).collect()
    }

    fn output_names(payload: &HostValue) -> Vec<String> {
        items(field(payload, "outputs"))
            .iter()
            .map(|o| match field(o, "name") {
                HostValue::String(n) => n.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_hosted_step_moves_tensors_as_float32_bytes_both_ways() {
        big_stack(|| {
            let mut h = Hosted::new();
            let (id, payload) = h.request();
            // Every captured tensor leaves as bytes — parameters and data;
            // only the Python scalars the step used (the learning rate, the
            // mean's divisor) are still one-element lists.
            let mut inputs = 0;
            for node in items(field(&payload, "nodes")) {
                if field(node, "op") == &s("input") {
                    match field(node, "data") {
                        HostValue::Float32Array(values) => inputs += values.len(),
                        HostValue::Array(scalar) => assert_eq!(scalar.len(), 1, "{scalar:?}"),
                        other => panic!("{other:?}"),
                    }
                }
            }
            assert!(inputs >= 3 * 4 + 4 + 4 * 2 + 2 + 5 * 3 + 5 * 2, "{inputs}");
            let names = output_names(&payload);
            assert_eq!(names[0], "result");
            let before = h.call("weights", vec![]);
            assert_eq!(
                h.deliver(id, &payload, |i, n| HostValue::Float32Array(ramp(i, n))),
                HostValue::Bool(true)
            );
            // The loss arrives, and each parameter takes its update exactly.
            let log = h.call("taken", vec![]);
            assert_eq!(items(&log), &[HostValue::Number(0.0)]);
            let after = h.call("weights", vec![]);
            assert_ne!(after, before);
            for (p, weights) in items(&after).iter().enumerate() {
                let i = names
                    .iter()
                    .position(|n| n == &format!("weight{p}"))
                    .unwrap();
                let got: Vec<f64> = items(weights).iter().map(number).collect();
                let want: Vec<f64> = ramp(i, got.len()).into_iter().map(f64::from).collect();
                assert_eq!(got, want, "parameter {p}");
            }
        });
    }

    #[test]
    fn a_list_reply_from_an_older_host_still_commits() {
        big_stack(|| {
            let mut h = Hosted::new();
            let (id, payload) = h.request();
            // Integral numbers arrive as Python ints; the storage takes them.
            h.deliver(id, &payload, |i, n| {
                HostValue::Array(
                    ramp(i, n)
                        .into_iter()
                        .map(|v| HostValue::Number(v.into()))
                        .collect(),
                )
            });
            assert_eq!(items(&h.call("taken", vec![])), &[HostValue::Number(0.0)]);
            let names = output_names(&payload);
            let i = names.iter().position(|n| n == "weight0").unwrap();
            let first: Vec<f64> = items(&items(&h.call("weights", vec![]))[0])
                .iter()
                .map(number)
                .collect();
            assert_eq!(
                first,
                ramp(i, 12).into_iter().map(f64::from).collect::<Vec<_>>()
            );
        });
    }

    #[test]
    fn an_in_place_edit_while_pending_makes_the_result_stale() {
        big_stack(|| {
            let mut h = Hosted::new();
            let (id, payload) = h.request();
            // `fill_` writes into the parameter's existing storage: only its
            // version records the edit.
            h.call("change", vec![]);
            let edited = h.call("weights", vec![]);
            h.deliver(id, &payload, |i, n| HostValue::Float32Array(ramp(i, n)));
            let log = h.call("taken", vec![]);
            assert_eq!(
                items(&log),
                &[s(
                    "GPU training result is stale; a captured tensor changed before completion"
                )]
            );
            assert_eq!(h.call("weights", vec![]), edited);
            // Two steps recorded against the same weights: the first commits
            // (rebinding each parameter's storage), the second is stale.
            let (first, p1) = h.request();
            let (second, p2) = h.request();
            h.deliver(first, &p1, |i, n| HostValue::Float32Array(ramp(i, n)));
            h.deliver(second, &p2, |i, n| HostValue::Float32Array(ramp(i + 1, n)));
            let log = h.call("taken", vec![]);
            assert_eq!(
                log.clone(),
                HostValue::Array(vec![
                    HostValue::Number(0.0),
                    s("GPU training result is stale; a captured tensor changed before completion"),
                ])
            );
        });
    }

    #[test]
    fn malformed_outputs_are_refused_before_anything_commits() {
        big_stack(|| {
            let mut h = Hosted::new();
            let before = h.call("weights", vec![]);
            let (id, payload) = h.request();
            h.deliver(id, &payload, |i, n| {
                HostValue::Float32Array(ramp(i, n + usize::from(i == 1)))
            });
            let names = output_names(&payload);
            let log = h.call("taken", vec![]);
            let [HostValue::String(message)] = items(&log) else {
                panic!("{log:?}")
            };
            assert!(
                message.starts_with(&format!("GPU output {} has", names[1])),
                "{message}"
            );
            assert_eq!(h.call("weights", vec![]), before);
            let (id, payload) = h.request();
            h.deliver(id, &payload, |i, n| {
                let mut v = ramp(i, n);
                if i == 2 {
                    v[0] = f32::NAN;
                }
                HostValue::Float32Array(v)
            });
            assert_eq!(
                items(&h.call("taken", vec![])),
                &[s(
                    "GPU training produced non-finite values; parameters were not updated"
                )]
            );
            assert_eq!(h.call("weights", vec![]), before);
        });
    }

    #[test]
    fn storage_versions_count_writes_into_a_storage() {
        let out = run(r#"
import torch
import _zipp_tensor as k
t = torch.zeros(2, 3)
s = t._s
seen = [k.version(s)]
t.fill_(1.0); seen.append(k.version(s))
t.zero_(); seen.append(k.version(s))
t.copy_(torch.ones(2, 3)); seen.append(k.version(s))
t[0] = 5.0; seen.append(k.version(s))
t[1, 2] = 4.0; seen.append(k.version(s))
t[torch.tensor([1])] = 3.0; seen.append(k.version(s))
t.uniform_(); seen.append(k.version(s))
t.data.normal_(); seen.append(k.version(s))
t.view(6).fill_(2.0); seen.append(k.version(s))
u = t + 1; v = t.relu(); w = t.sum(); seen.append(k.version(s))
print(seen, t._s is s)
t.add_(1)
print(t._s is s, k.version(t._s))
"#);
        assert_eq!(out, ["[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 9] True", "False 0"]);
    }

    #[test]
    fn graph_tensor_takes_storage_as_a_checked_snapshot() {
        let out = run(r#"
import torch
import _zipp_tensor as k
from zipp_gpu import Graph, GraphError, execute_locally
t = torch.tensor([[0.1, -2.0], [3.5, 1e-3]])
g = Graph()
a = g.tensor(t._s, (2, 2))
b = g.tensor(t.tolist())
t.fill_(9.0)
r = a @ b + a
got = []
g.submit(got.append, None, r=r, s=r.sum())
print(got[0]["outputs"]["r"]["data"] == execute_locally(g.program(r=r, s=r.sum()))["outputs"]["r"]["data"])
print(got[0]["outputs"]["r"]["data"], type(got[0]["outputs"]["r"]["data"]).__name__)
for label, action in [
    ("nan", lambda: Graph().tensor(torch.tensor([1.0, float("nan")])._s)),
    ("inf", lambda: Graph().tensor(torch.tensor([float("-inf")])._s)),
    ("int", lambda: Graph().tensor(torch.tensor([1, 2])._s)),
    ("shape", lambda: Graph().tensor(t._s, (3,))),
    ("rank", lambda: Graph().tensor(t._s, (1, 2, 2))),
]:
    try:
        action()
        print(label, "accepted")
    except GraphError as error:
        print(label, error)
# Storage outputs: each owns its array, the recorded input included.
g = Graph()
a = g.tensor(t._s, (4,))
twice = a * 2
got = []
g._submit(got.append, None, {"x": twice, "y": twice, "z": a}, True)
o = got[0]["outputs"]
print([isinstance(o[n]["data"], k.Storage) for n in "xyz"], o["x"]["data"] is o["y"]["data"],
      o["z"]["data"] is g._nodes[0]["data"], k.to_list(o["y"]["data"]), k.to_list(o["z"]["data"]))
"#);
        assert_eq!(
            out,
            [
                "True",
                // CPython's `execute_locally` on the same graph with list inputs.
                "[-6.889999866485596, -2.2019999027252197, 3.8534998893737793, -6.998999118804932] list",
                "nan Only finite float32 values are supported",
                "inf Only finite float32 values are supported",
                "int Tensor storage must be float32",
                "shape Input length does not match shape",
                "rank Use a scalar (), vector (N,), or matrix (M, N)",
                "[True, True, True] False False [18.0, 18.0, 18.0, 18.0] [9.0, 9.0, 9.0, 9.0]",
            ]
        );
    }

    #[test]
    fn a_native_compile_runs_on_the_tensor_kernels() {
        let out = run(r#"
import torch
from torch import nn
torch.manual_seed(0)
model = nn.Sequential(nn.Linear(16, 32), nn.ReLU(), nn.Linear(32, 4))
x = torch.rand(8, 16)
with torch.no_grad():
    eager = model(x)
pending = torch.compile(model)(x)
got = []
pending.submit(got.append)
print(pending.backend, pending.stats["nodes"] > 0, got[0].shape, torch.allclose(got[0], eager, atol=1e-6))
print(type(got[0]._s).__name__)
"#);
        assert_eq!(out, ["cpu-python True torch.Size([8, 4]) True", "_Storage"]);
    }

    /// The interpreter-specialized kernels against a scalar reference that
    /// walks every element with explicit broadcast indices, computes in
    /// double and rounds to float32 once per store, as the kernels promise.
    #[test]
    fn specialized_kernels_match_a_scalar_reference() {
        let out = run(r#"
import struct
import _zipp_tensor as k

def f32(v):
    return struct.unpack("<f", struct.pack("<f", v))[0]

def numel(shape):
    n = 1
    for d in shape:
        n *= d
    return n

def index(flat, shape):
    out = []
    for d in reversed(shape):
        out.append(flat % d)
        flat //= d
    return list(reversed(out))

def offset(idx, shape):
    # Right-aligned broadcast of `shape` into an index of the result.
    idx = idx[len(idx) - len(shape):]
    off = 0
    for i, d in zip(idx, shape):
        off = off * d + (i if d != 1 else 0)
    return off

def values(n, seed):
    out = []
    for _ in range(n):
        seed = (seed * 1103515245 + 12345) % 2147483648
        out.append(f32((seed / 2147483648.0 - 0.5) * 8.0))
    return out

OPS = {"add": lambda x, y: x + y, "sub": lambda x, y: x - y, "mul": lambda x, y: x * y,
       "div": lambda x, y: x / y, "gt": lambda x, y: 1.0 if x > y else 0.0, "lt": lambda x, y: 1.0 if x < y else 0.0,
       "ge": lambda x, y: 1.0 if x >= y else 0.0, "le": lambda x, y: 1.0 if x <= y else 0.0,
       "pow": lambda x, y: abs(x) ** y}
layouts = [((3, 4), (3, 4)), ((3, 4), ()), ((), (3, 4)), ((3, 4), (4,)), ((4,), (3, 4)),
           ((3, 4), (1, 4)), ((1, 4), (3, 4)), ((2, 3, 4), (3, 4)), ((3, 4), (2, 3, 4)),
           ((3, 1), (3, 4)), ((2, 1, 4), (3, 1)), ((5,), (1,)), ((1,), (5,))]
bad = 0
for op, fn in OPS.items():
    for sa, sb in layouts:
        av, bv = values(numel(sa), 1 + len(sa)), values(numel(sb), 7 + len(sb))
        if op == "pow":
            av = [abs(v) for v in av]
            sb_vals = [2.0] * numel(sb)
            bv = sb_vals
        a, b = k.from_flat("float32", av), k.from_flat("float32", bv)
        out, shape = k.binary(op, a, sa, b, sb)
        shape = tuple(shape)
        got = k.to_list(out)
        want = []
        for flat in range(numel(shape)):
            idx = index(flat, shape)
            x, y = av[offset(idx, sa)], bv[offset(idx, sb)]
            r = fn(x, y)
            want.append(r if op in ("gt", "lt", "ge", "le") else f32(r))
        if [float(v) for v in got] != want:
            bad += 1
            print("binary", op, sa, sb, got[:4], want[:4])
print("binary layouts checked", bad)

bad = 0
for shape, dims in [((3, 4), None), ((3, 4), (0,)), ((3, 4), (1,)), ((3, 4), (-1,)), ((2, 3, 4), (1,)),
                    ((2, 3, 4), (0, 1)), ((2, 3, 4), (1, 2)), ((2, 3, 4), (0, 2)), ((5,), (0,)), ((2, 3, 4), ())]:
    data = values(numel(shape), 11)
    flags = [v > 0.5 for v in data]
    for op in ("sum", "mean", "max", "min", "prod", "argmax", "argmin", "all", "any"):
        for keep in (False, True):
            if op in ("all", "any"):
                source = k.binary("gt", k.from_flat("float32", data), shape, k.full("float32", 1, 0.5), ())[0]
            else:
                source = k.from_flat("float32", data)
            out, oshape = k.reduce(op, source, shape, dims, keep)
            red = list(range(len(shape))) if dims is None else [d % len(shape) for d in dims]
            kept = [1 if d in red else shape[d] for d in range(len(shape))]
            acc = {}
            arg = {}
            for flat in range(numel(shape)):
                idx = index(flat, shape)
                o = offset([0 if d in red else idx[d] for d in range(len(shape))], kept)
                v = data[flat]
                r = 0
                for d in range(len(shape)):
                    if d in red:
                        r = r * shape[d] + idx[d]
                if op in ("sum", "mean"):
                    acc[o] = f32(acc.get(o, 0.0) + v)
                elif op == "prod":
                    acc[o] = f32(acc.get(o, 1.0) * v)
                elif op in ("max", "argmax"):
                    if o not in acc or v > acc[o]:
                        acc[o], arg[o] = v, r
                elif op in ("min", "argmin"):
                    if o not in acc or v < acc[o]:
                        acc[o], arg[o] = v, r
                elif op == "all":
                    acc[o] = acc.get(o, True) and flags[flat]
                else:
                    acc[o] = acc.get(o, False) or flags[flat]
            want = [(arg if op.startswith("arg") else acc)[o] for o in range(numel(kept))]
            if op == "mean":
                want = [f32(v / (numel(shape) // numel(kept))) for v in want]
            if k.to_list(out) != want or tuple(oshape) != (tuple(kept) if keep else tuple(s for d, s in enumerate(shape) if d not in red)):
                bad += 1
                print("reduce", op, shape, dims, keep, k.to_list(out)[:3], want[:3], tuple(oshape))
print("reductions checked", bad)

bad = 0
data = values(24, 5)
src = k.from_flat("float32", data)
for shape, perm in [((4, 6), (1, 0)), ((2, 3, 4), (2, 0, 1)), ((2, 3, 4), (0, 2, 1)), ((2, 3, 4), (1, 0, 2)), ((24,), (0,))]:
    out, oshape = k.permute(src, shape, perm)
    oshape = tuple(oshape)
    want = []
    for flat in range(24):
        idx = index(flat, oshape)
        src_idx = [0] * len(shape)
        for i, p in enumerate(perm):
            src_idx[p] = idx[i]
        want.append(data[offset(src_idx, shape)])
    if k.to_list(out) != want:
        bad += 1
        print("permute", shape, perm)
for shape, target in [((4,), (3, 4)), ((3, 1), (3, 4)), ((1,), (2, 2)), ((2, 1, 3), (2, 4, 3))]:
    part = values(numel(shape), 9)
    out = k.expand(k.from_flat("float32", part), shape, target)
    want = [part[offset(index(flat, target), shape)] for flat in range(numel(target))]
    if k.to_list(out) != want:
        bad += 1
        print("expand", shape, target)
m = k.from_flat("float32", data)
for spec, shape in [([(1, 4, 2), None], (4, 6)), ([None, (0, 6, 3)], (4, 6)), ([2, (1, 5, 1)], (4, 6)), ([(3, None, 1), 1], (4, 6))]:
    out, oshape = k.slice(m, shape, spec)
    rows = [[data[r * 6 + c] for c in range(6)] for r in range(4)]
    def sel(d, s):
        if s is None:
            return list(range(shape[d]))
        if isinstance(s, int):
            return [s]
        start, stop, step = s
        return list(range(shape[d]))[start:stop:step]
    want = [rows[r][c] for r in sel(0, spec[0]) for c in sel(1, spec[1])]
    if k.to_list(out) != want:
        bad += 1
        print("slice", spec)
print("shape kernels checked", bad)

bad = 0
for (m_, k_, n_) in [(3, 5, 4), (1, 7, 1), (6, 1, 3), (8, 24, 6)]:
    av, bv = values(m_ * k_, 21), values(k_ * n_, 22)
    out, shape = k.matmul(k.from_flat("float32", av), (m_, k_), k.from_flat("float32", bv), (k_, n_))
    want = []
    for i in range(m_):
        for j in range(n_):
            acc = 0.0
            for p in range(k_):
                acc += av[i * k_ + p] * bv[p * n_ + j]
            want.append(f32(acc))
    if k.to_list(out) != want:
        bad += 1
        print("matmul", m_, k_, n_)
    # float64 accumulates exactly as before: in double, in k order.
    out, _ = k.matmul(k.from_flat("float64", av), (m_, k_), k.from_flat("float64", bv), (k_, n_))
    want64 = []
    for i in range(m_):
        for j in range(n_):
            acc = 0.0
            for p in range(k_):
                acc += av[i * k_ + p] * bv[p * n_ + j]
            want64.append(acc)
    if k.to_list(out) != want64:
        bad += 1
        print("matmul64", m_, k_, n_)
print("matmuls checked", bad)

data = values(9, 3) + [0.0, -0.0]
src = k.from_flat("float32", data)
bad = 0
for op, fn in [("neg", lambda v: -v), ("relu", lambda v: v if v > 0 else 0.0), ("square", lambda v: v * v),
               ("abs", abs), ("sign", lambda v: 1.0 if v > 0 else -1.0 if v < 0 else 0.0)]:
    got = k.to_list(k.unary(op, src))
    want = [f32(fn(v)) for v in data]
    if [struct.pack("<f", v) for v in got] != [struct.pack("<f", v) for v in want]:
        bad += 1
        print("unary", op, got, want)
print("unary checked", bad)
"#);
        assert_eq!(
            out,
            [
                "binary layouts checked 0",
                "reductions checked 0",
                "shape kernels checked 0",
                "matmuls checked 0",
                "unary checked 0",
            ]
        );
    }
}
