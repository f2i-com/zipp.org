"""torch.nn.utils for Zipp."""
import math
import torch
import torch.nn.utils.rnn as rnn


def _grads(parameters):
    if isinstance(parameters, torch.Tensor):
        parameters = [parameters]
    return [p.grad for p in parameters if p.grad is not None]


def get_total_norm(tensors, norm_type=2.0, error_if_nonfinite=False, foreach=None):
    if isinstance(tensors, torch.Tensor):
        tensors = [tensors]
    tensors = list(tensors)
    if not tensors:
        return torch.tensor(0.0)
    norm_type = float(norm_type)
    if norm_type == math.inf:
        total = max(torch.abs(t).max().item() if t.numel() else 0.0 for t in tensors)
    elif norm_type == 2.0:
        total = math.sqrt(sum(float((t.double() * t.double()).sum().item()) for t in tensors))
    else:
        total = sum(float((torch.abs(t.double()) ** norm_type).sum().item()) for t in tensors) ** (1.0 / norm_type)
    if error_if_nonfinite and (math.isnan(total) or math.isinf(total)):
        raise RuntimeError("The total norm of order %s for gradients from `parameters` is non-finite, so it cannot be clipped. To disable this error and scale the gradients by the non-finite norm anyway, set `error_if_nonfinite=False`" % norm_type)
    return torch.tensor(total)


def clip_grads_with_norm_(parameters, max_norm, total_norm, foreach=None):
    grads = _grads(parameters)
    coef = float(max_norm) / (float(total_norm) + 1e-6)
    if coef < 1.0:
        with torch.no_grad():
            for g in grads:
                g.copy_(g * coef)


def clip_grad_norm_(parameters, max_norm, norm_type=2.0, error_if_nonfinite=False, foreach=None):
    if isinstance(parameters, torch.Tensor):
        parameters = [parameters]
    parameters = list(parameters)
    if error_if_nonfinite:
        get_total_norm(_grads(parameters), norm_type, True)
    return torch.clip_grad_norm_(parameters, max_norm, norm_type)


def clip_grad_value_(parameters, clip_value, foreach=None):
    if isinstance(parameters, torch.Tensor):
        parameters = [parameters]
    for p in parameters:
        if p.grad is not None:
            p.grad.clamp_(-clip_value, clip_value)


def parameters_to_vector(parameters):
    return torch.cat([p.reshape(-1) for p in parameters])


def vector_to_parameters(vec, parameters):
    start = 0
    for p in parameters:
        n = p.numel()
        with torch.no_grad():
            p.copy_(vec[start:start + n].reshape(*p.shape))
        start += n


def _norm_except_dim(v, dim):
    if dim is None or dim == -1:
        return torch.sqrt((v * v).sum())
    dims = [d for d in range(len(v.shape)) if d != dim % len(v.shape)]
    return torch.sqrt((v * v).sum(dims, keepdim=True))


class _WeightNorm:
    def __init__(self, name, dim):
        self.name = name
        self.dim = dim

    def compute_weight(self, module):
        g = getattr(module, self.name + "_g")
        v = getattr(module, self.name + "_v")
        return v * (g / _norm_except_dim(v, self.dim))

    def __call__(self, module, inputs):
        setattr(module, self.name, self.compute_weight(module))


def weight_norm(module, name="weight", dim=0):
    """The legacy `torch.nn.utils.weight_norm`: `name` becomes g * v / ||v||
    recomputed before every forward from the parameters `name_g` and `name_v`."""
    from torch.nn import Parameter
    fn = _WeightNorm(name, dim)
    weight = getattr(module, name)
    del module._parameters[name]
    module.register_parameter(name + "_g", Parameter(_norm_except_dim(weight, dim).detach()))
    module.register_parameter(name + "_v", Parameter(weight.detach()))
    setattr(module, name, fn.compute_weight(module))
    handle = module.register_forward_pre_hook(fn)
    module.__dict__.setdefault("_weight_norm_hooks", {})[name] = (fn, handle)
    return module


def remove_weight_norm(module, name="weight"):
    from torch.nn import Parameter
    hooks = module.__dict__.get("_weight_norm_hooks", {})
    if name not in hooks:
        raise ValueError("weight_norm of '%s' not found in %s" % (name, module))
    fn, handle = hooks.pop(name)
    weight = fn.compute_weight(module).detach()
    handle.remove()
    delattr(module, name + "_g")
    delattr(module, name + "_v")
    if name in module.__dict__:
        del module.__dict__[name]
    module.register_parameter(name, Parameter(weight))
    return module


class SpectralNorm:
    """The legacy `torch.nn.utils.spectral_norm` forward pre-hook: `name`
    becomes weight_orig / sigma, sigma estimated by power iteration on the
    buffers `name_u`/`name_v` (updated in place in training mode)."""
    _version = 1

    def __init__(self, name="weight", n_power_iterations=1, dim=0, eps=1e-12):
        self.name = name
        self.dim = dim
        if n_power_iterations <= 0:
            raise ValueError("Expected n_power_iterations to be positive, but got n_power_iterations=%s" % n_power_iterations)
        self.n_power_iterations = n_power_iterations
        self.eps = eps

    def reshape_weight_to_matrix(self, weight):
        weight_mat = weight
        if self.dim != 0:
            weight_mat = weight_mat.permute(self.dim, *[d for d in range(weight_mat.dim()) if d != self.dim])
        height = weight_mat.size(0)
        return weight_mat.reshape(height, -1)

    def compute_weight(self, module, do_power_iteration):
        import torch.nn.functional as F
        weight = getattr(module, self.name + "_orig")
        u = getattr(module, self.name + "_u")
        v = getattr(module, self.name + "_v")
        weight_mat = self.reshape_weight_to_matrix(weight)
        if do_power_iteration:
            with torch.no_grad():
                for _ in range(self.n_power_iterations):
                    v = F.normalize(torch.mv(weight_mat.t(), u), dim=0, eps=self.eps, out=v)
                    u = F.normalize(torch.mv(weight_mat, v), dim=0, eps=self.eps, out=u)
                if self.n_power_iterations > 0:
                    u = u.clone()
                    v = v.clone()
        sigma = torch.dot(u, torch.mv(weight_mat, v))
        return weight / sigma

    def remove(self, module):
        from torch.nn import Parameter
        with torch.no_grad():
            weight = self.compute_weight(module, do_power_iteration=False)
        delattr(module, self.name)
        delattr(module, self.name + "_u")
        delattr(module, self.name + "_v")
        delattr(module, self.name + "_orig")
        module.register_parameter(self.name, Parameter(weight.detach()))

    def __call__(self, module, inputs):
        setattr(module, self.name, self.compute_weight(module, do_power_iteration=module.training))

    def _solve_v_and_rescale(self, weight_mat, u, target_sigma):
        # PyTorch: v = pinv(W^T W) W^T u, rescaled so that u^T W v = sigma.
        v = _min_norm_solve(weight_mat.double().t() @ weight_mat.double(), torch.mv(weight_mat.double().t(), u.double())).to(weight_mat.dtype)
        return v * (target_sigma / torch.dot(u, torch.mv(weight_mat, v)))

    @staticmethod
    def apply(module, name, n_power_iterations, dim, eps):
        import torch.nn.functional as F
        from torch.nn.parameter import UninitializedParameter
        for hook in module._forward_pre_hooks.values():
            if isinstance(hook, SpectralNorm) and hook.name == name:
                raise RuntimeError("Cannot register two spectral_norm hooks on the same parameter %s" % name)
        fn = SpectralNorm(name, n_power_iterations, dim, eps)
        weight = module._parameters[name]
        if weight is None:
            raise ValueError("`SpectralNorm` cannot be applied as parameter `%s` is None" % name)
        if isinstance(weight, UninitializedParameter):
            raise ValueError("The module passed to `SpectralNorm` can't have uninitialized parameters. Make sure to run the dummy forward before applying spectral normalization")
        with torch.no_grad():
            weight_mat = fn.reshape_weight_to_matrix(weight)
            h, w = weight_mat.shape
            u = F.normalize(torch.empty(h, dtype=weight.dtype).normal_(0, 1), dim=0, eps=fn.eps)
            v = F.normalize(torch.empty(w, dtype=weight.dtype).normal_(0, 1), dim=0, eps=fn.eps)
        delattr(module, fn.name)
        module.register_parameter(fn.name + "_orig", weight)
        setattr(module, fn.name, weight.data)
        module.register_buffer(fn.name + "_u", u)
        module.register_buffer(fn.name + "_v", v)
        module.register_forward_pre_hook(fn)
        module._register_state_dict_hook(SpectralNormStateDictHook(fn))
        module._register_load_state_dict_pre_hook(SpectralNormLoadStateDictPreHook(fn))
        return fn


def _min_norm_solve(m, rhs):
    """pinv(m) @ rhs for a symmetric positive semi-definite float64 matrix,
    by Gauss-Jordan elimination that drops (numerically) dependent columns."""
    a = m.tolist()
    b = rhs.tolist()
    n = len(a)
    tol = 1e-10 * max([abs(a[i][i]) for i in range(n)] + [1e-300])
    pivots = []
    row = 0
    for col in range(n):
        if row >= n:
            break
        best = row
        for r in range(row + 1, n):
            if abs(a[r][col]) > abs(a[best][col]):
                best = r
        if abs(a[best][col]) <= tol:
            continue
        a[row], a[best] = a[best], a[row]
        b[row], b[best] = b[best], b[row]
        piv = a[row][col]
        for r in range(n):
            if r != row and a[r][col] != 0.0:
                f = a[r][col] / piv
                for c in range(col, n):
                    a[r][c] -= f * a[row][c]
                b[r] -= f * b[row]
        pivots.append((row, col))
        row += 1
    x = [0.0] * n
    for r, c in pivots:
        x[c] = b[r] / a[r][c]
    return torch.tensor(x, dtype=torch.float64)


class SpectralNormLoadStateDictPreHook:
    def __init__(self, fn):
        self.fn = fn

    def __call__(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        fn = self.fn
        version = local_metadata.get("spectral_norm", {}).get(fn.name + ".version", None)
        if version is None or version < 1:
            weight_key = prefix + fn.name
            if version is None and all(weight_key + s in state_dict for s in ("_orig", "_u", "_v")) and weight_key not in state_dict:
                return
            has_missing_keys = False
            for suffix in ("_orig", "", "_u"):
                key = weight_key + suffix
                if key not in state_dict:
                    has_missing_keys = True
                    if strict:
                        missing_keys.append(key)
            if has_missing_keys:
                return
            with torch.no_grad():
                weight_orig = state_dict[weight_key + "_orig"]
                weight = state_dict.pop(weight_key)
                sigma = (weight_orig / weight).mean()
                weight_mat = fn.reshape_weight_to_matrix(weight_orig)
                u = state_dict[weight_key + "_u"]
                v = fn._solve_v_and_rescale(weight_mat, u, sigma)
                state_dict[weight_key + "_v"] = v


class SpectralNormStateDictHook:
    def __init__(self, fn):
        self.fn = fn

    def __call__(self, module, state_dict, prefix, local_metadata):
        if "spectral_norm" not in local_metadata:
            local_metadata["spectral_norm"] = {}
        key = self.fn.name + ".version"
        if key in local_metadata["spectral_norm"]:
            raise RuntimeError("Unexpected key in metadata['spectral_norm']: %s" % key)
        local_metadata["spectral_norm"][key] = self.fn._version


def spectral_norm(module, name="weight", n_power_iterations=1, eps=1e-12, dim=None):
    import torch.nn as nn
    if dim is None:
        dim = 1 if isinstance(module, (nn.ConvTranspose1d, nn.ConvTranspose2d, nn.ConvTranspose3d)) else 0
    SpectralNorm.apply(module, name, n_power_iterations, dim, eps)
    return module


def remove_spectral_norm(module, name="weight"):
    for k, hook in list(module._forward_pre_hooks.items()):
        if isinstance(hook, SpectralNorm) and hook.name == name:
            hook.remove(module)
            del module._forward_pre_hooks[k]
            break
    else:
        raise ValueError("spectral_norm of '%s' not found in %s" % (name, module))
    for k, hook in list(module._state_dict_hooks.items()):
        if isinstance(hook, SpectralNormStateDictHook) and hook.fn.name == name:
            del module._state_dict_hooks[k]
            break
    # PyTorch tests the stored hooks, which are wrapped, so its load pre-hook
    # stays registered (a harmless no-op for current checkpoints); match it.
    for k, hook in list(module._load_state_dict_pre_hooks.items()):
        if isinstance(hook, SpectralNormLoadStateDictPreHook) and hook.fn.name == name:
            del module._load_state_dict_pre_hooks[k]
            break
    return module


def fuse_conv_bn_weights(conv_w, conv_b, bn_rm, bn_rv, bn_eps, bn_w, bn_b, transpose=False):
    from torch.nn import Parameter
    conv_weight_dtype = conv_w.dtype
    conv_bias_dtype = conv_b.dtype if conv_b is not None else conv_weight_dtype
    if conv_b is None:
        conv_b = torch.zeros_like(bn_rm)
    if bn_w is None:
        bn_w = torch.ones_like(bn_rm)
    if bn_b is None:
        bn_b = torch.zeros_like(bn_rm)
    bn_var_rsqrt = torch.rsqrt(bn_rv + bn_eps)
    shape = [1] * len(conv_w.shape)
    shape[1 if transpose else 0] = -1
    fused_conv_w = (conv_w * (bn_w * bn_var_rsqrt).reshape(*shape)).to(conv_weight_dtype)
    fused_conv_b = ((conv_b - bn_rm) * bn_var_rsqrt * bn_w + bn_b).to(conv_bias_dtype)
    return Parameter(fused_conv_w.detach(), conv_w.requires_grad), Parameter(fused_conv_b.detach(), conv_b.requires_grad)


def fuse_conv_bn_eval(conv, bn, transpose=False):
    import copy
    if conv.training or bn.training:
        raise AssertionError("Fusion only for eval!")
    fused_conv = copy.deepcopy(conv)
    if bn.running_mean is None or bn.running_var is None:
        raise AssertionError("bn.running_mean and bn.running_var must not be None")
    fused_conv.weight, fused_conv.bias = fuse_conv_bn_weights(fused_conv.weight, fused_conv.bias, bn.running_mean, bn.running_var, bn.eps, bn.weight, bn.bias, transpose)
    return fused_conv


def fuse_linear_bn_weights(linear_w, linear_b, bn_rm, bn_rv, bn_eps, bn_w, bn_b):
    from torch.nn import Parameter
    if linear_b is None:
        linear_b = torch.zeros_like(bn_rm)
    bn_scale = bn_w * torch.rsqrt(bn_rv + bn_eps)
    fused_w = linear_w * bn_scale.unsqueeze(-1)
    fused_b = (linear_b - bn_rm) * bn_scale + bn_b
    return Parameter(fused_w.detach(), linear_w.requires_grad), Parameter(fused_b.detach(), linear_b.requires_grad)


def fuse_linear_bn_eval(linear, bn):
    import copy
    if linear.training or bn.training:
        raise AssertionError("Fusion only for eval!")
    fused_linear = copy.deepcopy(linear)
    if bn.running_mean is None or bn.running_var is None:
        raise AssertionError("bn.running_mean and bn.running_var must not be None")
    fused_linear.weight, fused_linear.bias = fuse_linear_bn_weights(fused_linear.weight, fused_linear.bias, bn.running_mean, bn.running_var, bn.eps, bn.weight, bn.bias)
    return fused_linear
