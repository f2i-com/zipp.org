# Torch-shaped ML on Zipp

Zipp bundles an experimental Python implementation of a Torch API subset.
It is not the native PyTorch package, TorchInductor, CUDA or a pip environment.
Supported ordinary scripts can use `import torch`, `torch.nn`, autograd and
optimizers directly; their eager operations run on the CPU inside Zipp.

## Supported paths

| Path | Execution | Current contract |
|---|---|---|
| `model(x)` | Eager CPU tensor kernels in Zipp | Supported tensor operations, autograd, modules and optimizers. |
| `torch.compile(model)(x)` | Records a float32 compute graph | Returns a pending result; call `.submit(callback, on_error=None)`. |
| `torch.compile(step, training=True)(x, target)` | Records forward, backward and SGD update nodes | One zero_grad/backward/step; gradients and weights commit after successful readback. |
| `torch.compile(step, training=True).prepare(x, target)` | Records the step once; a device-resident session | `step`/`steps` feed only the batch; weights and optimizer state carry on the device until `sync()`; `dispose()` frees them. |
| Pending result in the playground | Host-selected WebGPU, WebGL2, WASM or JavaScript | Console and `result.backend` identify actual execution. |
| Pending result in native Zipp | CPU graph evaluator | Does not acquire CUDA or a native GPU. |

The default inference path accepts tensor positional/keyword inputs and records a callable's
forward pass under `torch.no_grad()`. Supported operations are elementwise
add/subtract/multiply (operators and Torch functions), ReLU, matrix multiplication,
whole-tensor sum/mean, matrix transpose, square, scalar division and `nn.Linear`/`nn.ReLU`/`nn.Sequential` combinations.
Shapes must match except for scalars and a vector bias expanded across a matrix's
rows. Each call snapshots and uploads its float32 inputs and weights.
Each call must return one tensor. Results are read back into ordinary CPU tensors.

```python
import torch
from torch import nn

model = nn.Sequential(nn.Linear(2, 4), nn.ReLU(), nn.Linear(4, 1))
x = torch.tensor([[1.0, 2.0], [3.0, 4.0]])
pending = torch.compile(model)(x)

def ready(y):
    print(pending.backend, y.tolist())

pending.submit(ready, lambda error: print(error))
```

`@torch.compile` also records a supported function. No `zipp_gpu` import or
backend argument is needed. The optional `backend="zipp_gpu"` spelling remains
accepted for explicit code. Backend selection belongs to the host/playground.

## GPU training (experimental)

Use `torch.compile(train_step, training=True)` and call `.submit(callback, on_error)`
on the returned result. The function must perform exactly one
`optimizer.zero_grad()`, scalar `loss.backward()` and `optimizer.step()`, in that
order with the same SGD, Adam or AdamW optimizer, and return a supported graph tensor (normally
that loss). The [model example](../examples/python/torch_training/model.py) uses
ordinary Torch syntax; its [driver](../examples/python/torch_training/main.py)
schedules one completed step at a time and plots the loss.

The graph contains the forward pass, first-order reverse-mode gradients and the
optimizer update. Float32 scalar/vector/matrix elementwise add/subtract/multiply
(NumPy-style broadcasting, with gradients reduced back to each operand's shape),
square, scalar division, matrix multiplication/transpose, `nn.Linear`, the
activations `relu`, `gelu` (the exact erf form is one graph operation; the tanh
form composes from recorded operations), `sigmoid`, `tanh`, `exp` and `log`,
`softmax`/`log_softmax` over the last dimension, `sum`/`mean` over all elements
or one dimension with `keepdim`, and `F.cross_entropy` are supported.
`F.cross_entropy` with integer class targets (`int64`/`int32`, shape `[N]` for
logits `[N, C]`) records the protocol's fused cross-entropy and its gradient;
the targets are converted exactly to float32 for the graph and checked to be
class indices in `[0, C)`. Probability targets compose from `log_softmax`.
`reduction='mean'` or `'sum'` are supported; class weights, label smoothing,
`reduction='none'` and `F.nll_loss` are rejected. ReLU's derivative at zero is
zero. Shared parameters accumulate gradients from every captured use. Bias
gradients reduce across the batch. A leaf first used inside `no_grad()` retains
its `requires_grad` flag; only operations inside that context are excluded from
the backward graph.

Optimizers follow `torch.optim`'s single-tensor update order. SGD supports
learning rate, weight decay, maximize, parameter groups, momentum, dampening
and Nesterov (the first step's buffer is the gradient itself, as in PyTorch).
Adam and AdamW support learning rate, betas, eps, weight decay (coupled or
decoupled), maximize and parameter groups; bias correction uses the incremented
step. Optimizer state round-trips through the CPU: momentum buffers, Adam
moments and the step count are read from `optimizer.state` when `step()` is
captured, their next values are read back with the weights and written under
the eager keys (`momentum_buffer`, `exp_avg`, `exp_avg_sq`, `step`), so compiled
and eager steps can alternate. `amsgrad`, RMSprop, closures, higher-order
gradients and retained graphs are rejected. Every trainable optimizer parameter
must participate in the loss. Dense relu networks with MSE and plain SGD still
record a protocol version 1 graph, which natively runs on the tensor kernels;
anything newer records version 2, which natively runs on `zipp_gpu`'s
pure-Python float32 reference (the same numbers, not fast).

Weights and gradients stay unchanged while a supported step is recorded or pending.
After successful readback, finite results are checked before committing weights,
optimizer state and gradients. A backend error or changed captured tensor rejects the result and
leaves the model unchanged by that submission. "Changed" means written in place
(`fill_`, `copy_`, `zero_`, indexed assignment, an optimizer) or rebound (`.data =`,
in-place arithmetic) after capture, even back to equal values: each tensor
storage carries a version counter, and a result is judged against the storage
and version it was recorded from rather than by comparing values. Changes to
captured gradients, shape, dtype or `requires_grad` also invalidate a pending step,
as does a change to captured class targets. Optimizer parameter identities,
group membership/order, all supported options and each parameter's step count
are snapshotted when `step()` is captured; changing them, or replacing a state
buffer, before completion rejects that step. Options must be numeric or boolean
scalars (`betas` a pair of them); mutable option values are rejected.
Gradient cleanup uses the captured parameter set, so newly added parameters
cannot have their gradients cleared by an older request. Each training result is single-use.
Wait for completion before preparing the next step: overlapping captures generally
become stale after the first update. These guarantees cover recorded tensor/optimizer
updates, not arbitrary Python side effects inside a user function.

**A compiled call is capture per call, with uploads and readbacks per call.**
Uploads and readbacks move tensor bytes (`Float32Array`s at the host boundary,
straight into tensor storage on the way back), not a number per element. Native
Zipp evaluates the graph on its CPU tensor kernels, with the same float32 results
as `zipp_gpu`'s pure-Python reference; a browser GPU requires the host GPU grant
and an available WebGL2/WebGPU backend. Auto selection can fall back to CPU;
inspect `.backend`. There is no graph cache or automatic multi-GPU distribution.

Each step returns the requested result plus leaf gradients and updated parameters.
The runtime's default 16-output limit means a model with four parameter tensors
uses nine outputs (more if differentiating inputs). All intermediate forward and
backward nodes count toward the same 512-node, storage and work budgets. This
bounded implementation is intended for small experiments, not large ML workloads.
GPU convolution is follow-on work; NCA remains outside this repository.

### Prepared steps: weights and optimizer state resident on the device

`compiled.prepare(*args, backend=None, on_ready=None, on_error=None, **kwargs)`
records the step once from an example call and returns a `Prepared` session
built on `zipp_gpu.Graph.prepare`:

```python
compiled = torch.compile(train_step, training=True)
prepared = compiled.prepare(x, target)                  # records once, uploads weights
prepared.step(callback, x, target, on_error=None)       # one step; callback(loss tensor)
prepared.steps(callback, [(x1, t1), (x2, t2)], on_error=None)   # one submission; callback([loss, ...])
prepared.sync(callback, on_error=None)                  # download into the model; callback(prepared)
prepared.dispose()
```

- **Feeds.** The float tensor arguments and the integer class-target arguments
  the recorded step reads are the feeds; every step passes them again with the
  recorded shapes and dtypes (a `ValueError` otherwise). Any other tensor the
  step reads (parameters, constants, buffers) is uploaded at `prepare()`.
  Non-tensor arguments are recorded constants and must be passed unchanged.
- **Resident state.** Each parameter's weight, its gradient and its optimizer
  buffers (momentum buffer, or Adam's two moments) are outputs kept on the
  device and carried into the next step; only the returned tensor is read back
  per step. Buffers that do not exist yet start as zeros on the device (the
  update PyTorch's first step performs on them, exactly, for Adam and for SGD
  momentum without dampening; SGD momentum *with* dampening needs one eager or
  compiled step first, because PyTorch's first step clones the gradient into
  the buffer and one program for every step cannot express that from zeros).
  The `adam_update` step count advances by one per executed step.
- **Outputs bound.** One result plus, per parameter tensor, a weight, a
  gradient and its buffers must fit the protocol's 64 outputs: at most 15
  parameter tensors with Adam or AdamW (4 each), 21 with SGD momentum, 31 with
  plain SGD. Beyond that `prepare()` raises `NotImplementedError` naming the
  limit; it does not fall back to per-call capture. A tensor outside the
  optimizer that requires grad is rejected too (its gradient would have to
  accumulate across steps).
- **`sync()`** downloads every resident tensor and, after checking each is
  finite, replaces the parameters' data, writes `optimizer.state` (the eager
  keys: `momentum_buffer`, `exp_avg`, `exp_avg_sq`, and `step` as the count
  after the executed steps) and sets `.grad` to the last step's gradients: the
  model is what the same number of eager `optimizer.step()` calls would have
  left. The session stays live and keeps training from the same values; a
  sync with nothing new executed calls back at once. Checked against PyTorch
  2.11 (`crates/zipp-vm/tests/fixtures/torch_prepared.py`, six distinct
  batches, relu/gelu/sigmoid/tanh, MSE and `F.cross_entropy`, SGD with weight
  decay, momentum, Nesterov, Adam and AdamW): losses within 2.4e-7, weights,
  gradients and optimizer state within 1.2e-7 after six steps, and eager steps
  after `sync()`/`dispose()` continue the same trajectory. Without a host the
  session's steps equal the same steps as separate compiled calls bit for bit.
- **Exclusivity.** While a session is live its parameters are *refused* (a
  `RuntimeError`) to compiled calls, to a second `prepare()` and to eager
  `optimizer.step()`; `sync()` and `dispose()` first. Optimizer options are
  snapshotted at `prepare()`; changing them makes the next step raise. Eager
  edits to the parameters themselves are not detected: `sync()` overwrites them.
- **Failure.** A step the device refuses (`ComputeError`, delivered to
  `on_error` or raised) poisons the session, since carried state may have
  advanced before the readback check; only `dispose()` remains. What was not
  synced is lost on `dispose()`.
- **Asynchrony.** In the playground the host creates the session after the
  current call returns (`on_ready(prepared)`, `prepared.backend`); steps
  requested meanwhile queue behind it. Without a host (`zipp py`, CPython for
  `zipp_gpu` itself) the session runs on the tensor kernels and every callback
  runs before the call returns. A `steps()` run submits at most 64 steps.
- **Inference.** `torch.compile(model).prepare(x)` also works: the weights are
  uploaded once and each `step` feeds only `x`; `sync()` has nothing to do.

Measured on an RTX 5090 in headless Chrome, driving the engine from Python
through the gpu-lab adapter (784-256-10, batch 64, Adam, cross-entropy; warm
medians over 15 calls, 7 runs for the eight-step row):

| Backend | `compiled()` per call | `prepared.step` per call | `prepared.steps`, 8 per run, per step |
|---|---|---|---|
| WebGPU | 74.4 ms | 5.1 ms | 1.84 ms |
| WebGL2 | 88.8 ms | 3.3 ms | 2.24 ms |
| WebAssembly | 73.7 ms | 4.9 ms | 3.63 ms |

Of a `compiled()` call about 34 ms is the guest recording and validating the
step (every input storage is scanned for finiteness in the interpreter), the
host's own execution with the 2.4 MB readback is 4-19 ms, and the rest is
transport and the commit of 16 tensors. A prepared step feeds one 64x784 batch
(about 1 ms of guest time), the host runs it in 1.4-3.7 ms, and a `sync()` of
the 2.4 MB of weights, gradients and moments takes 26-29 ms (download, delivery
into tensor storage, the finiteness check of every value, and the commit). These
are one machine's numbers for one small model, not a benchmark of the backends.

## Compatibility boundaries

This is an experimental asynchronous extension, not PyTorch's synchronous compiler contract.
Browser GPU work completes asynchronously, so existing code that immediately
calls `.item()`, `.tolist()` or `.backward()` on a compiled result needs adapting.
The returned object exposes `.shape`, `.submit`, `.backend` and `.stats`; its
backend and stats become available before the success callback runs.

General GPU autograd (outside the training subset above), GPU convolution, GPU recurrent modules,
data-dependent tensor branches, device tensors outside a prepared session,
kernel fusion, ONNX import and live model proxies are not implemented.
Unsupported graph operations fail rather than claim GPU acceleration. `device="cuda"`
and `.to("cuda")` are rejected; the CPU layer does not silently relabel storage.
Browser adapter selection is controlled by the browser, not CUDA device indices.

Eager CPU support is broader: tensors, common arithmetic and shape operations,
autograd, Linear, Conv2d, recurrent cells and several optimizers are implemented.
Loading arbitrary third-party ML packages is not supported. `torch.save` and
`torch.load` implement a subset of PyTorch's ZIP/protocol-2 checkpoint format,
with allowed tensor/storage globals. They do not implement arbitrary pickle
objects, all archive versions or general strided checkpoint tensors.

## CPU Conv2d

`torch.nn.Conv2d` and `torch.nn.functional.conv2d` support float32/float64,
NCHW batches or an unbatched CHW input, rectangular kernels, integer/pair stride,
symmetric zero padding, dilation and groups (including depthwise convolution).
Input, weight and optional bias gradients participate in eager autograd and
ordinary optimizers. Try the [ordinary Torch CPU training example](../examples/python/conv2d/main.py)
with `zipp py examples/python/conv2d`. Numeric padding is supported; string padding and nonzero
padding modes are not. GPU compilation does not support Conv2d yet.

The [reference fixture](../crates/zipp-vm/tests/fixtures/torch_conv2d.py) compares
four forward/input-gradient/weight-gradient/bias-gradient cases against PyTorch
CPU and verifies an SGD update reduces a simple loss.

## Integer precision and storage semantics

The CPU `int64` and `int32` storage currently uses Float64Array. All integers
within [-2**53, 2**53] can be represented exactly; values outside that range can
round. Arithmetic does not provide true signed 64-bit or 32-bit overflow
semantics. Keep indices and integer data within the exact range. Full-width
integer storage is future work, not a current compatibility claim.

Eager float32 `matmul`/`@` accumulates each dot product in float64 and rounds
once when storing the float32 result, so results can differ from PyTorch's
float32 kernels (and from earlier Zipp builds) in the last bits. The GPU graph
reference (`zipp_gpu`) keeps float32 rounding after every step, matching the
WebGPU/WebGL2 backends.

General strided views are also future work. Slices and permutations materialize
copies, so mutating a slice does not update its parent as ordinary PyTorch
code may expect. Reshape/view and detach can share contiguous storage; this is
not a blanket promise that every view copies. Programs that depend on aliasing,
offsets, noncontiguous strides or overlapping writes need adapting.

GPU convolution remains future work; device tensors are resident only within a
prepared session (`compiled.prepare`). Dense GPU training does not imply GPU
Conv2d support.

See the [runnable visual example](../examples/python/torch_gpu/main.py),
[CPU training regression](../crates/zipp-vm/tests/python_torch.rs) and
[actual WASM graph round-trip tests](../crates/zipp-wasm/tests/node/python-gpu.cjs).
GPU graph limits, fallback behavior and physical WebGL allocation accounting
are described in the [GPU runtime guide](../crates/zipp-wasm/gpu-lab/README.md).

## Validation of this update

The portable [Life rules](../examples/python/gpu/life.py) matched an independent
PyTorch rolled-neighbor implementation across 1x1, 2x2, 7x7 and 96x96 grids.
The same source advanced a glider correctly through the actual WASM GPU bridge.
The visual sample passed on WebGL2, WebGPU and WASM, with 301 live cells in its
first generation, matching the independent PyTorch reference.

CPU Conv2d's forward and backward fixture passed in native Zipp and actual WASM.
The ordinary training example produced the same ten rounded losses in native
PyTorch and Zipp (0.085254 down to 0.010812). These are functional checks, not
performance benchmarks or a claim of full Torch compatibility.

The GPU training fixture records five steps from CPU PyTorch 2.11 and compares
losses, all parameter gradients and all updated weights against Zipp's graph
execution. The [WASM training checks](../crates/zipp-wasm/tests/node/python-training.cjs)
also cover failed submissions, duplicate submission, stale results and rejected
optimizer options. These are correctness checks, not performance benchmarks.
