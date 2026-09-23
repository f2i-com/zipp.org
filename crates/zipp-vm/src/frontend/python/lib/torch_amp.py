"""torch.amp for Zipp: autocast, GradScaler and the custom_fwd/custom_bwd
decorators, with PyTorch's semantics on a machine without CUDA.

* `autocast(device_type, dtype=None, enabled=True, cache_enabled=None)` is
  a context manager and decorator that keeps PyTorch's autocast state
  (`torch.is_autocast_enabled(device)`, `torch.get_autocast_dtype(device)`,
  nesting and restore-on-exit). Zipp has no CUDA, so `device_type="cuda"`
  warns and disables itself exactly as PyTorch does when CUDA is not
  available. On the CPU the state is recorded but operations are not
  re-typed: eager kernels keep computing in the tensors' own dtypes (float32
  by default), so an autocast region gives full-precision results where
  PyTorch would round matmul/linear/conv inputs to bfloat16.
* `GradScaler(device="cuda", ...)` follows torch/amp/grad_scaler.py: with
  `device="cuda"` it warns and disables itself (CUDA is unavailable), after
  which `scale()` returns its argument, `step()` is `optimizer.step()` and
  `update()` does nothing. `device="cpu"` runs the full machinery: a float32
  scale, unscale_ with inf/NaN detection per optimizer, skipped steps,
  backoff/growth/interval counting and state_dict.
"""
import math as _math
import sys as _sys
from collections import defaultdict as _defaultdict
from enum import Enum as _Enum

import torch

__all__ = ["autocast", "GradScaler", "OptState", "custom_fwd", "custom_bwd", "is_autocast_available"]

# Device types PyTorch parses; the first group has an autocast backend.
_AUTOCAST_DEVICES = ("cpu", "cuda", "mps", "xpu", "hpu", "xla", "mtia", "maia", "ipu", "privateuseone")
_KNOWN_DEVICES = ("cpu", "cuda", "ipu", "xpu", "mkldnn", "opengl", "opencl", "ideep", "hip", "ve", "fpga", "maia", "xla",
                  "lazy", "vulkan", "mps", "meta", "hpu", "mtia", "privateuseone")


def _warn(message, category="UserWarning"):
    print("%s: %s" % (category, message), file=_sys.stderr)


def _device_kind(device_type):
    kind = device_type.split(":")[0]
    if kind not in _KNOWN_DEVICES:
        raise RuntimeError("Expected one of %s device type at start of device string: %s" % (", ".join(_KNOWN_DEVICES), device_type))
    return kind


def is_autocast_available(device_type):
    """Whether autocast has a backend for `device_type` (as PyTorch
    answers; on Zipp only the CPU state takes effect, see autocast)."""
    return _device_kind(device_type) in _AUTOCAST_DEVICES


# ---- the autocast state (PyTorch keeps it per thread; Zipp has one) ----------------------
_enabled = {}
_dtypes = {}
_nesting = [0]
_cache_enabled = [True]


def _default_fast_dtype(device_type):
    if device_type == "cpu":
        return torch.bfloat16
    return getattr(torch, "float16", torch.half)


def is_autocast_enabled(device_type=None):
    """`torch.is_autocast_enabled(device_type)`; with no argument, CUDA's
    state (PyTorch's legacy meaning)."""
    return bool(_enabled.get("cuda" if device_type is None else _device_kind(device_type), False))


def set_autocast_enabled(device_type, enabled=None):
    if enabled is None:
        # The legacy one-argument form sets CUDA's state.
        device_type, enabled = "cuda", device_type
    _enabled[_device_kind(device_type)] = bool(enabled)


def get_autocast_dtype(device_type):
    kind = _device_kind(device_type)
    found = _dtypes.get(kind)
    return _default_fast_dtype(kind) if found is None else found


def set_autocast_dtype(device_type, dtype):
    _dtypes[_device_kind(device_type)] = dtype


def is_autocast_cache_enabled():
    return _cache_enabled[0]


def set_autocast_cache_enabled(enabled):
    _cache_enabled[0] = bool(enabled)


def clear_autocast_cache():
    return None


def autocast_increment_nesting():
    _nesting[0] += 1
    return _nesting[0]


def autocast_decrement_nesting():
    _nesting[0] -= 1
    return _nesting[0]


def get_autocast_cpu_dtype():
    return get_autocast_dtype("cpu")


def get_autocast_gpu_dtype():
    return get_autocast_dtype("cuda")


def is_autocast_cpu_enabled():
    return is_autocast_enabled("cpu")


def _supported_dtypes(device_type):
    if device_type == "cpu":
        return [torch.bfloat16, getattr(torch, "float16", torch.half)]
    return [getattr(torch, "float16", torch.half), torch.bfloat16, torch.float32]


def _device_available(device_type):
    if device_type == "cpu":
        return True
    if device_type == "cuda":
        return torch.cuda.is_available()
    return False


def _wraps(fn, wrapper):
    for attr in ("__name__", "__qualname__", "__doc__", "__module__"):
        try:
            setattr(wrapper, attr, getattr(fn, attr))
        except (AttributeError, TypeError):
            pass
    wrapper.__wrapped__ = fn
    return wrapper


def autocast_decorator(autocast_instance, func):
    def decorate_autocast(*args, **kwargs):
        with autocast_instance:
            return func(*args, **kwargs)
    return _wraps(func, decorate_autocast)


# The no-op torch.autocast stub in torch.py, when it is a class.
_StubAutocast = torch.autocast if isinstance(torch.autocast, type) else object


class autocast(_StubAutocast):
    """PyTorch's autocast context manager/decorator (a subclass of the
    `torch.autocast` stub, so isinstance holds both ways). See the module
    docstring for what an enabled region does on Zipp."""

    def __init__(self, device_type, dtype=None, enabled=True, cache_enabled=None):
        if not isinstance(device_type, str):
            raise ValueError("Expected `device_type` of type `str`, got: `%s`" % (type(device_type),))
        self.fast_dtype = get_autocast_dtype(device_type) if dtype is None else dtype
        self.device = device_type
        if not is_autocast_available(self.device):
            raise RuntimeError("User specified an unsupported autocast device_type '%s'" % self.device)
        self._cache_enabled = is_autocast_cache_enabled() if cache_enabled is None else cache_enabled
        device_name = self.device.upper()
        if enabled:
            if self.device == "cuda":
                if not _device_available("cuda"):
                    _warn("CUDA is not available or torch_xla is imported. Disabling autocast.")
                    enabled = False
            elif not _device_available(self.device):
                _warn("User provided device_type of '%s', but it is not available on Zipp. Disabling autocast." % self.device)
                enabled = False
            elif self.fast_dtype not in _supported_dtypes(self.device):
                _warn("In %s autocast, but the target dtype is not supported. Disabling autocast.\n%s Autocast only supports dtypes of %s currently."
                      % (device_name, device_name, ", ".join(str(d) for d in _supported_dtypes(self.device))))
                enabled = False
        self._enabled = bool(enabled)
        # The attributes of the torch.autocast stub.
        self.device_type = device_type
        self.dtype = self.fast_dtype
        self.enabled = self._enabled

    def __enter__(self):
        self.prev_cache_enabled = is_autocast_cache_enabled()
        self.prev = is_autocast_enabled(self.device)
        self.prev_fastdtype = get_autocast_dtype(self.device)
        set_autocast_enabled(self.device, self._enabled)
        set_autocast_dtype(self.device, self.fast_dtype)
        autocast_increment_nesting()
        set_autocast_cache_enabled(self._cache_enabled)
        return self

    def __exit__(self, exc_type=None, exc_val=None, exc_tb=None):
        if autocast_decrement_nesting() == 0:
            clear_autocast_cache()
        set_autocast_enabled(self.device, self.prev)
        set_autocast_dtype(self.device, self.prev_fastdtype)
        set_autocast_cache_enabled(self.prev_cache_enabled)
        return False

    def __call__(self, func):
        return autocast_decorator(self, func)


def _cast(value, device_type, dtype):
    if isinstance(value, torch.Tensor):
        eligible = value.is_floating_point() and value.device.type == device_type and value.dtype != torch.float64
        return value.to(dtype) if eligible else value
    if isinstance(value, (str, bytes)):
        return value
    if isinstance(value, dict):
        return {_cast(k, device_type, dtype): _cast(v, device_type, dtype) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        items = [_cast(v, device_type, dtype) for v in value]
        return tuple(items) if isinstance(value, tuple) else items
    return value


def custom_fwd(fwd=None, *, device_type, cast_inputs=None):
    """Decorator for a custom autograd Function's forward: records the
    autocast state for custom_bwd and, with `cast_inputs`, casts floating
    inputs and runs forward with autocast disabled."""
    if not isinstance(device_type, str):
        raise ValueError("Expected `device_type` of type `str`, got: `%s`" % (type(device_type),))
    if fwd is None:
        return lambda f: custom_fwd(f, device_type=device_type, cast_inputs=cast_inputs)

    def decorate_fwd(*args, **kwargs):
        args[0]._dtype = get_autocast_dtype(device_type)
        if cast_inputs is None:
            args[0]._fwd_used_autocast = is_autocast_enabled(device_type)
            return fwd(*args, **kwargs)
        in_autocast = is_autocast_enabled(device_type)
        args[0]._fwd_used_autocast = False
        if in_autocast:
            with autocast(device_type=device_type, enabled=False):
                return fwd(*_cast(args, device_type, cast_inputs), **_cast(kwargs, device_type, cast_inputs))
        return fwd(*args, **kwargs)
    return _wraps(fwd, decorate_fwd)


def custom_bwd(bwd=None, *, device_type):
    """Decorator for a custom autograd Function's backward: runs it with the
    autocast state its forward saw."""
    if not isinstance(device_type, str):
        raise ValueError("Expected `device_type` of type `str`, got: `%s`" % (type(device_type),))
    if bwd is None:
        return lambda f: custom_bwd(f, device_type=device_type)

    def decorate_bwd(*args, **kwargs):
        with autocast(device_type=device_type, enabled=args[0]._fwd_used_autocast, dtype=args[0]._dtype):
            return bwd(*args, **kwargs)
    return _wraps(bwd, decorate_bwd)


# ---- GradScaler --------------------------------------------------------------------------
class OptState(_Enum):
    READY = 0
    UNSCALED = 1
    STEPPED = 2


def _refresh_per_optimizer_state():
    return {"stage": OptState.READY, "found_inf_per_device": {}}


class GradScaler:
    """Dynamic loss scaling, following torch.amp.GradScaler: `scale(loss)`,
    `unscale_(optimizer)`, `step(optimizer)` (skipped when a gradient is
    inf/NaN), `update(new_scale=None)`, `get_scale()`, `state_dict()` /
    `load_state_dict()`. With `device="cuda"` it disables itself (Zipp has
    no CUDA), as PyTorch does when CUDA is unavailable."""

    def __init__(self, device="cuda", init_scale=2.0 ** 16, growth_factor=2.0, backoff_factor=0.5, growth_interval=2000, enabled=True):
        self._device = device
        self._enabled = enabled
        if self._device == "cuda":
            if enabled and not torch.cuda.is_available():
                _warn("torch.cuda.amp.GradScaler is enabled, but CUDA is not available.  Disabling.")
                self._enabled = False
        if self._enabled:
            if growth_factor <= 1.0:
                raise AssertionError("The growth factor must be > 1.0.")
            if backoff_factor >= 1.0:
                raise AssertionError("The backoff factor must be < 1.0.")
            self._init_scale = init_scale
            self._scale = None
            self._growth_factor = growth_factor
            self._backoff_factor = backoff_factor
            self._growth_interval = growth_interval
            self._init_growth_tracker = 0
            self._growth_tracker = None
            self._per_optimizer_states = _defaultdict(_refresh_per_optimizer_state)

    def _check_scale_growth_tracker(self, funcname):
        fix = "This may indicate your script did not use scaler.scale(loss or outputs) earlier in the iteration."
        if self._scale is None:
            raise AssertionError("Attempted %s but _scale is None.  " % funcname + fix)
        if self._growth_tracker is None:
            raise AssertionError("Attempted %s but _growth_tracker is None.  " % funcname + fix)
        return (self._scale, self._growth_tracker)

    def _lazy_init_scale_growth_tracker(self, dev=None):
        if self._growth_tracker is not None:
            raise AssertionError("_growth_tracker initialized before _scale")
        self._scale = torch.full((), self._init_scale, dtype=torch.float32)
        self._growth_tracker = torch.full((), self._init_growth_tracker, dtype=torch.int32)

    def scale(self, outputs):
        """outputs times the current scale (a tensor, or tensors nested in
        lists/tuples); unchanged when disabled."""
        if not self._enabled:
            return outputs
        if isinstance(outputs, torch.Tensor):
            if self._scale is None:
                self._lazy_init_scale_growth_tracker(outputs.device)
            return outputs * self._scale

        def apply_scale(val):
            if isinstance(val, torch.Tensor):
                if self._scale is None:
                    self._lazy_init_scale_growth_tracker(val.device)
                return val * self._scale
            if isinstance(val, (list, tuple)):
                items = [apply_scale(v) for v in val]
                return tuple(items) if isinstance(val, tuple) else items
            if hasattr(val, "__iter__"):
                return map(apply_scale, val)
            raise ValueError("outputs must be a Tensor or an iterable of Tensors")
        return apply_scale(outputs)

    def _unscale_grads_(self, optimizer, inv_scale, found_inf, allow_fp16):
        seen = False
        with torch.no_grad():
            for group in optimizer.param_groups:
                for param in group["params"]:
                    if not isinstance(param, torch.Tensor):
                        raise AssertionError("expected param to be torch.Tensor, got %s" % type(param).__name__)
                    if param.grad is None:
                        continue
                    g = param.grad
                    if (not allow_fp16) and g.dtype.name == "float16":
                        raise ValueError("Attempting to unscale FP16 gradients.")
                    seen = True
                    # _amp_foreach_non_finite_check_and_unscale_: flag a
                    # non-finite value, then multiply in place.
                    if not bool(torch.isfinite(g).all()):
                        found_inf.fill_(1.0)
                    if inv_scale.item() != 1.0:
                        g.mul_(inv_scale)
        return {"cpu": found_inf} if seen else {}

    def unscale_(self, optimizer):
        """Divides the optimizer's gradients by the scale, in place, and
        records whether any is inf/NaN. Once per optimizer per step()."""
        if not self._enabled:
            return
        self._check_scale_growth_tracker("unscale_")
        optimizer_state = self._per_optimizer_states[id(optimizer)]
        if optimizer_state["stage"] is OptState.UNSCALED:
            raise RuntimeError("unscale_() has already been called on this optimizer since the last update().")
        elif optimizer_state["stage"] is OptState.STEPPED:
            raise RuntimeError("unscale_() is being called after step().")
        # The reciprocal in float64, rounded to float32, as PyTorch computes it.
        inv_scale = self._scale.double().reciprocal().float()
        found_inf = torch.full((), 0.0, dtype=torch.float32)
        optimizer_state["found_inf_per_device"] = self._unscale_grads_(optimizer, inv_scale, found_inf, False)
        optimizer_state["stage"] = OptState.UNSCALED

    def _maybe_opt_step(self, optimizer, optimizer_state, *args, **kwargs):
        retval = None
        if not sum(v.item() for v in optimizer_state["found_inf_per_device"].values()):
            retval = optimizer.step(*args, **kwargs)
        return retval

    def step(self, optimizer, *args, **kwargs):
        """unscale_(optimizer) unless already done, then optimizer.step()
        unless a gradient was inf/NaN. Returns what optimizer.step returns."""
        if not self._enabled:
            return optimizer.step(*args, **kwargs)
        if "closure" in kwargs:
            raise RuntimeError("Closure use is not currently supported if GradScaler is enabled.")
        self._check_scale_growth_tracker("step")
        optimizer_state = self._per_optimizer_states[id(optimizer)]
        if optimizer_state["stage"] is OptState.STEPPED:
            raise RuntimeError("step() has already been called since the last update().")
        if optimizer_state["stage"] is OptState.READY:
            self.unscale_(optimizer)
        if len(optimizer_state["found_inf_per_device"]) == 0:
            raise AssertionError("No inf checks were recorded for this optimizer.")
        retval = self._maybe_opt_step(optimizer, optimizer_state, *args, **kwargs)
        optimizer_state["stage"] = OptState.STEPPED
        return retval

    def update(self, new_scale=None):
        """Backs the scale off after a skipped step, grows it after
        `growth_interval` consecutive good ones, or sets `new_scale`."""
        if not self._enabled:
            return
        _scale, _growth_tracker = self._check_scale_growth_tracker("update")
        if new_scale is not None:
            if isinstance(new_scale, float):
                self._scale.fill_(new_scale)
            else:
                reason = "new_scale should be a float or a 1-element torch.cuda.FloatTensor or torch.FloatTensor with requires_grad=False."
                if not isinstance(new_scale, torch.Tensor) or new_scale.device.type != self._device:
                    raise AssertionError(reason)
                if new_scale.numel() != 1:
                    raise AssertionError(reason)
                if new_scale.requires_grad is True:
                    raise AssertionError(reason)
                self._scale.copy_(new_scale.reshape(()))
        else:
            found_infs = [found_inf for state in self._per_optimizer_states.values() for found_inf in state["found_inf_per_device"].values()]
            if len(found_infs) == 0:
                raise AssertionError("No inf checks were recorded prior to update.")
            found = sum(float(f.item()) for f in found_infs)
            # torch._amp_update_scale_
            scale = float(_scale.item())
            tracker = int(_growth_tracker.item())
            if found:
                _scale.fill_(scale * self._backoff_factor)
                _growth_tracker.fill_(0)
            else:
                successful = tracker + 1
                if successful == self._growth_interval:
                    grown = float(torch.full((), scale * self._growth_factor, dtype=torch.float32).item())
                    if _math.isfinite(grown):
                        _scale.fill_(grown)
                    _growth_tracker.fill_(0)
                else:
                    _growth_tracker.fill_(successful)
        self._per_optimizer_states = _defaultdict(_refresh_per_optimizer_state)

    def _get_scale_async(self):
        return self._scale

    def get_scale(self):
        """The current scale as a Python float (1.0 when disabled)."""
        if self._enabled:
            scale = self._get_scale_async()
            return self._init_scale if scale is None else float(scale.item())
        return 1.0

    def get_growth_factor(self):
        return self._growth_factor

    def set_growth_factor(self, new_factor):
        self._growth_factor = new_factor

    def get_backoff_factor(self):
        return self._backoff_factor

    def set_backoff_factor(self, new_factor):
        self._backoff_factor = new_factor

    def get_growth_interval(self):
        return self._growth_interval

    def set_growth_interval(self, new_interval):
        self._growth_interval = new_interval

    def _get_growth_tracker(self):
        if self._enabled:
            return self._init_growth_tracker if self._growth_tracker is None else int(self._growth_tracker.item())
        return 0

    def is_enabled(self):
        return self._enabled

    def state_dict(self):
        if self._enabled:
            return {
                "scale": self.get_scale(),
                "growth_factor": self._growth_factor,
                "backoff_factor": self._backoff_factor,
                "growth_interval": self._growth_interval,
                "_growth_tracker": self._get_growth_tracker(),
            }
        return {}

    def load_state_dict(self, state_dict):
        if not self._enabled:
            return
        if len(state_dict) == 0:
            raise RuntimeError("The source state dict is empty, possibly because it was saved from a disabled instance of GradScaler.")
        self._init_scale = state_dict["scale"]
        if self._scale is not None:
            self._scale.fill_(state_dict["scale"])
        self._growth_factor = state_dict["growth_factor"]
        self._backoff_factor = state_dict["backoff_factor"]
        self._growth_interval = state_dict["growth_interval"]
        self._init_growth_tracker = state_dict["_growth_tracker"]
        if self._growth_tracker is not None:
            self._growth_tracker.fill_(state_dict["_growth_tracker"])

    def _check_inf_per_device(self, optimizer):
        _scale, _ = self._check_scale_growth_tracker("_check_inf_per_device")
        dummy_inv_scale = torch.full((), 1.0, dtype=torch.float32)
        found_inf = torch.full((), 0.0, dtype=torch.float32)
        self._per_optimizer_states[id(optimizer)]["found_inf_per_device"] = self._unscale_grads_(optimizer, dummy_inv_scale, found_inf, True)
        return self._per_optimizer_states[id(optimizer)]["found_inf_per_device"]

    def _found_inf_per_device(self, optimizer):
        return self._per_optimizer_states[id(optimizer)]["found_inf_per_device"]


# ---- torch.cuda.amp (deprecated spellings) and the torch.* autocast state ----------------
class _CudaGradScaler(GradScaler):
    """`torch.cuda.amp.GradScaler(...)`: `GradScaler("cuda", ...)`, which
    disables itself on Zipp."""

    def __init__(self, init_scale=2.0 ** 16, growth_factor=2.0, backoff_factor=0.5, growth_interval=2000, enabled=True):
        _warn("`torch.cuda.amp.GradScaler(args...)` is deprecated. Please use `torch.amp.GradScaler('cuda', args...)` instead.", "FutureWarning")
        GradScaler.__init__(self, "cuda", init_scale=init_scale, growth_factor=growth_factor, backoff_factor=backoff_factor,
                            growth_interval=growth_interval, enabled=enabled)



class _CudaAutocast(autocast):
    """`torch.cuda.amp.autocast(...)`: `autocast("cuda", ...)`."""

    def __init__(self, enabled=True, dtype=None, cache_enabled=True):
        _warn("`torch.cuda.amp.autocast(args...)` is deprecated. Please use `torch.amp.autocast('cuda', args...)` instead.", "FutureWarning")
        autocast.__init__(self, "cuda", enabled=enabled, dtype=getattr(torch, "float16", torch.half) if dtype is None else dtype,
                          cache_enabled=cache_enabled)



class _CudaAmp:
    GradScaler = _CudaGradScaler
    autocast = _CudaAutocast

    @staticmethod
    def custom_fwd(fwd=None, *, cast_inputs=None):
        return custom_fwd(fwd, device_type="cuda", cast_inputs=cast_inputs)

    @staticmethod
    def custom_bwd(bwd=None):
        return custom_bwd(bwd, device_type="cuda")


def _install():
    names = {
        "is_autocast_enabled": is_autocast_enabled, "set_autocast_enabled": set_autocast_enabled,
        "get_autocast_dtype": get_autocast_dtype, "set_autocast_dtype": set_autocast_dtype,
        "is_autocast_cache_enabled": is_autocast_cache_enabled, "set_autocast_cache_enabled": set_autocast_cache_enabled,
        "clear_autocast_cache": clear_autocast_cache, "autocast_increment_nesting": autocast_increment_nesting,
        "autocast_decrement_nesting": autocast_decrement_nesting, "get_autocast_cpu_dtype": get_autocast_cpu_dtype,
        "get_autocast_gpu_dtype": get_autocast_gpu_dtype, "is_autocast_cpu_enabled": is_autocast_cpu_enabled,
    }
    for name, fn in names.items():
        if not hasattr(torch, name):
            setattr(torch, name, fn)
    # torch.autocast is torch.amp.autocast in PyTorch. The no-op stub in
    # torch.py is this class's base, so replacing it keeps isinstance checks
    # against it true while nested `torch.autocast(...)` regions update the
    # same state as `torch.amp.autocast(...)`.
    if torch.autocast is _StubAutocast:
        torch.autocast = autocast
    cuda = getattr(torch, "cuda", None)
    if cuda is not None and not hasattr(cuda, "amp"):
        try:
            cuda.amp = _CudaAmp()
        except (AttributeError, TypeError):
            pass


_install()
