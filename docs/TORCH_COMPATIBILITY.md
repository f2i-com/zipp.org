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
| Pending result in native Zipp (`zipp py`) | A hardware GPU through wgpu (Vulkan, Direct3D 12 or Metal) when one is present; the CPU graph evaluator otherwise | Synchronous either way; `result.backend` is `webgpu` or `cpu-python`. No CUDA. See *Native GPU*. |

The default inference path accepts tensor positional/keyword inputs and records a callable's
forward pass under `torch.no_grad()`. Supported operations are elementwise
add/subtract/multiply/divide (operators and Torch functions, including tensor by
tensor and `1/x`), ReLU, `sqrt`/`rsqrt`/`abs`/`square` and `**` with the
exponents 2, 1, 0.5, -1 and -0.5, matrix multiplication (including 1-D inputs
to `nn.Linear` and matrix-vector, vector-matrix and dot products), sum/mean over
all elements or over any set of dimensions, `softmax`/`log_softmax` over any
dimension, `reshape`/`view`/`flatten`/`squeeze`/`unsqueeze`/`permute`/`transpose`/`.t()`/`.T`,
`.detach()` as a stop-gradient, indexing (see *Element selection*), and `nn.Linear`/`nn.ReLU`/`nn.Sequential`
combinations. Comparisons (`<`, `<=`, `>`, `>=`, `==`, `!=`, `torch.gt`/`eq`/...,
also with an eager tensor on the left), `torch.where`/`Tensor.where`,
`masked_fill`, `clamp`/`clip`/`clamp_min`/`clamp_max` (scalar or tensor bounds),
`maximum`/`minimum` (and two-tensor `torch.max`/`torch.min`),
`F.hardtanh`/`F.relu6`/`F.leaky_relu` (and their `nn` modules), mask logic (`&`,
`|`, `^`, `~`, `logical_and`/`or`/`xor`/`not`) and dropout record graph protocol
version 3 operations (see *Masks, clamping and dropout*). Slicing,
`split`/`chunk`/`unbind`/`narrow`/`select`/`flip`, `cat`/`stack`, `index_select`,
`gather`, `take_along_dim`, `F.embedding` and one integer tensor index record
graph protocol version 4 operations (see *Element selection*); boolean-mask
indexing and indices computed on the device raise `NotImplementedError` naming
the operation.
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
optimizer update. Float32 scalar/vector/matrix elementwise add/subtract/multiply/divide
(NumPy-style broadcasting, with gradients reduced back to each operand's shape),
the powers and roots listed above, matrix multiplication/transpose, `nn.Linear`,
the shape operations listed above, the activations `relu`, `gelu` (the exact erf
form is one graph operation; the tanh form composes from recorded operations),
`sigmoid`, `tanh`, `silu`, `abs`, `exp` and `log`, `softmax`/`log_softmax` over
any dimension, `sum`/`mean` over all elements or several dimensions with
`keepdim`, and `F.cross_entropy` are supported; comparisons used as masks,
`where`, `masked_fill`, `clamp`, `maximum`/`minimum`, `hardtanh`, `relu6`,
`leaky_relu` (and `F.threshold`/`F.elu`, which compose from them) and dropout
are supported with PyTorch's gradients; a hand-written LayerNorm composes from
them.
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

An eager operation on a parameter inside the step (`W.T`, `W * 2`,
`(p ** 2).sum()` for an L2 penalty, `torch.add(h, b)`) is re-recorded as graph
operations on that parameter, so its gradient reaches the parameter, and a
prepared session recomputes it from the resident weights every step.
Re-recording covers add/sub/mul/div, neg, the powers above, square,
exp/log/tanh/sigmoid/relu/sqrt/rsqrt/gelu/abs/silu, view/reshape,
permute/transpose, sum/mean, matmul, maximum/minimum, softmax/log_softmax, basic
slicing (`pos[:T]`, `W[0]`, `W[:, ::2]`), `torch.cat` of parameters, clone and a
float32 `to`; any other operation raises `NotImplementedError` naming it rather
than becoming a separate leaf with no gradient (indexing a parameter by a tensor,
`W[idx]`, names `torch.index_select`/`F.embedding`, which record on the device).
`p.detach()` and `p.data` read the parameter's input.

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
gradients and retained graphs are rejected on the compiled path (eager
`optimizer.step(closure)` is supported). Every trainable optimizer parameter
must participate in the loss. Dense relu networks with MSE and plain SGD still
record a protocol version 1 graph, which natively runs on the tensor kernels;
anything newer records version 2, version 3 once it uses a comparison,
`where`, clamping, `maximum`/`minimum` or dropout, or version 4 once it slices,
concatenates or indexes. Natively these run on a hardware GPU when one is present
(see *Native GPU*) and otherwise on `zipp_gpu`'s float32 reference (the same
numbers, not fast).

Weights and gradients stay unchanged while a supported step is recorded or pending.
After successful readback, finite results are checked before committing weights,
optimizer state and gradients. A backend error or changed captured tensor rejects the result and
leaves the model unchanged by that submission. "Changed" means written in place
(`fill_`, `copy_`, `zero_`, in-place arithmetic such as `add_` or `+=`, including
through `.data`, indexed assignment, an optimizer) or rebound (`.data =`) after
capture, even back to equal values: each tensor
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
  A target the step reads through a view of its argument (`y.view(-1)`,
  `reshape`, `squeeze`, `flatten`, `.long()` of an int64 target) is fed from
  that argument each step. A tensor argument the step ignores, or reads only
  through a copy (`y[:, 0]`, `t.float()`), is refused at `prepare()` naming its
  position, rather than being frozen at its first value. A tensor the step
  creates and reads as a graph input is uploaded once at `prepare()` when
  nothing in its history is a parameter or a step argument: `torch.ones(n)`,
  `torch.arange(n)`, `torch.tensor(0.5)`, `zeros_like(h)`,
  `torch.tril(torch.ones(T, T))` and masks compared from it, arithmetic on
  buffers. `prepare()` decides this by tracing every tensor-kernel call the
  step makes while it records. A graph input whose storage descends from a
  parameter (a value computed under `no_grad` or from `.detach()`/`.data`
  arithmetic) or from a tensor argument (a one-hot or cast of the targets)
  would keep its `prepare()` value and is refused, naming which. Compute
  parameter-derived values with gradients enabled (they are then recorded on
  the device from the resident weights) and pass argument-derived values as
  arguments. Python values (`.item()`, Python's `random`, other Python state)
  are recorded constants, frozen like non-tensor arguments. A bool tensor
  argument read as a mask is fed each step, and an integer argument read as an
  index (embedding tokens, `gather` positions, through a view such as
  `idx.view(-1)`) is fed each step and checked against the dimension it
  indexes. CPU random draws inside the step are feeds too (see *CPU random
  draws in prepared steps*); dropout and `torch.rand_like`/`torch.bernoulli` of
  graph tensors draw on the device, afresh every step. Per-call
  `torch.compile` accepts all of these.
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
  requested meanwhile queue behind it. Without a host (`zipp py`, with or
  without a GPU, and CPython for `zipp_gpu` itself) every callback runs before
  the call returns. A `steps()` run submits at most 64 steps. Fed
  batches are copied when a step is posted, so refilling one buffer in place
  between queued steps is safe.
- **Inference.** `torch.compile(model).prepare(x)` also works: the weights are
  uploaded once and each `step` feeds only `x`; `sync()` has nothing to do.

### Masks, clamping and dropout (graph protocol version 3)

Comparisons return bool masks (float32 0/1 on the device, so `mask.sum()` is
float32). `where` and `masked_fill` need a bool condition, as PyTorch does. Fill
values must be finite: use -1e9 rather than `-inf` before a softmax (the same
result unless a whole row is masked). `maximum`/`minimum` propagate NaN and give
a tie (-0 against +0 included) to the first operand. Gradients follow PyTorch
2.11: `where` routes the gradient to the branch taken (zero to the other) and
`masked_fill`'s value gets the sum over masked elements. `clamp` passes it where
lo <= x <= hi; a tensor bound gets it where x lies beyond it, the lower bound
only while lo < hi. `hardtanh`/`relu6` pass it strictly inside their bounds,
`leaky_relu` scales it by the slope where x <= 0, `maximum`/`minimum` split a tie
evenly, and comparisons have none. This is checked against PyTorch with inputs
exactly on every boundary (`crates/zipp-vm/tests/python_torch_gpu2.rs`).

Dropout draws its mask on the device with the `uniform` operation: a
counter-based hash of (seed, step, element) in 32-bit integer arithmetic. The
mask is therefore the same bits on WebGPU, WebGL2, WASM, JavaScript and native
Zipp (checked in a real browser on WebGPU and WebGL2), but it is not PyTorch's
(or eager Zipp's) random stream: compare statistics, not values. Kept elements
are multiplied by float32(1/float32(1 - p)), exactly as PyTorch's CPU dropout,
and the gradient is masked and scaled identically (eager CPU `F.dropout`,
`nn.Dropout` and `dropout1d/2d/3d` keep the same `x * float32(1/float32(1 - p))`,
so kept values match PyTorch's CPU dropout bit for bit). Eval mode and p = 0 are the
identity and draw nothing; p = 1 multiplies by 0; `dropout1d`/`2d` (and
unbatched `3d`) drop whole channels. Each dropout call takes one 32-bit seed
from torch's default generator, so `torch.manual_seed` makes compiled calls
reproducible. Per-call compilation draws new seeds every call; a prepared
session draws them at `prepare()` and advances the step every executed step,
so every step has a fresh mask. `torch.rand_like`/`torch.bernoulli` of graph
tensors use the same generator. In-place dropout of a graph tensor is refused.
`randn` has no device form, because a normal draw needs transcendental functions
that backends round differently; inside a prepared step it is drawn on the host
every step and fed (see *CPU random draws in prepared steps*).

Measured on an RTX 5090 in headless Chrome, driving the engine from Python
through the gpu-lab adapter (784-256-10, batch 64, Adam, cross-entropy; warm
medians over 15 calls, 7 runs for the eight-step row):

| Backend | `compiled()` per call | `prepared.step` per call | `prepared.steps`, 8 per run, per step |
|---|---|---|---|
| WebGPU | 74.4 ms | 5.1 ms | 1.84 ms |
| WebGL2 | 88.8 ms | 3.3 ms | 2.24 ms |
| WebAssembly | 73.7 ms | 4.9 ms | 3.63 ms |
| Native Zipp, WebGPU over wgpu/Vulkan | ≈26 ms | ≈0.42 ms | ≈0.19 ms |

Of a `compiled()` call about 34 ms is the guest recording and validating the
step (every input storage is scanned for finiteness in the interpreter), the
host's own execution with the 2.4 MB readback is 4-19 ms, and the rest is
transport and the commit of 16 tensors. A prepared step feeds one 64x784 batch
(about 1 ms of guest time), the host runs it in 1.4-3.7 ms, and a `sync()` of
the 2.4 MB of weights, gradients and moments takes 26-29 ms (download, delivery
into tensor storage, the finiteness check of every value, and the commit). These
are one machine's numbers for one small model, not a benchmark of the backends.

### Element selection (graph protocol version 4)

Basic indexing of a graph tensor (integers, slices with positive steps, `None`,
`...`) records one `slice` node and a reshape; `x[:, -1]`, `x[1::2]`, `h[b]` and
`qkv[..., :d]` are all one strided box. `narrow`, `select`,
`split`/`split_with_sizes`/`chunk`/`unbind` (methods and `torch.` functions) and
`flip`/`fliplr`/`flipud` (a box with negative strides) are slices too.
`torch.cat`/`concat`/`concatenate`/`stack` (and `hstack`/`vstack`/`column_stack`)
write each piece into zeros with `slice_scatter`; eager tensors among the pieces
are uploaded like any operand. One integer tensor or list index (`x[idx]`,
`x[:, [2, 0]]`, a 0-d index) records `index_select` and a reshape to the index's
shape; `torch.index_select`, `torch.gather`, `torch.take_along_dim` and the
tensor methods record `index_select`/`gather`; `x[torch.arange(n), idx]` of a
matrix records `gather`. `F.embedding` and `nn.Embedding` record `index_select`
on the table whenever the table trains or the tokens are a step argument, so a
prepared session feeds the tokens every step.

Gradients follow PyTorch 2.11: a slice's gradient is its box written into zeros
(`slice_scatter`), `cat`'s is each piece's slice, `index_select`'s (and the
embedding's) accumulates with `index_add` into zeros and `gather`'s with
`scatter_add` into zeros. The protocol fixes the accumulation order -- each
element's base value, then every contribution in ascending index position, one
float32 addition at a time, which is PyTorch's CPU order -- so a repeated token's
embedding gradient is the same bits on every backend. The pieces of one
`split`/`chunk`/`unbind` are merged by writing each into the others' buffer
(PyTorch's split backward is a `cat`), while separately taken slices of one
tensor add their zero-padded gradients, as they do in PyTorch; the two differ
only in the sign of a zero. Eager slices and `cat`s of a parameter inside a
training step (`self.pos[:T]`, `torch.cat([Wa, Wb], 1)`) are re-recorded on the
parameter's input.

Indices are integer tensors on the CPU. The graph's only dtype is float32, so an
index is converted exactly (every extent is below 65536) and checked to lie in
`[0, size)` when the step records, and a fed index at every step. Negative index
values, boolean-mask indexing (a data-dependent shape), indices computed on the
device (an `argmax`, a comparison), slices that select nothing, several tensor
indices in one indexing (other than `x[torch.arange(n), idx]`), an integer
combined with a tensor index, and indexing a parameter by a tensor on the CPU
(`W[idx]`; use `torch.index_select` or `F.embedding`) raise `NotImplementedError`
naming the case; a negative step raises PyTorch's own `ValueError`. Checked
against PyTorch 2.11 (`crates/zipp-vm/tests/python_torch_gpu3.rs`: a transformer
head with an embedding lookup, a sliced positional table, a split qkv projection,
causal attention and last-token logits; `gather` and `x[arange, idx]` NLL;
`cat`/`stack`/`chunk`/`unbind`/`narrow`/`flip`; strided and integer slicing;
eager slices and cats of parameters): losses, gradients, weights and optimizer
state within 1.1e-7 over four steps as per-call calls, eager Zipp and a prepared
session, which equals the per-call calls bit for bit.

### CPU random draws in prepared steps

Inside a step being prepared, `torch.randn`, `torch.rand`, `torch.randint`,
`torch.randperm`, `torch.normal`, `torch.multinomial`, `torch.randn_like`,
`torch.rand_like`/`torch.randint_like` of CPU tensors and `torch.bernoulli` of a
CPU tensor record as per-step feeds. While `prepare()` records, each draws from a
copy of the generator's state, so preparing does not move torch's stream; every
executed step then makes the same call on the host, from the real generator (or
the `generator=` given), in the order the step made them, and feeds the values.
A `steps()` run draws step by step, in order, before it is submitted, and a
hosted session receives each queued step's own copy. The session therefore
consumes the stream exactly as the same eager steps do: under the same
`torch.manual_seed` a prepared session draws the same values as eager Zipp's steps
and leaves the generator at the same point, and its results equal per-call
compilation bit for bit (VAE reparameterisation with `randn_like`, input noise,
`torch.normal` of a graph mean, `randint` negative samples looked up in an
embedding, a `bernoulli` mask and `randint` class targets). A float32 draw
becomes a graph input, so arithmetic on it records on the device
(`mu + eps * std`); an integer draw stays a CPU tensor and is fed wherever the
step reads it as class targets, an index or a mask. (Zipp's `randn` is its own,
not PyTorch's bit stream; `rand`/`randint` follow PyTorch's.)

Refused, naming why: an in-place draw (`normal_`, `uniform_`, `random_`,
`bernoulli_`, `exponential_`, ...), `torch.poisson` or `nn.init` inside the step;
an integer draw the graph does not read as targets, an index or a mask; a tensor
an eager operation computed from an integer draw; `torch.multinomial` of a graph
tensor, or of probabilities computed from a parameter, an argument or another
draw; and a draw with `requires_grad=True`. Device-drawn dropout seeds still come
from torch's generator at `prepare()`, so a step mixing dropout with CPU draws
does not reproduce eager Zipp's stream.

## Native GPU (`zipp py`)

The native CLI runs `torch.compile` and `zipp_gpu` graphs on a hardware GPU when
one is present. It embeds gpu-lab's JavaScript runtime, runs it unchanged in a
second, trusted Zipp state, and gives it a WebGPU API backed by
[wgpu](https://wgpu.rs): Vulkan, Direct3D 12 or Metal, loaded at run time. No SDK
is needed to build, and a machine without a driver simply has no adapter.

**Semantics.** A graph runs on the GPU synchronously, exactly where the CPU
evaluator would have run it: `submit(callback)` calls back before it returns,
plain loops of compiled calls train, `sync()` works anywhere, and a program's
output does not depend on whether a GPU is present. The differences are
`result.backend` / `prepared.backend` (`webgpu`), an added `stats["adapter"]`
(for example `"NVIDIA GeForce RTX 5090 (vulkan, NVIDIA 616.92)"`), and float
results within the tolerances below. The browser playground remains
asynchronous.

**Starting and choosing the GPU.** The GPU starts on a program's first graph
(adapter discovery and runtime start-up, about 200 ms, mostly the driver
loading; on Direct3D 12 the first graph also compiles its kernels with the
system's FXC compiler, about a second for a training step), so a program that
never submits one pays nothing. Software rasterizers (WARP, lavapipe) are not used.
`--no-gpu` (`zipp py --no-gpu main.py`) or `ZIPP_GPU=0` keeps the CPU evaluator.
`ZIPP_GPU_BACKEND=vulkan|dx12|metal` picks the backend; the default order is
Vulkan, then Direct3D 12, then Metal. `ZIPP_GPU_LOG=1` names the adapter when it
starts. If an adapter exists but the runtime cannot start on it, one line on
stderr says so and the program runs on the CPU.

**Fallback.** Whatever the GPU cannot do goes to the CPU evaluator, with the
result or error the CPU evaluator gives. A compiled call the GPU refuses or fails
is re-evaluated on the CPU. A prepared session that fails on the GPU before its
first step has run moves to the CPU (`prepared.backend` becomes `cpu-python`),
unless it was prepared with `backend="webgpu"`. A failure after steps have run on
the device is raised as the CPU evaluator raises it, and poisons the session.
More than 16 live sessions run on the CPU. Runs longer than 64 steps are
split transparently.

**Limits.** The device bounds a graph: a tensor up to the largest storage
binding (1 GiB here; `zipp_gpu` allows 268,435,456 elements in the native CLI,
with or without a GPU, and the protocol's 4,194,304 in the browser), 8 GiB of
logical graph storage, no estimated-work budget. The protocol's 512 nodes and
64 outputs still apply.

**Accuracy.** gpu-lab's browser protocol cases pass 276/276 on the native backend
(Vulkan, and Direct3D 12 with its default FXC compiler) with the browser
harness's tolerances. The 112 exact cases (masks, where,
dropout's `uniform` draws, slices, index_select, gather, and index_add/scatter_add
accumulation order) match the JavaScript reference bit for bit. The Python
fixtures print the same lines on the GPU as on the CPU evaluator: losses and
state within 1.2e-7 of PyTorch 2.11 for prepared sessions, 5.9e-7 relative for
the protocol v3/v4 families.

**Speed** (RTX 5090, Vulkan, ms per training step; measured on a shared
machine, treat as ±30%):

| Model | `compiled()` per call | `prepared.step` | `prepared.steps`, 8 per run, per step | CPU evaluator, `compiled()` |
|---|---|---|---|---|
| 784-256-10, batch 64, Adam | ≈26 ms | ≈0.42 ms | ≈0.19 ms | ≈2.9-4.8 s |
| 784-1024-1024-10, batch 256, Adam | ≈100-120 ms | ≈1.05 ms | ≈0.75 ms | 118 s |
| 784-2048-2048-10, batch 1024, Adam | ≈270-340 ms | ≈2.5 ms | ≈2.0 ms | 1,487 s |

Once a prepared session has run a step, the native host replays that step's
recorded commands itself: it writes each batch straight from the program to
the device, patches the few step-dependent values gpu-lab computes, and reads
back only the result. The results are gpu-lab's own steps bit for bit
(`ZIPP_GPU_REPLAY=0` turns replay off). Adam updates each parameter in one
pass, in place in a prepared session, with each element's arithmetic
unchanged. Elementwise kernels, the optimizer updates and Adam's pass read and
write four values at a time, each computed as before. After a session's first
step, a `prepared.step` on the native GPU builds its request from what every
step shares; the host checks each fed value as the full path does, and a value
it refuses gets that path's own error. Before it runs a graph, gpu-lab plans it for the backend (`src/fusion.mjs`):
nodes no output needs are dropped, a prepared session computes nodes that
cannot change between steps once, and WebGPU runs several nodes as one kernel
where each element's arithmetic is unchanged: a matmul reads a transposed
operand in place or writes its result transposed, a whole sum or mean of up to
2048 values and a cross-entropy of up to 1024 rows take one dispatch, and
chains of elementwise operations (with a bias broadcast into them) run as one
kernel whose every intermediate is rounded as before. The small MLP's step
went from about 45 dispatches to about 20, with the same bits. Float matmuls
with enough output tiles run a register-blocked tile: 128x128 (48 TFLOP/s on a
4096³ product) where the device has 32 KB of workgroup memory, else 64x64 (37
TFLOP/s; the 16x16 kernel: about 5), every output with the same additions in
the same order. Direct3D 12 compiles with DXC when a `dxcompiler.dll` is next
to `zipp` or on PATH, and keeps the 64x64 tile (DXC takes about 28 s over the
128x128 one); with the system's FXC it keeps the 16x16 kernel, because FXC
takes about 20 s to compile the 64x64 tile. A per-call
`compiled()` uploads and reads back every weight, gradient and moment; prepared
sessions are where the GPU pays off. `ZIPP_GPU_PROFILE=1` prints where a run's
host time went, and `=kernels` adds per-kernel GPU time. Reproduce with
`crates/zipp-cli/tests/native_gpu/bench.py` and `bench_vs_torch.py`.

**Against PyTorch CUDA** (PyTorch 2.11 + CUDA 12.8, same GPU, same weights and
batches; ms per step; `crates/zipp-cli/tests/native_gpu/bench_vs_torch.py`):

| Case | PyTorch CUDA eager | PyTorch CUDA graph | Zipp `prepared.step` / 8 per run | Zipp in Chrome (WebGPU) / 8 per run |
|---|---|---|---|---|
| 784-256-10, batch 64, Adam | 0.71 | 0.17 | 0.42 / 0.19 | 3.1 / 0.68 |
| 784-1024-1024-10, batch 256 | 0.99 | 0.29 | 1.05 / 0.74 | 3.8 / 1.65 |
| 784-2048-2048-10, batch 1024 | 1.11 | 0.91 | 2.48 / 1.96 | 6.8 / 8.8 |
| 2048² matmul x4, inference | 1.24 (TF32 0.87) | – | 1.91 / 1.72 | 3.8 / 2.7 |
| 4096² matmul x4, inference | 10.2 (TF32 5.9) | – | 13.8 / 14.2 | 22.5 / 20.3 |
| Embedding 8192x128 + gather NLL | 1.14 | 0.32 | 0.46 / 0.28 | 2.7 / 0.41 |

Losses agree within 1.4e-7 (small MLP) and 3.1e-5 (medium). A single step of
the small and medium MLPs and the embedding model is faster than PyTorch's
eager CUDA (0.42 vs about 0.7 ms, 1.05 vs 1.0, 0.46 vs 1.1), and with 8 steps
per run the small MLP is within 20% of a CUDA graph. A single step is still
launch-bound: about 20 dispatches, 0.11 ms of GPU time, about 0.13 ms of
driver submit and wait, and about 0.14 ms in Python. On large ones the gap is
matmul throughput: fp32 without tensor cores or fused multiply-add.
torch.compile's default backend needs triton, which is unavailable on Windows.

## Compatibility boundaries

This is an experimental asynchronous extension, not PyTorch's synchronous compiler contract.
Browser GPU work completes asynchronously, so existing code that immediately
calls `.item()`, `.tolist()` or `.backward()` on a compiled result needs adapting.
The returned object exposes `.shape`, `.submit`, `.backend` and `.stats`; its
backend and stats become available before the success callback runs.

General GPU autograd (outside the training subset above), GPU convolution, GPU recurrent modules,
data-dependent tensor branches, device tensors outside a prepared session,
general kernel fusion (beyond the exact elementwise and matmul fusions
above), ONNX import and live model proxies are not implemented.
Unsupported graph operations fail rather than claim GPU acceleration. `device="cuda"`
and `.to("cuda")` are rejected; the CPU layer does not silently relabel storage.
`torch.amp.autocast("cuda")` and `GradScaler("cuda")` are accepted and disable
themselves with PyTorch's warning.
Browser adapter selection is controlled by the browser, not CUDA device indices.

## Eager CPU coverage

Eager CPU support is much broader than the compiled subset. Loading arbitrary
third-party ML packages is not supported; within the bundled `torch`:

- **Tensors.** Creation (including `torch.Tensor(...)`, the legacy
  `FloatTensor`/`LongTensor`/... constructors, `*_like(dtype=, requires_grad=)`,
  `normal`, `logspace`, `finfo`/`iinfo`), arithmetic with PyTorch's type
  promotion, float16/bfloat16/int8/int16 tensors (see *Reduced-precision and
  small integer dtypes*), the special functions (`lgamma`, `digamma`,
  `polygamma`, `mvlgamma`, `i0`, `sinc`, `logit`, `xlogy`, `igamma`/`igammac`,
  and in `torch.special` also `erfcx`, `i0e`, `i1`, `i1e`, `ndtr`, `ndtri`,
  `log_ndtr`, `entr`, `xlog1py`, `zeta`, `gammaln`, `psi`, `expit`,
  `multigammaln`, `gammainc`/`gammaincc`, with gradients), elementwise math
  (logs, exponentials, trigonometric and hyperbolic
  functions and their inverses, `erf`/`erfinv`, rounding, `lerp`/`addcmul`/`addcdiv`,
  `remainder`/`fmod`/`floor_divide`, bitwise ops and shifts), comparisons and
  logical ops, reductions (`amax`/`amin`/`logsumexp`/`median`/`mode`/`kthvalue`/`quantile`,
  cumulative ops, `nansum`/`nanmean`, `var_mean`/`std_mean`, `unique`, `bincount`/`histc`),
  indexing and scatter (`scatter`/`scatter_add`/`index_add`/`index_fill`/`index_copy`,
  `masked_select`/`masked_fill`, `take_along_dim`, one-argument `where`,
  `nonzero(as_tuple=)`, `searchsorted`/`bucketize`, mixed basic and advanced
  assignment), shape ops (`tile`, `hstack`/`vstack`, `tensor_split`,
  `broadcast_to`, `meshgrid(indexing=)`, `rot90`, `diagonal`, `diag_embed`, ...),
  linear algebra (`mm`/`bmm`/`mv`/`addmm`/`baddbmm`, `tensordot`, `kron`, `cross`,
  `trace`, `outer`, `cdist`, `einsum`) and the in-place `*_` family, which writes
  into the tensor's storage.
- **Autograd.** First-order gradients through all of the above;
  `create_graph=True` for elementwise, reduction, shape, indexing, matmul and
  softmax operations (convolutions -- conv1d/2d/3d and the transposed
  convolutions -- run their backward as differentiable transposed convolutions
  and unfolded matmuls under `create_graph`, and keep the native kernel
  otherwise; other operations whose gradient is a dedicated kernel, such as
  `prod`, `cumprod` and the fused nn losses, raise `NotImplementedError` under
  `create_graph`); `autograd.grad`,
  `torch.autograd.Function` (several outputs, `needs_input_grad`),
  `torch.autograd.functional` (`vjp`, `jacobian`, `hessian`), `grad_fn`,
  `register_hook`, `set_grad_enabled` (function, context manager or decorator),
  `no_grad`/`enable_grad`/`inference_mode`. Writing in place into a tensor that a
  recorded operation saved for backward raises PyTorch's version-counter error
  at backward (`.data` writes are not counted). `retain_graph` is accepted, but
  graphs are never freed, so a second backward over one graph succeeds where
  PyTorch would raise.
- **`torch.nn`.** Linear and Bilinear; Conv1d/Conv2d/Conv3d and
  ConvTranspose1d/2d/3d; BatchNorm1d/2d/3d, InstanceNorm1d/2d/3d, GroupNorm,
  LayerNorm, RMSNorm and LocalResponseNorm; max/avg/adaptive/Lp pooling in
  1-D, 2-D and 3-D and MaxUnpool1d/2d/3d; fold/unfold; `interpolate`/`Upsample`
  (nearest, nearest-exact, linear, bilinear, bicubic, trilinear, area, and
  `antialias=True` for bilinear and bicubic, reproducing PyTorch's CPU kernels
  including their quirks); padding modules (1-D to 3-D) and `F.pad` with
  reflect/replicate/circular modes; pixel shuffle; Embedding (`from_pretrained`,
  `max_norm`) and EmbeddingBag; `F.scaled_dot_product_attention`,
  MultiheadAttention and the Transformer encoder/decoder layers, stacks and
  `nn.Transformer` (an evaluation-mode `TransformerEncoder` that PyTorch would
  run as nested tensors returns zeros at padded positions before its final
  norm, as PyTorch does); RNN/LSTM/GRU (layers, bidirectional, dropout,
  `proj_size`, packed sequences via `torch.nn.utils.rnn`) and their cells;
  about 35 activations (with the `F.*_` in-place aliases) and 21 losses
  (cross-entropy and NLL with `weight`, `ignore_index`, `label_smoothing` and
  N-D input); Sequential/ModuleList/ModuleDict/ParameterList/ParameterDict;
  forward, forward-pre and full-backward hooks, and state_dict and
  load_state_dict pre/post hooks; `nn.init` in full; `clip_grad_*`,
  `weight_norm`, `spectral_norm`/`remove_spectral_norm`,
  `fuse_conv_bn_eval`/`fuse_linear_bn_eval` and `parameters_to_vector`;
  `torch.nn.utils.parametrize` (`register_parametrization`,
  `remove_parametrizations`, `is_parametrized`, `cached`,
  `type_before_parametrizations`) and `torch.nn.utils.parametrizations`
  (`spectral_norm`, `weight_norm`, `orthogonal` with the matrix_exp, cayley
  and householder maps); Lazy modules (LazyLinear, LazyConv1d/2d/3d,
  LazyConvTranspose1d/2d/3d, LazyBatchNorm1d/2d/3d, LazyInstanceNorm1d/2d/3d)
  with `UninitializedParameter`/`UninitializedBuffer`, materialized in place
  by the first forward or by loading a state dict; the elementwise functions,
  matmul/`F.linear`, `cat`, `sum`/`mean` and the tensor methods given one raise
  PyTorch's `ValueError` (other torch.* functions that read the shape first
  raise `RuntimeError`). Module attribute
  assignment, non-persistent buffers, `nn.Buffer`, `state_dict(keep_vars=)`,
  `load_state_dict(strict=, assign=)` and dotted names follow PyTorch, so
  PyTorch state dicts load directly, including the spectral-norm
  `weight_orig`/`weight_u`/`weight_v` and `parametrizations.<name>.original`
  layouts (power iteration and `orthogonal`'s completion of a non-square
  weight start from Zipp's random stream, so results match PyTorch once its
  buffers are loaded). Full backward hooks see tensor positional arguments
  only, as in PyTorch; `half()`/`bfloat16()`/`to(dtype)` convert floating
  parameters and buffers. `nn.Buffer(t)` returns a `Buffer`-typed tensor
  rather than a plain `Tensor` (the runtime does not consult metaclass
  `__instancecheck__`), so `isinstance(b, nn.Buffer)` holds as in PyTorch while
  `type(b)` differs. `F.grid_sample` (4-D bilinear/nearest/bicubic, 5-D
  bilinear/nearest; zeros/border/reflection padding; `align_corners`; gradients
  for input and grid, following PyTorch's CPU kernels) and `F.affine_grid` (2-D
  and 3-D); `F.ctc_loss`/`nn.CTCLoss` (padded or concatenated targets, tuple or
  tensor lengths, `blank`, `reduction` with PyTorch's mean over target lengths,
  `zero_infinity`; PyTorch's gradient, which assumes `log_softmax` inputs);
  fractional max pooling (`F.fractional_max_pool2d/3d`,
  `nn.FractionalMaxPool2d/3d`, with `_random_samples` reproducing PyTorch's
  windows exactly); `nn.SyncBatchNorm` (local batch statistics, as PyTorch
  without a process group) and `convert_sync_batchnorm`; and
  `nn.DataParallel`/`nn.parallel.DistributedDataParallel` as single-device
  wrappers (`.module`, `module.`-prefixed state_dict keys, forward passes
  through; DDP follows PyTorch's CPU-module path, uses
  `torch.distributed`'s default process group when one is initialized and,
  unlike PyTorch, also works without one; `register_comm_hook` hooks are
  recorded but never run). `torch.grid_sampler`, `torch.affine_grid_generator`
  and `torch.ctc_loss` take PyTorch's integer mode and reduction codes.
- **`torch.optim`.** SGD, Adam, AdamW, RMSprop (`centered`, `maximize`),
  Adagrad, Adamax, NAdam, RAdam, Adadelta, ASGD, Rprop and LBFGS (one group,
  closure required), with PyTorch's option validation, `step(closure)`,
  parameter groups, `add_param_group`, step hooks and PyTorch's `state_dict`
  layout (a checkpoint's `step` is an int after loading; PyTorch resumes
  Zipp-written optimizer states). `foreach`, `fused`, `capturable` and
  `differentiable` are accepted and have no effect. `torch.optim.lr_scheduler`
  provides LambdaLR, MultiplicativeLR, StepLR, MultiStepLR, ConstantLR,
  LinearLR, ExponentialLR, PolynomialLR, CosineAnnealingLR,
  CosineAnnealingWarmRestarts, CyclicLR, OneCycleLR, ReduceLROnPlateau,
  SequentialLR and ChainedScheduler with PyTorch's chainable and closed-form
  semantics and `state_dict`.
- **`torch.utils.data`.** Dataset, IterableDataset, TensorDataset,
  StackDataset, ConcatDataset, ChainDataset, Subset, `random_split`, the
  samplers, `default_collate` and DataLoader. Loading runs in-process:
  `num_workers`, `pin_memory` and `persistent_workers` are accepted and
  ignored.
- **Random numbers.** `manual_seed` and `Generator` streams are deterministic,
  and `randperm`/`randint` follow PyTorch's CPU stream (a range of exactly
  2**32 draws one 32-bit word per element where PyTorch draws 64 bits).
  `get_state`/`set_state` and `torch.get_rng_state`/`set_rng_state` capture the
  full generator state in Zipp's own layout, not interchangeable with PyTorch's.
  `torch.poisson` draws with `torch.distributions.Poisson`'s sampler (inversion
  below rate 10, transformed rejection above), so any rate is cheap.
- **`torch.linalg`.** `det`, `slogdet`, `inv`/`inv_ex`, `solve`/`solve_ex`
  (vector and broadcast right-hand sides, `left=False`), `solve_triangular`,
  `cholesky`/`cholesky_ex`, `qr` (`reduced`/`complete`/`r`), `eigh`/`eigvalsh`
  (`UPLO`), `eig`/`eigvals`, `svd`/`svdvals` (`full_matrices`), `pinv`
  (`hermitian`, `atol`/`rtol`), `matrix_rank`, `lstsq`,
  `norm`/`vector_norm`/`matrix_norm` (every order), `cond`, `matrix_power`
  (negative powers), `matrix_exp`, `cross`, `multi_dot`, `vecdot`,
  `diagonal`, `vander`, `householder_product`, `tensorinv`/`tensorsolve` and
  `lu`/`lu_factor`/`lu_solve`, with the top-level `torch.det`/`logdet`/
  `slogdet`/`inverse`/`cholesky`/`cholesky_solve`/`cholesky_inverse`/`qr`/
  `svd` (returning V)/`pinverse`/`matrix_power`/`matrix_exp` and tensor
  methods, over batches `[..., m, n]` of float32/float64 (`solve`/`inv` and
  the vector and Frobenius/1/inf matrix norms also take complex64/complex128;
  complex solves run as the real 2n x 2n system, so complex64 results are
  more accurate than PyTorch's float32 LAPACK, which differs by ~1e-5).
  Factorizations run as native loops (partial-pivoting LU with LAPACK's
  pivot choice, Householder QR with LAPACK's signs, Cholesky, triangular
  solves, tridiagonal QL for `eigh`, one-sided Jacobi SVD, Hessenberg +
  Francis QR with back-substituted eigenvectors for `eig`), charged to the
  instruction budget; when the engine declines one (a budget that cannot
  cover it, a recorded trace) the same algorithms run in Python (cyclic
  Jacobi for `eigh`). Both compute in double precision and round once to
  the input dtype: a 64x64 `svd` takes about 2 ms and `eigh` under 1 ms,
  a 256x256 `inv`/`solve`/`cholesky`/`qr` under 10 ms. Gradients are
  PyTorch's formulas as tensor operations (including `linalg.lu`,
  `lu_factor` and `lu_solve`), so they batch, broadcast and support
  `create_graph=True`. Values and gradients match PyTorch 2.11 to 1e-12 in
  float64 (2e-6 in float32); eigenvector and singular-vector signs may
  differ. `eig`/`eigvals` return complex tensors (complex64 for float32
  input, complex128 for float64), as PyTorch does, eigenvalues in the Schur
  form's diagonal order (a conjugate pair with its positive imaginary part
  first: LAPACK's order for most matrices, not guaranteed) and unit
  eigenvectors whose largest component is real and positive; their
  gradients (PyTorch's `linalg_eig_backward`, including its phase-invariance
  check) flow to the real input. Failures raise `torch.linalg.LinAlgError`
  with PyTorch's messages. Other functions refuse complex input with
  `NotImplementedError`.
- **Complex tensors.** `torch.complex64`/`cfloat` and `complex128`/`cdouble`
  (`complex32` is named but cannot hold data) are stored as interleaved
  (real, imaginary) float32/float64 pairs, PyTorch's layout. Creation by
  `torch.complex`, `polar`, `view_as_complex`, `zeros`/`ones`/`full`/`eye`/
  `tensor(..., dtype=)`, `randn`/`rand` (each part N(0, 1/2) for `randn`) and
  `to(complex dtype)`; `.real`/`.imag` (and their setters), `real`/`imag`,
  `conj`/`conj_physical`/`resolve_conj`, `abs`, `angle`, `sgn`, `isreal`,
  `view_as_real`/`view_as_complex`; arithmetic with PyTorch's complex type
  promotion (real with complex is complex, float64 with complex64 is
  complex128), `pow` (with PyTorch's shortcuts for the exponents 2, 3, 0.5,
  -0.5, -1, -2), `exp`/`log`/`log2`/`log10`/`log1p`/`expm1`/`sqrt`/`rsqrt`/
  `sin`/`cos`/`tan`/`sinh`/`cosh`/`tanh`/`sigmoid`/`reciprocal`/`square`,
  `matmul`/`mm`/`bmm`/`mv`/`dot`/`vdot`/`einsum`, `sum`/`mean`/`prod`/
  `cumsum`/`cumprod`, `eq`/`ne`/`isclose`/`allclose`, the shape, indexing,
  indexed-assignment and in-place ops, `finfo`, `is_complex`, printing as
  PyTorch prints (`tensor([1.+2.j, ...])`), and checkpoints
  (`ComplexFloatStorage`/`ComplexDoubleStorage`, both directions). Each
  elementwise op computes in double precision and rounds once, so
  complex64 results can differ from PyTorch's float arithmetic in the last
  bit. Autograd follows PyTorch's convention for a real loss (the gradient
  of a complex input is dL/d(re) + i dL/d(im); a real input of a complex
  result takes the real part). `.real`, `.imag` and `conj()` are copies
  (strided views copy here), and `is_conj()` is always False;
  `view_as_real`/`view_as_complex` share the storage and its version
  counter, but an in-place write through one does not update the other's
  autograd history. Casting complex to real drops the imaginary part with
  PyTorch's warning (printed to stderr once). Python complex scalars work
  as in PyTorch 2.11: `item()`, `tolist()` and iteration give Python `complex`
  values (`torch.fft` results included), complex literals build complex
  tensors (`torch.tensor([1+2j])` is complex64, and promotion with real
  tensors follows PyTorch), a complex scalar mixes into tensor arithmetic
  (`t * 2j`), `torch.full` and `fill_` take one, `complex(t)`, `float(t)` and
  `int(t)` of a one-element tensor use PyTorch's checked conversion, and a
  0-d complex tensor formats as its value. Ops without a complex kernel (comparisons other than eq/ne, max/min, softmax,
  activations, convolutions, ...) raise instead of reading the pairs as
  reals.
- **`torch.fft`.** `fft`/`ifft`, `rfft`/`irfft`, `hfft`/`ihfft` and their
  2-D and N-D forms (`n`/`s`, `dim`, `norm` "backward"/"ortho"/"forward"),
  `fftfreq`/`rfftfreq`, `fftshift`/`ifftshift`, plus `torch.stft`/`istft`
  and the `hann`/`hamming`/`blackman`/`bartlett` windows; `torch.fft` is an
  attribute after `import torch`. Transforms run as a native mixed-radix
  Cooley-Tukey (lengths whose prime factors are at most 31) or Bluestein
  (any other length) in double precision, rounded once to complex64/
  complex128 (a JavaScript loop with the same bytes when the engine declines);
  a 64 x 4096 float64 `fft` takes about 12 ms. Values match PyTorch 2.11 to
  1e-12 (float64) and 2e-6 (float32) of each result's scale; gradients are
  PyTorch's (FftC2C/FftR2C/FftC2R backward, differentiable again).
  float16/bfloat16 inputs raise PyTorch's "Unsupported dtype" (under CPU
  autocast they compute in float32, as PyTorch's autocast lists them).
- **Sparse tensors.** The `torch.sparse_coo` layout (hybrid tensors with dense
  dimensions included), `torch.sparse_csr` and `torch.sparse_csc`;
  `torch.sparse_bsr`/`sparse_bsc` and batched compressed tensors raise
  `NotImplementedError`. Construction by `torch.sparse_coo_tensor` (size
  inference, `is_coalesced`, `check_invariants` and
  `torch.sparse.check_sparse_tensor_invariants`),
  `sparse_csr_tensor`/`sparse_csc_tensor`/`sparse_compressed_tensor`, the
  legacy `torch.sparse.FloatTensor(...)` constructors, and `to_sparse()`/`to_s
  parse(sparse_dim)`/`to_sparse(layout=)`/`to_sparse_coo`/`to_sparse_csr`/`to_
  sparse_csc`/`to_dense`. `coalesce` sorts stably by index and sums duplicates
  in order, as PyTorch does. Also: `indices`/`values`/`_indices`/`_values`/`cr
  ow_indices`/`col_indices`/`ccol_indices`/`row_indices`, `is_sparse`/`is_spar
  se_csr`/`layout`/`_nnz`/`sparse_dim`/`dense_dim`/`is_coalesced`; sparse +
  sparse (PyTorch's merge, with its index order and `is_coalesced` flag),
  dense ± sparse and in-place `add_`/`sub_`/`mul_`/`div_`/`zero_`;
  multiplication by a scalar, a dense tensor or another sparse tensor
  (PyTorch's intersection rule), division by a scalar, `neg`, `pow` and the
  zero-preserving unary functions (`abs`, `sin`, `sqrt`, `tanh`, `relu`,
  `sign`, `isnan`, ...); `torch.sparse.sum` (`dim`, `dtype`; `torch.sum(...,
  keepdim=True)`), `torch.sparse.softmax`/`log_softmax`,
  `torch.sparse.mm`/`addmm`/`sampled_addmm`/`spdiags`; `torch.mm`/`matmul`/`@`
  for sparse @ dense, dense @ sparse and sparse @ sparse (a CSR product lists
  each row's columns in the order Gustavson's algorithm reaches them, as
  PyTorch's does); `torch.smm`/`hspmm`, `t`/`transpose`, `index_select`,
  integer indexing, `torch.cat`/`stack`/`zeros_like`,
  `clone`/`detach`/`to`/`type`/`copy.deepcopy`; sparse buffers in modules
  (state dicts, `load_state_dict`, `.double()`), printing as PyTorch prints,
  and checkpoints (`torch._utils._rebuild_sparse_tensor` records with
  `torch.serialization._get_layout`, both directions). Gradients follow
  PyTorch: `torch.sparse.mm`/`addmm` give a sparse input a gradient masked to
  its coalesced entries, `torch.mm` a dense one, and sparse @ sparse a sparse
  product. `to_dense`, `values()`, `coalesce`, `torch.sparse.sum`, `softmax`
  and the constructors' `values` are differentiable. Accumulated sparse
  gradients take PyTorch's order and `is_coalesced` flags.
  `nn.Embedding(sparse=True)` and `EmbeddingBag(sparse=True)` produce sparse
  COO gradients: `optim.SGD` (momentum, Nesterov), `Adagrad` and `SparseAdam`
  apply PyTorch's updates to them, and the other optimizers raise PyTorch's
  errors. Gradients PyTorch refuses for want of a sparse kernel (through `sin`
  of a sparse tensor, or `index_select`) are computed. Coalescing, merging,
  sparse @ dense products, scatter-adds and linear keys run as native loops
  charged to the instruction budget, with a JavaScript loop storing the same
  bytes when the engine declines. A 20000 x 20000 matrix with 200k nonzeros
  coalesces in about 25 ms and multiplies a 64-column dense matrix in about 10
  ms. Values and gradients match PyTorch 2.11 to 1e-12 (float64) and 2e-6
  (float32). Printing, index order, `is_coalesced` flags and errors match
  exactly, with these exceptions:
  - Sparse gradients accumulated from several uses of one tensor merge where
    PyTorch concatenates them after an expanded upstream gradient; the
    coalesced gradient is the same.
  - `type(t) is torch.Tensor` is False for a sparse tensor (`isinstance`
    holds).
  - An operator without a sparse kernel names the SparseCPU backend but not
    always the operator.
  - Multiplying by a dense tensor of a larger broadcast shape raises
    `NotImplementedError`.
  - PyTorch 2.11 has no `Tensor.nnz()`; `_nnz()` is the count.
- **Quantization.** Quantized tensors of `torch.quint8`, `qint8` and `qint32`
  (`quint4x2` is named only): `torch.quantize_per_tensor` (scalar or tensor
  parameters, list form), `quantize_per_channel`,
  `quantize_per_tensor_dynamic`, `_make_per_tensor_quantized_tensor`/`_make_pe
  r_channel_quantized_tensor`/`_empty_affine_quantized` (zero-filled),
  `torch.dequantize` and `dequantize`/`int_repr`/`q_scale`/`q_zero_point`/`q_p
  er_channel_scales`/`q_per_channel_zero_points`/`q_per_channel_axis`/`qscheme
  `/`is_quantized`, printed as PyTorch prints them; shape ops, indexing,
  `clone`/`detach`/`copy.deepcopy`, `torch.cat`/`stack`/`equal`/`max`/`min`,
  `relu`/`relu6`/`hardtanh`, max/avg/adaptive-avg pooling and nearest
  `interpolate` on quantized tensors (arithmetic raises PyTorch's QuantizedCPU
  error). `torch.fake_quantize_per_tensor_affine`/`per_channel_affine` and
  `fused_moving_avg_obs_fake_quant` with PyTorch's straight-through gradients.
  `torch.ao.quantization` (and `torch.quantization`): the MinMax,
  MovingAverageMinMax, PerChannelMinMax, MovingAveragePerChannelMinMax,
  Histogram, FixedQParams, Placeholder, Recording and Noop observers;
  FakeQuantize, FixedQParamsFakeQuantize, FusedMovingAvgObsFakeQuantize and
  the default fake quantizers; QConfig, the default qconfigs,
  `get_default_qconfig`/`get_default_qat_qconfig` ('x86', 'fbgemm', 'qnnpack',
  'onednn'); QuantStub/DeQuantStub/QuantWrapper; `prepare`, `convert`,
  `quantize`, `prepare_qat`, `quantize_qat`, `quantize_dynamic`,
  `fuse_modules`/`fuse_modules_qat`. Modules: `torch.ao.nn.quantized` Linear,
  Conv1d, Conv2d, ReLU6, Quantize, DeQuantize, Dropout,
  FloatFunctional/QFunctional; `torch.ao.nn.intrinsic` and its
  `.quantized`/`.qat` forms; `torch.ao.nn.qat`; dynamic Linear (qint8 or
  float16 weights) and LSTM; the `torch.nn.quantized`/`intrinsic`/`qat`
  aliases. `torch.backends.quantized.engine` picks the arithmetic ('x86'
  default, 'fbgemm', 'onednn'; 'qnnpack' raises): Linear requantizes as
  fbgemm, convolutions under 'x86' round as oneDNN for symmetric weights and
  at most 100 groups (PyTorch's Linux AVX512-VNNI dispatch) and as fbgemm
  otherwise, dynamic Linear returns `fma(acc, sx*sw, bias)` with
  `reduce_range`. The integer and requantization loops are native, charged to
  the budget, with JavaScript loops storing the same bytes. Checkpoints use
  PyTorch's `_rebuild_qtensor` records and quantized state-dict keys, with
  `_metadata` versions, so PyTorch loads Zipp's files. Quantized values,
  observer parameters, fake quantization and gradients match PyTorch 2.11
  exactly, with these exceptions:
  - Scales observed from float activations (prepare/convert and QAT) can
    differ in the last bits (Zipp sums float convolutions in a double).
  - HistogramObserver may choose a neighbouring range at a near-tie.
  - Dynamic LSTM gates use Zipp's sigmoid/tanh (within 2e-7).
  - 'onednn' dynamic Linear uses fbgemm's arithmetic.
  - PyTorch's x86 convolutions on Windows/macOS use fbgemm's rounding and can
    differ by one step at exact ties.
  - `quint4x2` tensors, float zero points, FX graph mode and PT2E, reference
    modules, and quantized BatchNorm, LayerNorm/GroupNorm/InstanceNorm,
    Hardswish, ELU, LeakyReLU, PReLU, Sigmoid, Softmax, Embedding(Bag),
    Conv3d, ConvTranspose, static LSTM, MultiheadAttention, dynamic GRU and
    the RNN cells raise `NotImplementedError`.
- **`torch.distributed`.** One process: `init_process_group` accepts world
  size 1 (rank 0) with gloo (or no backend, reported as "undefined" as CPU-
  only PyTorch does) via env://, tcp://, file:// or a store; nccl/mpi/ucc/xccl
  raise PyTorch's "not built in" errors and a world size above 1 raises an
  explanation. Queries, `new_group`, `barrier`, `destroy_process_group` and
  every collective (`all_reduce` with each `ReduceOp`, `reduce`, `broadcast`,
  the gathers, scatters, object collectives,
  `reduce_scatter`/`reduce_scatter_tensor`, `all_to_all_single`; `all_to_all`
  raises as gloo does) behave as on a one-rank gloo group, sync or `async_op`,
  with PyTorch's argument errors; `send`/`recv` raise; stores are in memory.
  `torch.utils.data.DistributedSampler` reproduces PyTorch's index order.
  `torch.distributed.elastic`, `launch` and `run` raise ImportError.
- **`torch.distributions`.** Every distribution in PyTorch 2.11's
  `torch.distributions.__all__`: Normal, LogNormal, Uniform, Bernoulli,
  Categorical, OneHotCategorical (and StraightThrough), Binomial, Multinomial,
  NegativeBinomial, Poisson, Geometric, Exponential, Laplace, Cauchy, Gumbel,
  Gamma, Chi2, InverseGamma, Beta, Kumaraswamy, ContinuousBernoulli, Dirichlet,
  StudentT, FisherSnedecor, HalfNormal, HalfCauchy, Pareto, GeneralizedPareto,
  Weibull, VonMises, LogisticNormal, MultivariateNormal (covariance, precision
  or `scale_tril`), LowRankMultivariateNormal, Wishart, LKJCholesky,
  Independent, MixtureSameFamily, TransformedDistribution (Exp, Affine,
  Sigmoid, Tanh, Softmax, Softplus, Power, Abs, StickBreaking, Reshape,
  Independent, Compose, CorrCholesky, LowerCholesky, PositiveDefinite, Cat,
  Stack and CumulativeDistribution transforms), RelaxedBernoulli and
  RelaxedOneHotCategorical, with PyTorch's shapes, `expand`, constraints,
  argument and sample validation, `biject_to`/`transform_to`, and
  `kl_divergence`/`register_kl` for all 87 pairs PyTorch registers, including
  the exponential-family Bregman divergence. `log_prob`, `entropy`,
  `cdf`/`icdf`, moments and KL values and gradients match PyTorch 2.11 to 1e-12
  in float64. `rsample` is reparameterised where PyTorch's is; Gamma, Chi2,
  StudentT, Beta, Dirichlet, InverseGamma, FisherSnedecor and Wishart use the
  implicit Gamma gradient, with Dirichlet and Beta differentiated through
  normalised Gamma draws. Samples come from torch's generator, so they follow
  `manual_seed` but not PyTorch's streams. Poisson and NegativeBinomial sample
  any rate; Wishart samples by the Bartlett decomposition and redraws singular
  samples as PyTorch intends (its own check is inverted); VonMises samples in
  float64 by Best-Fisher rejection. LKJCholesky samples by the onion method as
  published; PyTorch 2.11's sampler draws rows below the second too spread out,
  so its samples do not follow its own `log_prob`, which Zipp matches.
  `torch.distributions.constraints`, `.transforms`, `.kl` and one module per
  PyTorch file (`from torch.distributions.normal import Normal`, `.utils`,
  `.distribution`, ...) are importable and hold the package's own objects.
- **`torch.amp`.** `autocast` (context manager and decorator; `torch.autocast`
  is the same class), `is_autocast_available`, `custom_fwd`/`custom_bwd`,
  `GradScaler` and the deprecated `torch.cuda.amp` spellings. `GradScaler("cpu")`
  follows PyTorch exactly (float32 scale, per-optimizer inf/NaN checks, skipped
  steps, backoff/growth/interval, `update(new_scale)`, `state_dict`). An enabled
  CPU `autocast` region re-types operations with PyTorch 2.11's CPU policy:
  matmul/`@`/`mm`/`bmm`/`addmm`/`addbmm`/`baddbmm`, `F.linear`, the 1-D to 3-D
  convolutions and transposed convolutions, `F.prelu`,
  `F.scaled_dot_product_attention` and `linalg.vecdot` cast floating inputs to
  the autocast dtype (so do the composites PyTorch routes through them:
  contracting `einsum`/`tensordot`, `multi_dot`, `matrix_power`, `linalg.pinv`,
  and the Linear, convolution, attention, Transformer, LSTM and RNN modules); the
  losses, `prod`, `trace`, `cdist`, `quantile`/`nanquantile`, 3-D and unpooling
  pools, reflect/replicate padding, `inverse`/`pinverse`/`cholesky*`/`qr`/`svd`,
  the factorizing `torch.linalg` functions and `torch.fft` cast to float32;
  `cat`/`stack`/`index_copy` promote to the widest input (raising PyTorch's
  `prioritize` error for the other reduced format); every other op, and every
  float64 input, keeps its dtype. The casts are differentiable (float32
  parameters receive float32 gradients), backward runs with autocast off, and
  weight casts are cached for the region when `cache_enabled`; `nn.GRU` keeps
  its tensors' dtypes.
- **Checkpoints.** `torch.save` and `torch.load` implement PyTorch's ZIP
  checkpoint format: pickle reads protocols 0-5 and writes protocol 2, and
  loading uses the same allowlist as PyTorch's `weights_only=True`. Tensors,
  `nn.Parameter`, `torch.Size`, `torch.device`, dtypes, bytes, bytearray, set
  and OrderedDict round-trip with PyTorch in both directions; float16,
  bfloat16, int8, int16, complex64 and complex128 tensors are written and
  read as PyTorch's `HalfStorage`/`BFloat16Storage`/`CharStorage`/
  `ShortStorage`/`ComplexFloatStorage`/`ComplexDoubleStorage` records (the
  elements are copied as they are). Strided checkpoint
  tensors (transposes, slices, expands, storage offsets) load as contiguous
  copies; tensors that shared a storage in the file no longer share it after
  loading. `torch.save(module)` is refused: save `module.state_dict()` instead.
  Arbitrary pickled objects are not supported.

The PyTorch 2.11 parity references are
[python_torch_core.rs](../crates/zipp-vm/tests/python_torch_core.rs),
[python_torch_nn.rs](../crates/zipp-vm/tests/python_torch_nn.rs) (fixtures
regenerated by `fixtures/torch_nn/gen.py`),
[python_torch_nn2.rs](../crates/zipp-vm/tests/python_torch_nn2.rs) (3-D
layers, bicubic/antialias, spectral norm, parametrizations and Lazy modules),
[python_torch_dtypes.rs](../crates/zipp-vm/tests/python_torch_dtypes.rs),
[python_torch_linalg.rs](../crates/zipp-vm/tests/python_torch_linalg.rs),
[python_torch_distributions.rs](../crates/zipp-vm/tests/python_torch_distributions.rs),
[python_torch_amp.rs](../crates/zipp-vm/tests/python_torch_amp.rs),
[python_torch_gpu2.rs](../crates/zipp-vm/tests/python_torch_gpu2.rs),
[python_torch_gpu3.rs](../crates/zipp-vm/tests/python_torch_gpu3.rs),
[python_torch_nn3.rs](../crates/zipp-vm/tests/python_torch_nn3.rs) (grid_sample,
CTC, fractional pooling, conv padding modes, parallel wrappers, conv second
derivatives; fixtures regenerated by `fixtures/torch_nn3/gen.py`),
[python_torch_complex.rs](../crates/zipp-vm/tests/python_torch_complex.rs),
[python_torch_fft.rs](../crates/zipp-vm/tests/python_torch_fft.rs),
[python_torch_sparse.rs](../crates/zipp-vm/tests/python_torch_sparse.rs) (fixtures
regenerated by `fixtures/torch_sparse/gen.py`),
[python_torch_quant.rs](../crates/zipp-vm/tests/python_torch_quant.rs) (fixtures
regenerated by `fixtures/torch_quant/gen.py` under a PyTorch with the x86
quantized engine, e.g. Linux/WSL),
[python_torch_distributed.rs](../crates/zipp-vm/tests/python_torch_distributed.rs)
(fixtures regenerated by `fixtures/torch_distributed/gen.py` under a CPU-only
build),
[python_torch_optim.rs](../crates/zipp-vm/tests/python_torch_optim.rs) and
[python_torch_gpu.rs](../crates/zipp-vm/tests/python_torch_gpu.rs).

### Reduced-precision and small integer dtypes

`torch.float16` (`torch.half`) and `torch.bfloat16` have PyTorch's value
semantics: every operation that produces one rounds each result to the format
(round to nearest even; float16 keeps subnormals and overflows to infinity),
and conversions from float64 go through float32 as `c10::Half` does. Each takes two bytes per element: a float16 storage is a `Float16Array` and a
bfloat16 one a `Uint16Array` of the upper 16 bits of the float32 value, so a
tensor uses half of float32's memory; kernels compute in float as before and
round each result to the format once. Checkpoints read and write the 2-byte
elements directly. `view(dtype)` among float16, bfloat16 and int16 (and between
uint8 and int8) shares the storage as PyTorch's does, though the view keeps its
own version counter; other pairs of one element size copy the bytes, and a view
to a different element size rescales the last dimension. Promotion follows PyTorch: float16 with float32 is float32,
float16 with bfloat16 is float32, an integer tensor with float16 is float16,
uint8 with int8 is int16. With a Python scalar, add/sub/pow/remainder/atan2
first round the scalar to the format, mul/div keep it in float, `s / x` is
`x.reciprocal() * s`, and `add(..., alpha=)` rounds alpha to the format and
forms the product in float; lerp/addcmul/addcdiv compute in float and round
once. sum/mean/cumsum/var/std/norm accumulate in double and round once; prod
rounds every partial product to the format, as PyTorch does. matmul, softmax
and log_softmax agree with PyTorch within one ulp of the format (PyTorch's
kernels sum in float in a blocked order). `rand` draws 11 (float16) or 8
(bfloat16) bits per element from the CPU stream and `random_()` spans
[0, 2**11] / [0, 2**8], matching PyTorch. `torch.optim` updates
float16/bfloat16 parameters with PyTorch's single-tensor sequence of in-place
`add_(alpha=)`, `lerp_`, `addcmul_` and `addcdiv_`, matching PyTorch bit for bit
over the elements its vectorized CPU loop covers (PyTorch's scalar tail, the
last `numel % 32` elements on AVX-512, can differ by an ulp); float32/float64
updates are unchanged. `torch.compile` graphs
remain float32 only. `torch.int8` and `torch.int16` (`torch.short`) wrap modulo
2**8 / 2**16 as uint8 does, refuse out-of-range Python values when a tensor is
created or filled (as PyTorch does), and cannot be used as index tensors.
uint16/uint32/uint64 are not provided.

## CPU Conv2d

`torch.nn.Conv2d` and `torch.nn.functional.conv2d` support float32/float64,
NCHW batches or an unbatched CHW input, rectangular kernels, integer/pair stride,
symmetric zero padding, dilation and groups (including depthwise convolution).
Input, weight and optional bias gradients participate in eager autograd and
ordinary optimizers. Try the [ordinary Torch CPU training example](../examples/python/conv2d/main.py)
with `zipp py examples/python/conv2d`. Numeric and string (`'same'`, `'valid'`)
padding are supported; `nn.Conv1d/2d/3d` accept `padding_mode` `'zeros'`,
`'reflect'`, `'replicate'` and `'circular'` (as PyTorch, the non-zero modes pad
with `F.pad` and convolve with padding 0); transposed convolutions accept
`'zeros'` only, as in PyTorch. `nn.Conv1d`/`F.conv1d` support stride,
dilation, groups, `'same'` and unbatched input; `nn.ConvTranspose1d/2d/3d` support
stride, padding, output_padding, groups and dilation. GPU compilation does not
support convolution yet.

`nn.Conv3d`/`F.conv3d` (stride, padding including `'same'`/`'valid'`,
dilation, groups, bias, unbatched input) and `nn.ConvTranspose3d`/
`F.conv_transpose3d` (stride, padding, output_padding, `output_size`, groups,
dilation) each run as one native 2-D convolution: the kernel's depth taps are
stacked into the channels (a transposed convolution first zero-inserts its
depth stride), so autograd flows through the stacking. Transposed
convolutions accept `output_padding` at or above the stride when the dilation
is larger, as PyTorch does; one whose output would have a zero-size dimension
raises where PyTorch returns an empty tensor.

The [reference fixture](../crates/zipp-vm/tests/fixtures/torch_conv2d.py) compares
four forward/input-gradient/weight-gradient/bias-gradient cases against PyTorch
CPU and verifies an SGD update reduces a simple loss.

## Integer precision and storage semantics

The CPU `int64` and `int32` storage currently uses Float64Array. All integers
within [-2**53, 2**53] can be represented exactly; values outside that range can
round. Arithmetic does not provide true signed 64-bit or 32-bit overflow
semantics. Keep indices and integer data within the exact range. Full-width
integer storage is future work, not a current compatibility claim. Python
scalars and 0-d tensors take part in type promotion as PyTorch's `result_type`
defines (float32 times a 0-d float64 is float32, int32 + 1 is int32); uint8,
int8 and int16 arithmetic wraps modulo 2**8, 2**8 and 2**16, while int32 and
int64 do not wrap.

Eager float32 `matmul`/`@` accumulates each dot product in float64 and rounds
once when storing the float32 result, so results can differ from PyTorch's
float32 kernels (and from earlier Zipp builds) in the last bits. The GPU graph
reference (`zipp_gpu`) keeps float32 rounding after every step, matching the
WebGPU/WebGL2 backends. Eager float32 `sum`, `mean`, `prod` (whole and per
dimension) and `var`/`std` likewise accumulate in float64 and round once, so
large float32 reductions are closer to the exact value than PyTorch's float32
kernels.

General strided views are also future work. `view`/`reshape`,
`unsqueeze`/`squeeze`/`flatten`, `detach()` and `.data` share storage, and
in-place operations (arithmetic, `fill_`, `copy_`, `zero_`, indexed assignment,
the `*_` family) write into it, so every such alias sees the change; an
in-place operation on a view of a non-leaf tensor carries into its base's
history. Slices, integer indexing (`x[0]`), permutations, transposes (including
`t_`/`transpose_`), expand and advanced indexing still copy, so mutating a
slice does not update its parent (`x[1][2] = v` does not update `x`; write
`x[1, 2] = v`). Programs that depend on aliasing through those, offsets,
noncontiguous strides or overlapping writes need adapting.

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
