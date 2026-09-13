"""torch._utils: the checkpoint reconstruction hooks pickles refer to."""
import torch

_rebuild_tensor_v2 = torch._rebuild_tensor_v2


def _rebuild_parameter(data, requires_grad, backward_hooks):
    return data.requires_grad_(requires_grad)
