//! torch quantization checked against CPU PyTorch 2.11 (x86 engine).
//! `fixtures/torch_quant/quant_cases.py` runs unchanged under both; `gen.py`
//! writes its PyTorch results to `quant_expected.json`, `text_expected.txt`
//! and `ckpt_expected.txt` (and the checkpoint `quant.pt`). Quantized
//! tensors, fake quantization and its gradients, every observer's and fake
//! quantizer's scale and zero point, the quantized Linear/Conv kernels under
//! the x86, fbgemm and onednn engines (tie-heavy inputs included), dynamic
//! Linear, quantized pooling and QFunctional are compared exactly;
//! prepare/convert and QAT workflows (whose observers see float
//! activations, which Zipp sums in a double) and the dynamic LSTM (float
//! sigmoid/tanh) within a tolerance.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run_with(source: &str, files: Vec<(String, Vec<u8>)>) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &files, &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(8_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn run(source: &str) -> Result<Vec<String>, String> {
    run_with(source, Vec::new())
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Exact, except the float paths: `*_float_*` values and the dynamic LSTM
/// (`tol_*`) to 1e-5, quantized workflow results whose scales come from
/// float activations (`ptq_*`, `qat_*`, `ckpt_model`) to 1e-6, each of the
/// larger of 1 and the value. A result one quantization step away is far
/// outside either.
const COMPARE: &str = r#"
got = results()
got.update({"tol_" + k: v for k, v in tol_results().items()})
bad = []
for name in sorted(expected):
    e = expected[name]
    if name not in got:
        bad.append("%s: missing" % name)
        continue
    g = got[name]
    if len(e) != len(g):
        bad.append("%s: %d values, expected %d" % (name, len(g), len(e)))
        continue
    if name.startswith("tol_") or "_float_" in name:
        tol = 1e-5
    elif name.startswith("ptq_") or name.startswith("qat_") or name == "ckpt_model":
        tol = 1e-6
    else:
        tol = 0.0
    worst = 0.0
    at = -1
    for i in range(len(e)):
        a, b = g[i], e[i]
        if a != a and b != b:
            continue
        d = abs(a - b) / max(1.0, abs(b))
        if not d <= tol and (at < 0 or d > worst):
            worst = d
            at = i
    if at >= 0:
        bad.append("%s: off by %.3g at %d (%r vs %r)" % (name, worst, at, g[at], e[at]))
extra = sorted(set(got) - set(expected))
print("compared", len(expected), bad, extra)
"#;

#[test]
fn quant_values_and_gradients_match_pytorch() {
    let source = format!(
        "{}\nimport json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_quant/quant_cases.py"),
        include_str!("fixtures/torch_quant/quant_expected.json"),
        COMPARE
    );
    let out = run(&source).unwrap();
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].starts_with("compared ") && out[0].ends_with(" [] []"), "{}", out[0]);
}

#[test]
fn quant_printing_modules_and_errors_match_pytorch() {
    let source = format!(
        "{}\nfor line in text():\n    print(line)\n",
        include_str!("fixtures/torch_quant/quant_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("\n"));
    let expected = lines(include_str!("fixtures/torch_quant/text_expected.txt"));
    for (i, (g, e)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "line {i}");
    }
    assert_eq!(out.len(), expected.len(), "{out:#?}");
}

/// Checkpoints: torch.load reads PyTorch's quantized records
/// (`torch._utils._rebuild_qtensor` over QUInt8/QInt8/QInt32 storages with
/// per-tensor or per-channel quantizer params, a strided one included)
/// and a converted model's state dict (`_packed_params._packed_params` as a
/// (weight, bias) tuple), and torch.save writes the same records, so a
/// Zipp checkpoint round-trips (PyTorch 2.11 loads it back as well).
#[test]
fn quant_checkpoints_round_trip_with_pytorch() {
    let files = vec![(
        "quant.pt".to_owned(),
        include_bytes!("fixtures/torch_quant/quant.pt").to_vec(),
    )];
    let source = format!(
        "{}\nfor line in ckpt_text('quant.pt', 'rt.pt'):\n    print(line)\n",
        include_str!("fixtures/torch_quant/quant_cases.py")
    );
    let out = lines(&run_with(&source, files).unwrap().join("\n"));
    let expected = lines(include_str!("fixtures/torch_quant/ckpt_expected.txt"));
    for (i, (g, e)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "line {i}");
    }
    assert_eq!(out.len(), expected.len(), "{out:#?}");
}

/// The module layout PyTorch code imports from (torch.quantization,
/// torch.nn.quantized and the other aliases name the same objects as
/// torch.ao.*; the pieces of torch.ao.quantization import on their own),
/// `torch.ao` and `torch.quantization` reachable after `import torch`, the
/// quantized engine setting, and the refusals: FX graph mode, quint4x2
/// tensors, the qnnpack engine, quantized modules Zipp does not implement.
#[test]
fn quant_modules_engines_and_refusals() {
    let out = run(r#"
import torch
import torch.nn as nn

def err(fn):
    try:
        fn()
    except Exception as e:
        return "%s: %s" % (type(e).__name__, str(e))
    return "ok"

print(torch.ao.quantization.QuantStub is torch.quantization.QuantStub, torch.ao.nn.quantized.Linear.__name__)
import torch.quantization as tq
import torch.ao.quantization as aq
import torch.nn.quantized as nq
import torch.ao.nn.quantized as anq
import torch.nn.quantized.dynamic as nqd
import torch.ao.nn.quantized.dynamic as anqd
import torch.nn.intrinsic as ni
import torch.ao.nn.intrinsic as ani
import torch.nn.intrinsic.quantized as niq
import torch.nn.intrinsic.qat as niqat
import torch.nn.qat as nqat
import torch.ao.nn.intrinsic.quantized.dynamic as niqd
from torch.ao.quantization.observer import MinMaxObserver, HistogramObserver, _PartialWrapper
from torch.ao.quantization.fake_quantize import FakeQuantize, default_fake_quant
from torch.ao.quantization.qconfig import QConfig, get_default_qconfig
from torch.ao.quantization.quantize import prepare, convert, _convert
from torch.quantization.quantize import quantize_dynamic
from torch.ao.quantization.fuse_modules import fuse_modules
from torch.ao.quantization.stubs import QuantStub, DeQuantStub
from torch.ao.quantization.utils import calculate_qmin_qmax
from torch.ao.quantization.quantize_fx import prepare_fx
print(tq.prepare is aq.prepare, nq.Linear is anq.Linear, nqd.Linear is anqd.Linear, ni.ConvReLU2d is ani.ConvReLU2d)
print(niq.LinearReLU.__name__, niqat.ConvBnReLU2d.__name__, nqat.Linear.__name__, niqd.LinearReLU.__name__)
print(MinMaxObserver is aq.MinMaxObserver, QConfig is aq.QConfig, calculate_qmin_qmax(None, None, False, torch.qint8, True))
print(err(lambda: prepare_fx(nn.Linear(2, 2), {"": aq.default_qconfig}, (torch.ones(1, 2),))).split(" (")[0])
print(torch.backends.quantized.engine, torch.backends.quantized.supported_engines)
torch.backends.quantized.engine = "fbgemm"
print(torch.backends.quantized.engine)
print(err(lambda: setattr(torch.backends.quantized, "engine", "qnnpack")))
print(err(lambda: setattr(torch.backends.quantized, "engine", "tpu")))
torch.backends.quantized.engine = "x86"
print(err(lambda: torch.quantize_per_tensor(torch.ones(2), 0.1, 0, torch.quint4x2)))
print(err(lambda: aq.quantize_dynamic(nn.Sequential(nn.GRU(2, 2)))))
print(err(lambda: nq.LayerNorm(3)))
m = nn.Sequential(aq.QuantStub(), nn.Conv2d(1, 1, 1), nn.BatchNorm2d(1), aq.DeQuantStub()).eval()
m.qconfig = aq.default_qconfig
p = aq.prepare(m)
p(torch.ones(1, 1, 2, 2))
print(err(lambda: aq.convert(p)))
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "True Linear",
            "True True True True",
            "LinearReLU ConvBnReLU2d Linear LinearReLU",
            "True True (-64, 63)",
            "NotImplementedError: FX graph mode quantization",
            "x86 ['onednn', 'x86', 'fbgemm']",
            "fbgemm",
            "RuntimeError: quantized engine QNNPACK is not supported",
            "RuntimeError: tpu is not a valid value for quantized engine",
            "NotImplementedError: torch.quint4x2 tensors are not supported on Zipp (quint8, qint8 and qint32 are)",
            "NotImplementedError: dynamically quantized GRU is not supported on Zipp (Linear and LSTM are)",
            "NotImplementedError: torch.ao.nn.quantized.LayerNorm is not supported on Zipp",
            "NotImplementedError: quantized BatchNorm2d is not supported on Zipp; fuse it into the preceding convolution with torch.ao.quantization.fuse_modules, or keep it in float between a DeQuantStub and a QuantStub",
        ]
    );
}

/// The native loops behind the quantization kernels (quantize,
/// dequantize, fake quantization, the integer GEMM and convolution, and
/// requantization) store exactly the bytes the JavaScript loops store,
/// at sizes where the loops matter, for every dtype and mode.
#[test]
fn quant_native_loops_match_javascript() {
    let out = run(r#"
import math
import torch
import _zipp_tensor as _k
import torch.ao.nn.quantized as nnq
import torch.ao.nn.quantized.dynamic as nnqd
import torch.ao.nn.intrinsic.quantized as nniq
BAD, CASES = [], [0]

def image(t):
    if isinstance(t, (list, tuple)):
        return [image(x) for x in t]
    if t.is_quantized:
        return [image(t.int_repr()), image(t.dequantize())]
    return (str(t.dtype), _k.tobytes(t._s))

def both(label, fn):
    CASES[0] += 1
    results = []
    for on in (False, True):
        _k._native(on)
        try:
            results.append(("ok", image(fn())))
        except Exception as e:
            results.append(("err", type(e).__name__, str(e)))
    if results[0] != results[1]:
        BAD.append(label)

def data(shape, seed, scale=1.0):
    n = 1
    for s in shape:
        n *= s
    vals = [math.sin(0.37 * i + seed) * scale + (0.5 * scale if i % 7 == 0 else 0.0) for i in range(n)]
    for i in range(0, n, 97):
        vals[i] = [float("nan"), float("inf"), -float("inf"), 3e9, -3e9][(i // 97) % 5]
    return torch.tensor(vals).reshape(shape)

x = data((37, 53), 0.3, 4.0)
for dt, zps in ((torch.quint8, (0, 9, 200, 255)), (torch.qint8, (-128, -2, 3, 127)), (torch.qint32, (-7, 0, 5))):
    for zp in zps:
        both("quantize %s %d" % (dt, zp), lambda: torch.quantize_per_tensor(x, 0.0371, zp, dt))
    both("quantize_per_channel %s" % dt, lambda: torch.quantize_per_channel(x, torch.tensor([0.01 + 0.003 * c for c in range(53)], dtype=torch.float64), torch.tensor([(c % 5) for c in range(53)]), 1, dt))
    both("quantize_dynamic %s" % dt, lambda: torch.quantize_per_tensor_dynamic(x.nan_to_num(0.0, 1e3, -1e3), dt, True) if dt != torch.qint32 else None)
y = data((5, 11, 40), 1.1, 3.0).nan_to_num(0.0, 50.0, -50.0).requires_grad_(True)
for s, z, lo, hi in ((0.1, 3, 0, 255), (0.0371, -5, -128, 127), (2.0 ** -5, 0, -64, 63)):
    both("fake quant %g" % s, lambda: torch.fake_quantize_per_tensor_affine(y, s, z, lo, hi))
    both("fake quant f64 %g" % s, lambda: torch.fake_quantize_per_tensor_affine(y.double(), s, z, lo, hi))
both("fake quant channel", lambda: torch.fake_quantize_per_channel_affine(y, torch.tensor([0.01 * (c + 1) for c in range(11)]), torch.tensor([c - 5 for c in range(11)], dtype=torch.int32), 1, -128, 127))
for engine in ("x86", "fbgemm", "onednn"):
    torch.backends.quantized.engine = engine
    for pc in (False, True):
        w = data((29, 64), 2.0, 0.5).nan_to_num(0.0, 1.0, -1.0)
        xq = torch.quantize_per_tensor(data((33, 64), 3.0, 2.0).nan_to_num(0.0, 5.0, -5.0), 2.0 ** -4, 3, torch.quint8)
        wq = torch.quantize_per_channel(w, torch.tensor([2.0 ** -6] * 29, dtype=torch.float64), torch.zeros(29, dtype=torch.long), 0, torch.qint8) if pc else torch.quantize_per_tensor(w, 2.0 ** -6, 0, torch.qint8)
        for relu in (False, True):
            def lin():
                m = nniq.LinearReLU(64, 29) if relu else nnq.Linear(64, 29)
                m.set_weight_bias(wq, torch.tensor([round(math.sin(j) * 20) * 2.0 ** -11 for j in range(29)]))
                m.scale, m.zero_point = 2.0 ** -8, 77
                return m(xq)
            both("linear %s %s %s" % (engine, pc, relu), lin)
            def conv():
                m = nniq.ConvReLU2d(6, 8, 3, stride=(1, 2), padding=1, dilation=(2, 1), groups=2) if relu else nnq.Conv2d(6, 8, 3, stride=(1, 2), padding=1, dilation=(2, 1), groups=2)
                wt = data((8, 3, 3, 3), 4.0, 0.5).nan_to_num(0.0, 1.0, -1.0)
                m.set_weight_bias(torch.quantize_per_channel(wt, torch.tensor([2.0 ** -6] * 8, dtype=torch.float64), torch.zeros(8, dtype=torch.long), 0, torch.qint8) if pc else torch.quantize_per_tensor(wt, 2.0 ** -6, 0, torch.qint8), data((8,), 5.0, 0.2).nan_to_num(0.0, 1.0, -1.0))
                m.scale, m.zero_point = 2.0 ** -8, 31
                return m(torch.quantize_per_tensor(data((2, 6, 9, 10), 6.0, 2.0).nan_to_num(0.0, 5.0, -5.0), 2.0 ** -4, 5, torch.quint8))
            both("conv %s %s %s" % (engine, pc, relu), conv)
        def dyn():
            m = nnqd.Linear(64, 29)
            m.set_weight_bias(wq, data((29,), 7.0, 0.3).nan_to_num(0.0, 1.0, -1.0))
            return m(data((4, 33, 64), 8.0, 2.0).nan_to_num(0.0, 5.0, -5.0))
        both("dynamic %s %s" % (engine, pc), dyn)
torch.backends.quantized.engine = "x86"
q = torch.quantize_per_tensor(data((2, 3, 17, 19), 9.0, 3.0).nan_to_num(0.0, 5.0, -5.0), 0.05, 70, torch.quint8)
both("dequantize", lambda: q.dequantize())
both("dequantize channel", lambda: torch.quantize_per_channel(data((3, 70), 9.5, 2.0).nan_to_num(0.0, 5.0, -5.0), torch.tensor([0.02, 0.03, 0.05], dtype=torch.float64), torch.tensor([1, -2, 0]), 0, torch.qint8).dequantize())
_k._native(True)
print(CASES[0], BAD)
"#)
    .unwrap();
    assert_eq!(out, ["56 []"]);
}
