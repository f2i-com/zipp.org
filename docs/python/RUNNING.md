# Running Python on ZIPP

## Run Python (experimental)

The CLI also runs Python: Zipp's own Python 3 implementation, parsed by its
own front end ([`zipp-pyparse`](../../crates/zipp-vm/src/lang/python/parser)) and lowered
straight to the engine's register bytecode (no transpilation to JavaScript and
no second interpreter), so a `.py` file runs on the same VM. On the command
line hot Python loops are JIT-compiled (x86-64), and over the 36 programs of
the Python benchmark suite Zipp runs at about 1.1x CPython 3.13's time (its
30 small programs level with CPython); fifteen of them, including
`range_loop`, `float_arith`, `int_arith`, `global_read`, `dict_get` and
`json_roundtrip`, take less time than in CPython. WebAssembly
and budgeted embedders run the same code interpreted. Compiled code is
cached per user, so a hello-world starts in about 20 ms. Save this as
`fib.py`:

```python
def fib(n):
    a = 0
    b = 1
    for i in range(n):
        a, b = b, a + b
    return a

print(fib(30))
```

```sh
zipp py fib.py                 # 832040
zipp run fib.py                # frontend chosen by extension, shebang or directive
zipp run tool                  # an extensionless `#!/usr/bin/env python3` script
zipp run --lang=python -       # from standard input (no project folder)
```

Classes (including metaclasses, descriptors and `__slots__`), exceptions
with full tracebacks, generators, closures, comprehensions, `match`
statements, f-strings, the builtin types and a set of standard-library
modules (`math`, `json`, `re`, `collections`, `itertools`, `functools`,
`dataclasses`, `enum`, `contextlib`, `typing`, `struct`, `hashlib`, ...)
all work; `async` does not yet. Semantics are checked
differentially against CPython: the 195 programs of `tests/python_corpus/*.py`
must print exactly what CPython 3.13 prints, in the default, no-fast-path and
no-JIT modes.

A folder runs as a project: `zipp py examples/python/project` runs its
`main.py`, and `zipp py lab/train.py --steps 20` runs one script of a
folder with arguments. Every file of the folder (subfolders included, up to
8 MiB each and 64 MiB in total) is loaded into the program's virtual
filesystem, so `open()`, `os`, `os.path`, `pathlib` and `json.load` see the
project's data; `.py` files are modules and packages by folder
(`legacy/fast_memory.py` is `legacy.fast_memory`, with or without an
`__init__.py`), and a package's own modules import each other relatively
(`from .graph import Graph`) as CPython resolves them; `sys.argv` carries the
arguments; and files the program
writes are copied back under the folder when it finishes (never over a file
it could not see, such as one in `dist/`, a dot-folder or over the limits).
Any script name runs, extensionless shebang scripts included, and output
appears as the program prints it. A `test_*.py`
entry runs its tests through the bundled `pytest` subset. The bundled
library also includes a `torch` subset (tensors over typed arrays with
reverse-mode autograd, `nn`, `nn.functional`, `optim` and its schedulers,
`torch.utils.data`, `linalg`, `fft`, `distributions`, half-precision, complex,
sparse and quantized tensors, `save`/`load` in supported PyTorch checkpoint
layouts) that runs on the engine's native CPU kernels, so
supported ML code can train and evaluate inside Zipp. Eager execution is CPU;
`torch.compile` records supported inference and training steps for the GPU
(dense layers with relu, gelu, sigmoid or tanh, softmax, MSE or fused
cross-entropy, SGD with momentum, Adam or AdamW), and `compiled.prepare()`
keeps the weights and optimizer state on the device between steps (see
[Train a small model on the browser GPU](../../docs/GPU.md#train-a-small-model-on-the-browser-gpu)).
Natively, `zipp py` runs the same recorded graphs on a hardware GPU through wgpu when one is present, synchronously and with the same results, and on the engine's tensor kernels otherwise ([Native GPU](../python/TORCH_COMPATIBILITY.md#native-gpu-zipp-py)). The
subset covers enough of what real model code reaches for -- `torch.autocast`
among it, a no-op here since every tensor is float32 -- that Hugging Face's
`modeling_qwen3.py`, `modeling_qwen2.py`, `modeling_llama.py` and
`modeling_mistral.py` compile and run exactly as published, within 1.8e-7 of
transformers ([interop](../../crates/zipp-wasm/model-plugins/interop/README.md)). The scope
matrix, limits and the bytecode design are in
[docs/python/FRONTEND.md](../python/FRONTEND.md). The
feature is on by default in the CLI (`--no-default-features` builds the
JavaScript-only binary) and off by default in the `zipp-vm` library and the
WebAssembly package, which offers it as a
[separate build variant](../../crates/zipp-wasm/README.md#build-variants-zipp-lite-javascript-python-and-the-torch-package),
with torch either built in or added at run time as a separate package.

Python programs can also compute on the GPU in the browser: the bundled
`zipp_gpu` library records a float32 graph (broadcasting arithmetic, `@`,
relu/gelu/sigmoid/tanh, softmax, reductions, fused cross-entropy, SGD and
Adam update steps, a Conway-life step) and `submit`s it, and the host runs it
through WebGPU, WebGL2, compiled WebAssembly kernels or a JavaScript
reference, calling the program back with the outputs. `Graph.prepare` turns
a graph into a session: inputs are fed per step, carried tensors such as
weights and optimizer state stay on the device, and several steps go in one
submission (0.79 ms per step on an RTX 5090 at eight steps per run, against
3.6 ms for one step per submission). A run that fails after device work
began poisons its session - only `dispose()` remains - rather than letting a
retry run on state of no particular step. Natively the same code runs on a
GPU when one is present, and otherwise evaluates on the CPU, bit for bit the
same as the reference.
A weight input can also be a checkpoint's own Q4_K or Q6_K blocks, decoded
inside the matmul on every backend, and `matmul_fixed` computes a product in
integers, the same bits on every backend that runs it
([FIXED-POINT.md](../../crates/zipp-wasm/gpu-lab/docs/FIXED-POINT.md)). The
WebAssembly kernels are `no_std` Rust, and the committed binary is rebuilt and
compared in CI.
See [crates/zipp-wasm/README.md](../../crates/zipp-wasm/README.md#gpu-compute-for-python-programs).

There is also a local [playground](../../crates/zipp-wasm/playground/README.md)
that runs a folder of Python or JavaScript files on the WebAssembly engine:
open or drop a whole project folder (subfolders, data files and binary
checkpoints included), browse it in a file tree, pick any script as the
entry, give it arguments, and run it in the browser; files the program
writes show up in the tree. It has an editor, a console and a canvas the
program draws on through a small `ui` API (`draw`/`update`/`on_click`/
`on_key` hooks for animation and input), and a GPU sample that steps life
on the compute backend every frame:

```sh
cd crates/zipp-wasm && ./build-variants.sh all && node playground/serve.cjs
```
