"""torch._utils: the checkpoint reconstruction hooks pickles refer to, and
the globals PyTorch's weights-only loader resolves."""
import torch
from collections import OrderedDict

_rebuild_tensor_v2 = torch._rebuild_tensor_v2


def _rebuild_sparse_tensor(layout, data):
    """A checkpoint's sparse tensor: (indices, values, size, is_coalesced)
    for COO (PyTorch before 2.1 wrote no is_coalesced), (compressed
    indices, plain indices, values, size) for CSR."""
    return torch.sparse._rebuild(layout, data)


def _rebuild_qtensor(storage, storage_offset, size, stride, quantizer_params, requires_grad=False, backward_hooks=None):
    """A checkpoint's quantized tensor: its integers in a QUInt8/QInt8/QInt32
    storage and (qscheme, scale, zero_point) or (qscheme, scales,
    zero_points, axis)."""
    return torch._quant._rebuild_qtensor(storage, storage_offset, size, stride, quantizer_params, requires_grad, backward_hooks)


def _rebuild_parameter(data, requires_grad, backward_hooks):
    # How PyTorch pickles an nn.Parameter: its data, then requires_grad.
    return torch.nn.Parameter(data, requires_grad)


def _rebuild_parameter_with_state(data, requires_grad, backward_hooks, state):
    param = _rebuild_parameter(data, requires_grad, backward_hooks)
    if state:
        for key, value in (state[0] if isinstance(state, tuple) else state).items():
            setattr(param, key, value)
    return param


def _weights_only_find_class(module, name):
    """The globals `torch.load(weights_only=True)` accepts, by PyTorch's
    allowlist: tensor/parameter rebuild hooks, storages, dtypes,
    torch.Size, torch.device and the plain-data builtins (sets, bytes via
    `_codecs.encode`, bytearray, complex, OrderedDict, Counter)."""
    import pickle
    from collections import Counter
    module = {"__builtin__": "builtins", "copy_reg": "copyreg"}.get(module, module)
    if module in ("torch._utils", "torch"):
        if name == "_rebuild_tensor_v2":
            return _rebuild_tensor_v2
        if name == "_rebuild_parameter":
            return _rebuild_parameter
        if name == "_rebuild_parameter_with_state":
            return _rebuild_parameter_with_state
        if name == "_rebuild_sparse_tensor":
            return _rebuild_sparse_tensor
        if name == "_rebuild_qtensor":
            return _rebuild_qtensor
    if module == "torch.serialization" and name == "_get_layout":
        return torch._get_layout
    if module == "torch":
        if name in torch._STORAGE_NAMES:
            return torch._STORAGE_NAMES[name]
        if name in torch._DTYPES:
            return torch._DTYPES[name]
        if name in torch._QDTYPES:
            return torch._QDTYPES[name]
        if name in torch._QSCHEMES:
            return torch._QSCHEMES[name]
        if name == "Size":
            return torch.Size
        if name == "device":
            return torch.device
        if name == "Tensor":
            return torch.Tensor
    if module == "torch.nn.parameter" and name == "Parameter":
        return torch.nn.Parameter
    if module == "collections":
        if name == "OrderedDict":
            return OrderedDict
        if name == "Counter":
            return Counter
    found = pickle._DATA_GLOBALS.get((module, name))
    if found is not None:
        return found
    raise pickle.UnpicklingError("Weights only load failed: global %s.%s is not allowed" % (module, name))
