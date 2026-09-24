"""torch.distributed for Zipp: PyTorch 2.11's process-group API for the one
process Zipp runs. init_process_group accepts world_size 1 (rank 0) with
the gloo backend (nccl, mpi, ucc and xccl are refused as a CPU-only
PyTorch build refuses them) and the env://, file:// and tcp:// init
methods or a store; every collective then behaves as PyTorch's does on a
one-rank group: all_reduce / reduce / broadcast leave the tensor as it is
(a bitwise reduction still refuses a float tensor), the gathers and
scatters copy the one contribution into place, async_op returns a
completed Work. Point-to-point send/recv have no peer and raise.

A world_size above 1 raises: Zipp cannot start or reach other processes
(torch.distributed.elastic, launch and run are not available either).
"""
import copy as _copy
import os as _os
import torch

__all__ = ["Backend", "GroupMember", "P2POp", "ProcessGroup", "ReduceOp", "Store", "HashStore", "FileStore", "TCPStore",
           "Work", "all_gather", "all_gather_into_tensor", "all_gather_object", "all_reduce", "all_to_all", "all_to_all_single", "barrier",
           "batch_isend_irecv", "broadcast", "broadcast_object_list", "destroy_process_group", "gather", "gather_object", "get_backend",
           "get_global_rank", "get_group_rank", "get_process_group_ranks", "get_rank", "get_world_size", "group", "init_process_group",
           "irecv", "is_available", "is_backend_available", "is_gloo_available", "is_initialized", "is_mpi_available", "is_nccl_available",
           "is_torchelastic_launched", "is_ucc_available", "is_xccl_available", "isend", "monitored_barrier", "new_group", "new_subgroups",
           "recv", "reduce", "reduce_scatter", "reduce_scatter_tensor", "scatter", "scatter_object_list", "send", "get_node_local_rank"]


def is_available():
    return True


def is_gloo_available():
    return True


def is_nccl_available():
    return False


def is_mpi_available():
    return False


def is_ucc_available():
    return False


def is_xccl_available():
    return False


def is_torchelastic_launched():
    return False


class Backend:
    """Backend names: Backend("GLOO") is "gloo"."""
    UNDEFINED = "undefined"
    GLOO = "gloo"
    NCCL = "nccl"
    UCC = "ucc"
    MPI = "mpi"
    XCCL = "xccl"
    backend_list = ["undefined", "gloo", "nccl", "ucc", "mpi", "xccl"]
    default_device_backend_map = {"cpu": "gloo", "cuda": "nccl", "xpu": "xccl"}

    def __new__(cls, name):
        if not isinstance(name, str):
            raise ValueError("Backend constructor parameter must be string-ish")
        value = getattr(Backend, name.upper(), Backend.UNDEFINED)
        if value == Backend.UNDEFINED:
            value = name.lower()
        return value


def is_backend_available(backend):
    return str(backend).lower() == "gloo"


class _RedOpType:
    def __init__(self, name, value):
        self.name = name
        self.value = value

    def __repr__(self):
        return "<RedOpType.%s: %d>" % (self.name, self.value)

    def __str__(self):
        return "RedOpType." + self.name

    def __eq__(self, other):
        if isinstance(other, _RedOpType):
            return other.value == self.value
        return NotImplemented

    def __hash__(self):
        return self.value

    def __int__(self):
        return self.value


class ReduceOp:
    """The reduction a collective applies (ReduceOp.SUM, ...)."""
    SUM = _RedOpType("SUM", 0)
    AVG = _RedOpType("AVG", 1)
    PRODUCT = _RedOpType("PRODUCT", 2)
    MIN = _RedOpType("MIN", 3)
    MAX = _RedOpType("MAX", 4)
    BAND = _RedOpType("BAND", 5)
    BOR = _RedOpType("BOR", 6)
    BXOR = _RedOpType("BXOR", 7)
    PREMUL_SUM = _RedOpType("PREMUL_SUM", 8)
    UNUSED = _RedOpType("UNUSED", 9)
    RedOpType = _RedOpType


reduce_op = ReduceOp


class Work:
    """The handle of an async_op collective: already completed here (one
    process has nothing to wait for)."""

    def __init__(self, result=None):
        self._result = [] if result is None else result

    def wait(self, timeout=None):
        return True

    def is_completed(self):
        return True

    def is_success(self):
        return True

    def exception(self):
        return None

    def result(self):
        return self._result

    def source_rank(self):
        raise RuntimeError("source_rank() is only valid for a recv Work, and Zipp's one process receives nothing")

    def synchronize(self):
        return None

    def __repr__(self):
        return "<torch.distributed.distributed_c10d.Work object>"


class ProcessGroup:
    """A group of ranks: here always rank 0 of one."""

    def __init__(self, backend, ranks=(0,), name="0"):
        self._backend = backend
        self._ranks = list(ranks)
        self.group_name = name
        self.group_desc = "default_pg" if name == "0" else "undefined"

    def rank(self):
        return 0

    def size(self):
        return len(self._ranks)

    def name(self):
        return self._backend

    def _get_backend_name(self):
        return self._backend

    @property
    def bound_device_id(self):
        return None

    def __repr__(self):
        return "<torch.distributed.distributed_c10d.ProcessGroup object>"


class GroupMember:
    WORLD = None
    NON_GROUP_MEMBER = -100


group = GroupMember

_state = {"default": None, "backend": None, "groups": [], "count": 0}


def _default_group_error():
    return ValueError("Default process group has not been initialized, please make sure to call init_process_group.")


def is_initialized():
    return _state["default"] is not None


def _get_default_group():
    if _state["default"] is None:
        raise _default_group_error()
    return _state["default"]


def _group(group):
    pg = _get_default_group()
    if group is None or group is GroupMember.WORLD:
        return pg
    if group == GroupMember.NON_GROUP_MEMBER:
        return None
    if not isinstance(group, ProcessGroup) or group not in _state["groups"]:
        raise ValueError("Invalid process group specified")
    return group


def _one_process(world_size, rank=0):
    if world_size is not None and world_size not in (-1, 1):
        raise RuntimeError("torch.distributed on Zipp runs a single process: world_size must be 1 (rank 0), got world_size=%d. Zipp cannot start or reach other processes; run with world_size=1 or use PyTorch for multi-process training." % world_size)
    if rank not in (-1, 0):
        raise RuntimeError("torch.distributed on Zipp runs a single process: rank must be 0, got rank=%d" % rank)


def _parse_url(url):
    """(scheme, netloc, path, query dict) of an init_method URL."""
    scheme, _, rest = url.partition("://")
    rest, _, query = rest.partition("?")
    params = {}
    for part in query.split("&") if query else ():
        k, _, v = part.partition("=")
        params[k] = v
    if scheme == "file":
        return scheme, "", rest, params
    netloc, _, path = rest.partition("/")
    return scheme, netloc, path, params


def _rendezvous(init_method, rank, world_size):
    if init_method.startswith("env://"):
        env = _os.environ
        prefix = "Error initializing torch.distributed using env:// rendezvous: environment variable "
        if rank == -1:
            if "RANK" not in env:
                raise ValueError(prefix + "RANK expected, but not set")
            rank = int(env["RANK"])
        if world_size == -1:
            if "WORLD_SIZE" not in env:
                raise ValueError(prefix + "WORLD_SIZE expected, but not set")
            world_size = int(env["WORLD_SIZE"])
        if "MASTER_ADDR" not in env:
            raise ValueError(prefix + "MASTER_ADDR expected, but not set")
        if "MASTER_PORT" not in env:
            raise ValueError(prefix + "MASTER_PORT expected, but not set")
        return rank, world_size
    if init_method.startswith("tcp://") or init_method.startswith("file://"):
        scheme, netloc, path, params = _parse_url(init_method)
        if "rank" in params:
            rank = int(params["rank"])
        if "world_size" in params:
            world_size = int(params["world_size"])
        if rank == -1:
            raise ValueError("Error initializing torch.distributed using %s:// rendezvous: rank parameter missing" % scheme)
        if world_size == -1:
            raise ValueError("Error initializing torch.distributed using %s:// rendezvous: world size parameter missing" % scheme)
        if scheme == "tcp" and ":" not in netloc:
            raise ValueError("Error initializing torch.distributed using tcp:// rendezvous: port number missing")
        return rank, world_size
    raise RuntimeError("No rendezvous handler for %s" % init_method.split("://")[0] + "://")


def init_process_group(backend=None, init_method=None, timeout=None, world_size=-1, rank=-1, store=None, group_name="", pg_options=None, device_id=None):
    if _state["default"] is not None:
        raise ValueError("trying to initialize the default process group twice!")
    if store is not None:
        if init_method is not None:
            raise AssertionError("Cannot specify both init_method and store.")
        if world_size <= 0:
            raise AssertionError("world_size must be positive if using store")
        if rank < 0:
            raise AssertionError("rank must be non-negative if using store")
    elif init_method is None:
        init_method = "env://"
    if backend is None:
        # A CPU-only PyTorch build's default: a gloo group reporting its
        # backend as "undefined".
        name = "undefined"
    else:
        name = Backend(backend)
        for part in name.split(","):
            b = part.split(":")[-1]
            if b in ("nccl", "mpi", "ucc", "xccl") and ":" not in part:
                raise RuntimeError({"nccl": "Distributed package doesn't have NCCL built in",
                                    "mpi": "Distributed package doesn't have MPI built in. MPI is only included if you build PyTorch from source on a host that has MPI installed.",
                                    "ucc": "Distributed package doesn't have UCC built in",
                                    "xccl": "Distributed package doesn't have XCCL built in"}[b])
            if b not in ("gloo", "nccl", "mpi", "ucc", "xccl"):
                raise AssertionError("Unknown backend type %s" % b)
    if store is None:
        rank, world_size = _rendezvous(init_method, rank, world_size)
    _one_process(world_size, rank)
    pg = ProcessGroup(name, (0,), "0")
    _state["default"] = pg
    _state["backend"] = name
    _state["groups"] = [pg]
    _state["count"] = 1
    GroupMember.WORLD = pg


def destroy_process_group(group=None):
    if group is None or group is GroupMember.WORLD:
        if _state["default"] is None:
            raise AssertionError("Process group cannot be None")
        _state["default"] = None
        _state["backend"] = None
        _state["groups"] = []
        GroupMember.WORLD = None
        return
    pg = _group(group)
    _state["groups"].remove(pg)


def get_rank(group=None):
    pg = _group(group)
    return -1 if pg is None else 0


def get_world_size(group=None):
    pg = _group(group)
    return -1 if pg is None else pg.size()


def get_backend(group=None):
    pg = _group(group)
    if pg is None:
        raise ValueError("Invalid process group specified")
    return pg._backend


def get_backend_config(group=None):
    return get_backend(group)


def get_process_group_ranks(group):
    return list(_group(group)._ranks)


def get_global_rank(group, group_rank):
    pg = _group(group)
    if group_rank not in range(pg.size()):
        raise ValueError("Group rank %d is not part of group %s" % (group_rank, pg))
    return pg._ranks[group_rank]


def get_group_rank(group, global_rank):
    pg = _group(group)
    if global_rank not in pg._ranks:
        raise ValueError("Global rank %d is not part of group %s" % (global_rank, pg))
    return pg._ranks.index(global_rank)


def get_node_local_rank(fallback_rank=None):
    if "LOCAL_RANK" in _os.environ:
        return int(_os.environ["LOCAL_RANK"])
    if fallback_rank is not None:
        return int(fallback_rank)
    raise RuntimeError("LOCAL_RANK is not in the environment. Consider passing fallback_rank to allow `get_node_local_rank` to work, assuming you are not running in a multi-device context and want the code to run locally instead.")


def new_group(ranks=None, timeout=None, backend=None, pg_options=None, use_local_synchronization=False, group_desc=None, device_id=None):
    default = _get_default_group()
    ranks = [0] if ranks is None else sorted(ranks)
    if len(ranks) > default.size():
        raise ValueError("the new group's world size should be less or equal to the world size set by init_process_group")
    for r in ranks:
        if r < 0 or r >= default.size():
            raise ValueError("Invalid rank %d, should be in range [0, %d)" % (r, default.size()))
    if not ranks:
        return GroupMember.NON_GROUP_MEMBER
    _state["count"] += 1
    pg = ProcessGroup(default._backend if backend is None else str(Backend(backend)), ranks, str(_state["count"] - 1))
    _state["groups"].append(pg)
    return pg


def new_subgroups(group_size=None, group=None, timeout=None, backend=None, pg_options=None, group_desc=None):
    if group_size is None:
        group_size = 1
    if group_size != 1:
        raise ValueError("The arg 'group_size' (%d) must not exceed the world size (1)" % group_size)
    pg = new_group([0])
    return pg, [pg]


def _done(async_op, result=None):
    return Work(result) if async_op else None


def _check_tensor(t, name="tensor"):
    if not isinstance(t, torch.Tensor):
        raise TypeError("Invalid function argument. Expected parameter `%s` of type torch.Tensor\n             but got %s instead." % (name, type(t)))


def _check_list(ts, name):
    if not isinstance(ts, list) or not all(isinstance(t, torch.Tensor) for t in ts):
        raise TypeError("Invalid function argument. Expected parameter `%s` of type List[torch.Tensor]\n             but got %s instead." % (name, type(ts)))


def _check_op(tensor, op):
    if op in (ReduceOp.BAND, ReduceOp.BOR, ReduceOp.BXOR) and (tensor.dtype.is_floating_point or tensor.dtype.is_complex):
        raise RuntimeError("Cannot use %s with non-integral dtype" % str(op).replace("RedOpType", "ReduceOp"))
    if op == ReduceOp.PREMUL_SUM:
        raise RuntimeError("PREMUL_SUM is not supported by the gloo backend")


def all_reduce(tensor, op=ReduceOp.SUM, group=None, async_op=False):
    _check_tensor(tensor)
    pg = _group(group)
    if pg is None:
        return None
    _check_op(tensor, op)
    return _done(async_op, [tensor])


def all_reduce_coalesced(tensors, op=ReduceOp.SUM, group=None, async_op=False):
    for t in tensors:
        _check_op(t, op)
    _group(group)
    return _done(async_op, list(tensors))


def reduce(tensor, dst=None, op=ReduceOp.SUM, group=None, async_op=False, group_dst=None):
    _check_tensor(tensor)
    pg = _group(group)
    if pg is None:
        return None
    _root(dst, group_dst, "reduce")
    _check_op(tensor, op)
    return _done(async_op, [tensor])


def _root(rank, group_rank, what):
    r = 0 if rank is None and group_rank is None else (group_rank if rank is None else rank)
    if r != 0:
        raise RuntimeError("ProcessGroupGloo::%s: invalid root rank: %d" % (what, r))


def broadcast(tensor, src=None, group=None, async_op=False, group_src=None):
    _check_tensor(tensor)
    pg = _group(group)
    if pg is None:
        return None
    _root(src, group_src, "broadcast")
    return _done(async_op, [tensor])


def broadcast_object_list(object_list, src=None, group=None, device=None, group_src=None):
    pg = _group(group)
    if pg is None:
        return None
    _root(src, group_src, "broadcast")
    return None


def _copy_into(dst, src):
    with torch.no_grad():
        dst.copy_(src)


def all_gather(tensor_list, tensor, group=None, async_op=False):
    _check_list(tensor_list, "tensor_list")
    _check_tensor(tensor)
    pg = _group(group)
    if pg is None:
        return None
    if len(tensor_list) != pg.size():
        raise RuntimeError("ProcessGroupGloo::allgather: invalid output tensor list at index 0 (expected length %d, got %d)" % (pg.size(), len(tensor_list)))
    if tuple(tensor_list[0].shape) != tuple(tensor.shape):
        raise RuntimeError("ProcessGroupGloo::allgather: invalid tensor size at index 0 (expected %s, got %s)" % (_size(tensor), _size(tensor_list[0])))
    _copy_into(tensor_list[0], tensor)
    return _done(async_op, tensor_list)


def _size(t):
    return "(" + ", ".join(str(d) for d in t.shape) + ")"


def all_gather_into_tensor(output_tensor, input_tensor, group=None, async_op=False):
    _check_tensor(output_tensor, "output_tensor")
    _check_tensor(input_tensor, "input_tensor")
    pg = _group(group)
    if pg is None:
        return None
    if output_tensor.numel() != input_tensor.numel() * pg.size():
        raise RuntimeError("ProcessGroupGloo::allgather: invalid tensor size at index 0 (expected %s, got %s)" % (_size(input_tensor), _size(output_tensor)))
    _copy_into(output_tensor, input_tensor.reshape(output_tensor.shape))
    return _done(async_op, [output_tensor])


def all_gather_object(object_list, obj, group=None):
    pg = _group(group)
    if pg is None:
        return None
    if len(object_list) != pg.size():
        raise IndexError("list assignment index out of range")
    object_list[0] = _copy.deepcopy(obj)


def gather(tensor, gather_list=None, dst=None, group=None, async_op=False, group_dst=None):
    _check_tensor(tensor)
    pg = _group(group)
    if pg is None:
        return None
    _root(dst, group_dst, "gather")
    if gather_list is None:
        raise ValueError("Argument ``gather_list`` must be specified on destination rank.")
    _check_list(gather_list, "gather_list")
    if len(gather_list) != pg.size():
        raise RuntimeError("ProcessGroupGloo::gather: Incorrect output list size %d. Output list size should be %d, same as size of the process group." % (len(gather_list), pg.size()))
    _copy_into(gather_list[0], tensor)
    return _done(async_op, gather_list)


def gather_object(obj, object_gather_list=None, dst=None, group=None, group_dst=None):
    pg = _group(group)
    if pg is None:
        return None
    _root(dst, group_dst, "gather")
    if object_gather_list is None:
        raise ValueError("Argument ``gather_list`` must be specified on destination rank.")
    object_gather_list[0] = _copy.deepcopy(obj)


def scatter(tensor, scatter_list=None, src=None, group=None, async_op=False, group_src=None):
    _check_tensor(tensor)
    pg = _group(group)
    if pg is None:
        return None
    _root(src, group_src, "scatter")
    if scatter_list is None:
        raise ValueError("Argument ``scatter_list`` must be specified on source rank.")
    _check_list(scatter_list, "scatter_list")
    if len(scatter_list) != pg.size():
        raise RuntimeError("ProcessGroupGloo::scatter: Incorrect input list size %d. Input list size should be %d, same as size of the process group." % (len(scatter_list), pg.size()))
    _copy_into(tensor, scatter_list[0])
    return _done(async_op, [tensor])


def scatter_object_list(scatter_object_output_list, scatter_object_input_list=None, src=None, group=None, group_src=None):
    pg = _group(group)
    if pg is None:
        return None
    _root(src, group_src, "scatter")
    if scatter_object_input_list is None:
        raise ValueError("Expected argument scatter_object_input_list to be a list on the source rank")
    scatter_object_output_list[0] = _copy.deepcopy(scatter_object_input_list[0])


def reduce_scatter(output, input_list, op=ReduceOp.SUM, group=None, async_op=False):
    _check_tensor(output, "output")
    _check_list(input_list, "input_list")
    pg = _group(group)
    if pg is None:
        return None
    _check_op(output, op)
    if len(input_list) != pg.size():
        raise RuntimeError("ProcessGroupGloo::reduce_scatter: invalid input list size %d (expected %d)" % (len(input_list), pg.size()))
    _copy_into(output, input_list[0])
    return _done(async_op, [output])


def reduce_scatter_tensor(output, input, op=ReduceOp.SUM, group=None, async_op=False):
    _check_tensor(output, "output")
    _check_tensor(input, "input")
    pg = _group(group)
    if pg is None:
        return None
    _check_op(output, op)
    if input.numel() != output.numel() * pg.size():
        raise RuntimeError("ProcessGroupGloo::reduce_scatter_tensor: input tensor must be the same size as output size times world size")
    _copy_into(output, input.reshape(output.shape))
    return _done(async_op, [output])


def all_to_all(output_tensor_list, input_tensor_list, group=None, async_op=False):
    pg = _group(group)
    if pg is None:
        return None
    if "gloo" in pg._backend or pg._backend == "undefined":
        raise RuntimeError("Backend gloo does not support alltoall")
    _copy_into(output_tensor_list[0], input_tensor_list[0])
    return _done(async_op, output_tensor_list)


def all_to_all_single(output, input, output_split_sizes=None, input_split_sizes=None, group=None, async_op=False):
    _check_tensor(output, "output")
    _check_tensor(input, "input")
    pg = _group(group)
    if pg is None:
        return None
    if output.numel() != input.numel():
        raise RuntimeError("Tensor size mismatch between output and input for all_to_all_single")
    _copy_into(output, input.reshape(output.shape))
    return _done(async_op, [output])


def _p2p(peer, what):
    _get_default_group()
    if peer is None:
        raise RuntimeError("%s from any source cannot complete: Zipp runs one process, so no other rank exists" % what)
    if peer == 0:
        if what == "send":
            raise ValueError("Invalid destination rank: destination rank should not be the same as the rank of the current process.")
        raise ValueError("Invalid source rank: source rank should not be the same as the rank of the current process.")
    raise ValueError("Invalid %s rank %d: Zipp runs one process (world_size 1), so rank %d does not exist" % ("destination" if what == "send" else "source", peer, peer))


def send(tensor, dst=None, group=None, tag=0, group_dst=None):
    _check_tensor(tensor)
    _p2p(group_dst if dst is None else dst, "send")


def recv(tensor, src=None, group=None, tag=0, group_src=None):
    _check_tensor(tensor)
    _p2p(group_src if src is None else src, "recv")


def isend(tensor, dst=None, group=None, tag=0, group_dst=None):
    send(tensor, dst, group, tag, group_dst)


def irecv(tensor, src=None, group=None, tag=0, group_src=None):
    recv(tensor, src, group, tag, group_src)


class P2POp:
    def __init__(self, op, tensor, peer=None, group=None, tag=0, group_peer=None):
        self.op = op
        self.tensor = tensor
        self.peer = peer
        self.group = group
        self.tag = tag
        self.group_peer = group_peer


def batch_isend_irecv(p2p_op_list):
    works = []
    for p in p2p_op_list:
        works.append(p.op(p.tensor, p.peer, p.group, p.tag))
    return works


def barrier(group=None, async_op=False, device_ids=None):
    pg = _group(group)
    if pg is None:
        return None
    return _done(async_op)


def monitored_barrier(group=None, timeout=None, wait_all_ranks=False):
    pg = _group(group)
    if pg is None:
        return None
    return None


# ---- stores ------------------------------------------------------------------------------
class Store:
    """A key-value store (the rendezvous a process group can be built on):
    here one process's dictionary."""

    def __init__(self):
        self._data = {}
        self.timeout = None

    @staticmethod
    def _bytes(value):
        if isinstance(value, str):
            return value.encode()
        return bytes(value)

    def set(self, key, value):
        self._data[key] = self._bytes(value)

    def get(self, key):
        if key not in self._data:
            raise RuntimeError("Timeout waiting for key: %s after 300000 ms" % key)
        return self._data[key]

    def add(self, key, amount):
        value = int(self._data.get(key, b"0")) + amount
        self._data[key] = str(value).encode()
        return value

    def compare_set(self, key, expected_value, desired_value):
        current = self._data.get(key)
        expected = self._bytes(expected_value)
        if (current is None and expected == b"") or current == expected:
            self._data[key] = self._bytes(desired_value)
        return self._data.get(key, expected)

    def check(self, keys):
        return all(k in self._data for k in keys)

    def wait(self, keys, timeout=None):
        for k in keys:
            if k not in self._data:
                raise RuntimeError("Timeout waiting for key: %s" % k)

    def delete_key(self, key):
        return self._data.pop(key, None) is not None

    def num_keys(self):
        return len(self._data)

    def set_timeout(self, timeout):
        self.timeout = timeout


class HashStore(Store):
    pass


class FileStore(Store):
    def __init__(self, file_name, world_size=-1):
        super().__init__()
        self.path = file_name
        _one_process(world_size if world_size != -1 else 1)

    def num_keys(self):
        # PyTorch's FileStore counts its own cleanup key too.
        return len(self._data) + 1


class TCPStore(Store):
    """A TCPStore without the network: Zipp has no sockets, and one process
    needs none."""

    def __init__(self, host_name, port, world_size=None, is_master=False, timeout=None, wait_for_workers=True, multi_tenant=False, master_listen_fd=None, use_libuv=True):
        super().__init__()
        self.host = host_name
        self.port = port
        _one_process(world_size if world_size is not None else 1)
