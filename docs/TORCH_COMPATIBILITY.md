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
add/subtract/multiply/divide (operators and Torch functions, including tensor by
tensor and `1/x`), ReLU, `sqrt`/`rsqrt`/`abs`/`square` and `**` with the
exponents 2, 1, 0.5, -1 and -0.5, matrix multiplication (including 1-D inputs
to `nn.Linear` and matrix-vector, vector-matrix and dot products), sum/mean over
all elements or over any set of dimensions, `softmax`/`log_softmax` over any
dimension, `reshape`/`view`/`flatten`/`squeeze`/`unsqueeze`/`permute`/`transpose`/`.t()`/`.T`,
`.detach()` as a stop-gradient, indexing that only reshapes (`None`, full
slices, `...`, index 0 of a size-1 dimension), and `nn.Linear`/`nn.ReLU`/`nn.Sequential`
combinations. Comparisons (`<`, `<=`, `>`, `>=`, `==`, `!=`, `torch.gt`/`eq`/...,
also with an eager tensor on the left), `torch.where`/`Tensor.where`,
`masked_fill`, `clamp`/`clip`/`clamp_min`/`clamp_max` (scalar or tensor bounds),
`maximum`/`minimum` (and two-tensor `torch.max`/`torch.min`),
`F.hardtanh`/`F.relu6`/`F.leaky_relu` (and their `nn` modules), mask logic (`&`,
`|`, `^`, `~`, `logical_and`/`or`/`xor`/`not`) and dropout record graph protocol
version 3 operations (see *Masks, clamping and dropout*). Element-selecting
slices cannot be expressed by the graph protocol and raise
`NotImplementedError` naming the operation.
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
permute/transpose, sum/mean, matmul, maximum/minimum, softmax/log_softmax, clone and a float32
`to`; any other operation raises `NotImplementedError` naming it rather than
becoming a separate leaf with no gradient. `p.detach()` and `p.data` read the
parameter's input.

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
anything newer records version 2, or version 3 once it uses a comparison,
`where`, clamping, `maximum`/`minimum` or dropout, which natively runs on `zipp_gpu`'s
pure-Python float32 reference (the same numbers, not fast).

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
  argument read as a mask is fed each step. A step that draws random numbers
  on the CPU (`torch.rand`/`randn`/`randint`/`normal`) is refused for the same
  reason; dropout and `torch.rand_like`/`torch.bernoulli` of graph tensors draw
  on the device, afresh every step. Per-call `torch.compile` accepts all of
  these.
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
  runs before the call returns. A `steps()` run submits at most 64 steps. Fed
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
and the gradient is masked and scaled identically. Eval mode and p = 0 are the
identity and draw nothing; p = 1 multiplies by 0; `dropout1d`/`2d` (and
unbatched `3d`) drop whole channels. Each dropout call takes one 32-bit seed
from torch's default generator, so `torch.manual_seed` makes compiled calls
reproducible. Per-call compilation draws new seeds every call; a prepared
session draws them at `prepare()` and advances the step every executed step,
so every step has a fresh mask. `torch.rand_like`/`torch.bernoulli` of graph
tensors use the same generator. In-place dropout of a graph tensor is refused,
and `randn` has no device form, because a normal draw needs transcendental
functions that backends round differently.

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
  softmax operations (operations whose gradient is a dedicated kernel, such as
  convolution, `prod`, `cumprod` and the fused nn losses, raise
  `NotImplementedError` under `create_graph`); `autograd.grad`,
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
  by the first forward or by loading a state dict. Module attribute
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
  `type(b)` differs. `DataParallel`, `SyncBatchNorm`, `grid_sample`, `ctc_loss`
  and fractional max pooling are not implemented.
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
- **`torch.linalg`.** `det`, `slogdet`, `inv`/`inv_ex`, `solve`/`solve_ex`
  (vector and broadcast right-hand sides, `left=False`), `solve_triangular`,
  `cholesky`/`cholesky_ex`, `qr` (`reduced`/`complete`/`r`), `eigh`/`eigvalsh`
  (`UPLO`), `svd`/`svdvals` (`full_matrices`), `pinv` (`hermitian`,
  `atol`/`rtol`), `matrix_rank`, `lstsq`, `norm`/`vector_norm`/`matrix_norm`
  (every order), `cond`, `matrix_power` (negative powers), `matrix_exp`,
  `cross`, `multi_dot`, `vecdot`, `diagonal`, `vander`, `householder_product`,
  `tensorinv`/`tensorsolve` and `lu`/`lu_factor`/`lu_solve` (no gradient), with
  the top-level `torch.det`/`logdet`/`slogdet`/`inverse`/`cholesky`/
  `cholesky_solve`/`cholesky_inverse`/`qr`/`svd` (returning V)/`pinverse`/
  `matrix_power`/`matrix_exp` and tensor methods, over batches `[..., m, n]`
  of float32/float64. Factorizations run in double precision on the CPU
  (partial-pivoting LU, Householder QR with LAPACK's signs, Jacobi `eigh` and
  SVD, Francis QR for `eig`) and round once to the input dtype; gradients are
  PyTorch's formulas as tensor operations, so they batch, broadcast and support
  `create_graph=True`. Values and gradients match PyTorch 2.11 to 1e-12 in
  float64 (2e-6 in float32); eigenvector and singular-vector signs may differ.
  Failures raise `torch.linalg.LinAlgError` with PyTorch's messages. With no
  complex dtype, `eig`/`eigvals` return real tensors for real spectra and raise
  `NotImplementedError` otherwise. These are small-matrix algorithms (a 64x64
  `svd` takes about 4 s).
- **`torch.distributions`.** Normal, LogNormal, Uniform, Bernoulli,
  Categorical, OneHotCategorical (and StraightThrough), Binomial, Multinomial,
  Poisson, Geometric, Exponential, Laplace, Cauchy, Gamma, Chi2, Beta,
  Dirichlet, StudentT, HalfNormal, HalfCauchy, MultivariateNormal (covariance,
  precision or `scale_tril`), LowRankMultivariateNormal, Independent,
  MixtureSameFamily, TransformedDistribution (Exp, Affine, Sigmoid, Tanh,
  Softmax, Softplus, Power, Abs, StickBreaking, Reshape, Independent and
  Compose transforms), RelaxedBernoulli and RelaxedOneHotCategorical, with
  PyTorch's shapes, `expand`, constraints, argument and sample validation,
  `biject_to`/`transform_to`, and `kl_divergence`/`register_kl` for the pairs
  PyTorch registers among these. `log_prob`, `entropy`, `cdf`/`icdf`, moments
  and KL values and gradients match PyTorch 2.11 to 1e-12 in float64. `rsample`
  is reparameterised where PyTorch's is; Gamma, Chi2, StudentT, Beta and
  Dirichlet use the implicit Gamma gradient, with Dirichlet and Beta
  differentiated through normalised Gamma draws. Samples come from torch's
  generator, so they follow `manual_seed` but not PyTorch's streams.
  `constraints`, `transforms` and `kl` are attributes of `torch.distributions`,
  not importable submodules. Gumbel, Pareto, Weibull, NegativeBinomial and
  Wishart are not implemented.
- **`torch.amp`.** `autocast` (context manager and decorator; `torch.autocast`
  is the same class), `is_autocast_available`, `custom_fwd`/`custom_bwd`,
  `GradScaler` and the deprecated `torch.cuda.amp` spellings. `GradScaler("cpu")`
  follows PyTorch exactly (float32 scale, per-optimizer inf/NaN checks, skipped
  steps, backoff/growth/interval, `update(new_scale)`, `state_dict`). CPU
  `autocast` keeps PyTorch's autocast state but does not re-type operations:
  matmul, linear and convolution inside it compute in the tensors' own dtypes.
- **Checkpoints.** `torch.save` and `torch.load` implement PyTorch's ZIP
  checkpoint format: pickle reads protocols 0-5 and writes protocol 2, and
  loading uses the same allowlist as PyTorch's `weights_only=True`. Tensors,
  `nn.Parameter`, `torch.Size`, `torch.device`, dtypes, bytes, bytearray, set
  and OrderedDict round-trip with PyTorch in both directions; float16,
  bfloat16, int8 and int16 tensors are written and read as PyTorch's
  `HalfStorage`/`BFloat16Storage`/`CharStorage`/`ShortStorage` records. Strided checkpoint
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
[python_torch_optim.rs](../crates/zipp-vm/tests/python_torch_optim.rs) and
[python_torch_gpu.rs](../crates/zipp-vm/tests/python_torch_gpu.rs).

### Reduced-precision and small integer dtypes

`torch.float16` (`torch.half`) and `torch.bfloat16` have PyTorch's value
semantics: every operation that produces one rounds each result to the format
(round to nearest even; float16 keeps subnormals and overflows to infinity),
and conversions from float64 go through float32 as `c10::Half` does. Storage
stays a `Float32Array` (every value of either format is a float32), so memory
use is not halved. Promotion follows PyTorch: float16 with float32 is float32,
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
[0, 2**11] / [0, 2**8], matching PyTorch. `torch.optim`'s update
(`p - lr * grad`) can round once more than PyTorch's fused `add_`, so an
optimizer step on half parameters may differ by an ulp. `torch.compile` graphs
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
padding are supported; `padding_mode` must be `'zeros'` (`F.pad` itself supports
reflect, replicate and circular). `nn.Conv1d`/`F.conv1d` support stride,
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
