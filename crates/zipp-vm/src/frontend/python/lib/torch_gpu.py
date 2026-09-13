"""Explicit asynchronous GPU inference for the Zipp torch compatibility layer.

This records supported operations into zipp_gpu; it is not TorchInductor and
does not compile arbitrary Python, fuse kernels or provide GPU autograd.
"""
import torch
from zipp_gpu import Graph, Tensor as GraphTensor


class _Capture:
    def __init__(self):
        self.graph = Graph()
        self.inputs = {}

    def tensor(self, value):
        if isinstance(value, _Tensor):
            if value._capture is not self:
                raise ValueError("Cannot mix separate compiled GPU calls")
            return value
        if isinstance(value, torch.Tensor):
            if value.dtype != torch.float32:
                raise TypeError("zipp_gpu compilation requires float32 tensors")
            key = id(value)
            if key not in self.inputs:
                self.inputs[key] = (value, _Tensor(self, self.graph.tensor(value.detach().tolist())))
            return self.inputs[key][1]
        return _Tensor(self, self.graph.tensor(value))


class _Tensor:
    def __init__(self, capture, value):
        self._capture = capture
        self._value = value
        self._zipp_graph = True
        self.shape = torch.Size(value.shape)
        self.dtype = torch.float32

    def _binary(self, operation, other, reverse=False):
        # Linear's vector bias is expanded once on upload, not by CPU evaluation
        # of the matrix result. General broadcasting is deliberately unsupported.
        if isinstance(other, torch.Tensor) and len(self.shape) == 2 and tuple(other.shape) == (self.shape[1],):
            row = other.detach().tolist()
            if other.dtype != torch.float32:
                raise TypeError("zipp_gpu compilation requires float32 tensors")
            other = [row for _ in range(self.shape[0])]
        rhs = self._capture.tensor(other)._value
        a, b = (rhs, self._value) if reverse else (self._value, rhs)
        result = a + b if operation == "add" else a - b if operation == "sub" else a * b
        return _Tensor(self._capture, result)

    def __add__(self, other): return self._binary("add", other)
    def __radd__(self, other): return self._binary("add", other, True)
    def __sub__(self, other): return self._binary("sub", other)
    def __rsub__(self, other): return self._binary("sub", other, True)
    def __mul__(self, other): return self._binary("mul", other)
    def __rmul__(self, other): return self._binary("mul", other, True)
    def __matmul__(self, other):
        return _Tensor(self._capture, self._value @ self._capture.tensor(other)._value)
    def __rmatmul__(self, other):
        return _Tensor(self._capture, self._capture.tensor(other)._value @ self._value)
    def relu(self): return _Tensor(self._capture, self._value.relu())
    def sum(self, dim=None, keepdim=False, dtype=None):
        if dim is not None or keepdim or dtype is not None:
            raise NotImplementedError("GPU sum currently reduces all elements")
        return _Tensor(self._capture, self._value.sum())
    def mean(self, dim=None, keepdim=False, dtype=None):
        if dim is not None or keepdim or dtype is not None:
            raise NotImplementedError("GPU mean currently reduces all elements")
        return self.sum() * (1.0 / self.shape.numel())
    def __bool__(self):
        raise TypeError("Compiled GPU tensors cannot control Python branches")


class GPUResult:
    """A recorded inference call. Use submit(callback) to execute and read it."""
    def __init__(self, value):
        self.shape = value.shape
        self._value = value
        self.backend = None
        self.stats = None

    def submit(self, callback, on_error=None):
        if not callable(callback) or (on_error is not None and not callable(on_error)):
            raise TypeError("submit requires a callback and optional error callback")
        def arrived(result):
            self.backend = result["backend"]
            self.stats = result.get("stats")
            output = result["outputs"]["result"]
            value = torch.tensor(output["data"], dtype=torch.float32).reshape(output["shape"])
            callback(value)
        self._value._capture.graph.submit(arrived, on_error, result=self._value._value)

    def backward(self, *args, **kwargs):
        raise NotImplementedError("Compiled GPU inference has no autograd; use eager torch for CPU training")


def compile(model=None, *, backend="zipp_gpu"):
    if backend != "zipp_gpu":
        raise ValueError("The Zipp compile extension supports only backend='zipp_gpu'")
    if model is None:
        return lambda fn: compile(fn, backend=backend)
    if not callable(model):
        raise TypeError("torch.compile requires a callable or nn.Module")
    def invoke(*args, **kwargs):
        capture = _Capture()
        convert = lambda value: capture.tensor(value) if isinstance(value, torch.Tensor) else value
        with torch.no_grad():
            output = model(*[convert(value) for value in args], **{key: convert(value) for key, value in kwargs.items()})
        if not isinstance(output, _Tensor):
            raise TypeError("Compiled GPU inference must return one supported graph tensor")
        return GPUResult(output)
    return invoke
