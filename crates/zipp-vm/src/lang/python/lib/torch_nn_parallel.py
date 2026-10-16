"""torch.nn.parallel for Zipp: DataParallel and DistributedDataParallel as
single-device wrappers.

Zipp runs one CPU process, so both wrappers hold the module as `.module`
(their state_dict keys carry PyTorch's `module.` prefix, so checkpoints
move between the two unchanged) and forward calls it directly.
DataParallel behaves as PyTorch's does when no accelerator is available:
`device_ids`, `output_device` and `dim` are accepted and ignored, and
`device_ids` reads `[]`. DistributedDataParallel follows PyTorch's CPU
module path (`device_ids`/`output_device` must be None, `device` is cpu)
and uses torch.distributed's default process group (world_size 1) when one
is initialized; unlike PyTorch it does not require one. With one process
there is nothing to synchronize, so gradients are left exactly as backward
computed them (the all-reduce of a one-rank group) and communication
hooks are recorded but never run.
"""
from contextlib import contextmanager
import torch
from torch.nn import Module

__all__ = ["DataParallel", "DistributedDataParallel", "data_parallel"]


class DataParallel(Module):
    def __init__(self, module, device_ids=None, output_device=None, dim=0):
        super().__init__()
        self.module = module
        # PyTorch without an accelerator: the device arguments (and dim)
        # are ignored and not even recorded.
        self.device_ids = []

    def forward(self, *inputs, **kwargs):
        return self.module(*inputs, **kwargs)

    def replicate(self, module, device_ids):
        return [module]

    def scatter(self, inputs, kwargs, device_ids):
        return (tuple(inputs),), (dict(kwargs or {}),)

    def parallel_apply(self, replicas, inputs, kwargs):
        return [m(*i, **k) for m, i, k in zip(replicas, inputs, kwargs)]

    def gather(self, outputs, output_device):
        return outputs[0]


def data_parallel(module, inputs, device_ids=None, output_device=None, dim=0, module_kwargs=None):
    """The functional DataParallel: on one CPU device, module(*inputs)."""
    if not isinstance(inputs, tuple):
        inputs = (inputs,) if inputs is not None else ()
    return module(*inputs, **(module_kwargs or {}))


class DistributedDataParallel(Module):
    def __init__(self, module, device_ids=None, output_device=None, dim=0, broadcast_buffers=True, init_sync=True, process_group=None, bucket_cap_mb=None, find_unused_parameters=False, check_reduction=False, gradient_as_bucket_view=False, static_graph=False, delay_all_reduce_named_params=None, param_to_hook_all_reduce=None, mixed_precision=None, device_mesh=None, skip_all_reduce_unused_params=False, bucket_cap_mb_list=None):
        super().__init__()
        if (delay_all_reduce_named_params is not None) != (param_to_hook_all_reduce is not None):
            raise ValueError("delay_all_reduce_named_params and param_to_hook_all_reduce need to be set at the same time.")
        if process_group and device_mesh is not None:
            raise RuntimeError("Cannot specify both process_group and device_mesh arguments.")
        if not any(p.requires_grad for p in module.parameters()):
            raise RuntimeError("DistributedDataParallel is not needed when a module doesn't have any parameter that requires a gradient.")
        if device_ids is not None and len(device_ids) > 1:
            raise ValueError("device_ids can only be None or contain a single element.")
        if device_ids or output_device:
            raise ValueError("DistributedDataParallel device_ids and output_device arguments only work with single-device/multiple-device GPU modules or CPU modules, but got device_ids %s, output_device %s, and module parameters {device(type='cpu')}." % (device_ids, output_device))
        self.module = module
        if process_group is None and device_mesh is None:
            import torch.distributed as dist
            if dist.is_initialized():
                process_group = dist.group.WORLD
        self.process_group = process_group
        self.device_mesh = device_mesh
        self.device_ids = None
        self.output_device = None
        self.device = torch.device("cpu")
        self.device_type = "cpu"
        self.is_multi_device_module = False
        self.dim = dim
        self.broadcast_buffers = broadcast_buffers
        self.find_unused_parameters = find_unused_parameters
        self.gradient_as_bucket_view = gradient_as_bucket_view
        self.static_graph = static_graph
        self.mixed_precision = mixed_precision
        self.skip_all_reduce_unused_params = skip_all_reduce_unused_params
        self.require_backward_grad_sync = True
        self.require_forward_param_sync = True
        self.parameters_to_ignore = set(getattr(module, "_ddp_params_and_buffers_to_ignore", ()))
        self.bucket_bytes_cap_default = bucket_cap_mb is None
        self.bucket_bytes_cap = int((25 if bucket_cap_mb is None else bucket_cap_mb) * 1024 * 1024)
        self.broadcast_bucket_size = int(250 * 1024 * 1024)
        self.use_side_stream_for_tensor_copies = False
        self._comm_hooks = []

    def forward(self, *inputs, **kwargs):
        return self.module(*inputs, **kwargs)

    @contextmanager
    def no_sync(self):
        old = self.require_backward_grad_sync
        self.require_backward_grad_sync = False
        try:
            yield
        finally:
            self.require_backward_grad_sync = old

    @contextmanager
    def join(self, divide_by_initial_world_size=True, enable=True, throw_on_early_termination=False):
        yield

    def register_comm_hook(self, state, hook):
        if not callable(hook):
            raise TypeError("Communication hook must be callable.")
        # One process: there is no all-reduce for a hook to replace.
        self._comm_hooks.append((state, hook))

    def will_sync_module_buffers(self):
        return False

    def _set_static_graph(self):
        self.static_graph = True

    def scatter(self, inputs, kwargs, device_ids):
        return (tuple(inputs),), (dict(kwargs or {}),)

    def to_kwargs(self, inputs, kwargs, device_id):
        return self.scatter(inputs, kwargs, [device_id])

    def gather(self, outputs, output_device):
        return outputs

    @staticmethod
    def _set_params_and_buffers_to_ignore_for_model(module, params_and_buffers_to_ignore):
        module._ddp_params_and_buffers_to_ignore = params_and_buffers_to_ignore
