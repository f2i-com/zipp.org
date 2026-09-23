"""torch.nn for Zipp: Module, containers and the layers the runtime supports.

Module keeps PyTorch's registries (`_parameters`, `_buffers`, `_modules`),
attribute rules, state_dict format, hooks and repr, so model code and
checkpoints written for PyTorch carry over. Layers compute with
`torch.nn.functional` on the eager CPU kernels.
"""
import math
import copy as _copy
import torch
from torch import Tensor
from collections import OrderedDict
import torch.nn.functional as F
import torch.nn.init as init
import torch.nn.utils as utils
import torch.nn.parameter as parameter
from torch.nn.parameter import Parameter, Buffer, UninitializedParameter, UninitializedBuffer
from torch.nn.utils.rnn import PackedSequence
import torch.nn.utils.rnn as _rnn_utils


def _typename(value):
    if isinstance(value, Tensor):
        return value.type()
    t = type(value)
    module = getattr(t, "__module__", "builtins")
    return t.__name__ if module in ("builtins", None) else module + "." + t.__name__


def _addindent(s, num_spaces):
    lines = s.split("\n")
    if len(lines) == 1:
        return s
    first = lines.pop(0)
    return first + "\n" + "\n".join((num_spaces * " ") + line for line in lines)


_hook_ids = [0]


class RemovableHandle:
    """A handle that removes a registered hook (also a context manager)."""

    def __init__(self, hooks_dict, extra_dict=None):
        self.hooks_dict_ref = hooks_dict
        self.id = _hook_ids[0]
        _hook_ids[0] += 1
        self.extra_dict_ref = () if extra_dict is None else tuple(extra_dict) if isinstance(extra_dict, list) else (extra_dict,)

    def remove(self):
        if self.id in self.hooks_dict_ref:
            del self.hooks_dict_ref[self.id]
        for d in self.extra_dict_ref:
            if self.id in d:
                del d[self.id]

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.remove()


def _add_hook(hooks, handle, hook, prepend):
    if not prepend:
        hooks[handle.id] = hook
        return
    items = list(hooks.items())
    hooks.clear()
    hooks[handle.id] = hook
    for k, v in items:
        hooks[k] = v


class _WrappedHook:
    """A load_state_dict pre-hook, called with its module first when it was
    registered through the public API."""

    def __init__(self, hook, module=None):
        self.hook = hook
        self.with_module = module is not None
        self.module = module

    def __call__(self, *args, **kwargs):
        if self.with_module:
            return self.hook(self.module, *args, **kwargs)
        return self.hook(*args, **kwargs)

    def __deepcopy__(self, memo):
        # A copied module's hooks (often its own bound methods) follow it.
        return _WrappedHook(_deepcopy_value(self.hook, memo), _deepcopy_value(self.module, memo) if self.with_module else None)


class _IncompatibleKeys(tuple):
    def __new__(cls, missing_keys, unexpected_keys):
        return tuple.__new__(cls, (missing_keys, unexpected_keys))

    def __init__(self, missing_keys, unexpected_keys):
        self.missing_keys = missing_keys
        self.unexpected_keys = unexpected_keys

    def __repr__(self):
        if not self.missing_keys and not self.unexpected_keys:
            return "<All keys matched successfully>"
        return "_IncompatibleKeys(missing_keys=%r, unexpected_keys=%r)" % (self.missing_keys, self.unexpected_keys)

    __str__ = __repr__


def _deepcopy_value(value, memo):
    key = id(value)
    if key in memo:
        return memo[key]
    if isinstance(value, Module):
        return value.__deepcopy__(memo)
    if isinstance(value, Parameter):
        return value.__deepcopy__(memo)
    if isinstance(value, Tensor):
        out = value.detach().clone()
        out.requires_grad = value.requires_grad
        if value.grad is not None:
            out.grad = value.grad.clone()
        if isinstance(value, Buffer):
            out = Buffer(out)
        for name in ("persistent", "_is_buffer"):
            if name in value.__dict__:
                setattr(out, name, value.__dict__[name])
        memo[key] = out
        return out
    if isinstance(value, OrderedDict):
        out = OrderedDict()
        memo[key] = out
        for k, v in value.items():
            out[k] = _deepcopy_value(v, memo)
        return out
    if type(value) is dict:
        out = {}
        memo[key] = out
        for k, v in value.items():
            out[k] = _deepcopy_value(v, memo)
        return out
    if type(value) is list:
        out = []
        memo[key] = out
        for v in value:
            out.append(_deepcopy_value(v, memo))
        return out
    if type(value) is tuple:
        return tuple(_deepcopy_value(v, memo) for v in value)
    if type(value) is set:
        return set(_deepcopy_value(v, memo) for v in value)
    if type(value).__name__ == "method" and hasattr(value, "__func__"):
        # As CPython's deepcopy: a bound method (a hook such as a lazy
        # module's own) is rebound to the copy of its object.
        return value.__func__.__get__(_deepcopy_value(value.__self__, memo))
    return _copy.deepcopy(value, memo)


class _BackwardHooks:
    """Module full-backward hooks: the module's tensor inputs and outputs pass
    through one identity node each side, so the hooks see every grad_input
    and grad_output together and may replace them."""

    def __init__(self, module):
        self.module = module
        self.grad_outputs = None
        self.n_inputs = 0
        self.input_positions = None

    def _hub(self, tensors, backward, name):
        dtype = tensors[0].dtype
        for t in tensors:
            if t.dtype != dtype:
                dtype = torch.float64
        flat = torch.cat([t.reshape(-1).to(dtype) for t in tensors])
        hub = Tensor(flat._s, flat.shape, flat.dtype)
        hub.requires_grad = True
        hub._node = torch._Node(backward, (flat,), name)
        out, start = [], 0
        for t in tensors:
            n = t.numel()
            out.append(hub.narrow(0, start, n).reshape(*t.shape).to(t.dtype))
            start += n
        return out

    @staticmethod
    def _split(g, tensors):
        parts, start = [], 0
        for t in tensors:
            n = t.numel()
            parts.append(g.narrow(0, start, n).reshape(*t.shape).to(t.dtype))
            start += n
        return parts

    def setup_inputs(self, args):
        positions = [i for i, a in enumerate(args) if isinstance(a, Tensor)]
        self.n_inputs = len(positions)
        live = [i for i in positions if args[i].requires_grad] if torch.is_grad_enabled() else []
        if not live:
            self.input_positions = None
            return args
        self.input_positions = live
        tensors = [args[i] for i in live]
        state = self

        def backward(g):
            grads = state._split(g, tensors)
            grad_inputs = [None] * len(positions)
            for i, gi in zip(live, grads):
                grad_inputs[positions.index(i)] = gi
            grad_inputs = tuple(grad_inputs)
            for hook in list(state.module._backward_hooks.values()):
                res = hook(state.module, grad_inputs, state.grad_outputs)
                if res is not None:
                    res = tuple(res) if isinstance(res, (tuple, list)) else (res,)
                    if len(res) != len(grad_inputs):
                        raise RuntimeError("Backward hook returned an invalid number of grad_input, got %d, but expected %d" % (len(res), len(grad_inputs)))
                    grad_inputs = res
            chosen = [grad_inputs[positions.index(i)] for i in live]
            return (torch.cat([(c if c is not None else torch.zeros_like(t)).reshape(-1).to(g.dtype) for c, t in zip(chosen, tensors)]),)
        wrapped = self._hub(tensors, backward, "BackwardHookFunctionBackward")
        args = list(args)
        for i, w in zip(live, wrapped):
            args[i] = w
        return tuple(args)

    def setup_outputs(self, result):
        single = isinstance(result, Tensor)
        items = [result] if single else list(result) if isinstance(result, (tuple, list)) else None
        if items is None:
            return result
        live = [i for i, r in enumerate(items) if isinstance(r, Tensor) and r.requires_grad]
        if not live:
            return result
        tensors = [items[i] for i in live]
        state = self

        def backward(g):
            grads = state._split(g, tensors)
            grad_outputs = [None] * len(items)
            for i, gi in zip(live, grads):
                grad_outputs[i] = gi
            grad_outputs = tuple(grad_outputs)
            for hook in list(state.module._backward_pre_hooks.values()):
                res = hook(state.module, grad_outputs)
                if res is not None:
                    grad_outputs = tuple(res) if isinstance(res, (tuple, list)) else (res,)
            state.grad_outputs = grad_outputs
            if state.input_positions is None:
                grad_inputs = (None,) * state.n_inputs
                for hook in list(state.module._backward_hooks.values()):
                    res = hook(state.module, grad_inputs, grad_outputs)
                    if res is not None and not all(r is None for r in (res if isinstance(res, tuple) else (res,))):
                        raise RuntimeError("Backward hook for Modules where no input requires gradient should always return None or None for all gradients.")
            chosen = [grad_outputs[i] for i in live]
            return (torch.cat([(c if c is not None else torch.zeros_like(t)).reshape(-1).to(g.dtype) for c, t in zip(chosen, tensors)]),)
        wrapped = self._hub(tensors, backward, "BackwardHookFunctionBackward")
        if single:
            return wrapped[0]
        for i, w in zip(live, wrapped):
            items[i] = w
        return tuple(items) if isinstance(result, tuple) else items


class Module:
    training = True
    _version = 1
    call_super_init = False

    def __init__(self, *args, **kwargs):
        if args or kwargs:
            raise TypeError("%s.__init__() takes 1 positional argument but %d were given" % (type(self).__name__, len(args) + 1) if args else "%s.__init__() got an unexpected keyword argument '%s'" % (type(self).__name__, next(iter(kwargs))))
        d = self.__dict__
        d["training"] = True
        d["_parameters"] = OrderedDict()
        d["_buffers"] = OrderedDict()
        d["_non_persistent_buffers_set"] = set()
        d["_modules"] = OrderedDict()
        d["_forward_hooks"] = OrderedDict()
        d["_forward_hooks_with_kwargs"] = OrderedDict()
        d["_forward_pre_hooks"] = OrderedDict()
        d["_forward_pre_hooks_with_kwargs"] = OrderedDict()
        d["_backward_hooks"] = OrderedDict()
        d["_backward_pre_hooks"] = OrderedDict()
        d["_state_dict_hooks"] = OrderedDict()
        d["_state_dict_pre_hooks"] = OrderedDict()
        d["_load_state_dict_pre_hooks"] = OrderedDict()
        d["_load_state_dict_post_hooks"] = OrderedDict()

    # -- attribute registries ---------------------------------------------------------------
    def __setattr__(self, name, value):
        d = self.__dict__
        params = d.get("_parameters")
        if isinstance(value, Parameter):
            if params is None:
                raise AttributeError("cannot assign parameters before Module.__init__() call")
            d.pop(name, None)
            self._buffers.pop(name, None)
            self._modules.pop(name, None)
            self._non_persistent_buffers_set.discard(name)
            self.register_parameter(name, value)
        elif params is not None and name in params:
            if value is not None:
                raise TypeError("cannot assign '%s' as parameter '%s' (torch.nn.Parameter or None expected)" % (_typename(value), name))
            self.register_parameter(name, value)
        else:
            modules = d.get("_modules")
            if isinstance(value, Module):
                if modules is None:
                    raise AttributeError("cannot assign module before Module.__init__() call")
                d.pop(name, None)
                params.pop(name, None)
                self._buffers.pop(name, None)
                self._non_persistent_buffers_set.discard(name)
                modules[name] = value
            elif modules is not None and name in modules:
                if value is not None:
                    raise TypeError("cannot assign '%s' as child module '%s' (torch.nn.Module or None expected)" % (_typename(value), name))
                modules[name] = value
            else:
                buffers = d.get("_buffers")
                if isinstance(value, Tensor) and value.__dict__.get("_is_buffer", False):
                    if buffers is None:
                        raise AttributeError("cannot assign buffer before Module.__init__() call")
                    d.pop(name, None)
                    params.pop(name, None)
                    modules.pop(name, None)
                    self.register_buffer(name, value, value.__dict__.get("persistent", True))
                elif buffers is not None and name in buffers:
                    if value is not None and not isinstance(value, Tensor):
                        raise TypeError("cannot assign '%s' as buffer '%s' (torch.nn.Buffer, torch.Tensor or None expected)" % (_typename(value), name))
                    buffers[name] = value
                else:
                    object.__setattr__(self, name, value)

    def __getattr__(self, name):
        d = self.__dict__
        if "_parameters" in d:
            p = d["_parameters"]
            if name in p:
                return p[name]
        if "_buffers" in d:
            b = d["_buffers"]
            if name in b:
                return b[name]
        if "_modules" in d:
            m = d["_modules"]
            if name in m:
                return m[name]
        raise AttributeError("'%s' object has no attribute '%s'" % (type(self).__name__, name))

    def __delattr__(self, name):
        if name in self._parameters:
            del self._parameters[name]
        elif name in self._buffers:
            del self._buffers[name]
            self._non_persistent_buffers_set.discard(name)
        elif name in self._modules:
            del self._modules[name]
        else:
            object.__delattr__(self, name)

    def _check_name(self, name, kind, registry):
        if not isinstance(name, str):
            raise TypeError("%s name should be a string. Got %s" % (kind, _typename(name)))
        if "." in name:
            raise KeyError("%s name can't contain \".\"" % kind)
        if name == "":
            raise KeyError("%s name can't be empty string \"\"" % kind)
        if hasattr(self, name) and name not in registry:
            raise KeyError("attribute '%s' already exists" % name)

    def register_buffer(self, name, tensor, persistent=True):
        if "_buffers" not in self.__dict__:
            raise AttributeError("cannot assign buffer before Module.__init__() call")
        self._check_name(name, "buffer", self._buffers)
        if tensor is not None and not isinstance(tensor, Tensor):
            raise TypeError("cannot assign '%s' object to buffer '%s' (torch Tensor or None required)" % (_typename(tensor), name))
        self._buffers[name] = tensor
        if persistent:
            self._non_persistent_buffers_set.discard(name)
        else:
            self._non_persistent_buffers_set.add(name)

    def register_parameter(self, name, param):
        if "_parameters" not in self.__dict__:
            raise AttributeError("cannot assign parameter before Module.__init__() call")
        self._check_name(name, "parameter", self._parameters)
        if param is None:
            self._parameters[name] = None
            return
        if not isinstance(param, Parameter):
            raise TypeError("cannot assign '%s' object to parameter '%s' (torch.nn.Parameter or None required)" % (_typename(param), name))
        if param._node is not None:
            raise ValueError("Cannot assign non-leaf Tensor to parameter '%s'. Model parameters must be created explicitly. To express '%s' as a function of another Tensor, compute the value in the forward() method." % (name, name))
        self._parameters[name] = param

    def add_module(self, name, module):
        if not isinstance(module, Module) and module is not None:
            raise TypeError("%s is not a Module subclass" % _typename(module))
        if not isinstance(name, str):
            raise TypeError("module name should be a string. Got %s" % _typename(name))
        if hasattr(self, name) and name not in self._modules:
            raise KeyError("attribute '%s' already exists" % name)
        if "." in name:
            raise KeyError("module name can't contain \".\", got: %s" % name)
        if name == "":
            raise KeyError("module name can't be empty string \"\"")
        self._modules[name] = module

    register_module = add_module

    def get_submodule(self, target):
        if target == "":
            return self
        mod = self
        for item in target.split("."):
            if not hasattr(mod, item):
                raise AttributeError(mod._get_name() + " has no attribute `" + item + "`")
            mod = getattr(mod, item)
            if not isinstance(mod, Module):
                raise AttributeError("`" + item + "` is not an nn.Module")
        return mod

    def set_submodule(self, target, module, strict=False):
        if target == "":
            raise ValueError("Cannot set the submodule without a target name!")
        atoms = target.split(".")
        mod = self.get_submodule(".".join(atoms[:-1]))
        if strict and not hasattr(mod, atoms[-1]):
            raise AttributeError(mod._get_name() + " has no attribute `" + atoms[-1] + "`")
        setattr(mod, atoms[-1], module)

    def get_parameter(self, target):
        module_path, _, name = target.rpartition(".")
        mod = self.get_submodule(module_path)
        if not hasattr(mod, name):
            raise AttributeError(mod._get_name() + " has no attribute `" + name + "`")
        param = getattr(mod, name)
        if not isinstance(param, Parameter):
            raise AttributeError("`" + name + "` is not an nn.Parameter")
        return param

    def get_buffer(self, target):
        module_path, _, name = target.rpartition(".")
        mod = self.get_submodule(module_path)
        if not hasattr(mod, name):
            raise AttributeError(mod._get_name() + " has no attribute `" + name + "`")
        if name not in mod._buffers:
            raise AttributeError("`" + name + "` is not a buffer")
        return getattr(mod, name)

    def get_extra_state(self):
        raise RuntimeError("Reached a code path in Module.get_extra_state() that should never be called.")

    def set_extra_state(self, state):
        raise RuntimeError("Reached a code path in Module.set_extra_state() that should never be called.")

    # -- calling ----------------------------------------------------------------------------
    def forward(self, *args, **kwargs):
        raise NotImplementedError("Module [%s] is missing the required \"forward\" function" % type(self).__name__)

    def __call__(self, *args, **kwargs):
        d = self.__dict__
        if not (d.get("_forward_hooks") or d.get("_forward_pre_hooks") or d.get("_backward_hooks") or d.get("_backward_pre_hooks")):
            return self.forward(*args, **kwargs)
        return self._call_with_hooks(args, kwargs)

    _call_impl = __call__

    def _call_with_hooks(self, args, kwargs):
        for hook_id, hook in list(self._forward_pre_hooks.items()):
            if hook_id in self._forward_pre_hooks_with_kwargs:
                res = hook(self, args, kwargs)
                if res is not None:
                    if isinstance(res, tuple) and len(res) == 2:
                        args, kwargs = res
                    else:
                        raise RuntimeError("forward pre-hook must return None or a tuple of (new_args, new_kwargs), but got %r." % (res,))
            else:
                res = hook(self, args)
                if res is not None:
                    args = res if isinstance(res, tuple) else (res,)
        bw = None
        if self._backward_hooks or self._backward_pre_hooks:
            bw = _BackwardHooks(self)
            args = bw.setup_inputs(args)
        result = self.forward(*args, **kwargs)
        for hook_id, hook in list(self._forward_hooks.items()):
            if hook_id in self._forward_hooks_with_kwargs:
                res = hook(self, args, kwargs, result)
            else:
                res = hook(self, args, result)
            if res is not None:
                result = res
        if bw is not None:
            result = bw.setup_outputs(result)
        return result

    def _hooks(self, name):
        hooks = self.__dict__.get(name)
        if hooks is None:
            hooks = OrderedDict()
            self.__dict__[name] = hooks
        return hooks

    def register_forward_pre_hook(self, hook, prepend=False, with_kwargs=False):
        hooks = self._hooks("_forward_pre_hooks")
        kw = self._hooks("_forward_pre_hooks_with_kwargs")
        handle = RemovableHandle(hooks, extra_dict=kw)
        _add_hook(hooks, handle, hook, prepend)
        if with_kwargs:
            kw[handle.id] = True
        return handle

    def register_forward_hook(self, hook, prepend=False, with_kwargs=False, always_call=False):
        hooks = self._hooks("_forward_hooks")
        kw = self._hooks("_forward_hooks_with_kwargs")
        handle = RemovableHandle(hooks, extra_dict=kw)
        _add_hook(hooks, handle, hook, prepend)
        if with_kwargs:
            kw[handle.id] = True
        return handle

    def register_full_backward_hook(self, hook, prepend=False):
        hooks = self._hooks("_backward_hooks")
        handle = RemovableHandle(hooks)
        _add_hook(hooks, handle, hook, prepend)
        return handle

    def register_backward_hook(self, hook):
        return self.register_full_backward_hook(hook)

    def register_full_backward_pre_hook(self, hook, prepend=False):
        hooks = self._hooks("_backward_pre_hooks")
        handle = RemovableHandle(hooks)
        _add_hook(hooks, handle, hook, prepend)
        return handle

    def _register_state_dict_hook(self, hook):
        hooks = self._hooks("_state_dict_hooks")
        handle = RemovableHandle(hooks)
        hooks[handle.id] = hook
        return handle

    def register_state_dict_post_hook(self, hook):
        hook._from_public_api = True
        return self._register_state_dict_hook(hook)

    def register_state_dict_pre_hook(self, hook):
        hooks = self._hooks("_state_dict_pre_hooks")
        handle = RemovableHandle(hooks)
        hooks[handle.id] = hook
        return handle

    def _register_load_state_dict_pre_hook(self, hook, with_module=False):
        hooks = self._hooks("_load_state_dict_pre_hooks")
        handle = RemovableHandle(hooks)
        hooks[handle.id] = _WrappedHook(hook, self if with_module else None)
        return handle

    def register_load_state_dict_pre_hook(self, hook):
        return self._register_load_state_dict_pre_hook(hook, with_module=True)

    def register_load_state_dict_post_hook(self, hook):
        hooks = self._hooks("_load_state_dict_post_hooks")
        handle = RemovableHandle(hooks)
        hooks[handle.id] = hook
        return handle

    # -- traversal --------------------------------------------------------------------------
    def _named_modules_list(self, memo, prefix, remove_duplicate, out):
        if id(self) in memo:
            return
        if remove_duplicate:
            memo.add(id(self))
        out.append((prefix, self))
        for name, module in self._modules.items():
            if module is None:
                continue
            module._named_modules_list(memo, prefix + ("." if prefix else "") + name, remove_duplicate, out)

    def named_modules(self, memo=None, prefix="", remove_duplicate=True):
        out = []
        self._named_modules_list(set() if memo is None else memo, prefix, remove_duplicate, out)
        return iter(out)

    def modules(self):
        return iter([m for _, m in self.named_modules()])

    def named_children(self):
        memo, out = set(), []
        for name, module in self._modules.items():
            if module is not None and id(module) not in memo:
                memo.add(id(module))
                out.append((name, module))
        return iter(out)

    def children(self):
        return iter([m for _, m in self.named_children()])

    def _named_members(self, attr, prefix, recurse, remove_duplicate):
        modules = list(self.named_modules(prefix=prefix, remove_duplicate=remove_duplicate)) if recurse else [(prefix, self)]
        memo, out = set(), []
        for module_prefix, module in modules:
            for k, v in getattr(module, attr).items():
                if v is None or id(v) in memo:
                    continue
                if remove_duplicate:
                    memo.add(id(v))
                out.append((module_prefix + ("." if module_prefix else "") + k, v))
        return out

    def named_parameters(self, prefix="", recurse=True, remove_duplicate=True):
        return iter(self._named_members("_parameters", prefix, recurse, remove_duplicate))

    def parameters(self, recurse=True):
        return iter([p for _, p in self._named_members("_parameters", "", recurse, True)])

    def named_buffers(self, prefix="", recurse=True, remove_duplicate=True):
        return iter(self._named_members("_buffers", prefix, recurse, remove_duplicate))

    def buffers(self, recurse=True):
        return iter([b for _, b in self._named_members("_buffers", "", recurse, True)])

    # -- modes and conversion ----------------------------------------------------------------
    def train(self, mode=True):
        if not isinstance(mode, bool):
            raise ValueError("training mode is expected to be boolean")
        self.training = mode
        for module in self.children():
            module.train(mode)
        return self

    def eval(self):
        return self.train(False)

    def requires_grad_(self, requires_grad=True):
        for p in self.parameters():
            p.requires_grad_(requires_grad)
        return self

    def zero_grad(self, set_to_none=True):
        for p in self.parameters():
            if p.grad is None:
                continue
            if set_to_none:
                p.grad = None
            else:
                if p.grad._node is not None:
                    p.grad = p.grad.detach()
                else:
                    p.grad.requires_grad_(False)
                p.grad.zero_()

    def share_memory(self):
        return self

    def _apply(self, fn, recurse=True):
        if recurse:
            for module in self.children():
                module._apply(fn)
        for key, param in self._parameters.items():
            if param is None:
                continue
            if _is_lazy(param):
                # An uninitialized parameter only takes the new dtype.
                param.__dict__["dtype"] = fn(param).dtype
                continue
            with torch.no_grad():
                applied = fn(param)
            if applied is not param:
                # The Parameter object stays the same (optimizers hold it).
                param.data = applied
            if param.grad is not None:
                with torch.no_grad():
                    g = fn(param.grad)
                if g is not param.grad:
                    param.grad = g
        for key, buf in self._buffers.items():
            if buf is not None:
                self._buffers[key] = fn(buf)
        return self

    def to(self, *args, **kwargs):
        torch._check_cpu_device(kwargs.get("device"))
        target = kwargs.get("dtype")
        for a in args:
            if isinstance(a, (str, torch.device)):
                torch._check_cpu_device(a)
            elif isinstance(a, torch.dtype):
                target = a
            elif isinstance(a, Tensor):
                target = a.dtype
        if target is not None:
            if not target.is_floating_point:
                raise TypeError("nn.Module.to only accepts floating point or complex dtypes, but got desired dtype=%s" % target)
            self._apply(lambda t: t.to(target) if t.is_floating_point() else t)
        return self

    def cpu(self):
        return self

    def cuda(self, *a, **k):
        raise RuntimeError("CUDA is not available on Zipp")

    def type(self, dst_type):
        return self._apply(lambda t: t.type(dst_type))

    def float(self):
        return self._apply(lambda t: t.float() if t.is_floating_point() else t)

    def double(self):
        return self._apply(lambda t: t.double() if t.is_floating_point() else t)

    def half(self):
        # Zipp has no 16-bit float storage: torch.half is float32.
        return self._apply(lambda t: t.to(torch.half) if t.is_floating_point() else t)

    def bfloat16(self):
        return self._apply(lambda t: t.to(torch.bfloat16) if t.is_floating_point() else t)

    def apply(self, fn):
        for module in self.children():
            module.apply(fn)
        fn(self)
        return self

    # -- state dict -------------------------------------------------------------------------
    def _save_to_state_dict(self, destination, prefix, keep_vars):
        for name, param in self._parameters.items():
            if param is not None:
                destination[prefix + name] = param if keep_vars else param.detach()
        for name, buf in self._buffers.items():
            if buf is not None and name not in self._non_persistent_buffers_set:
                destination[prefix + name] = buf if keep_vars else buf.detach()
        if type(self).get_extra_state is not Module.get_extra_state:
            destination[prefix + "_extra_state"] = self.get_extra_state()

    def state_dict(self, *args, destination=None, prefix="", keep_vars=False):
        if args:
            destination = args[0] if len(args) > 0 else destination
            prefix = args[1] if len(args) > 1 else prefix
            keep_vars = args[2] if len(args) > 2 else keep_vars
        if destination is None:
            destination = OrderedDict()
        d = self.__dict__
        pre = d.get("_state_dict_pre_hooks")
        if pre:
            for hook in list(pre.values()):
                hook(self, prefix, keep_vars)
        self._save_to_state_dict(destination, prefix, keep_vars)
        for name, module in self._modules.items():
            if module is not None:
                module.state_dict(destination=destination, prefix=prefix + name + ".", keep_vars=keep_vars)
        post = d.get("_state_dict_hooks")
        if post:
            local_metadata = {"version": self._version}
            for hook in list(post.values()):
                result = hook(self, destination, prefix, local_metadata)
                if not getattr(hook, "_from_public_api", False):
                    if result is not None:
                        destination = result
                elif result is not None:
                    raise RuntimeError("state_dict post-hook must return None")
        return destination

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        pre = self.__dict__.get("_load_state_dict_pre_hooks")
        if pre:
            for hook in list(pre.values()):
                hook(state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs)
        persistent = [(k, v) for k, v in self._buffers.items() if k not in self._non_persistent_buffers_set]
        local_state = OrderedDict((k, v) for k, v in list(self._parameters.items()) + persistent if v is not None)
        assign = local_metadata.get("assign_to_params_buffers", False)
        for name, param in local_state.items():
            key = prefix + name
            if key not in state_dict:
                if strict:
                    missing_keys.append(key)
                continue
            input_param = state_dict[key]
            if not isinstance(input_param, Tensor):
                error_msgs.append("While copying the parameter named \"%s\", expected torch.Tensor or Tensor-like object from checkpoint but received %s" % (key, type(input_param)))
                continue
            if tuple(input_param.shape) != tuple(param.shape):
                error_msgs.append("size mismatch for %s: copying a param with shape %s from checkpoint, the shape in current model is %s." % (key, input_param.shape, param.shape))
                continue
            try:
                if assign:
                    if isinstance(param, Parameter):
                        value = Parameter(input_param.detach(), requires_grad=param.requires_grad)
                    else:
                        value = input_param.detach()
                    setattr(self, name, value)
                else:
                    with torch.no_grad():
                        param.copy_(input_param.detach().to(param.dtype))
            except Exception as ex:
                error_msgs.append("While copying the parameter named \"%s\", whose dimensions in the model are %s and whose dimensions in the checkpoint are %s, an exception occurred : %s." % (key, param.shape, input_param.shape, ex.args))
        extra_key = prefix + "_extra_state"
        if type(self).set_extra_state is not Module.set_extra_state:
            if extra_key in state_dict:
                self.set_extra_state(state_dict[extra_key])
            elif strict:
                missing_keys.append(extra_key)
        elif strict and extra_key in state_dict:
            unexpected_keys.append(extra_key)
        if strict:
            for key in state_dict.keys():
                if key.startswith(prefix) and key != extra_key:
                    input_name = key[len(prefix):].split(".", 1)
                    if len(input_name) > 1:
                        if input_name[0] not in self._modules:
                            unexpected_keys.append(key)
                    elif input_name[0] not in local_state:
                        unexpected_keys.append(key)

    def load_state_dict(self, state_dict, strict=True, assign=False):
        if not hasattr(state_dict, "keys") or not hasattr(state_dict, "__getitem__"):
            raise TypeError("Expected state_dict to be dict-like, got %s." % type(state_dict))
        missing_keys, unexpected_keys, error_msgs = [], [], []
        state = OrderedDict()
        for k in state_dict.keys():
            state[k] = state_dict[k]
        metadata = {"assign_to_params_buffers": assign}

        def load(module, local_state, prefix=""):
            module._load_from_state_dict(local_state, prefix, metadata, True, missing_keys, unexpected_keys, error_msgs)
            for name, child in module._modules.items():
                if child is not None:
                    child_prefix = prefix + name + "."
                    child_state = OrderedDict((k, v) for k, v in local_state.items() if k.startswith(child_prefix))
                    load(child, child_state, child_prefix)
            post = module.__dict__.get("_load_state_dict_post_hooks")
            if post:
                incompatible = _IncompatibleKeys(missing_keys, unexpected_keys)
                for hook in list(post.values()):
                    if hook(module, incompatible) is not None:
                        raise AssertionError("Hooks registered with ``register_load_state_dict_post_hook`` are notexpected to return new values, if incompatible_keys need to be modified,it should be done inplace.")
        load(self, state)
        if strict:
            if unexpected_keys:
                error_msgs.insert(0, "Unexpected key(s) in state_dict: %s. " % ", ".join("\"%s\"" % k for k in unexpected_keys))
            if missing_keys:
                error_msgs.insert(0, "Missing key(s) in state_dict: %s. " % ", ".join("\"%s\"" % k for k in missing_keys))
        if error_msgs:
            raise RuntimeError("Error(s) in loading state_dict for %s:\n\t%s" % (type(self).__name__, "\n\t".join(error_msgs)))
        return _IncompatibleKeys(missing_keys, unexpected_keys)

    # -- copying and repr -------------------------------------------------------------------
    def __deepcopy__(self, memo):
        if id(self) in memo:
            return memo[id(self)]
        new = type(self).__new__(type(self))
        memo[id(self)] = new
        for k, v in self.__dict__.items():
            new.__dict__[k] = _deepcopy_value(v, memo)
        return new

    def _get_name(self):
        return type(self).__name__

    def extra_repr(self):
        return ""

    def __repr__(self):
        extra = self.extra_repr()
        extra_lines = extra.split("\n") if extra else []
        child_lines = []
        for key, module in self._modules.items():
            child_lines.append("(" + key + "): " + _addindent(repr(module), 2))
        lines = extra_lines + child_lines
        main = self._get_name() + "("
        if lines:
            if len(extra_lines) == 1 and not child_lines:
                main += extra_lines[0]
            else:
                main += "\n  " + "\n  ".join(lines) + "\n"
        return main + ")"

    def __dir__(self):
        keys = list(object.__dir__(self)) if hasattr(object, "__dir__") else []
        keys += list(self._parameters.keys()) + list(self._modules.keys()) + list(self._buffers.keys())
        return sorted(set(k for k in keys if not k[0].isdigit()))


# ---- containers ------------------------------------------------------------------------------
def _index(idx, size):
    idx = idx.__index__() if not isinstance(idx, int) else idx
    if not -size <= idx < size:
        raise IndexError("index %d is out of range" % idx)
    return idx % size


class Sequential(Module):
    def __init__(self, *args):
        super().__init__()
        if len(args) == 1 and isinstance(args[0], OrderedDict):
            for key, module in args[0].items():
                self.add_module(key, module)
        else:
            for idx, module in enumerate(args):
                self.add_module(str(idx), module)

    def __getitem__(self, idx):
        if isinstance(idx, slice):
            return self.__class__(OrderedDict(list(self._modules.items())[idx]))
        return list(self._modules.values())[_index(idx, len(self._modules))]

    def __setitem__(self, idx, module):
        key = list(self._modules.keys())[_index(idx, len(self._modules))]
        setattr(self, key, module)

    def __delitem__(self, idx):
        if isinstance(idx, slice):
            for key in list(self._modules.keys())[idx]:
                delattr(self, key)
        else:
            delattr(self, list(self._modules.keys())[_index(idx, len(self._modules))])
        self._modules = OrderedDict((str(i), m) for i, m in enumerate(self._modules.values()))

    def __len__(self):
        return len(self._modules)

    def __iter__(self):
        return iter(list(self._modules.values()))

    def __add__(self, other):
        if not isinstance(other, Sequential):
            raise ValueError("add operator supports only objects of Sequential class, but %s is given." % type(other))
        ret = Sequential()
        for layer in self:
            ret.append(layer)
        for layer in other:
            ret.append(layer)
        return ret

    def __iadd__(self, other):
        if not isinstance(other, Sequential):
            raise ValueError("add operator supports only objects of Sequential class, but %s is given." % type(other))
        for layer in other:
            self.append(layer)
        return self

    def __mul__(self, other):
        if not isinstance(other, int) or other <= 0:
            raise ValueError("Non-positive multiplication factor %r for %s" % (other, type(self)))
        ret = Sequential()
        for _ in range(other):
            for layer in self:
                ret.append(layer)
        return ret

    __rmul__ = __mul__

    def forward(self, input):
        for module in self._modules.values():
            input = module(input)
        return input

    def append(self, module):
        self.add_module(str(len(self)), module)
        return self

    def insert(self, index, module):
        if not isinstance(module, Module):
            raise AssertionError("module should be of type: %s" % Module)
        n = len(self._modules)
        if not -n <= index <= n:
            raise IndexError("Index out of range: %d" % index)
        if index < 0:
            index += n
        for i in range(n, index, -1):
            self._modules[str(i)] = self._modules[str(i - 1)]
        self._modules[str(index)] = module
        return self

    def extend(self, sequential):
        for layer in sequential:
            self.append(layer)
        return self

    def pop(self, key):
        v = self[key]
        del self[key]
        return v


class ModuleList(Module):
    def __init__(self, modules=None):
        super().__init__()
        if modules is not None:
            self.extend(modules)

    def _abs(self, idx):
        return str(_index(idx, len(self._modules)))

    def __getitem__(self, idx):
        if isinstance(idx, slice):
            return self.__class__(list(self._modules.values())[idx])
        return self._modules[self._abs(idx)]

    def __setitem__(self, idx, module):
        setattr(self, self._abs(idx), module)

    def __delitem__(self, idx):
        if isinstance(idx, slice):
            for k in list(range(len(self._modules)))[idx]:
                delattr(self, str(k))
        else:
            delattr(self, self._abs(idx))
        self._modules = OrderedDict((str(i), m) for i, m in enumerate(self._modules.values()))

    def __len__(self):
        return len(self._modules)

    def __iter__(self):
        return iter(list(self._modules.values()))

    def __iadd__(self, modules):
        return self.extend(modules)

    def __add__(self, other):
        combined = ModuleList()
        for i, module in enumerate(list(self) + list(other)):
            combined.add_module(str(i), module)
        return combined

    def __repr__(self):
        reprs = [repr(item) for item in self]
        if not reprs:
            return self._get_name() + "()"
        blocks, spans = [reprs[0]], [[0, 0]]
        for i, r in enumerate(reprs[1:], 1):
            if r == blocks[-1]:
                spans[-1][1] += 1
                continue
            spans.append([i, i])
            blocks.append(r)
        lines = []
        for (start, end), b in zip(spans, blocks):
            local = "(%d): %s" % (start, b)
            if start != end:
                local = "(%d-%d): %d x %s" % (start, end, end - start + 1, b)
            lines.append(_addindent(local, 2))
        return self._get_name() + "(\n  " + "\n  ".join(lines) + "\n)"

    def insert(self, index, module):
        for i in range(len(self._modules), index, -1):
            self._modules[str(i)] = self._modules[str(i - 1)]
        self._modules[str(index)] = module

    def append(self, module):
        self.add_module(str(len(self)), module)
        return self

    def pop(self, key):
        v = self[key]
        del self[key]
        return v

    def extend(self, modules):
        if not hasattr(modules, "__iter__"):
            raise TypeError("ModuleList.extend should be called with an iterable, but got " + type(modules).__name__)
        offset = len(self)
        for i, module in enumerate(modules):
            self.add_module(str(offset + i), module)
        return self


class ModuleDict(Module):
    def __init__(self, modules=None):
        super().__init__()
        if modules is not None:
            self.update(modules)

    def __getitem__(self, key):
        return self._modules[key]

    def __setitem__(self, key, module):
        self.add_module(key, module)

    def __delitem__(self, key):
        del self._modules[key]

    def __len__(self):
        return len(self._modules)

    def __iter__(self):
        return iter(list(self._modules.keys()))

    def __contains__(self, key):
        return key in self._modules

    def clear(self):
        self._modules.clear()

    def pop(self, key):
        v = self[key]
        del self[key]
        return v

    def keys(self):
        return self._modules.keys()

    def items(self):
        return self._modules.items()

    def values(self):
        return self._modules.values()

    def update(self, modules):
        if hasattr(modules, "items"):
            for key, module in modules.items():
                self[key] = module
        else:
            for j, m in enumerate(modules):
                if not isinstance(m, (list, tuple)):
                    raise TypeError("ModuleDict update sequence element #%d should be Iterable; is %s" % (j, type(m).__name__))
                if len(m) != 2:
                    raise ValueError("ModuleDict update sequence element #%d has length %d; 2 is required" % (j, len(m)))
                self[m[0]] = m[1]


def _param_line(key, p):
    if isinstance(p, Tensor):
        size = "x".join(str(s) for s in p.shape)
        return "  (%s): %s containing: [%s of size %s]" % (key, "Parameter" if isinstance(p, Parameter) else "Tensor", p.dtype, size)
    return "  (%s): Object of type: %s" % (key, type(p).__name__)


def _as_parameter(value):
    if isinstance(value, Tensor) and not isinstance(value, Parameter):
        return Parameter(value)
    return value


class ParameterList(Module):
    def __init__(self, values=None):
        super().__init__()
        self._size = 0
        if values is not None:
            self.extend(values)

    def _abs(self, idx):
        return _index(idx, self._size)

    def __getitem__(self, idx):
        if isinstance(idx, slice):
            out = self.__class__()
            for i in range(self._size)[idx]:
                out.append(self[i])
            return out
        return getattr(self, str(self._abs(idx)))

    def __setitem__(self, idx, param):
        idx = self._abs(idx)
        setattr(self, str(idx), _as_parameter(param))

    def __len__(self):
        return self._size

    def __iter__(self):
        return iter([self[i] for i in range(self._size)])

    def __iadd__(self, parameters):
        return self.extend(parameters)

    def append(self, value):
        new_idx = self._size
        self._size += 1
        setattr(self, str(new_idx), _as_parameter(value))
        return self

    def extend(self, values):
        for value in values:
            self.append(value)
        return self

    def extra_repr(self):
        return "\n".join(_param_line(str(k), p) for k, p in enumerate(self))


class ParameterDict(Module):
    def __init__(self, parameters=None):
        super().__init__()
        self._keys = OrderedDict()
        if parameters is not None:
            self.update(parameters)

    def __getitem__(self, key):
        return getattr(self, key)

    def __setitem__(self, key, value):
        self._keys[key] = None
        setattr(self, key, _as_parameter(value))

    def __delitem__(self, key):
        del self._keys[key]
        delattr(self, key)

    def __len__(self):
        return len(self._keys)

    def __iter__(self):
        return iter(list(self._keys))

    def __contains__(self, key):
        return key in self._keys

    def keys(self):
        return list(self._keys)

    def items(self):
        return [(k, self[k]) for k in self._keys]

    def values(self):
        return [self[k] for k in self._keys]

    def get(self, key, default=None):
        return self[key] if key in self else default

    def pop(self, key):
        v = self[key]
        del self[key]
        return v

    def clear(self):
        for k in list(self._keys):
            del self[k]

    def update(self, parameters):
        items = parameters.items() if hasattr(parameters, "items") else parameters
        for key, value in items:
            self[key] = value

    def extra_repr(self):
        return "\n".join(_param_line(k, p) for k, p in self.items())


# ---- basic layers ----------------------------------------------------------------------------
class Identity(Module):
    def __init__(self, *args, **kwargs):
        super().__init__()

    def forward(self, input):
        return input


class Linear(Module):
    def __init__(self, in_features, out_features, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.in_features = in_features
        self.out_features = out_features
        self.weight = Parameter(torch.empty(out_features, in_features, dtype=dtype))
        if bias:
            self.bias = Parameter(torch.empty(out_features, dtype=dtype))
        else:
            self.register_parameter("bias", None)
        self.reset_parameters()

    def reset_parameters(self):
        init.kaiming_uniform_(self.weight, a=math.sqrt(5))
        if self.bias is not None:
            bound = 1 / math.sqrt(self.in_features) if self.in_features > 0 else 0
            init.uniform_(self.bias, -bound, bound)

    def forward(self, x):
        return F.linear(x, self.weight, self.bias)

    def extra_repr(self):
        return "in_features=%d, out_features=%d, bias=%s" % (self.in_features, self.out_features, self.bias is not None)


class NonDynamicallyQuantizableLinear(Linear):
    pass


class Bilinear(Module):
    def __init__(self, in1_features, in2_features, out_features, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.in1_features, self.in2_features, self.out_features = in1_features, in2_features, out_features
        self.weight = Parameter(torch.empty(out_features, in1_features, in2_features, dtype=dtype))
        if bias:
            self.bias = Parameter(torch.empty(out_features, dtype=dtype))
        else:
            self.register_parameter("bias", None)
        self.reset_parameters()

    def reset_parameters(self):
        bound = 1 / math.sqrt(self.weight.shape[1])
        init.uniform_(self.weight, -bound, bound)
        if self.bias is not None:
            init.uniform_(self.bias, -bound, bound)

    def forward(self, input1, input2):
        return F.bilinear(input1, input2, self.weight, self.bias)

    def extra_repr(self):
        return "in1_features=%d, in2_features=%d, out_features=%d, bias=%s" % (self.in1_features, self.in2_features, self.out_features, self.bias is not None)


class Flatten(Module):
    def __init__(self, start_dim=1, end_dim=-1):
        super().__init__()
        self.start_dim = start_dim
        self.end_dim = end_dim

    def forward(self, x):
        return x.flatten(self.start_dim, self.end_dim)

    def extra_repr(self):
        return "start_dim=%s, end_dim=%s" % (self.start_dim, self.end_dim)


class Unflatten(Module):
    def __init__(self, dim, unflattened_size):
        super().__init__()
        if isinstance(dim, int):
            if not isinstance(unflattened_size, (tuple, list)) or not all(isinstance(v, int) for v in unflattened_size):
                raise TypeError("unflattened_size must be tuple of ints, but found type %s" % type(unflattened_size).__name__)
        self.dim = dim
        self.unflattened_size = unflattened_size

    def forward(self, input):
        d = self.dim % len(input.shape)
        sizes = list(self.unflattened_size)
        if -1 in sizes:
            known = 1
            for s in sizes:
                if s != -1:
                    known *= s
            sizes[sizes.index(-1)] = input.shape[d] // known
        return input.reshape(*(list(input.shape[:d]) + sizes + list(input.shape[d + 1:])))

    def extra_repr(self):
        return "dim=%s, unflattened_size=%s" % (self.dim, self.unflattened_size)


class Embedding(Module):
    def __init__(self, num_embeddings, embedding_dim, padding_idx=None, max_norm=None, norm_type=2.0, scale_grad_by_freq=False, sparse=False, _weight=None, _freeze=False, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.num_embeddings = num_embeddings
        self.embedding_dim = embedding_dim
        if padding_idx is not None:
            if padding_idx > 0:
                if padding_idx >= num_embeddings:
                    raise AssertionError("Padding_idx must be within num_embeddings")
            elif padding_idx < 0:
                if padding_idx < -num_embeddings:
                    raise AssertionError("Padding_idx must be within num_embeddings")
                padding_idx = num_embeddings + padding_idx
        self.padding_idx = padding_idx
        self.max_norm = max_norm
        self.norm_type = norm_type
        self.scale_grad_by_freq = scale_grad_by_freq
        self.sparse = sparse
        if _weight is None:
            self.weight = Parameter(torch.empty(num_embeddings, embedding_dim, dtype=dtype), requires_grad=not _freeze)
            self.reset_parameters()
        else:
            if list(_weight.shape) != [num_embeddings, embedding_dim]:
                raise AssertionError("Shape of weight does not match num_embeddings and embedding_dim")
            self.weight = Parameter(_weight, requires_grad=not _freeze)

    def reset_parameters(self):
        init.normal_(self.weight)
        self._fill_padding_idx_with_zero()

    def _fill_padding_idx_with_zero(self):
        if self.padding_idx is not None:
            with torch.no_grad():
                self.weight[self.padding_idx] = 0

    def forward(self, idx):
        return F.embedding(idx, self.weight, self.padding_idx, self.max_norm, self.norm_type, self.scale_grad_by_freq, self.sparse)

    def extra_repr(self):
        s = "%d, %d" % (self.num_embeddings, self.embedding_dim)
        if self.padding_idx is not None:
            s += ", padding_idx=%s" % self.padding_idx
        if self.max_norm is not None:
            s += ", max_norm=%s" % self.max_norm
        if self.norm_type != 2:
            s += ", norm_type=%s" % self.norm_type
        if self.scale_grad_by_freq is not False:
            s += ", scale_grad_by_freq=%s" % self.scale_grad_by_freq
        if self.sparse is not False:
            s += ", sparse=True"
        return s

    @classmethod
    def from_pretrained(cls, embeddings, freeze=True, padding_idx=None, max_norm=None, norm_type=2.0, scale_grad_by_freq=False, sparse=False):
        if len(embeddings.shape) != 2:
            raise AssertionError("Embeddings parameter is expected to be 2-dimensional")
        rows, cols = embeddings.shape
        return cls(num_embeddings=rows, embedding_dim=cols, _weight=embeddings, _freeze=freeze, padding_idx=padding_idx, max_norm=max_norm, norm_type=norm_type, scale_grad_by_freq=scale_grad_by_freq, sparse=sparse)


class EmbeddingBag(Module):
    def __init__(self, num_embeddings, embedding_dim, max_norm=None, norm_type=2.0, scale_grad_by_freq=False, mode="mean", sparse=False, _weight=None, include_last_offset=False, padding_idx=None, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.num_embeddings, self.embedding_dim = num_embeddings, embedding_dim
        self.max_norm, self.norm_type, self.scale_grad_by_freq = max_norm, norm_type, scale_grad_by_freq
        if padding_idx is not None:
            if padding_idx < 0:
                if padding_idx < -num_embeddings:
                    raise AssertionError("padding_idx must be within num_embeddings")
                padding_idx = num_embeddings + padding_idx
            elif padding_idx >= num_embeddings:
                raise AssertionError("padding_idx must be within num_embeddings")
        self.padding_idx = padding_idx
        if _weight is None:
            self.weight = Parameter(torch.empty(num_embeddings, embedding_dim, dtype=dtype))
            self.reset_parameters()
        else:
            if list(_weight.shape) != [num_embeddings, embedding_dim]:
                raise AssertionError("Shape of weight does not match num_embeddings and embedding_dim")
            self.weight = Parameter(_weight)
        self.mode = mode
        self.sparse = sparse
        self.include_last_offset = include_last_offset

    def reset_parameters(self):
        init.normal_(self.weight)
        if self.padding_idx is not None:
            with torch.no_grad():
                self.weight[self.padding_idx] = 0

    def forward(self, input, offsets=None, per_sample_weights=None):
        return F.embedding_bag(input, self.weight, offsets, self.max_norm, self.norm_type, self.scale_grad_by_freq, self.mode, self.sparse, per_sample_weights, self.include_last_offset, self.padding_idx)

    def extra_repr(self):
        s = "%d, %d" % (self.num_embeddings, self.embedding_dim)
        if self.max_norm is not None:
            s += ", max_norm=%s" % self.max_norm
        if self.norm_type != 2:
            s += ", norm_type=%s" % self.norm_type
        if self.scale_grad_by_freq is not False:
            s += ", scale_grad_by_freq=%s" % self.scale_grad_by_freq
        s += ", mode=%r" % (self.mode,)
        if self.padding_idx is not None:
            s += ", padding_idx=%s" % self.padding_idx
        return s

    @classmethod
    def from_pretrained(cls, embeddings, freeze=True, max_norm=None, norm_type=2.0, scale_grad_by_freq=False, mode="mean", sparse=False, include_last_offset=False, padding_idx=None):
        rows, cols = embeddings.shape
        bag = cls(rows, cols, max_norm, norm_type, scale_grad_by_freq, mode, sparse, embeddings, include_last_offset, padding_idx)
        bag.weight.requires_grad = not freeze
        return bag


# ---- convolution -----------------------------------------------------------------------------
class _ConvNd(Module):
    _nd = 2

    def __init__(self, in_channels, out_channels, kernel_size, stride, padding, dilation, transposed, output_padding, groups, bias, padding_mode, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        nd = self._nd
        if isinstance(groups, bool) or not isinstance(groups, int) or groups <= 0:
            raise ValueError("groups must be a positive integer")
        if in_channels % groups != 0:
            raise ValueError("in_channels must be divisible by groups")
        if out_channels % groups != 0:
            raise ValueError("out_channels must be divisible by groups")
        if isinstance(padding, str):
            if padding not in ("same", "valid"):
                raise ValueError("Invalid padding string %r, should be one of {'valid', 'same'}" % padding)
            if padding == "same" and any(s != 1 for s in F._ntuple(stride, nd)):
                raise ValueError("padding='same' is not supported for strided convolutions")
        if padding_mode not in ("zeros", "reflect", "replicate", "circular"):
            raise ValueError("padding_mode must be one of ['zeros', 'reflect', 'replicate', 'circular'], but got padding_mode='%s'" % padding_mode)
        if dtype is not None and dtype not in (torch.float32, torch.float64):
            raise TypeError("convolutions require float32 or float64")
        self.in_channels = in_channels
        self.out_channels = out_channels
        self.kernel_size = F._ntuple(kernel_size, nd, "kernel_size")
        self.stride = F._ntuple(stride, nd, "stride")
        self.padding = padding if isinstance(padding, str) else F._ntuple(padding, nd, "padding")
        self.dilation = F._ntuple(dilation, nd, "dilation")
        self.transposed = transposed
        self.output_padding = F._ntuple(output_padding, nd, "output_padding")
        self.groups = groups
        self.padding_mode = padding_mode
        if isinstance(self.padding, str):
            rev = [0, 0] * nd
            if padding == "same":
                for d, k, i in zip(self.dilation, self.kernel_size, range(nd - 1, -1, -1)):
                    total = d * (k - 1)
                    left = total // 2
                    rev[2 * i] = left
                    rev[2 * i + 1] = total - left
        else:
            rev = []
            for p in reversed(self.padding):
                rev += [p, p]
            rev = tuple(rev)
        self._reversed_padding_repeated_twice = rev
        if transposed:
            shape = (in_channels, out_channels // groups) + self.kernel_size
        else:
            shape = (out_channels, in_channels // groups) + self.kernel_size
        self.weight = Parameter(torch.empty(*shape, dtype=dtype))
        if bias:
            self.bias = Parameter(torch.empty(out_channels, dtype=dtype))
        else:
            self.register_parameter("bias", None)
        self.reset_parameters()

    def reset_parameters(self):
        init.kaiming_uniform_(self.weight, a=math.sqrt(5))
        if self.bias is not None:
            fan_in, _ = init._calculate_fan_in_and_fan_out(self.weight)
            if fan_in != 0:
                bound = 1 / math.sqrt(fan_in)
                init.uniform_(self.bias, -bound, bound)

    def extra_repr(self):
        s = "%d, %d, kernel_size=%s, stride=%s" % (self.in_channels, self.out_channels, self.kernel_size, self.stride)
        if self.padding != (0,) * len(self.padding):
            s += ", padding=%s" % (self.padding,)
        if self.dilation != (1,) * len(self.dilation):
            s += ", dilation=%s" % (self.dilation,)
        if self.output_padding != (0,) * len(self.output_padding):
            s += ", output_padding=%s" % (self.output_padding,)
        if self.groups != 1:
            s += ", groups=%d" % self.groups
        if self.bias is None:
            s += ", bias=False"
        if self.padding_mode != "zeros":
            s += ", padding_mode=%s" % self.padding_mode
        return s

    def _conv(self, x, fn):
        if self.padding_mode != "zeros":
            return fn(F.pad(x, self._reversed_padding_repeated_twice, mode=self.padding_mode), self.weight, self.bias, self.stride, 0, self.dilation, self.groups)
        return fn(x, self.weight, self.bias, self.stride, self.padding, self.dilation, self.groups)


class Conv1d(_ConvNd):
    _nd = 1

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        super().__init__(in_channels, out_channels, kernel_size, stride, padding, dilation, False, 0, groups, bias, padding_mode, device, dtype)

    def forward(self, input):
        return self._conv(input, F.conv1d)


class Conv2d(_ConvNd):
    _nd = 2

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        # Zero channels are allowed, as in PyTorch (lazy convolutions start there).
        if any(isinstance(v, bool) or not isinstance(v, int) or v < 0 for v in (in_channels, out_channels)) or isinstance(groups, bool) or not isinstance(groups, int) or groups < 1:
            raise ValueError("channels and groups must be positive integers")
        super().__init__(in_channels, out_channels, kernel_size, stride, padding, dilation, False, 0, groups, bias, padding_mode, device, dtype)

    def forward(self, input):
        return self._conv(input, F.conv2d)


class _ConvTransposeNd(_ConvNd):
    def _output_padding(self, input, output_size):
        if output_size is None:
            return self.output_padding
        nd = self._nd
        output_size = list(output_size)
        if len(output_size) == nd + 2 or len(output_size) == nd + 1:
            output_size = output_size[-nd:]
        if len(output_size) != nd:
            raise ValueError("ConvTranspose%dD: for %dD input, output_size must have %d or %d elements (got %d)" % (nd, len(input.shape), nd, nd + 2, len(output_size)))
        spatial = input.shape[-nd:]
        out = []
        for d in range(nd):
            min_size = (spatial[d] - 1) * self.stride[d] - 2 * self.padding[d] + self.dilation[d] * (self.kernel_size[d] - 1) + 1
            max_size = min_size + self.stride[d] - 1
            if not min_size <= output_size[d] <= max_size:
                raise ValueError("requested an output size of %s, but valid sizes range from %s to %s (for an input of %s)" % (output_size, min_size, max_size, list(spatial)))
            out.append(output_size[d] - min_size)
        return tuple(out)


class ConvTranspose1d(_ConvTransposeNd):
    _nd = 1

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, output_padding=0, groups=1, bias=True, dilation=1, padding_mode="zeros", device=None, dtype=None):
        if padding_mode != "zeros":
            raise ValueError('Only "zeros" padding mode is supported for ConvTranspose1d')
        super().__init__(in_channels, out_channels, kernel_size, stride, padding, dilation, True, output_padding, groups, bias, padding_mode, device, dtype)

    def forward(self, input, output_size=None):
        op = self._output_padding(input, output_size)
        return F.conv_transpose1d(input, self.weight, self.bias, self.stride, self.padding, op, self.groups, self.dilation)


class ConvTranspose2d(_ConvTransposeNd):
    _nd = 2

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, output_padding=0, groups=1, bias=True, dilation=1, padding_mode="zeros", device=None, dtype=None):
        if padding_mode != "zeros":
            raise ValueError('Only "zeros" padding mode is supported for ConvTranspose2d')
        super().__init__(in_channels, out_channels, kernel_size, stride, padding, dilation, True, output_padding, groups, bias, padding_mode, device, dtype)

    def forward(self, input, output_size=None):
        op = self._output_padding(input, output_size)
        return F.conv_transpose2d(input, self.weight, self.bias, self.stride, self.padding, op, self.groups, self.dilation)


class Conv3d(_ConvNd):
    _nd = 3

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        super().__init__(in_channels, out_channels, kernel_size, stride, padding, dilation, False, 0, groups, bias, padding_mode, device, dtype)

    def forward(self, input):
        return self._conv(input, F.conv3d)


class ConvTranspose3d(_ConvTransposeNd):
    _nd = 3

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, output_padding=0, groups=1, bias=True, dilation=1, padding_mode="zeros", device=None, dtype=None):
        if padding_mode != "zeros":
            raise ValueError('Only "zeros" padding mode is supported for ConvTranspose3d')
        super().__init__(in_channels, out_channels, kernel_size, stride, padding, dilation, True, output_padding, groups, bias, padding_mode, device, dtype)

    def forward(self, input, output_size=None):
        op = self._output_padding(input, output_size)
        return F.conv_transpose3d(input, self.weight, self.bias, self.stride, self.padding, op, self.groups, self.dilation)


class Unfold(Module):
    def __init__(self, kernel_size, dilation=1, padding=0, stride=1):
        super().__init__()
        self.kernel_size, self.dilation, self.padding, self.stride = kernel_size, dilation, padding, stride

    def forward(self, input):
        return F.unfold(input, self.kernel_size, self.dilation, self.padding, self.stride)

    def extra_repr(self):
        return "kernel_size=%s, dilation=%s, padding=%s, stride=%s" % (self.kernel_size, self.dilation, self.padding, self.stride)


class Fold(Module):
    def __init__(self, output_size, kernel_size, dilation=1, padding=0, stride=1):
        super().__init__()
        self.output_size, self.kernel_size, self.dilation, self.padding, self.stride = output_size, kernel_size, dilation, padding, stride

    def forward(self, input):
        return F.fold(input, self.output_size, self.kernel_size, self.dilation, self.padding, self.stride)

    def extra_repr(self):
        return "output_size=%s, kernel_size=%s, dilation=%s, padding=%s, stride=%s" % (self.output_size, self.kernel_size, self.dilation, self.padding, self.stride)


# ---- pooling ---------------------------------------------------------------------------------
class _MaxPoolNd(Module):
    def __init__(self, kernel_size, stride=None, padding=0, dilation=1, return_indices=False, ceil_mode=False):
        super().__init__()
        self.kernel_size = kernel_size
        self.stride = stride if stride is not None else kernel_size
        self.padding = padding
        self.dilation = dilation
        self.return_indices = return_indices
        self.ceil_mode = ceil_mode

    def extra_repr(self):
        return "kernel_size=%s, stride=%s, padding=%s, dilation=%s, ceil_mode=%s" % (self.kernel_size, self.stride, self.padding, self.dilation, self.ceil_mode)


class MaxPool1d(_MaxPoolNd):
    def forward(self, input):
        return F.max_pool1d(input, self.kernel_size, self.stride, self.padding, self.dilation, ceil_mode=self.ceil_mode, return_indices=self.return_indices)


class MaxPool2d(_MaxPoolNd):
    def forward(self, input):
        return F.max_pool2d(input, self.kernel_size, self.stride, self.padding, self.dilation, ceil_mode=self.ceil_mode, return_indices=self.return_indices)


class MaxPool3d(_MaxPoolNd):
    def forward(self, input):
        return F.max_pool3d(input, self.kernel_size, self.stride, self.padding, self.dilation, ceil_mode=self.ceil_mode, return_indices=self.return_indices)


class _MaxUnpoolNd(Module):
    _nd = 1

    def __init__(self, kernel_size, stride=None, padding=0):
        super().__init__()
        self.kernel_size = F._ntuple(kernel_size, self._nd)
        self.stride = F._ntuple(stride if stride is not None else kernel_size, self._nd)
        self.padding = F._ntuple(padding, self._nd)

    def forward(self, input, indices, output_size=None):
        return F._max_unpool(input, indices, self._nd, self.kernel_size, self.stride, self.padding, output_size, "max_unpool%dd" % self._nd)

    def extra_repr(self):
        return "kernel_size=%s, stride=%s, padding=%s" % (self.kernel_size, self.stride, self.padding)


class MaxUnpool1d(_MaxUnpoolNd):
    _nd = 1


class MaxUnpool2d(_MaxUnpoolNd):
    _nd = 2


class MaxUnpool3d(_MaxUnpoolNd):
    _nd = 3


class AvgPool1d(Module):
    def __init__(self, kernel_size, stride=None, padding=0, ceil_mode=False, count_include_pad=True):
        super().__init__()
        self.kernel_size = F._single(kernel_size)
        self.stride = F._single(stride if stride is not None else kernel_size)
        self.padding = F._single(padding)
        self.ceil_mode = ceil_mode
        self.count_include_pad = count_include_pad

    def forward(self, input):
        return F.avg_pool1d(input, self.kernel_size, self.stride, self.padding, self.ceil_mode, self.count_include_pad)

    def extra_repr(self):
        return "kernel_size=%s, stride=%s, padding=%s" % (self.kernel_size, self.stride, self.padding)


class AvgPool2d(Module):
    def __init__(self, kernel_size, stride=None, padding=0, ceil_mode=False, count_include_pad=True, divisor_override=None):
        super().__init__()
        self.kernel_size = kernel_size
        self.stride = stride if stride is not None else kernel_size
        self.padding = padding
        self.ceil_mode = ceil_mode
        self.count_include_pad = count_include_pad
        self.divisor_override = divisor_override

    def forward(self, input):
        return F.avg_pool2d(input, self.kernel_size, self.stride, self.padding, self.ceil_mode, self.count_include_pad, self.divisor_override)

    def extra_repr(self):
        return "kernel_size=%s, stride=%s, padding=%s" % (self.kernel_size, self.stride, self.padding)


class AvgPool3d(AvgPool2d):
    def forward(self, input):
        return F.avg_pool3d(input, self.kernel_size, self.stride, self.padding, self.ceil_mode, self.count_include_pad, self.divisor_override)


class LPPool1d(Module):
    def __init__(self, norm_type, kernel_size, stride=None, ceil_mode=False):
        super().__init__()
        self.norm_type, self.kernel_size, self.stride, self.ceil_mode = norm_type, kernel_size, stride, ceil_mode

    def forward(self, input):
        return F.lp_pool1d(input, float(self.norm_type), self.kernel_size, self.stride, self.ceil_mode)

    def extra_repr(self):
        return "norm_type=%s, kernel_size=%s, stride=%s, ceil_mode=%s" % (self.norm_type, self.kernel_size, self.stride, self.ceil_mode)


class LPPool2d(LPPool1d):
    def forward(self, input):
        return F.lp_pool2d(input, float(self.norm_type), self.kernel_size, self.stride, self.ceil_mode)


class LPPool3d(LPPool1d):
    def forward(self, input):
        return F.lp_pool3d(input, float(self.norm_type), self.kernel_size, self.stride, self.ceil_mode)


class _AdaptivePool(Module):
    def __init__(self, output_size, return_indices=False):
        super().__init__()
        self.output_size = output_size
        self.return_indices = return_indices

    def extra_repr(self):
        return "output_size=%s" % (self.output_size,)


class AdaptiveAvgPool1d(_AdaptivePool):
    def __init__(self, output_size):
        super().__init__(output_size)

    def forward(self, input):
        return F.adaptive_avg_pool1d(input, self.output_size)


class AdaptiveAvgPool2d(_AdaptivePool):
    def __init__(self, output_size):
        super().__init__(output_size)

    def forward(self, input):
        return F.adaptive_avg_pool2d(input, self.output_size)


class AdaptiveMaxPool1d(_AdaptivePool):
    def forward(self, input):
        return F.adaptive_max_pool1d(input, self.output_size, self.return_indices)


class AdaptiveMaxPool2d(_AdaptivePool):
    def forward(self, input):
        return F.adaptive_max_pool2d(input, self.output_size, self.return_indices)


class AdaptiveAvgPool3d(_AdaptivePool):
    def __init__(self, output_size):
        super().__init__(output_size)

    def forward(self, input):
        return F.adaptive_avg_pool3d(input, self.output_size)


class AdaptiveMaxPool3d(_AdaptivePool):
    def forward(self, input):
        return F.adaptive_max_pool3d(input, self.output_size, self.return_indices)


# ---- normalisation ---------------------------------------------------------------------------
class _NormBase(Module):
    _version = 2

    def __init__(self, num_features, eps=1e-5, momentum=0.1, affine=True, track_running_stats=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.num_features = num_features
        self.eps = eps
        self.momentum = momentum
        self.affine = affine
        self.track_running_stats = track_running_stats
        if affine:
            self.weight = Parameter(torch.empty(num_features, dtype=dtype))
            self.bias = Parameter(torch.empty(num_features, dtype=dtype))
        else:
            self.register_parameter("weight", None)
            self.register_parameter("bias", None)
        if track_running_stats:
            self.register_buffer("running_mean", torch.zeros(num_features, dtype=dtype))
            self.register_buffer("running_var", torch.ones(num_features, dtype=dtype))
            self.register_buffer("num_batches_tracked", torch.tensor(0, dtype=torch.int64))
        else:
            self.register_buffer("running_mean", None)
            self.register_buffer("running_var", None)
            self.register_buffer("num_batches_tracked", None)
        self.reset_parameters()

    def reset_running_stats(self):
        if self.track_running_stats:
            self.running_mean.zero_()
            self.running_var.fill_(1)
            self.num_batches_tracked.zero_()

    def reset_parameters(self):
        self.reset_running_stats()
        if self.affine:
            init.ones_(self.weight)
            init.zeros_(self.bias)

    def _check_input_dim(self, input):
        raise NotImplementedError

    def extra_repr(self):
        return "%s, eps=%s, momentum=%s, affine=%s, track_running_stats=%s" % (self.num_features, self.eps, self.momentum, self.affine, self.track_running_stats)

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        version = local_metadata.get("version", None)
        if (version is None or version < 2) and self.track_running_stats:
            key = prefix + "num_batches_tracked"
            if key not in state_dict:
                state_dict[key] = self.num_batches_tracked if self.num_batches_tracked is not None else torch.tensor(0, dtype=torch.int64)
        Module._load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs)


class _BatchNorm(_NormBase):
    def forward(self, input):
        self._check_input_dim(input)
        factor = 0.0 if self.momentum is None else self.momentum
        if self.training and self.track_running_stats and self.num_batches_tracked is not None:
            self.num_batches_tracked.copy_(self.num_batches_tracked + 1)
            factor = 1.0 / float(self.num_batches_tracked.item()) if self.momentum is None else self.momentum
        bn_training = True if self.training else (self.running_mean is None and self.running_var is None)
        use_stats = not self.training or self.track_running_stats
        return F.batch_norm(input, self.running_mean if use_stats else None, self.running_var if use_stats else None, self.weight, self.bias, bn_training, factor, self.eps)


class BatchNorm1d(_BatchNorm):
    def _check_input_dim(self, input):
        if input.dim() != 2 and input.dim() != 3:
            raise ValueError("expected 2D or 3D input (got %dD input)" % input.dim())


class BatchNorm2d(_BatchNorm):
    def _check_input_dim(self, input):
        if input.dim() != 4:
            raise ValueError("expected 4D input (got %dD input)" % input.dim())


class BatchNorm3d(_BatchNorm):
    def _check_input_dim(self, input):
        if input.dim() != 5:
            raise ValueError("expected 5D input (got %dD input)" % input.dim())


class _InstanceNorm(_NormBase):
    _batched_rank = 3

    def __init__(self, num_features, eps=1e-5, momentum=0.1, affine=False, track_running_stats=False, device=None, dtype=None):
        super().__init__(num_features, eps, momentum, affine, track_running_stats, device, dtype)

    def forward(self, input):
        rank = self._batched_rank
        if input.dim() not in (rank - 1, rank):
            raise ValueError("expected %dD or %dD input (got %dD input)" % (rank - 1, rank, input.dim()))
        unbatched = input.dim() == rank - 1
        x = input.unsqueeze(0) if unbatched else input
        if x.shape[1] != self.num_features and self.affine:
            raise ValueError("expected input's size at dim=%d to match num_features (%d), but got: %d." % (1 if not unbatched else 0, self.num_features, x.shape[1]))
        use_input_stats = self.training or not self.track_running_stats
        out = F.instance_norm(x, self.running_mean, self.running_var, self.weight, self.bias, use_input_stats, 0.1 if self.momentum is None else self.momentum, self.eps)
        return out.squeeze(0) if unbatched else out


class InstanceNorm1d(_InstanceNorm):
    _batched_rank = 3


class InstanceNorm2d(_InstanceNorm):
    _batched_rank = 4


class InstanceNorm3d(_InstanceNorm):
    _batched_rank = 5


class LayerNorm(Module):
    def __init__(self, normalized_shape, eps=1e-5, elementwise_affine=True, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        shape = (normalized_shape,) if isinstance(normalized_shape, int) else tuple(normalized_shape)
        self.normalized_shape = shape
        self.eps = eps
        self.elementwise_affine = elementwise_affine
        if elementwise_affine:
            self.weight = Parameter(torch.ones(*shape, dtype=dtype))
            if bias:
                self.bias = Parameter(torch.zeros(*shape, dtype=dtype))
            else:
                self.register_parameter("bias", None)
        else:
            self.register_parameter("weight", None)
            self.register_parameter("bias", None)

    def reset_parameters(self):
        if self.elementwise_affine:
            init.ones_(self.weight)
            if self.bias is not None:
                init.zeros_(self.bias)

    def forward(self, x):
        return F.layer_norm(x, self.normalized_shape, self.weight, self.bias, self.eps)

    def extra_repr(self):
        return "%s, eps=%s, elementwise_affine=%s" % (self.normalized_shape, self.eps, self.elementwise_affine)


class RMSNorm(Module):
    def __init__(self, normalized_shape, eps=None, elementwise_affine=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        shape = (normalized_shape,) if isinstance(normalized_shape, int) else tuple(normalized_shape)
        self.normalized_shape = shape
        self.eps = eps
        self.elementwise_affine = elementwise_affine
        if elementwise_affine:
            self.weight = Parameter(torch.ones(*shape, dtype=dtype))
        else:
            self.register_parameter("weight", None)

    def reset_parameters(self):
        if self.elementwise_affine:
            init.ones_(self.weight)

    def forward(self, x):
        return F.rms_norm(x, self.normalized_shape, self.weight, self.eps)

    def extra_repr(self):
        return "%s, eps=%s, elementwise_affine=%s" % (self.normalized_shape, self.eps, self.elementwise_affine)


class GroupNorm(Module):
    def __init__(self, num_groups, num_channels, eps=1e-5, affine=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        if num_channels % num_groups != 0:
            raise ValueError("num_channels must be divisible by num_groups")
        self.num_groups, self.num_channels, self.eps, self.affine = num_groups, num_channels, eps, affine
        if affine:
            self.weight = Parameter(torch.ones(num_channels, dtype=dtype))
            self.bias = Parameter(torch.zeros(num_channels, dtype=dtype))
        else:
            self.register_parameter("weight", None)
            self.register_parameter("bias", None)

    def reset_parameters(self):
        if self.affine:
            init.ones_(self.weight)
            init.zeros_(self.bias)

    def forward(self, input):
        return F.group_norm(input, self.num_groups, self.weight, self.bias, self.eps)

    def extra_repr(self):
        return "%s, %s, eps=%s, affine=%s" % (self.num_groups, self.num_channels, self.eps, self.affine)


class LocalResponseNorm(Module):
    def __init__(self, size, alpha=1e-4, beta=0.75, k=1.0):
        super().__init__()
        self.size, self.alpha, self.beta, self.k = size, alpha, beta, k

    def forward(self, input):
        return F.local_response_norm(input, self.size, self.alpha, self.beta, self.k)

    def extra_repr(self):
        return "%s, alpha=%s, beta=%s, k=%s" % (self.size, self.alpha, self.beta, self.k)


# ---- dropout ---------------------------------------------------------------------------------
class _DropoutNd(Module):
    def __init__(self, p=0.5, inplace=False):
        super().__init__()
        if p < 0 or p > 1:
            raise ValueError("dropout probability has to be between 0 and 1, but got %s" % p)
        self.p = p
        self.inplace = inplace

    def extra_repr(self):
        return "p=%s, inplace=%s" % (self.p, self.inplace)


class Dropout(_DropoutNd):
    def forward(self, x):
        return F.dropout(x, self.p, self.training, self.inplace)


class Dropout1d(_DropoutNd):
    def forward(self, x):
        return F.dropout1d(x, self.p, self.training, self.inplace)


class Dropout2d(_DropoutNd):
    def forward(self, x):
        return F.dropout2d(x, self.p, self.training, self.inplace)


class Dropout3d(_DropoutNd):
    def forward(self, x):
        return F.dropout3d(x, self.p, self.training, self.inplace)


class AlphaDropout(_DropoutNd):
    def forward(self, x):
        return F.alpha_dropout(x, self.p, self.training)


class FeatureAlphaDropout(_DropoutNd):
    def forward(self, x):
        return F.feature_alpha_dropout(x, self.p, self.training)


# ---- activations -----------------------------------------------------------------------------
class _Inplace(Module):
    """An activation with only an `inplace` flag."""
    _fn = None

    def __init__(self, inplace=False):
        super().__init__()
        self.inplace = inplace

    def forward(self, input):
        return type(self)._fn(input, self.inplace)

    def extra_repr(self):
        return "inplace=True" if self.inplace else ""


class _Plain(Module):
    """An activation without options."""
    _fn = None

    def forward(self, input):
        return type(self)._fn(input)


class ReLU(_Inplace):
    _fn = staticmethod(F.relu)


class SiLU(_Inplace):
    _fn = staticmethod(F.silu)


class Mish(_Inplace):
    _fn = staticmethod(F.mish)


class Hardswish(_Inplace):
    _fn = staticmethod(F.hardswish)


class Hardsigmoid(_Inplace):
    _fn = staticmethod(F.hardsigmoid)


class SELU(_Inplace):
    _fn = staticmethod(F.selu)


class Tanh(_Plain):
    _fn = staticmethod(torch.tanh)


class Sigmoid(_Plain):
    _fn = staticmethod(torch.sigmoid)


class LogSigmoid(_Plain):
    _fn = staticmethod(F.logsigmoid)


class Softsign(_Plain):
    _fn = staticmethod(F.softsign)


class Tanhshrink(_Plain):
    _fn = staticmethod(F.tanhshrink)


class GELU(Module):
    def __init__(self, approximate="none"):
        super().__init__()
        self.approximate = approximate

    def forward(self, x):
        return F.gelu(x, self.approximate)

    def extra_repr(self):
        return "approximate=%r" % self.approximate


class Threshold(Module):
    def __init__(self, threshold, value, inplace=False):
        super().__init__()
        self.threshold, self.value, self.inplace = threshold, value, inplace

    def forward(self, input):
        return F.threshold(input, self.threshold, self.value, self.inplace)

    def extra_repr(self):
        return "threshold=%s, value=%s%s" % (self.threshold, self.value, ", inplace=True" if self.inplace else "")


class Hardtanh(Module):
    def __init__(self, min_val=-1.0, max_val=1.0, inplace=False, min_value=None, max_value=None):
        super().__init__()
        if min_value is not None:
            min_val = min_value
        if max_value is not None:
            max_val = max_value
        if max_val <= min_val:
            raise AssertionError("max_val must be greater than min_val")
        self.min_val, self.max_val, self.inplace = min_val, max_val, inplace

    def forward(self, input):
        return F.hardtanh(input, self.min_val, self.max_val, self.inplace)

    def extra_repr(self):
        return "min_val=%s, max_val=%s%s" % (self.min_val, self.max_val, ", inplace=True" if self.inplace else "")


class ReLU6(Hardtanh):
    def __init__(self, inplace=False):
        super().__init__(0.0, 6.0, inplace)

    def extra_repr(self):
        return "inplace=True" if self.inplace else ""


class LeakyReLU(Module):
    def __init__(self, negative_slope=1e-2, inplace=False):
        super().__init__()
        self.negative_slope, self.inplace = negative_slope, inplace

    def forward(self, input):
        return F.leaky_relu(input, self.negative_slope, self.inplace)

    def extra_repr(self):
        return "negative_slope=%s%s" % (self.negative_slope, ", inplace=True" if self.inplace else "")


class ELU(Module):
    _fn = staticmethod(F.elu)

    def __init__(self, alpha=1.0, inplace=False):
        super().__init__()
        self.alpha, self.inplace = alpha, inplace

    def forward(self, input):
        return type(self)._fn(input, self.alpha, self.inplace)

    def extra_repr(self):
        return "alpha=%s%s" % (self.alpha, ", inplace=True" if self.inplace else "")


class CELU(ELU):
    _fn = staticmethod(F.celu)


class RReLU(Module):
    def __init__(self, lower=1.0 / 8, upper=1.0 / 3, inplace=False):
        super().__init__()
        self.lower, self.upper, self.inplace = lower, upper, inplace

    def forward(self, input):
        return F.rrelu(input, self.lower, self.upper, self.training, self.inplace)

    def extra_repr(self):
        return "lower=%s, upper=%s%s" % (self.lower, self.upper, ", inplace=True" if self.inplace else "")


class PReLU(Module):
    def __init__(self, num_parameters=1, init=0.25, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.num_parameters = num_parameters
        self.init = init
        self.weight = Parameter(torch.empty(num_parameters, dtype=dtype))
        self.reset_parameters()

    def reset_parameters(self):
        init.constant_(self.weight, self.init)

    def forward(self, input):
        return F.prelu(input, self.weight)

    def extra_repr(self):
        return "num_parameters=%s" % self.num_parameters


class Softplus(Module):
    def __init__(self, beta=1.0, threshold=20.0):
        super().__init__()
        self.beta, self.threshold = beta, threshold

    def forward(self, input):
        return F.softplus(input, self.beta, self.threshold)

    def extra_repr(self):
        return "beta=%s, threshold=%s" % (self.beta, self.threshold)


class Hardshrink(Module):
    def __init__(self, lambd=0.5):
        super().__init__()
        self.lambd = lambd

    def forward(self, input):
        return F.hardshrink(input, self.lambd)

    def extra_repr(self):
        return "%s" % self.lambd


class Softshrink(Hardshrink):
    def forward(self, input):
        return F.softshrink(input, self.lambd)


class GLU(Module):
    def __init__(self, dim=-1):
        super().__init__()
        self.dim = dim

    def forward(self, input):
        return F.glu(input, self.dim)

    def extra_repr(self):
        return "dim=%s" % self.dim


class Softmax(Module):
    _fn = staticmethod(F.softmax)

    def __init__(self, dim=None):
        super().__init__()
        self.dim = dim

    def forward(self, x):
        return type(self)._fn(x, self.dim)

    def extra_repr(self):
        return "dim=%s" % (self.dim,)


class LogSoftmax(Softmax):
    _fn = staticmethod(F.log_softmax)


class Softmin(Softmax):
    _fn = staticmethod(F.softmin)


class Softmax2d(Module):
    def forward(self, input):
        if input.dim() not in (3, 4):
            raise ValueError("Softmax2d: expected input to be 3D or 4D, got %dD instead" % input.dim())
        return F.softmax(input, -3)


# ---- distances -------------------------------------------------------------------------------
class CosineSimilarity(Module):
    def __init__(self, dim=1, eps=1e-8):
        super().__init__()
        self.dim, self.eps = dim, eps

    def forward(self, x1, x2):
        return F.cosine_similarity(x1, x2, self.dim, self.eps)


class PairwiseDistance(Module):
    def __init__(self, p=2.0, eps=1e-6, keepdim=False):
        super().__init__()
        self.norm, self.eps, self.keepdim = p, eps, keepdim

    def forward(self, x1, x2):
        return F.pairwise_distance(x1, x2, self.norm, self.eps, self.keepdim)


# ---- losses ----------------------------------------------------------------------------------
class _Loss(Module):
    def __init__(self, size_average=None, reduce=None, reduction="mean"):
        super().__init__()
        self.reduction = F._reduction(size_average, reduce, reduction)


class _WeightedLoss(_Loss):
    def __init__(self, weight=None, size_average=None, reduce=None, reduction="mean"):
        super().__init__(size_average, reduce, reduction)
        self.register_buffer("weight", weight)


class L1Loss(_Loss):
    def forward(self, input, target):
        return F.l1_loss(input, target, reduction=self.reduction)


class MSELoss(_Loss):
    def forward(self, input, target):
        return F.mse_loss(input, target, reduction=self.reduction)


class NLLLoss(_WeightedLoss):
    def __init__(self, weight=None, size_average=None, ignore_index=-100, reduce=None, reduction="mean"):
        super().__init__(weight, size_average, reduce, reduction)
        self.ignore_index = ignore_index

    def forward(self, input, target):
        return F.nll_loss(input, target, weight=self.weight, ignore_index=self.ignore_index, reduction=self.reduction)


class CrossEntropyLoss(_WeightedLoss):
    def __init__(self, weight=None, size_average=None, ignore_index=-100, reduce=None, reduction="mean", label_smoothing=0.0):
        super().__init__(weight, size_average, reduce, reduction)
        self.ignore_index = ignore_index
        self.label_smoothing = label_smoothing

    def forward(self, input, target):
        return F.cross_entropy(input, target, weight=self.weight, ignore_index=self.ignore_index, reduction=self.reduction, label_smoothing=self.label_smoothing)


class BCELoss(_WeightedLoss):
    def forward(self, input, target):
        return F.binary_cross_entropy(input, target, weight=self.weight, reduction=self.reduction)


class BCEWithLogitsLoss(_Loss):
    def __init__(self, weight=None, size_average=None, reduce=None, reduction="mean", pos_weight=None):
        super().__init__(size_average, reduce, reduction)
        self.register_buffer("weight", weight)
        self.register_buffer("pos_weight", pos_weight)

    def forward(self, input, target):
        return F.binary_cross_entropy_with_logits(input, target, self.weight, pos_weight=self.pos_weight, reduction=self.reduction)


class SmoothL1Loss(_Loss):
    def __init__(self, size_average=None, reduce=None, reduction="mean", beta=1.0):
        super().__init__(size_average, reduce, reduction)
        self.beta = beta

    def forward(self, input, target):
        return F.smooth_l1_loss(input, target, reduction=self.reduction, beta=self.beta)


class HuberLoss(_Loss):
    def __init__(self, reduction="mean", delta=1.0):
        super().__init__(reduction=reduction)
        self.delta = delta

    def forward(self, input, target):
        return F.huber_loss(input, target, reduction=self.reduction, delta=self.delta)


class KLDivLoss(_Loss):
    def __init__(self, size_average=None, reduce=None, reduction="mean", log_target=False):
        super().__init__(size_average, reduce, reduction)
        self.log_target = log_target

    def forward(self, input, target):
        return F.kl_div(input, target, reduction=self.reduction, log_target=self.log_target)


class PoissonNLLLoss(_Loss):
    def __init__(self, log_input=True, full=False, size_average=None, eps=1e-8, reduce=None, reduction="mean"):
        super().__init__(size_average, reduce, reduction)
        self.log_input, self.full, self.eps = log_input, full, eps

    def forward(self, log_input, target):
        return F.poisson_nll_loss(log_input, target, log_input=self.log_input, full=self.full, eps=self.eps, reduction=self.reduction)


class GaussianNLLLoss(_Loss):
    def __init__(self, full=False, eps=1e-6, reduction="mean"):
        super().__init__(None, None, reduction)
        self.full, self.eps = full, eps

    def forward(self, input, target, var):
        return F.gaussian_nll_loss(input, target, var, full=self.full, eps=self.eps, reduction=self.reduction)


class CosineEmbeddingLoss(_Loss):
    def __init__(self, margin=0.0, size_average=None, reduce=None, reduction="mean"):
        super().__init__(size_average, reduce, reduction)
        self.margin = margin

    def forward(self, input1, input2, target):
        return F.cosine_embedding_loss(input1, input2, target, margin=self.margin, reduction=self.reduction)


class MarginRankingLoss(_Loss):
    def __init__(self, margin=0.0, size_average=None, reduce=None, reduction="mean"):
        super().__init__(size_average, reduce, reduction)
        self.margin = margin

    def forward(self, input1, input2, target):
        return F.margin_ranking_loss(input1, input2, target, margin=self.margin, reduction=self.reduction)


class HingeEmbeddingLoss(_Loss):
    def __init__(self, margin=1.0, size_average=None, reduce=None, reduction="mean"):
        super().__init__(size_average, reduce, reduction)
        self.margin = margin

    def forward(self, input, target):
        return F.hinge_embedding_loss(input, target, margin=self.margin, reduction=self.reduction)


class SoftMarginLoss(_Loss):
    def forward(self, input, target):
        return F.soft_margin_loss(input, target, reduction=self.reduction)


class MultiLabelSoftMarginLoss(_WeightedLoss):
    def forward(self, input, target):
        return F.multilabel_soft_margin_loss(input, target, weight=self.weight, reduction=self.reduction)


class MultiMarginLoss(_WeightedLoss):
    def __init__(self, p=1, margin=1.0, weight=None, size_average=None, reduce=None, reduction="mean"):
        super().__init__(weight, size_average, reduce, reduction)
        if p not in (1, 2):
            raise ValueError("only p == 1 and p == 2 supported")
        self.p, self.margin = p, margin

    def forward(self, input, target):
        return F.multi_margin_loss(input, target, p=self.p, margin=self.margin, weight=self.weight, reduction=self.reduction)


class MultiLabelMarginLoss(_Loss):
    def forward(self, input, target):
        return F.multilabel_margin_loss(input, target, reduction=self.reduction)


class TripletMarginLoss(_Loss):
    def __init__(self, margin=1.0, p=2.0, eps=1e-6, swap=False, size_average=None, reduce=None, reduction="mean"):
        super().__init__(size_average, reduce, reduction)
        if margin <= 0:
            raise ValueError("TripletMarginLoss: expected margin to be greater than 0, got %s instead" % margin)
        self.margin, self.p, self.eps, self.swap = margin, p, eps, swap

    def forward(self, anchor, positive, negative):
        return F.triplet_margin_loss(anchor, positive, negative, margin=self.margin, p=self.p, eps=self.eps, swap=self.swap, reduction=self.reduction)


class TripletMarginWithDistanceLoss(_Loss):
    def __init__(self, distance_function=None, margin=1.0, swap=False, reduction="mean"):
        super().__init__(reduction=reduction)
        self.distance_function, self.margin, self.swap = distance_function, margin, swap

    def forward(self, anchor, positive, negative):
        return F.triplet_margin_with_distance_loss(anchor, positive, negative, distance_function=self.distance_function, margin=self.margin, swap=self.swap, reduction=self.reduction)


# ---- resampling and padding ------------------------------------------------------------------
class Upsample(Module):
    def __init__(self, size=None, scale_factor=None, mode="nearest", align_corners=None, recompute_scale_factor=None):
        super().__init__()
        self.name = type(self).__name__
        self.size = size
        self.scale_factor = float(scale_factor) if isinstance(scale_factor, (int, float)) else tuple(float(f) for f in scale_factor) if scale_factor else None
        self.mode = mode
        self.align_corners = align_corners
        self.recompute_scale_factor = recompute_scale_factor

    def forward(self, input):
        return F.interpolate(input, self.size, self.scale_factor, self.mode, self.align_corners, recompute_scale_factor=self.recompute_scale_factor)

    def extra_repr(self):
        info = ("scale_factor=" + repr(self.scale_factor)) if self.scale_factor is not None else ("size=" + repr(self.size))
        return info + ", mode=" + repr(self.mode)


class UpsamplingNearest2d(Upsample):
    def __init__(self, size=None, scale_factor=None):
        super().__init__(size, scale_factor, mode="nearest")


class UpsamplingBilinear2d(Upsample):
    def __init__(self, size=None, scale_factor=None):
        super().__init__(size, scale_factor, mode="bilinear", align_corners=True)


class PixelShuffle(Module):
    def __init__(self, upscale_factor):
        super().__init__()
        self.upscale_factor = upscale_factor

    def forward(self, input):
        return F.pixel_shuffle(input, self.upscale_factor)

    def extra_repr(self):
        return "upscale_factor=%s" % self.upscale_factor


class PixelUnshuffle(Module):
    def __init__(self, downscale_factor):
        super().__init__()
        self.downscale_factor = downscale_factor

    def forward(self, input):
        return F.pixel_unshuffle(input, self.downscale_factor)

    def extra_repr(self):
        return "downscale_factor=%s" % self.downscale_factor


class ChannelShuffle(Module):
    def __init__(self, groups):
        super().__init__()
        self.groups = groups

    def forward(self, input):
        return F.channel_shuffle(input, self.groups)

    def extra_repr(self):
        return "groups=%s" % self.groups


class _PadNd(Module):
    _mode = "constant"
    _n = 1

    def __init__(self, padding, value=0.0):
        super().__init__()
        self.padding = F._ntuple(padding, 2 * self._n, "padding")
        self.value = value

    def forward(self, input):
        if self._mode == "constant":
            return F.pad(input, self.padding, "constant", self.value)
        return F.pad(input, self.padding, self._mode)

    def extra_repr(self):
        if self._mode == "constant" and not isinstance(self, (ZeroPad1d, ZeroPad2d, ZeroPad3d)):
            return "padding=%s, value=%s" % (self.padding, self.value)
        return "%s" % (self.padding,)


class ConstantPad1d(_PadNd):
    _n = 1


class ConstantPad2d(_PadNd):
    _n = 2


class ZeroPad1d(_PadNd):
    _n = 1

    def __init__(self, padding):
        super().__init__(padding, 0.0)


class ZeroPad2d(_PadNd):
    _n = 2

    def __init__(self, padding):
        super().__init__(padding, 0.0)


class ReflectionPad1d(_PadNd):
    _mode, _n = "reflect", 1


class ReflectionPad2d(_PadNd):
    _mode, _n = "reflect", 2


class ReplicationPad1d(_PadNd):
    _mode, _n = "replicate", 1


class ReplicationPad2d(_PadNd):
    _mode, _n = "replicate", 2


class CircularPad1d(_PadNd):
    _mode, _n = "circular", 1


class CircularPad2d(_PadNd):
    _mode, _n = "circular", 2


class ConstantPad3d(_PadNd):
    _n = 3


class ZeroPad3d(_PadNd):
    _n = 3

    def __init__(self, padding):
        super().__init__(padding, 0.0)


class ReflectionPad3d(_PadNd):
    _mode, _n = "reflect", 3


class ReplicationPad3d(_PadNd):
    _mode, _n = "replicate", 3


class CircularPad3d(_PadNd):
    _mode, _n = "circular", 3


# ---- recurrent cells -------------------------------------------------------------------------
class RNNCellBase(Module):
    def __init__(self, input_size, hidden_size, bias, num_chunks, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.input_size = input_size
        self.hidden_size = hidden_size
        self.bias = bias
        self.weight_ih = Parameter(torch.empty(num_chunks * hidden_size, input_size, dtype=dtype))
        self.weight_hh = Parameter(torch.empty(num_chunks * hidden_size, hidden_size, dtype=dtype))
        if bias:
            self.bias_ih = Parameter(torch.empty(num_chunks * hidden_size, dtype=dtype))
            self.bias_hh = Parameter(torch.empty(num_chunks * hidden_size, dtype=dtype))
        else:
            self.register_parameter("bias_ih", None)
            self.register_parameter("bias_hh", None)
        self.reset_parameters()

    def reset_parameters(self):
        stdv = 1.0 / math.sqrt(self.hidden_size) if self.hidden_size > 0 else 0
        for p in self.parameters():
            init.uniform_(p, -stdv, stdv)

    def extra_repr(self):
        s = "%s, %s" % (self.input_size, self.hidden_size)
        if self.bias is not True:
            s += ", bias=%s" % self.bias
        nl = self.__dict__.get("nonlinearity", "tanh")
        if nl != "tanh":
            s += ", nonlinearity=%s" % nl
        return s

    def _batch(self, x, name):
        if x.dim() not in (1, 2):
            raise ValueError("%s: Expected input to be 1D or 2D, got %dD instead" % (name, x.dim()))
        return x.dim() == 2


def _gru_step(gi, h, w_hh, b_hh):
    gh = F.linear(h, w_hh, b_hh)
    i_r, i_z, i_n = gi.chunk(3, -1)
    h_r, h_z, h_n = gh.chunk(3, -1)
    r = torch.sigmoid(i_r + h_r)
    z = torch.sigmoid(i_z + h_z)
    n = torch.tanh(i_n + r * h_n)
    return (1 - z) * n + z * h


def _lstm_step(gi, h, c, w_hh, b_hh, w_hr=None):
    gates = gi + F.linear(h, w_hh, b_hh)
    i, f, g, o = gates.chunk(4, -1)
    c2 = torch.sigmoid(f) * c + torch.sigmoid(i) * torch.tanh(g)
    h2 = torch.sigmoid(o) * torch.tanh(c2)
    if w_hr is not None:
        h2 = F.linear(h2, w_hr)
    return h2, c2


def _rnn_step(gi, h, w_hh, b_hh, nonlinearity):
    pre = gi + F.linear(h, w_hh, b_hh)
    return torch.tanh(pre) if nonlinearity == "tanh" else torch.relu(pre)


class RNNCell(RNNCellBase):
    def __init__(self, input_size, hidden_size, bias=True, nonlinearity="tanh", device=None, dtype=None):
        super().__init__(input_size, hidden_size, bias, 1, device, dtype)
        self.nonlinearity = nonlinearity

    def forward(self, input, hx=None):
        batched = self._batch(input, "RNNCell")
        x = input if batched else input.unsqueeze(0)
        if hx is None:
            h = x.new_zeros(x.shape[0], self.hidden_size)
        else:
            h = hx if batched else hx.unsqueeze(0)
        if self.nonlinearity not in ("tanh", "relu"):
            raise RuntimeError("Unknown nonlinearity: %s" % self.nonlinearity)
        out = _rnn_step(F.linear(x, self.weight_ih, self.bias_ih), h, self.weight_hh, self.bias_hh, self.nonlinearity)
        return out if batched else out.squeeze(0)


class GRUCell(RNNCellBase):
    def __init__(self, input_size, hidden_size, bias=True, device=None, dtype=None):
        super().__init__(input_size, hidden_size, bias, 3, device, dtype)

    def forward(self, input, hx=None):
        x, h = input, hx
        batched = self._batch(x, "GRUCell")
        if not batched:
            x = x.unsqueeze(0)
            if h is not None:
                h = h.unsqueeze(0)
        if h is None:
            h = x.new_zeros(x.shape[0], self.hidden_size)
        out = _gru_step(F.linear(x, self.weight_ih, self.bias_ih), h, self.weight_hh, self.bias_hh)
        return out if batched else out.squeeze(0)


class LSTMCell(RNNCellBase):
    def __init__(self, input_size, hidden_size, bias=True, device=None, dtype=None):
        super().__init__(input_size, hidden_size, bias, 4, device, dtype)

    def forward(self, input, hx=None):
        x, state = input, hx
        batched = self._batch(x, "LSTMCell")
        if not batched:
            x = x.unsqueeze(0)
            if state is not None:
                state = (state[0].unsqueeze(0), state[1].unsqueeze(0))
        if state is None:
            h = x.new_zeros(x.shape[0], self.hidden_size)
            c = x.new_zeros(x.shape[0], self.hidden_size)
        else:
            h, c = state
        gates = F.linear(x, self.weight_ih, self.bias_ih) + F.linear(h, self.weight_hh, self.bias_hh)
        i, f, g, o = gates.chunk(4, 1)
        c2 = torch.sigmoid(f) * c + torch.sigmoid(i) * torch.tanh(g)
        h2 = torch.sigmoid(o) * torch.tanh(c2)
        if not batched:
            return h2.squeeze(0), c2.squeeze(0)
        return h2, c2


# ---- recurrent layers ------------------------------------------------------------------------
class RNNBase(Module):
    def __init__(self, mode, input_size, hidden_size, num_layers=1, bias=True, batch_first=False, dropout=0.0, bidirectional=False, proj_size=0, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.mode = mode
        self.input_size = input_size
        self.hidden_size = hidden_size
        self.num_layers = num_layers
        self.bias = bias
        self.batch_first = batch_first
        self.dropout = float(dropout)
        self.bidirectional = bidirectional
        self.proj_size = proj_size
        if isinstance(dropout, bool) or not isinstance(dropout, (int, float)) or not 0 <= dropout <= 1:
            raise ValueError("dropout should be a number in range [0, 1] representing the probability of an element being zeroed")
        if not isinstance(hidden_size, int):
            raise TypeError("hidden_size should be of type int, got: %s" % type(hidden_size).__name__)
        if hidden_size <= 0:
            raise ValueError("hidden_size must be greater than zero")
        if num_layers <= 0:
            raise ValueError("num_layers must be greater than zero")
        if proj_size < 0:
            raise ValueError("proj_size should be a positive integer or zero to disable projections")
        if proj_size >= hidden_size:
            raise ValueError("proj_size has to be smaller than hidden_size")
        if proj_size and mode != "LSTM":
            raise ValueError("proj_size argument is only supported for LSTM, not RNN or GRU")
        gate_size = {"LSTM": 4, "GRU": 3}.get(mode, 1) * hidden_size
        directions = 2 if bidirectional else 1
        real_hidden = proj_size if proj_size > 0 else hidden_size
        self._flat_weights_names = []
        self._all_weights = []
        for layer in range(num_layers):
            for direction in range(directions):
                layer_input = input_size if layer == 0 else real_hidden * directions
                suffix = "_reverse" if direction == 1 else ""
                names = ["weight_ih_l%d%s", "weight_hh_l%d%s"]
                shapes = [(gate_size, layer_input), (gate_size, real_hidden)]
                if bias:
                    names += ["bias_ih_l%d%s", "bias_hh_l%d%s"]
                    shapes += [(gate_size,), (gate_size,)]
                if proj_size > 0:
                    names += ["weight_hr_l%d%s"]
                    shapes += [(proj_size, hidden_size)]
                names = [n % (layer, suffix) for n in names]
                for name, shape in zip(names, shapes):
                    setattr(self, name, Parameter(torch.empty(*shape, dtype=dtype)))
                self._flat_weights_names.extend(names)
                self._all_weights.append(names)
        self.reset_parameters()

    def reset_parameters(self):
        stdv = 1.0 / math.sqrt(self.hidden_size) if self.hidden_size > 0 else 0
        for weight in self.parameters():
            init.uniform_(weight, -stdv, stdv)

    def flatten_parameters(self):
        pass

    @property
    def all_weights(self):
        return [[getattr(self, name) for name in names] for names in self._all_weights]

    @property
    def _flat_weights(self):
        return [getattr(self, name) for name in self._flat_weights_names]

    def extra_repr(self):
        s = "%s, %s" % (self.input_size, self.hidden_size)
        if self.proj_size != 0:
            s += ", proj_size=%s" % self.proj_size
        if self.num_layers != 1:
            s += ", num_layers=%s" % self.num_layers
        if self.bias is not True:
            s += ", bias=%s" % self.bias
        if self.batch_first is not False:
            s += ", batch_first=%s" % self.batch_first
        if self.dropout != 0:
            s += ", dropout=%s" % self.dropout
        if self.bidirectional is not False:
            s += ", bidirectional=%s" % self.bidirectional
        return s

    def _weights(self, layer, direction):
        sfx = "_reverse" if direction == 1 else ""
        get = lambda n: getattr(self, "%s_l%d%s" % (n, layer, sfx))
        b_ih = get("bias_ih") if self.bias else None
        b_hh = get("bias_hh") if self.bias else None
        w_hr = get("weight_hr") if self.proj_size > 0 else None
        return get("weight_ih"), get("weight_hh"), b_ih, b_hh, w_hr

    def _run(self, x, h0, c0, lengths):
        """x: (T, B, input) time-major; h0/c0: (layers * dirs, B, H) or None.
        With `lengths` (sorted, descending) rows stop updating past their end."""
        T, B = x.shape[0], x.shape[1]
        dirs = 2 if self.bidirectional else 1
        is_lstm = self.mode == "LSTM"
        masks = None
        if lengths is not None:
            lens = torch.tensor(lengths, dtype=torch.int64)
            masks = [(lens > t).unsqueeze(1) for t in range(T)]
        hs, cs = [], []
        layer_in = x
        for layer in range(self.num_layers):
            outs = []
            for direction in range(dirs):
                k = layer * dirs + direction
                w_ih, w_hh, b_ih, b_hh, w_hr = self._weights(layer, direction)
                gi_all = _unbind_time(F.linear(layer_in, w_ih, b_ih))
                h = h0[k]
                c = c0[k] if is_lstm else None
                steps = range(T) if direction == 0 else range(T - 1, -1, -1)
                seq = [None] * T
                for t in steps:
                    if is_lstm:
                        h2, c2 = _lstm_step(gi_all[t], h, c, w_hh, b_hh, w_hr)
                    elif self.mode == "GRU":
                        h2 = _gru_step(gi_all[t], h, w_hh, b_hh)
                    else:
                        h2 = _rnn_step(gi_all[t], h, w_hh, b_hh, "tanh" if self.mode == "RNN_TANH" else "relu")
                    if masks is not None:
                        m = masks[t]
                        h2 = torch.where(m, h2, h)
                        if is_lstm:
                            c2 = torch.where(m, c2, c)
                        seq[t] = torch.where(m, h2, torch.zeros_like(h2))
                    else:
                        seq[t] = h2
                    h = h2
                    if is_lstm:
                        c = c2
                outs.append(torch.stack(seq, 0))
                hs.append(h)
                if is_lstm:
                    cs.append(c)
            layer_in = outs[0] if dirs == 1 else torch.cat(outs, 2)
            if self.dropout > 0 and self.training and layer < self.num_layers - 1:
                layer_in = F.dropout(layer_in, self.dropout, True)
        h_n = torch.stack(hs, 0)
        c_n = torch.stack(cs, 0) if is_lstm else None
        return layer_in, h_n, c_n

    def forward(self, input, hx=None):
        is_lstm = self.mode == "LSTM"
        dirs = 2 if self.bidirectional else 1
        out_hidden = self.proj_size if self.proj_size > 0 else self.hidden_size
        packed = isinstance(input, PackedSequence)
        lengths = None
        sorted_indices = unsorted_indices = None
        if packed:
            x, lengths_t = _rnn_utils._pad_packed_sorted(input)
            lengths = [int(v) for v in lengths_t.tolist()]
            sorted_indices, unsorted_indices = input.sorted_indices, input.unsorted_indices
            batched = True
        else:
            if input.dim() not in (2, 3):
                raise ValueError("%s: Expected input to be 2D or 3D, got %dD instead" % (type(self).__name__, input.dim()))
            batched = input.dim() == 3
            x = input if batched else input.unsqueeze(1)
            if batched and self.batch_first:
                x = x.transpose(0, 1)
        if x.shape[-1] != self.input_size:
            raise RuntimeError("input.size(-1) must be equal to input_size. Expected %d, got %d" % (self.input_size, x.shape[-1]))
        B = x.shape[1]
        if hx is None:
            h0 = x.new_zeros(self.num_layers * dirs, B, out_hidden)
            c0 = x.new_zeros(self.num_layers * dirs, B, self.hidden_size) if is_lstm else None
        else:
            if is_lstm:
                h0, c0 = hx
            else:
                h0, c0 = hx, None
            if not batched:
                if h0.dim() != 2:
                    raise RuntimeError("For unbatched 2-D input, hx should also be 2-D but got %d-D tensor" % h0.dim())
                h0 = h0.unsqueeze(1)
                c0 = c0.unsqueeze(1) if c0 is not None else None
            elif h0.dim() != 3:
                raise RuntimeError("For batched 3-D input, hx should also be 3-D but got %d-D tensor" % h0.dim())
            expected = (self.num_layers * dirs, B, out_hidden)
            if tuple(h0.shape) != expected:
                raise RuntimeError("Expected hidden size %s, got %s" % (expected, list(h0.shape)))
            if c0 is not None and tuple(c0.shape) != (self.num_layers * dirs, B, self.hidden_size):
                raise RuntimeError("Expected hidden[1] size %s, got %s" % ((self.num_layers * dirs, B, self.hidden_size), list(c0.shape)))
            if sorted_indices is not None:
                h0 = h0.index_select(1, sorted_indices)
                c0 = c0.index_select(1, sorted_indices) if c0 is not None else None
        output, h_n, c_n = self._run(x, h0, c0, lengths)
        if packed:
            output = _rnn_utils._pack_sorted(output, input.batch_sizes, sorted_indices, unsorted_indices)
            if unsorted_indices is not None:
                h_n = h_n.index_select(1, unsorted_indices)
                c_n = c_n.index_select(1, unsorted_indices) if c_n is not None else None
        else:
            if batched and self.batch_first:
                output = output.transpose(0, 1)
            if not batched:
                output = output.squeeze(1)
                h_n = h_n.squeeze(1)
                c_n = c_n.squeeze(1) if c_n is not None else None
        return (output, (h_n, c_n)) if is_lstm else (output, h_n)


def _unbind_time(x):
    return F._unbind(x, 0)


class RNN(RNNBase):
    def __init__(self, *args, **kwargs):
        nonlinearity = kwargs.pop("nonlinearity", "tanh")
        if len(args) > 3:
            nonlinearity = args[3]
            args = args[:3] + args[4:]
        if nonlinearity == "tanh":
            mode = "RNN_TANH"
        elif nonlinearity == "relu":
            mode = "RNN_RELU"
        else:
            raise ValueError("Unknown nonlinearity '%s'. Select from 'tanh' or 'relu'." % nonlinearity)
        super().__init__(mode, *args, **kwargs)
        self.nonlinearity = nonlinearity


class LSTM(RNNBase):
    def __init__(self, *args, **kwargs):
        super().__init__("LSTM", *args, **kwargs)


class GRU(RNNBase):
    def __init__(self, *args, **kwargs):
        super().__init__("GRU", *args, **kwargs)


# ---- attention and transformers --------------------------------------------------------------
class MultiheadAttention(Module):
    def __init__(self, embed_dim, num_heads, dropout=0.0, bias=True, add_bias_kv=False, add_zero_attn=False, kdim=None, vdim=None, batch_first=False, device=None, dtype=None):
        torch._check_cpu_device(device)
        if embed_dim <= 0 or num_heads <= 0:
            raise ValueError("embed_dim and num_heads must be greater than 0, got embed_dim=%s and num_heads=%s instead" % (embed_dim, num_heads))
        super().__init__()
        self.embed_dim = embed_dim
        self.kdim = kdim if kdim is not None else embed_dim
        self.vdim = vdim if vdim is not None else embed_dim
        self._qkv_same_embed_dim = self.kdim == embed_dim and self.vdim == embed_dim
        self.num_heads = num_heads
        self.dropout = dropout
        self.batch_first = batch_first
        self.head_dim = embed_dim // num_heads
        if self.head_dim * num_heads != embed_dim:
            raise AssertionError("embed_dim must be divisible by num_heads")
        if not self._qkv_same_embed_dim:
            self.q_proj_weight = Parameter(torch.empty(embed_dim, embed_dim, dtype=dtype))
            self.k_proj_weight = Parameter(torch.empty(embed_dim, self.kdim, dtype=dtype))
            self.v_proj_weight = Parameter(torch.empty(embed_dim, self.vdim, dtype=dtype))
            self.register_parameter("in_proj_weight", None)
        else:
            self.in_proj_weight = Parameter(torch.empty(3 * embed_dim, embed_dim, dtype=dtype))
            self.register_parameter("q_proj_weight", None)
            self.register_parameter("k_proj_weight", None)
            self.register_parameter("v_proj_weight", None)
        if bias:
            self.in_proj_bias = Parameter(torch.empty(3 * embed_dim, dtype=dtype))
        else:
            self.register_parameter("in_proj_bias", None)
        self.out_proj = NonDynamicallyQuantizableLinear(embed_dim, embed_dim, bias=bias, dtype=dtype)
        if add_bias_kv:
            self.bias_k = Parameter(torch.empty(1, 1, embed_dim, dtype=dtype))
            self.bias_v = Parameter(torch.empty(1, 1, embed_dim, dtype=dtype))
        else:
            self.bias_k = self.bias_v = None
        self.add_zero_attn = add_zero_attn
        self._reset_parameters()

    def _reset_parameters(self):
        if self._qkv_same_embed_dim:
            init.xavier_uniform_(self.in_proj_weight)
        else:
            init.xavier_uniform_(self.q_proj_weight)
            init.xavier_uniform_(self.k_proj_weight)
            init.xavier_uniform_(self.v_proj_weight)
        if self.in_proj_bias is not None:
            init.constant_(self.in_proj_bias, 0.0)
            init.constant_(self.out_proj.bias, 0.0)
        if self.bias_k is not None:
            init.xavier_normal_(self.bias_k)
        if self.bias_v is not None:
            init.xavier_normal_(self.bias_v)

    def forward(self, query, key, value, key_padding_mask=None, need_weights=True, attn_mask=None, average_attn_weights=True, is_causal=False):
        is_batched = query.dim() == 3
        if self.batch_first and is_batched:
            query, key, value = query.transpose(1, 0), key.transpose(1, 0), value.transpose(1, 0)
        out, weights = F.multi_head_attention_forward(
            query, key, value, self.embed_dim, self.num_heads, self.in_proj_weight, self.in_proj_bias,
            self.bias_k, self.bias_v, self.add_zero_attn, self.dropout, self.out_proj.weight, self.out_proj.bias,
            training=self.training, key_padding_mask=key_padding_mask, need_weights=need_weights, attn_mask=attn_mask,
            use_separate_proj_weight=not self._qkv_same_embed_dim, q_proj_weight=self.q_proj_weight,
            k_proj_weight=self.k_proj_weight, v_proj_weight=self.v_proj_weight,
            average_attn_weights=average_attn_weights, is_causal=is_causal)
        if self.batch_first and is_batched:
            return out.transpose(1, 0), weights
        return out, weights


def _get_activation_fn(activation):
    if activation == "relu":
        return F.relu
    if activation == "gelu":
        return F.gelu
    raise RuntimeError("activation should be relu/gelu, not %s" % activation)


def _get_clones(module, n):
    return ModuleList([_copy.deepcopy(module) for _ in range(n)])


def _causal_mask(sz, dtype=torch.float32):
    return torch.triu(torch.full((sz, sz), float("-inf"), dtype=dtype), diagonal=1)


class TransformerEncoderLayer(Module):
    def __init__(self, d_model, nhead, dim_feedforward=2048, dropout=0.1, activation=F.relu, layer_norm_eps=1e-5, batch_first=False, norm_first=False, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.self_attn = MultiheadAttention(d_model, nhead, dropout=dropout, bias=bias, batch_first=batch_first, dtype=dtype)
        self.linear1 = Linear(d_model, dim_feedforward, bias=bias, dtype=dtype)
        self.dropout = Dropout(dropout)
        self.linear2 = Linear(dim_feedforward, d_model, bias=bias, dtype=dtype)
        self.norm_first = norm_first
        self.norm1 = LayerNorm(d_model, eps=layer_norm_eps, bias=bias, dtype=dtype)
        self.norm2 = LayerNorm(d_model, eps=layer_norm_eps, bias=bias, dtype=dtype)
        self.dropout1 = Dropout(dropout)
        self.dropout2 = Dropout(dropout)
        if isinstance(activation, str):
            activation = _get_activation_fn(activation)
        self.activation = activation
        if activation is F.relu or isinstance(activation, ReLU):
            self.activation_relu_or_gelu = 1
        elif activation is F.gelu or isinstance(activation, GELU):
            self.activation_relu_or_gelu = 2
        else:
            self.activation_relu_or_gelu = 0

    def forward(self, src, src_mask=None, src_key_padding_mask=None, is_causal=False):
        x = src
        if self.norm_first:
            x = x + self._sa_block(self.norm1(x), src_mask, src_key_padding_mask, is_causal)
            x = x + self._ff_block(self.norm2(x))
        else:
            x = self.norm1(x + self._sa_block(x, src_mask, src_key_padding_mask, is_causal))
            x = self.norm2(x + self._ff_block(x))
        return x

    def _sa_block(self, x, attn_mask, key_padding_mask, is_causal=False):
        x = self.self_attn(x, x, x, attn_mask=attn_mask, key_padding_mask=key_padding_mask, need_weights=False, is_causal=is_causal)[0]
        return self.dropout1(x)

    def _ff_block(self, x):
        x = self.linear2(self.dropout(self.activation(self.linear1(x))))
        return self.dropout2(x)


class TransformerDecoderLayer(Module):
    def __init__(self, d_model, nhead, dim_feedforward=2048, dropout=0.1, activation=F.relu, layer_norm_eps=1e-5, batch_first=False, norm_first=False, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.self_attn = MultiheadAttention(d_model, nhead, dropout=dropout, batch_first=batch_first, bias=bias, dtype=dtype)
        self.multihead_attn = MultiheadAttention(d_model, nhead, dropout=dropout, batch_first=batch_first, bias=bias, dtype=dtype)
        self.linear1 = Linear(d_model, dim_feedforward, bias=bias, dtype=dtype)
        self.dropout = Dropout(dropout)
        self.linear2 = Linear(dim_feedforward, d_model, bias=bias, dtype=dtype)
        self.norm_first = norm_first
        self.norm1 = LayerNorm(d_model, eps=layer_norm_eps, bias=bias, dtype=dtype)
        self.norm2 = LayerNorm(d_model, eps=layer_norm_eps, bias=bias, dtype=dtype)
        self.norm3 = LayerNorm(d_model, eps=layer_norm_eps, bias=bias, dtype=dtype)
        self.dropout1 = Dropout(dropout)
        self.dropout2 = Dropout(dropout)
        self.dropout3 = Dropout(dropout)
        if isinstance(activation, str):
            activation = _get_activation_fn(activation)
        self.activation = activation

    def forward(self, tgt, memory, tgt_mask=None, memory_mask=None, tgt_key_padding_mask=None, memory_key_padding_mask=None, tgt_is_causal=False, memory_is_causal=False):
        x = tgt
        if self.norm_first:
            x = x + self._sa_block(self.norm1(x), tgt_mask, tgt_key_padding_mask, tgt_is_causal)
            x = x + self._mha_block(self.norm2(x), memory, memory_mask, memory_key_padding_mask, memory_is_causal)
            x = x + self._ff_block(self.norm3(x))
        else:
            x = self.norm1(x + self._sa_block(x, tgt_mask, tgt_key_padding_mask, tgt_is_causal))
            x = self.norm2(x + self._mha_block(x, memory, memory_mask, memory_key_padding_mask, memory_is_causal))
            x = self.norm3(x + self._ff_block(x))
        return x

    def _sa_block(self, x, attn_mask, key_padding_mask, is_causal=False):
        x = self.self_attn(x, x, x, attn_mask=attn_mask, key_padding_mask=key_padding_mask, is_causal=is_causal, need_weights=False)[0]
        return self.dropout1(x)

    def _mha_block(self, x, mem, attn_mask, key_padding_mask, is_causal=False):
        x = self.multihead_attn(x, mem, mem, attn_mask=attn_mask, key_padding_mask=key_padding_mask, is_causal=is_causal, need_weights=False)[0]
        return self.dropout2(x)

    def _ff_block(self, x):
        x = self.linear2(self.dropout(self.activation(self.linear1(x))))
        return self.dropout3(x)


class TransformerEncoder(Module):
    def __init__(self, encoder_layer, num_layers, norm=None, enable_nested_tensor=True, mask_check=True):
        super().__init__()
        self.layers = _get_clones(encoder_layer, num_layers)
        self.num_layers = num_layers
        self.norm = norm
        self.enable_nested_tensor = enable_nested_tensor
        self.mask_check = mask_check
        # PyTorch's conditions for its nested-tensor fast path.
        layer = encoder_layer
        attn = getattr(layer, "self_attn", None)
        eligible = (isinstance(layer, TransformerEncoderLayer) and not layer.norm_first and attn.batch_first
                    and attn._qkv_same_embed_dim and attn.in_proj_bias is not None and layer.activation_relu_or_gelu
                    and layer.norm1.eps == layer.norm2.eps and attn.num_heads % 2 == 0)
        self.use_nested_tensor = bool(enable_nested_tensor and eligible)

    def _nested_padding(self, src, mask, key_padding_mask):
        """The padding mask when PyTorch would run this call as nested
        tensors (evaluation, batch_first, a left-aligned key padding mask, no
        attention mask, no gradient needed): its padded outputs are then
        zeros before the final norm. None otherwise."""
        if not self.__dict__.get("use_nested_tensor", False) or key_padding_mask is None or mask is not None:
            return None
        first = self.layers[0]
        if first.training or src.dim() != 3:
            return None
        pad = key_padding_mask if key_padding_mask.dtype == torch.bool else key_padding_mask != 0
        if self.__dict__.get("mask_check", True):
            for row in pad.tolist():
                seen = False
                for p in row:
                    if p:
                        seen = True
                    elif seen:
                        return None
        if torch.is_grad_enabled():
            attn = first.self_attn
            args = (src, attn.in_proj_weight, attn.in_proj_bias, attn.out_proj.weight, attn.out_proj.bias, first.norm1.weight, first.norm1.bias, first.norm2.weight, first.norm2.bias, first.linear1.weight, first.linear1.bias, first.linear2.weight, first.linear2.bias)
            if any(t is not None and t.requires_grad for t in args):
                return None
        return pad

    def forward(self, src, mask=None, src_key_padding_mask=None, is_causal=None):
        output = src
        pad = self._nested_padding(src, mask, src_key_padding_mask)
        for mod in self.layers:
            output = mod(output, src_mask=mask, is_causal=bool(is_causal), src_key_padding_mask=src_key_padding_mask)
        if pad is not None:
            output = output.masked_fill(pad.unsqueeze(-1), 0.0)
        if self.norm is not None:
            output = self.norm(output)
        return output


class TransformerDecoder(Module):
    def __init__(self, decoder_layer, num_layers, norm=None):
        super().__init__()
        self.layers = _get_clones(decoder_layer, num_layers)
        self.num_layers = num_layers
        self.norm = norm

    def forward(self, tgt, memory, tgt_mask=None, memory_mask=None, tgt_key_padding_mask=None, memory_key_padding_mask=None, tgt_is_causal=None, memory_is_causal=False):
        output = tgt
        for mod in self.layers:
            output = mod(output, memory, tgt_mask=tgt_mask, memory_mask=memory_mask, tgt_key_padding_mask=tgt_key_padding_mask, memory_key_padding_mask=memory_key_padding_mask, tgt_is_causal=bool(tgt_is_causal), memory_is_causal=memory_is_causal)
        if self.norm is not None:
            output = self.norm(output)
        return output


class Transformer(Module):
    def __init__(self, d_model=512, nhead=8, num_encoder_layers=6, num_decoder_layers=6, dim_feedforward=2048, dropout=0.1, activation=F.relu, custom_encoder=None, custom_decoder=None, layer_norm_eps=1e-5, batch_first=False, norm_first=False, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        if custom_encoder is not None:
            self.encoder = custom_encoder
        else:
            layer = TransformerEncoderLayer(d_model, nhead, dim_feedforward, dropout, activation, layer_norm_eps, batch_first, norm_first, bias, dtype=dtype)
            self.encoder = TransformerEncoder(layer, num_encoder_layers, LayerNorm(d_model, eps=layer_norm_eps, bias=bias, dtype=dtype))
        if custom_decoder is not None:
            self.decoder = custom_decoder
        else:
            layer = TransformerDecoderLayer(d_model, nhead, dim_feedforward, dropout, activation, layer_norm_eps, batch_first, norm_first, bias, dtype=dtype)
            self.decoder = TransformerDecoder(layer, num_decoder_layers, LayerNorm(d_model, eps=layer_norm_eps, bias=bias, dtype=dtype))
        self._reset_parameters()
        self.d_model = d_model
        self.nhead = nhead
        self.batch_first = batch_first

    def forward(self, src, tgt, src_mask=None, tgt_mask=None, memory_mask=None, src_key_padding_mask=None, tgt_key_padding_mask=None, memory_key_padding_mask=None, src_is_causal=None, tgt_is_causal=None, memory_is_causal=False):
        memory = self.encoder(src, mask=src_mask, src_key_padding_mask=src_key_padding_mask, is_causal=src_is_causal)
        return self.decoder(tgt, memory, tgt_mask=tgt_mask, memory_mask=memory_mask, tgt_key_padding_mask=tgt_key_padding_mask, memory_key_padding_mask=memory_key_padding_mask, tgt_is_causal=tgt_is_causal, memory_is_causal=memory_is_causal)

    @staticmethod
    def generate_square_subsequent_mask(sz, device=None, dtype=None):
        return _causal_mask(sz, torch.float32 if dtype is None else dtype)

    def _reset_parameters(self):
        for p in self.parameters():
            if p.dim() > 1:
                init.xavier_uniform_(p)



# ---- lazy modules ----------------------------------------------------------------------------
_is_lazy = parameter.is_lazy


class LazyModuleMixin:
    """Parameters (and buffers) start as UninitializedParameter; the first
    forward infers their shapes from the input, materializes them in place
    (the same objects, so an optimizer built earlier keeps working), resets
    them and turns the module into `cls_to_become`. Loading a state_dict
    into an uninitialized module materializes from the checkpoint's shapes."""
    cls_to_become = None

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._load_hook = self._register_load_state_dict_pre_hook(self._lazy_load_hook)
        self._initialize_hook = self.register_forward_pre_hook(self._infer_parameters, with_kwargs=True)

    def _save_to_state_dict(self, destination, prefix, keep_vars):
        for name, param in self._parameters.items():
            if param is not None:
                if not (_is_lazy(param) or keep_vars):
                    param = param.detach()
                destination[prefix + name] = param
        for name, buf in self._buffers.items():
            if buf is not None and name not in self._non_persistent_buffers_set:
                if not (_is_lazy(buf) or keep_vars):
                    buf = buf.detach()
                destination[prefix + name] = buf

    def _lazy_load_hook(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        for name, param in list(self._parameters.items()) + list(self._buffers.items()):
            key = prefix + name
            if key in state_dict and param is not None:
                input_param = state_dict[key]
                if _is_lazy(param) and not _is_lazy(input_param):
                    with torch.no_grad():
                        param.materialize(input_param.shape)

    def initialize_parameters(self, *args, **kwargs):
        raise NotImplementedError("initialize_parameters is not implemented for %s" % self.__class__.__name__)

    def has_uninitialized_params(self):
        for param in list(self._parameters.values()) + list(self._buffers.values()):
            if _is_lazy(param):
                return True
        return False

    def _infer_parameters(self, module, args, kwargs=None):
        kwargs = kwargs if kwargs else {}
        module.initialize_parameters(*args, **kwargs)
        if module.has_uninitialized_params():
            raise RuntimeError("module %s has not been fully initialized" % self._get_name())
        module._initialize_hook.remove()
        module._load_hook.remove()
        delattr(module, "_initialize_hook")
        delattr(module, "_load_hook")
        if module.cls_to_become is not None:
            module.__class__ = module.cls_to_become

    def _replicate_for_data_parallel(self):
        raise RuntimeError("Modules with uninitialized parameters can't be used with `DataParallel`. Run a dummy forward pass to correctly initialize the modules")


class LazyLinear(LazyModuleMixin, Linear):
    cls_to_become = Linear

    def __init__(self, out_features, bias=True, device=None, dtype=None):
        super().__init__(0, 0, False)
        self.weight = UninitializedParameter(device=device, dtype=dtype)
        self.out_features = out_features
        if bias:
            self.bias = UninitializedParameter(device=device, dtype=dtype)

    def reset_parameters(self):
        if not self.has_uninitialized_params() and self.in_features != 0:
            super().reset_parameters()

    def initialize_parameters(self, input):
        if self.has_uninitialized_params():
            with torch.no_grad():
                self.in_features = input.shape[-1]
                self.weight.materialize((self.out_features, self.in_features))
                if self.bias is not None:
                    self.bias.materialize((self.out_features,))
                self.reset_parameters()
        if self.in_features == 0:
            if input.shape[-1] != self.weight.shape[-1]:
                raise AssertionError("The in_features inferred from input: %d is not equal to in_features from self.weight: %d" % (input.shape[-1], self.weight.shape[-1]))
            self.in_features = input.shape[-1]


class _LazyConvXdMixin(LazyModuleMixin):
    _spatial = 2

    def _lazy_setup(self, out_channels, bias, device, dtype):
        self.weight = UninitializedParameter(device=device, dtype=dtype)
        self.out_channels = out_channels
        if bias:
            self.bias = UninitializedParameter(device=device, dtype=dtype)

    def reset_parameters(self):
        if not self.has_uninitialized_params() and self.in_channels != 0:
            super().reset_parameters()

    def initialize_parameters(self, input, *args, **kwargs):
        if self.has_uninitialized_params():
            self.in_channels = self._get_in_channels(input)
            if self.in_channels % self.groups != 0:
                raise ValueError("in_channels must be divisible by groups")
            if self.transposed:
                self.weight.materialize((self.in_channels, self.out_channels // self.groups) + tuple(self.kernel_size))
            else:
                self.weight.materialize((self.out_channels, self.in_channels // self.groups) + tuple(self.kernel_size))
            if self.bias is not None:
                self.bias.materialize((self.out_channels,))
            self.reset_parameters()

    def _get_in_channels(self, input):
        no_batch = self._spatial + 1
        if input.dim() not in (no_batch, no_batch + 1):
            raise RuntimeError("Expected %dD (unbatched) or %dD (batched) input to %s, but got input of size: %s" % (no_batch, no_batch + 1, self.__class__.__name__, input.shape))
        return input.shape[1] if input.dim() == no_batch + 1 else input.shape[0]

    def _get_num_spatial_dims(self):
        return self._spatial


class LazyConv1d(_LazyConvXdMixin, Conv1d):
    cls_to_become = Conv1d
    _spatial = 1

    def __init__(self, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        super().__init__(0, 0, kernel_size, stride, padding, dilation, groups, False, padding_mode, device, dtype)
        self._lazy_setup(out_channels, bias, device, dtype)


class LazyConv2d(_LazyConvXdMixin, Conv2d):
    cls_to_become = Conv2d
    _spatial = 2

    def __init__(self, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        super().__init__(0, 0, kernel_size, stride, padding, dilation, groups, False, padding_mode, device, dtype)
        self._lazy_setup(out_channels, bias, device, dtype)


class LazyConv3d(_LazyConvXdMixin, Conv3d):
    cls_to_become = Conv3d
    _spatial = 3

    def __init__(self, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        super().__init__(0, 0, kernel_size, stride, padding, dilation, groups, False, padding_mode, device, dtype)
        self._lazy_setup(out_channels, bias, device, dtype)


class LazyConvTranspose1d(_LazyConvXdMixin, ConvTranspose1d):
    cls_to_become = ConvTranspose1d
    _spatial = 1

    def __init__(self, out_channels, kernel_size, stride=1, padding=0, output_padding=0, groups=1, bias=True, dilation=1, padding_mode="zeros", device=None, dtype=None):
        super().__init__(0, 0, kernel_size, stride, padding, output_padding, groups, False, dilation, padding_mode, device, dtype)
        self._lazy_setup(out_channels, bias, device, dtype)


class LazyConvTranspose2d(_LazyConvXdMixin, ConvTranspose2d):
    cls_to_become = ConvTranspose2d
    _spatial = 2

    def __init__(self, out_channels, kernel_size, stride=1, padding=0, output_padding=0, groups=1, bias=True, dilation=1, padding_mode="zeros", device=None, dtype=None):
        super().__init__(0, 0, kernel_size, stride, padding, output_padding, groups, False, dilation, padding_mode, device, dtype)
        self._lazy_setup(out_channels, bias, device, dtype)


class LazyConvTranspose3d(_LazyConvXdMixin, ConvTranspose3d):
    cls_to_become = ConvTranspose3d
    _spatial = 3

    def __init__(self, out_channels, kernel_size, stride=1, padding=0, output_padding=0, groups=1, bias=True, dilation=1, padding_mode="zeros", device=None, dtype=None):
        super().__init__(0, 0, kernel_size, stride, padding, output_padding, groups, False, dilation, padding_mode, device, dtype)
        self._lazy_setup(out_channels, bias, device, dtype)


class _LazyNormBase(LazyModuleMixin, _NormBase):
    def __init__(self, eps=1e-5, momentum=0.1, affine=True, track_running_stats=True, device=None, dtype=None):
        super().__init__(0, eps, momentum, False, False, device=device, dtype=dtype)
        self.affine = affine
        self.track_running_stats = track_running_stats
        if self.affine:
            self.weight = UninitializedParameter(device=device, dtype=dtype)
            self.bias = UninitializedParameter(device=device, dtype=dtype)
        if self.track_running_stats:
            self.running_mean = UninitializedBuffer(device=device, dtype=dtype)
            self.running_var = UninitializedBuffer(device=device, dtype=dtype)
            self.num_batches_tracked = torch.tensor(0, dtype=torch.int64)

    def reset_parameters(self):
        if not self.has_uninitialized_params() and self.num_features != 0:
            super().reset_parameters()

    def initialize_parameters(self, input):
        if self.has_uninitialized_params():
            self.num_features = input.shape[1]
            if self.affine:
                self.weight.materialize((self.num_features,))
                self.bias.materialize((self.num_features,))
            if self.track_running_stats:
                self.running_mean.materialize((self.num_features,))
                self.running_var.materialize((self.num_features,))
            self.reset_parameters()


class LazyBatchNorm1d(_LazyNormBase, _BatchNorm):
    cls_to_become = BatchNorm1d
    _check_input_dim = BatchNorm1d._check_input_dim


class LazyBatchNorm2d(_LazyNormBase, _BatchNorm):
    cls_to_become = BatchNorm2d
    _check_input_dim = BatchNorm2d._check_input_dim


class LazyBatchNorm3d(_LazyNormBase, _BatchNorm):
    cls_to_become = BatchNorm3d
    _check_input_dim = BatchNorm3d._check_input_dim


class LazyInstanceNorm1d(_LazyNormBase, _InstanceNorm):
    cls_to_become = InstanceNorm1d
    _batched_rank = 3


class LazyInstanceNorm2d(_LazyNormBase, _InstanceNorm):
    cls_to_become = InstanceNorm2d
    _batched_rank = 4


class LazyInstanceNorm3d(_LazyNormBase, _InstanceNorm):
    cls_to_become = InstanceNorm3d
    _batched_rank = 5


# ---- fractional max pooling, CTC and SyncBatchNorm -----------------------------------------
def _float_tuple(value, n):
    if isinstance(value, (tuple, list)):
        return tuple(value)
    return (value,) * n


class _FractionalMaxPoolNd(Module):
    _nd = 2

    def __init__(self, kernel_size, output_size=None, output_ratio=None, return_indices=False, _random_samples=None):
        super().__init__()
        nd = self._nd
        name = type(self).__name__
        if nd == 3 and ((isinstance(kernel_size, int) and kernel_size <= 0) or (isinstance(kernel_size, (tuple, list)) and not all(k > 0 for k in kernel_size))):
            raise ValueError("kernel_size must greater than 0, but got %s" % (kernel_size,))
        self.kernel_size = F._ntuple(kernel_size, nd, "kernel_size")
        self.return_indices = return_indices
        self.register_buffer("_random_samples", _random_samples)
        self.output_size = F._ntuple(output_size, nd, "output_size") if output_size is not None else None
        self.output_ratio = _float_tuple(output_ratio, nd) if output_ratio is not None else None
        if output_size is None and output_ratio is None:
            raise ValueError("%s requires specifying either an output size, or a pooling ratio" % name)
        if output_size is not None and output_ratio is not None:
            raise ValueError("only one of output_size and output_ratio may be specified")
        if self.output_ratio is not None:
            if not all(0 < r < 1 for r in self.output_ratio[:nd]):
                raise ValueError("output_ratio must be between 0 and 1 (got %s)" % (output_ratio,))


class FractionalMaxPool2d(_FractionalMaxPoolNd):
    _nd = 2

    def forward(self, input):
        return F.fractional_max_pool2d(input, self.kernel_size, self.output_size, self.output_ratio, self.return_indices, _random_samples=self._random_samples)


class FractionalMaxPool3d(_FractionalMaxPoolNd):
    _nd = 3

    def forward(self, input):
        return F.fractional_max_pool3d(input, self.kernel_size, self.output_size, self.output_ratio, self.return_indices, _random_samples=self._random_samples)


class CTCLoss(_Loss):
    def __init__(self, blank=0, reduction="mean", zero_infinity=False):
        super().__init__(reduction=reduction)
        self.blank = blank
        self.zero_infinity = zero_infinity

    def forward(self, log_probs, targets, input_lengths, target_lengths):
        return F.ctc_loss(log_probs, targets, input_lengths, target_lengths, self.blank, self.reduction, self.zero_infinity)


class SyncBatchNorm(_BatchNorm):
    """BatchNorm whose statistics PyTorch all-reduces across the processes
    of a group. Zipp runs one process and has no torch.distributed, so
    (as PyTorch does without an initialized process group) it normalizes
    with the local batch statistics, exactly like BatchNorm."""

    def __init__(self, num_features, eps=1e-5, momentum=0.1, affine=True, track_running_stats=True, process_group=None, device=None, dtype=None):
        super().__init__(num_features, eps, momentum, affine, track_running_stats, device, dtype)
        self.process_group = process_group

    def _check_input_dim(self, input):
        if input.dim() < 2:
            raise ValueError("expected at least 2D input (got %dD input)" % input.dim())

    def _check_non_zero_input_channels(self, input):
        if input.size(1) == 0:
            raise ValueError("SyncBatchNorm number of input channels should be non-zero")

    def forward(self, input):
        self._check_input_dim(input)
        self._check_non_zero_input_channels(input)
        return _BatchNorm.forward(self, input)

    @classmethod
    def convert_sync_batchnorm(cls, module, process_group=None):
        module_output = module
        if isinstance(module, _BatchNorm):
            module_output = SyncBatchNorm(module.num_features, module.eps, module.momentum, module.affine, module.track_running_stats, process_group)
            if module.affine:
                with torch.no_grad():
                    module_output.weight = module.weight
                    module_output.bias = module.bias
            module_output.running_mean = module.running_mean
            module_output.running_var = module.running_var
            module_output.num_batches_tracked = module.num_batches_tracked
            module_output.training = module.training
            if hasattr(module, "qconfig"):
                module_output.qconfig = module.qconfig
        for name, child in module.named_children():
            module_output.add_module(name, cls.convert_sync_batchnorm(child, process_group))
        return module_output


functional = F

# torch.nn.utils.parametrize/parametrizations subclass ModuleList and Module,
# so they load once those exist (PyTorch exposes them as attributes of
# torch.nn.utils after `import torch`).
import torch.nn.utils.parametrize
import torch.nn.utils.parametrizations

# torch.nn.parallel subclasses Module too; PyTorch exposes its DataParallel
# as nn.DataParallel.
import torch.nn.parallel as parallel
from torch.nn.parallel import DataParallel
