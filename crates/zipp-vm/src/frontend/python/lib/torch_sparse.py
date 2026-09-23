"""torch.sparse for Zipp: sparse COO tensors (torch.sparse_coo, hybrid ones
with dense dimensions included) and compressed CSR/CSC matrices
(torch.sparse_csr, torch.sparse_csc), following PyTorch 2.11's CPU
semantics: coalescing order and duplicate summation, the merge PyTorch's
sparse add makes, the intersection its sparse multiply takes, printing,
errors, and gradients (sparse gradients where PyTorch gives them, such as
torch.sparse.mm and nn.Embedding(sparse=True)).

A sparse tensor is a `torch._SparseTensor` (a Tensor subclass named Tensor)
holding dense index and value tensors: COO keeps `_ind` [sparse_dim, nnz]
(int64) and `_val` [nnz, *dense_shape] with the `_coal` (is_coalesced) flag;
CSR/CSC keep `_crow` (compressed indices), `_col` (plain indices) and
`_val`. The heavy loops (coalescing, merging, sparse-dense products,
softmax pools) are `_zipp_tensor` kernels (`sp_*`). Operations on a
compressed tensor run on its COO form and convert back.
"""
import builtins as _b
import math as _math
import torch
import _zipp_tensor as _k

_int, _float, _bool, _len, _range, _tuple, _list, _isinstance = _b.int, _b.float, _b.bool, _b.len, _b.range, _b.tuple, _b.list, _b.isinstance
_T = torch.Tensor
_S = torch._SparseTensor
_Size = torch.Size
_onew = object.__new__
_int64 = torch.int64
_coo = torch.sparse_coo
_csr = torch.sparse_csr
_csc = torch.sparse_csc
_COMPRESSED = (torch.sparse_csr, torch.sparse_csc)
_BLOCKED = (torch.sparse_bsr, torch.sparse_bsc)
_BACKEND = {"sparse_coo": "Sparse", "sparse_csr": "SparseCsr", "sparse_csc": "SparseCsc", "sparse_bsr": "SparseBsr", "sparse_bsc": "SparseBsc", "strided": "Strided"}

__all__ = ["addmm", "check_sparse_tensor_invariants", "log_softmax", "mm", "sampled_addmm", "softmax", "spdiags", "sum"]


# ---- errors ------------------------------------------------------------------------------
def _layout_name(t):
    return _BACKEND[repr(t.layout)[6:]]


def _no_kernel(op, t):
    """PyTorch's error for an operator without a kernel for this layout
    (its first sentences)."""
    backend = "SparseCPU" if t.layout is _coo else "SparseCsrCPU"
    return NotImplementedError("Could not run '%s' with arguments from the '%s' backend. This could be because the operator doesn't exist for this backend, or was omitted during the selective/custom build process (if using custom build)." % (op, backend))


def _unsupported_layout(what):
    return NotImplementedError("%s is not supported on Zipp (the sparse layouts are sparse_coo, sparse_csr and sparse_csc)" % what)


def _need_coo(t, what):
    if t.layout is not _coo:
        raise RuntimeError("%s expected sparse coordinate tensor layout but got %s" % (what, _layout_name(t)))


# ---- construction ------------------------------------------------------------------------
def _new(layout, shape, values, coalesced=False):
    t = _onew(_S)
    t._layout = layout
    t.shape = shape if shape.__class__ is _Size else _Size(shape)
    t.dtype = values.dtype
    t._val = values
    t._coal = coalesced
    return t


def _make_coo(ind, val, shape, coalesced=False, vnc=False):
    """A COO tensor over `ind`/`val` (no history). `vnc`: its values are an
    expanded (non-contiguous) tensor in PyTorch, so adding another sparse
    tensor to it concatenates instead of merging."""
    t = _new(_coo, shape, val, coalesced)
    t._ind = ind
    if vnc:
        t._vnc = True
    return t


def _make_compressed(layout, cidx, pidx, val, shape):
    t = _new(layout, shape, val)
    t._crow = cidx
    t._col = pidx
    return t


def _dense(storage, shape, dt):
    return torch._new3(storage, shape if shape.__class__ is _Size else _Size(shape), dt)


def _plain(t):
    """t's elements as a tensor with no history (shares the storage)."""
    return t if t._node is None and not t.requires_grad else _T(t._s, t.shape, t.dtype)


def _grad_on(*ts):
    if not torch._grad_enabled:
        return False
    for t in ts:
        if _isinstance(t, _T) and t.requires_grad:
            return True
    return False


def _record(out, parents, backward, name):
    out.requires_grad = True
    out._node = torch._Node(backward, _tuple(parents), name)
    return out


def _prod(xs):
    n = 1
    for x in xs:
        n *= x
    return n


def _sparse_dim(t):
    return t._ind.shape[0] if t.layout is _coo else 2


def _dense_dim(t):
    return _len(t._val.shape) - 1


def _nnz_of(t):
    return t._val.shape[0] if t.layout is _coo else t._col.shape[0]


def _block(t):
    return _prod(t._val.shape[1:])


def _keys(ind, sizes):
    """Each nonzero's row-major linear key over the sparse dims (int64)."""
    n = ind.shape[1]
    return _dense(_k.sp_keys(_plain(ind)._s, n, _tuple(sizes)), (n,), _int64)


def _to_int64(t):
    return t if t.dtype is _int64 else t.to(_int64)


def _compact(mask):
    """The positions where the 1-d tensor `mask` is nonzero."""
    out = _k.sp_compact(_plain(mask)._s)
    return _dense(out, (_k.size(out),), _int64)


# ---- coalescing ------------------------------------------------------------------------
def _coalesce_raw(s):
    """PyTorch's _coalesce_sparse_cpu on a COO tensor, without history."""
    if s._coal:
        return s
    nnz = s._ind.shape[1]
    if nnz < 2:
        return _make_coo(s._ind.clone(), _plain(s._val).clone(), s.shape, True)
    sd = s._ind.shape[0]
    keys = _k.sp_keys(_plain(s._ind)._s, nnz, _tuple(s.shape[:sd]))
    first, vals = _k.sp_coalesce(keys, _plain(s._val)._s, _block(s))
    n = _k.size(first)
    ind = torch.index_select(_plain(s._ind), 1, _dense(first, (n,), _int64))
    return _make_coo(ind, _dense(vals, (n,) + _tuple(s._val.shape[1:]), s.dtype), s.shape, True)


def coalesce(s):
    _need_coo(s, "coalesce")
    if s._coal:
        return s
    out = _coalesce_raw(s)
    if _grad_on(s):
        _record(out, (s,), lambda g: (g,), "Coalesce")
    return out


# ---- layout conversions ------------------------------------------------------------------
def _expand_compressed(c, n):
    """The index each plain entry's row (CSR) or column (CSC) has."""
    if c.shape[0] != n + 1:
        raise RuntimeError("compressed indices must have %d elements, but got %d" % (n + 1, c.shape[0]))
    out = _k.sp_expand(_to_int64(c)._s)
    return _dense(out, (_k.size(out),), _int64)


def _compressed_to_coo_raw(t):
    if _len(t.shape) != 2 + _dense_dim(t):
        raise _unsupported_layout("a batched compressed tensor")
    n = t.shape[0] if t.layout is _csr else t.shape[1]
    major = _expand_compressed(_plain(t._crow), n)
    minor = _to_int64(_plain(t._col))
    if t.layout is _csr:
        return _make_coo(torch.stack([major, minor]), _plain(t._val), t.shape, True)
    # CSC keeps its column-major order: not a coalesced COO tensor.
    return _make_coo(torch.stack([minor, major]), _plain(t._val), t.shape, False)


def _coo_to_compressed_raw(s, layout, index_dtype=_int64, keep_order=False):
    """s as CSR/CSC. `keep_order`: s's entries are already grouped by row
    (CSR) in increasing order and keep their order within a row."""
    if s._ind.shape[0] != 2:
        raise RuntimeError("coo_to_sparse_%s: conversion from Sparse to %s for input tensors with sparse_dim()!=2 is not supported" % ("csr" if layout is _csr else "csc", "SparseCsr" if layout is _csr else "SparseCsc"))
    c = s if keep_order else _coalesce_raw(s)
    ind, val = c._ind, c._val
    if layout is _csc:
        # Column-major order: the transpose's coalesced order.
        t = _coalesce_raw(_make_coo(torch.stack([ind[1], ind[0]]), val, (s.shape[1], s.shape[0]) + _tuple(s.shape[2:]), False))
        ind, val = torch.stack([t._ind[1], t._ind[0]]), t._val
        major, minor, n = ind[1], ind[0], s.shape[1]
    else:
        major, minor, n = ind[0], ind[1], s.shape[0]
    crow = _dense(_k.sp_compress(_plain(major)._s, n), (n + 1,), _int64)
    if index_dtype is not _int64:
        crow, minor = crow.to(index_dtype), minor.to(index_dtype)
    return _make_compressed(layout, crow, minor, val, s.shape)


def _as_coo(t):
    """t (COO or compressed) as a COO tensor, with history."""
    if t.layout is _coo:
        return t
    out = _compressed_to_coo_raw(t)
    if _grad_on(t):
        layout = t.layout
        _record(out, (t,), lambda g: (_regrad(g, layout),), "ToSparse")
    return out


def _regrad(g, layout):
    """A gradient for a tensor of `layout` (dense gradients stay dense)."""
    if g is None or g.__class__ is not _S or g.layout is layout:
        return g
    if layout is _coo:
        return _compressed_to_coo_raw(g)
    return _coo_to_compressed_raw(g if g.layout is _coo else _compressed_to_coo_raw(g), layout)


def _as_layout(s, layout, index_dtype=_int64):
    """The COO tensor s converted to `layout`, with history."""
    if layout is _coo:
        return s
    out = _coo_to_compressed_raw(s, layout, index_dtype)
    if _grad_on(s):
        _record(out, (s,), lambda g: (_regrad(g, _coo),), "ToSparse" + ("Csr" if layout is _csr else "Csc"))
    return out


def _to_dense_raw(s):
    if s.layout is not _coo:
        s = _compressed_to_coo_raw(s)
    sd = s._ind.shape[0]
    sizes, dshape = _tuple(s.shape[:sd]), _tuple(s.shape[sd:])
    P = _prod(sizes)
    flat = (P,) + dshape
    st = _k.zeros(s.dtype.name, _prod(flat))
    nnz = s._ind.shape[1]
    if nnz and P:
        vals = _plain(s._val)
        _k.sp_scatter_add(st, _keys(s._ind, sizes)._s, vals._s, _prod(dshape))
    return _dense(st, s.shape, s.dtype)


def _gather(dense, s, keys=None):
    """The elements of `dense` (s's shape) at each of s's nonzeros, in s's
    order: [nnz, *dense_shape] (differentiable in `dense`)."""
    sd = s._ind.shape[0]
    sizes = _tuple(s.shape[:sd])
    if keys is None:
        keys = _keys(s._ind, sizes)
    flat = dense.reshape((_prod(sizes),) + _tuple(s.shape[sd:]))
    return torch.index_select(flat, 0, keys)


def to_dense(s, dtype=None, masked_grad=None):
    if s.__class__ is not _S:
        return s if dtype is None else s.to(dtype)
    if s.layout in _BLOCKED:
        raise _unsupported_layout(repr(s.layout))
    out = _to_dense_raw(s)
    if dtype is not None and dtype is not out.dtype:
        out = out.to(dtype)
    if _grad_on(s):
        layout = s.layout

        def backward(g):
            c = _coalesce_raw(s if layout is _coo else _compressed_to_coo_raw(s))
            return (_regrad(_make_coo(c._ind, _gather(g, c), s.shape, True), layout),)
        _record(out, (s,), backward, "ToDense")
    return out


def _grad_values(g, s):
    """The values the gradient g (sparse or dense, s's shape) has at each of
    s's nonzeros, in s's order (zeros where a sparse g has none)."""
    if g.__class__ is not _S:
        return _gather(g, s)
    if g.layout is not _coo:
        g = _compressed_to_coo_raw(g)
    if g._ind is s._ind:
        return g._val
    gc = _coalesce_raw(g)
    if gc._ind is s._ind:
        return gc._val
    sd = s._ind.shape[0]
    sizes = _tuple(s.shape[:sd])
    n = s._ind.shape[1]
    tail = _tuple(gc._val.shape[1:])
    if gc._ind.shape[1] == 0:
        return torch.zeros((n,) + tail, dtype=g.dtype)
    pos = _dense(_k.sp_search(_keys(gc._ind, sizes)._s, _keys(s._ind, sizes)._s), (n,), _int64)
    found = pos >= 0
    picked = torch.index_select(gc._val, 0, torch.clamp(pos, 0))
    if not tail:
        return torch.where(found, picked, torch.zeros((), dtype=g.dtype))
    return torch.where(found.reshape((n,) + (1,) * _len(tail)), picked, torch.zeros((), dtype=g.dtype))


def _with_values(s, vals, name="SparseCooTensorWithDimsAndTensors", coalesced=None, vnc=False):
    """A COO tensor with s's indices and `vals`, recording the history
    `vals` carries (the gradient of the result reaches it as values)."""
    out = _make_coo(s._ind, _plain(vals), s.shape, s._coal if coalesced is None else coalesced, vnc)
    if _grad_on(vals):
        _record(out, (vals,), lambda g: (_grad_values(g, out),), name)
    return out


def sparse_mask(dense, mask):
    """dense's values at mask's nonzeros (Tensor.sparse_mask)."""
    if mask.__class__ is not _S:
        raise RuntimeError("sparse_mask expects mask to be sparse, got %s" % _layout_name(mask))
    if _tuple(dense.shape) != _tuple(mask.shape):
        raise RuntimeError("sparse_mask(): operands have incompatible sizes; self has size %s but mask has size %s." % (_list(dense.shape), _list(mask.shape)))
    layout = mask.layout
    m = mask if layout is _coo else _compressed_to_coo_raw(mask)
    if dense.__class__ is _S:
        out = _make_coo(m._ind, _grad_values(_detached(dense), m), m.shape, m._coal)
        return _as_layout(out, layout) if layout is not _coo else out
    out = _with_values(m, _gather(dense, m), "SparseMask")
    return _as_layout(out, layout) if layout is not _coo else out


def _dense_to_coo(x, sparse_dim=None):
    rank = _len(x.shape)
    sd = rank if sparse_dim is None else _int(sparse_dim)
    if sd < 0 or sd > rank:
        raise RuntimeError("sparse_dim must be in [0, %d], but got %d" % (rank, sd))
    xp = _plain(x)
    if sd == 0:
        nonzero = _bool((xp != 0).any()) if xp.numel() else False
        ind = torch.zeros((0, 1 if nonzero else 0), dtype=_int64)
        vals = xp.unsqueeze(0) if nonzero else xp.unsqueeze(0)[:0]
        out = _make_coo(ind, vals, x.shape, True)
    else:
        if xp.dtype.is_complex:
            found = _k.sp_nonzero(torch.view_as_real(xp)._s, _tuple(x.shape) + (2,), sd)
        else:
            found = _k.sp_nonzero(xp._s, _tuple(x.shape), sd)
        n = _k.size(found) // sd
        ind = _dense(found, (sd, n), _int64)
        vals = _gather(xp, _make_coo(ind, xp, x.shape))
        out = _make_coo(ind, vals, x.shape, True)
    if _grad_on(x):
        _record(out, (x,), lambda g: (to_dense(g) if g.__class__ is _S else g,), "ToSparseBackward1")
    return out


def to_sparse(t, sparse_dim=None, layout=None, blocksize=None, dense_dim=None):
    """Tensor.to_sparse(sparseDims) / to_sparse(layout=, blocksize=, dense_dim=)."""
    if _isinstance(sparse_dim, torch._Layout):
        sparse_dim, layout = None, sparse_dim
    if layout is None or layout is _coo:
        if t.__class__ is _S:
            if t.layout is _coo:
                if sparse_dim is not None and sparse_dim != t._ind.shape[0]:
                    raise RuntimeError("to_sparse: conversion from Sparse to Sparse with sparse_dim argument !=self.sparse_dim() is not supported")
                return t
            return _as_coo(t)
        return _dense_to_coo(t, sparse_dim)
    if layout in _COMPRESSED:
        if sparse_dim is not None:
            raise RuntimeError("to_sparse: sparse_dim argument must be None when layout is %r" % (layout,))
        return _to_compressed(t, layout, dense_dim)
    raise _unsupported_layout(repr(layout))


def _to_compressed(t, layout, dense_dim=None):
    if t.__class__ is _S:
        if t.layout is layout:
            return t
        if t.layout is _coo:
            return _as_layout(t, layout)
        return _as_layout(_as_coo(t), layout)
    rank = _len(t.shape)
    dd = 0 if dense_dim is None else _int(dense_dim)
    if rank - dd < 2:
        raise RuntimeError("to_sparse_%s: expected at least 2 sparse dimensions, but got %d" % ("csr" if layout is _csr else "csc", rank - dd))
    if rank - dd > 2:
        raise _unsupported_layout("a batched compressed tensor")
    return _as_layout(_dense_to_coo(t, 2), layout)


def to_sparse_coo(t):
    return to_sparse(t)


def to_sparse_csr(t, dense_dim=None):
    return _to_compressed(t, _csr, dense_dim)


def to_sparse_csc(t, dense_dim=None):
    return _to_compressed(t, _csc, dense_dim)


def _to_blocked(t, blocksize=None, dense_dim=None):
    raise _unsupported_layout("the blocked layouts (sparse_bsr, sparse_bsc)")


# ---- constructors ------------------------------------------------------------------------
class check_sparse_tensor_invariants:
    """Whether sparse constructors validate their indices (PyTorch's
    torch.sparse.check_sparse_tensor_invariants): a context manager and
    decorator, with the global enable/disable/is_enabled switches."""
    _enabled = False

    def __init__(self, enable=True):
        self.state = enable
        self.saved_state = None

    @staticmethod
    def is_enabled():
        return check_sparse_tensor_invariants._enabled

    @staticmethod
    def enable():
        check_sparse_tensor_invariants._enabled = True

    @staticmethod
    def disable():
        check_sparse_tensor_invariants._enabled = False

    def __enter__(self):
        if self.saved_state is not None:
            raise RuntimeError("This context manager instance is already activated. Use a different context manager instance for context nesting.")
        self.saved_state = check_sparse_tensor_invariants._enabled
        check_sparse_tensor_invariants._enabled = self.state

    def __exit__(self, type, value, traceback):
        check_sparse_tensor_invariants._enabled = self.saved_state
        self.saved_state = None

    def __call__(self, mth):
        def test_mth(*args, **kwargs):
            with type(self)(self.state):
                return mth(*args, **kwargs)
        return test_mth


def _index_tensor(x):
    if _isinstance(x, _T):
        if x.__class__ is _S:
            raise TypeError("indices must be a strided tensor")
        return _plain(x)
    t = torch.tensor(x)
    return t


def _values_tensor(x, dtype):
    if _isinstance(x, _T):
        if dtype is not None and x.dtype is not dtype:
            return x.to(dtype)
        return x
    return torch.tensor(x, dtype=dtype)


def _check_leaf_dtype(t):
    if not t.dtype._inexact:
        raise RuntimeError("Only Tensors of floating point and complex dtype can require gradients")


def _sparse_coo_tensor(indices=None, values=None, size=None, dtype=None, device=None, pin_memory=False, requires_grad=False, check_invariants=None, is_coalesced=None):
    """torch.sparse_coo_tensor(indices, values, size=None, *, dtype=None,
    device=None, requires_grad=False, check_invariants=None,
    is_coalesced=None)."""
    torch._check_cpu_device(device)
    if values is None and size is None and indices is not None and not _isinstance(indices, _T):
        # sparse_coo_tensor(size): an empty tensor of that size.
        size, indices = indices, None
    if indices is None:
        if values is not None:
            raise TypeError("sparse_coo_tensor() missing required argument 'values'")
        shape = _Size(_tuple(_int(d) for d in size))
        dt = torch.get_default_dtype() if dtype is None else dtype
        out = _make_coo(torch.zeros((_len(shape), 0), dtype=_int64), torch.zeros(0, dtype=dt), shape, True)
        if requires_grad:
            _check_leaf_dtype(out)
            out.requires_grad = True
        return out
    ind = _index_tensor(indices)
    if ind.dtype is not _int64:
        ind = ind.to(_int64)
    vals = _values_tensor(values, dtype)
    if _len(ind.shape) == 1:
        sd, nnz = ind.shape[0], 1
        ind = ind.reshape(sd, 1)
    elif _len(ind.shape) == 2:
        sd, nnz = ind.shape
    else:
        raise RuntimeError("indices must be sparse_dim x nnz, but got: %s" % (_list(ind.shape),))
    if _len(vals.shape) == 0:
        vals = vals.reshape(1)
    dd = _len(vals.shape) - 1
    if size is not None:
        shape = _Size(_tuple(_int(d) for d in size))
        if _len(shape) != sd + dd:
            raise RuntimeError("'len(size) == sparse_dim + dense_dim' is not satisfied: len(size) = %d, sparse_dim = %d, dense_dim = %d" % (_len(shape), sd, dd))
    if vals.shape[0] != nnz:
        raise RuntimeError("indices and values must have same nnz, but got nnz from indices: %d, nnz from values: %d" % (nnz, vals.shape[0]))
    if size is None:
        if nnz:
            sizes = [_int(v) + 1 for v in torch.amax(ind, 1).tolist()] if sd else []
        else:
            sizes = [0] * sd
        shape = _Size(_tuple(sizes) + _tuple(vals.shape[1:]))
    elif _tuple(shape[sd:]) != _tuple(vals.shape[1:]):
        raise RuntimeError("values has incorrect size, expected %s, got %s" % (_list((nnz,) + _tuple(shape[sd:])), _list(vals.shape)))
    if (check_invariants if check_invariants is not None else check_sparse_tensor_invariants._enabled) and nnz:
        lo = torch.amin(ind, 1).tolist()
        hi = torch.amax(ind, 1).tolist()
        for d in _range(sd):
            if lo[d] < 0:
                raise RuntimeError("found negative index %d for dim %d" % (lo[d], d))
            if hi[d] >= shape[d]:
                raise RuntimeError("size is inconsistent with indices: for dim %d, size is %d but found index %d" % (d, shape[d], hi[d]))
    coalesced = nnz <= 1 if is_coalesced is None else _bool(is_coalesced)
    if requires_grad:
        out = _make_coo(ind, _plain(vals), shape, coalesced)
        _check_leaf_dtype(out)
        out.requires_grad = True
        return out
    out = _make_coo(ind, _plain(vals), shape, coalesced)
    if _grad_on(vals):
        _record(out, (vals,), lambda g: (_grad_values(g, out),), "SparseCooTensorWithDimsAndTensors")
    return out


def _sparse_compressed(layout, cidx, pidx, values, size, dtype, device, requires_grad, check_invariants):
    torch._check_cpu_device(device)
    if layout in _BLOCKED:
        raise _unsupported_layout(repr(layout))
    c = _index_tensor(cidx)
    p = _index_tensor(pidx)
    if c.dtype is not p.dtype and not (c.dtype in (torch.int32, _int64) and p.dtype in (torch.int32, _int64)):
        raise RuntimeError("compressed_indices and plain_indices must have the same dtype, but got %s and %s" % (c.dtype, p.dtype))
    if c.dtype not in (torch.int32, _int64):
        c = c.to(_int64)
    if p.dtype not in (torch.int32, _int64):
        p = p.to(_int64)
    vals = _values_tensor(values, dtype)
    if _len(c.shape) != 1 or _len(p.shape) != 1:
        raise _unsupported_layout("a batched compressed tensor")
    if size is None:
        major = c.shape[0] - 1
        minor = (_int(torch.amax(p).item()) + 1) if p.shape[0] else 0
        shape = (major, minor) if layout is _csr else (minor, major)
        shape = _Size(shape + _tuple(vals.shape[1:]))
    else:
        shape = _Size(_tuple(_int(d) for d in size))
        if _len(shape) != 2 + _len(vals.shape) - 1:
            raise _unsupported_layout("a batched compressed tensor")
    if (check_invariants if check_invariants is not None else check_sparse_tensor_invariants._enabled):
        _check_compressed(layout, c, p, vals, shape)
    out = _make_compressed(layout, c, p, _plain(vals), shape)
    if requires_grad:
        _check_leaf_dtype(out)
        out.requires_grad = True
        return out
    if _grad_on(vals):
        _record(out, (vals,), lambda g: (_regrad_values(g, out),), "SparseCompressedTensor")
    return out


def _regrad_values(g, t):
    """The gradient of a compressed tensor's values."""
    if g.__class__ is _S:
        g = _compressed_to_coo_raw(g) if g.layout is not _coo else g
    return _grad_values(g, _compressed_to_coo_raw(t))


def _check_compressed(layout, c, p, vals, shape):
    rows = shape[0] if layout is _csr else shape[1]
    cname, pname = ("crow_indices", "col_indices") if layout is _csr else ("ccol_indices", "row_indices")
    cl = c.tolist()
    if _len(cl) != rows + 1:
        raise RuntimeError("%s.shape[-1] must be equal to the number of %s + 1 (=%d), but got %d" % (cname, "rows" if layout is _csr else "columns", rows + 1, _len(cl)))
    if cl[0] != 0:
        raise RuntimeError("`%s[..., 0] == 0` is not satisfied." % cname)
    if cl[-1] != p.shape[0]:
        raise RuntimeError("`%s[..., -1] == nnz` is not satisfied." % cname)
    if p.shape[0] != vals.shape[0]:
        raise RuntimeError("%s and values must have the same number of elements, but got %s.numel(): %d, values.numel(): %d" % (pname, pname, p.shape[0], vals.shape[0]))
    pl = p.tolist()
    ncols = shape[1] if layout is _csr else shape[0]
    for r in _range(rows):
        if cl[r + 1] < cl[r]:
            raise RuntimeError("`0 <= %s[..., 1:] - %s[..., :-1] <= %s` is not satisfied." % (cname, cname, "ncols" if layout is _csr else "nrows"))
        seg = pl[cl[r]:cl[r + 1]]
        for i, j in enumerate(seg):
            if j < 0 or j >= ncols:
                raise RuntimeError("`0 <= %s < %s` is not satisfied." % (pname, "ncols" if layout is _csr else "nrows"))
            if i and seg[i - 1] >= j:
                raise RuntimeError("`%s[..., %s[..., i - 1]:%s[..., i]] for all i = 1, ..., nrows are sorted and distinct along the last dimension values` is not satisfied." % (pname, cname, cname))


def _sparse_csr_tensor(crow_indices, col_indices, values, size=None, dtype=None, device=None, pin_memory=False, requires_grad=False, check_invariants=None):
    return _sparse_compressed(_csr, crow_indices, col_indices, values, size, dtype, device, requires_grad, check_invariants)


def _sparse_csc_tensor(ccol_indices, row_indices, values, size=None, dtype=None, device=None, pin_memory=False, requires_grad=False, check_invariants=None):
    return _sparse_compressed(_csc, ccol_indices, row_indices, values, size, dtype, device, requires_grad, check_invariants)


def _sparse_bsr_tensor(crow_indices, col_indices, values, size=None, dtype=None, device=None, pin_memory=False, requires_grad=False, check_invariants=None):
    raise _unsupported_layout("torch.sparse_bsr")


def _sparse_bsc_tensor(ccol_indices, row_indices, values, size=None, dtype=None, device=None, pin_memory=False, requires_grad=False, check_invariants=None):
    raise _unsupported_layout("torch.sparse_bsc")


def _sparse_compressed_tensor(compressed_indices, plain_indices, values, size=None, dtype=None, layout=None, device=None, pin_memory=False, requires_grad=False, check_invariants=None):
    if layout is None:
        raise RuntimeError("sparse_compressed_tensor: layout must be specified")
    return _sparse_compressed(layout, compressed_indices, plain_indices, values, size, dtype, device, requires_grad, check_invariants)


# ---- printing ----------------------------------------------------------------------------
def _part(name, t, indent):
    """`name=tensor(...)` for one component, as PyTorch's _str_intern."""
    prefix = name + "=tensor("
    t = _plain(t)
    if t.numel() == 0:
        body = "[], size=" + str(_tuple(t.shape))
    else:
        body = torch._tensor_str(t, indent + _len(prefix))
    return prefix + body + ")"


def _repr(s):
    prefix = "tensor("
    indent = _len(prefix)
    suffixes = ["size=" + str(_tuple(s.shape)), "nnz=" + str(_nnz_of(s))]
    dt = s.dtype
    if not (dt in (torch.get_default_dtype(), _int64, torch.bool) or dt is torch._default_complex()):
        suffixes.append("dtype=" + repr(dt))
    if s.layout is _coo:
        body = _part("indices", s._ind, indent) + ",\n" + " " * indent + _part("values", s._val, indent)
    else:
        cname, pname = ("crow_indices", "col_indices") if s.layout is _csr else ("ccol_indices", "row_indices")
        body = _part(cname, s._crow, indent) + ",\n" + " " * indent + _part(pname, s._col, indent) + ",\n" + " " * indent + _part("values", s._val, indent)
    suffixes.append("layout=" + repr(s.layout))
    if s._node is not None:
        suffixes.append("grad_fn=<%s>" % torch._grad_fn_name(s._node.name))
    elif s.requires_grad:
        suffixes.append("requires_grad=True")
    return torch._add_suffixes(prefix + body, suffixes, indent, s.layout is _coo)


# ---- checkpoints -------------------------------------------------------------------------
def _reduce_args(t):
    if t.layout is _coo:
        return (_plain(t._ind), _plain(t._val), _tuple(t.shape), _bool(t._coal))
    return (_plain(t._crow), _plain(t._col), _plain(t._val), _tuple(t.shape))


def _rebuild(layout, data):
    if layout is _coo:
        if _len(data) == 3:
            indices, values, size = data
            coalesced = None
        else:
            indices, values, size, coalesced = data[0], data[1], data[2], data[3]
        return _sparse_coo_tensor(indices, values, size, check_invariants=False, is_coalesced=coalesced)
    if layout in _COMPRESSED:
        cidx, pidx, values, size = data[0], data[1], data[2], data[3]
        return _sparse_compressed(layout, cidx, pidx, values, size, None, None, False, False)
    raise _unsupported_layout(repr(layout))


# ---- elementwise arithmetic --------------------------------------------------------------
_ATEN = {"add": "aten::add.Tensor", "sub": "aten::sub.Tensor", "mul": "aten::mul.Tensor", "div": "aten::div.Tensor", "pow": "aten::pow.Tensor_Scalar",
         "floordiv": "aten::floor_divide", "mod": "aten::remainder.Tensor", "max": "aten::maximum", "min": "aten::minimum",
         "eq": "aten::eq.Tensor", "ne": "aten::ne.Tensor", "lt": "aten::lt.Tensor", "le": "aten::le.Tensor", "gt": "aten::gt.Tensor",
         "ge": "aten::ge.Tensor", "atan2": "aten::atan2", "and": "aten::bitwise_and.Tensor", "or": "aten::bitwise_or.Tensor",
         "xor": "aten::bitwise_xor.Tensor"}


def _is_number(v):
    return _isinstance(v, (_int, _float, _bool)) or type(v) is _b.complex


def _binary(op, a, b):
    """The elementwise binary ops torch.py hands over when an operand is
    sparse: add/sub (b already scaled by alpha), mul, div and pow."""
    sa = a.__class__ is _S
    sb = b.__class__ is _S
    if op == "add" or op == "sub":
        alpha = 1 if op == "add" else -1
        name = "Add" if op == "add" else "Sub"
        if sa and sb:
            return _add_ss(a, b, alpha, name)
        if sa:
            raise RuntimeError("add(sparse, dense) is not supported. Use add(dense, sparse) instead.")
        return _add_ds(a, b, alpha, name)
    if op == "mul":
        if sa and sb:
            return _mul_ss(a, b)
        return _mul_sd(a, b) if sa else _mul_sd(b, a)
    if op == "div" and sa:
        if sb or (_isinstance(b, _T) and b.shape):
            raise RuntimeError("Sparse division requires a scalar or zero-dim dense tensor divisor (got shape %s for divisor)" % (_list(b.shape),))
        return _div_s(a, b)
    if op == "pow" and sa and not sb and not _isinstance(b, _T):
        return _pow_s(a, b)
    t = a if sa else b
    if op == "div":
        raise _no_kernel("aten::reciprocal", t)
    raise _no_kernel(_ATEN.get(op, "aten::" + op), t)


def _same_layout(a, b, what):
    if a.layout is not b.layout:
        raise RuntimeError("%s: expected 'self' and 'other' to have the same layout, but got %s and %s" % (what, _layout_name(a), _layout_name(b)))


def _values_as(t, dt):
    v = _plain(t._val)
    return v if v.dtype is dt else v.to(dt)


def _add_ss(a, b, alpha, name):
    _same_layout(a, b, "add")
    layout = a.layout
    if layout is not _coo:
        return _as_layout(_add_ss(_as_coo(a), _as_coo(b), alpha, name), layout)
    if _tuple(a.shape) != _tuple(b.shape):
        raise RuntimeError("add: expected sizes of 'self' and 'other' to match, but %s != %s" % (_list(a.shape), _list(b.shape)))
    sd = a._ind.shape[0]
    if b._ind.shape[0] != sd:
        raise RuntimeError("add: expected 'self' and 'other' to have same density, but 'self' has %d sparse dimensions while 'other' has %d sparse dimensions" % (sd, b._ind.shape[0]))
    dt = torch._promote_types(a.dtype, b.dtype)
    av, bv = _values_as(a, dt), _values_as(b, dt)
    an, bn = a._ind.shape[1], b._ind.shape[1]
    if bn == 0:
        out = _make_coo(_plain(a._ind).clone(), av.clone(), a.shape, a._coal)
    elif an == 0:
        out = _make_coo(_plain(b._ind).clone(), bv.clone() if alpha == 1 else torch.mul(bv, alpha), a.shape, b._coal)
    elif a._vnc or b._vnc:
        # PyTorch's add_out_sparse_non_contiguous: the two lists joined,
        # coalesced once they hold more entries than the tensor has elements.
        out = _make_coo(torch.cat([_plain(a._ind), _plain(b._ind)], 1), torch.cat([av, bv if alpha == 1 else torch.mul(bv, alpha)], 0), a.shape, False)
        if an + bn > _prod(a.shape):
            out = _coalesce_raw(out)
    else:
        sizes = _tuple(a.shape[:sd])
        take, vals = _k.sp_merge(_keys(a._ind, sizes)._s, av._s, _keys(b._ind, sizes)._s, bv._s, _block(a), alpha)
        n = _k.size(take)
        ind = torch.index_select(torch.cat([_plain(a._ind), _plain(b._ind)], 1), 1, _dense(take, (n,), _int64))
        out = _make_coo(ind, _dense(vals, (n,) + _tuple(av.shape[1:]), dt), a.shape, a._coal and b._coal)
    if _grad_on(a, b):
        _record(out, (a, b), lambda g: (g, g if alpha == 1 else torch.neg(g)), name)
    return out


def _add_ds(d, s, alpha, name):
    """dense + alpha * sparse: a copy of the dense tensor with each nonzero
    added in turn (PyTorch's add_out_dense_sparse_cpu)."""
    if not _isinstance(d, _T):
        d = torch.tensor(d)
    if s.layout in _BLOCKED:
        raise _unsupported_layout(repr(s.layout))
    c = s if s.layout is _coo else _compressed_to_coo_raw(s)
    if _tuple(d.shape) != _tuple(s.shape):
        raise RuntimeError("add: expected 'self' and 'other' to have same size, but self has size %s while other has size %s (FYI: dense-sparse addition does not currently support broadcasting)" % (_list(d.shape), _list(s.shape)))
    dt = torch._promote_types(d.dtype, s.dtype)
    base = _plain(d)
    st = _k.copy(base._s) if base.dtype is dt else _k.astype(base._s, dt.name)
    nnz = c._ind.shape[1]
    if nnz:
        sd = c._ind.shape[0]
        sizes = _tuple(s.shape[:sd])
        vals = _values_as(c, dt)
        if alpha != 1:
            vals = torch.mul(vals, alpha)
        _k.sp_scatter_add(st, _keys(c._ind, sizes)._s, vals._s, _prod(s.shape[sd:]))
    out = _dense(st, d.shape, dt)
    if _grad_on(d, s):
        _record(out, (d, s), lambda g: (g, g if alpha == 1 else torch.neg(g)), name)
    return out


def _mul_ss(a, b):
    _same_layout(a, b, "mul")
    layout = a.layout
    if layout is not _coo:
        return _as_layout(_mul_ss(_as_coo(a), _as_coo(b)), layout)
    sd = a._ind.shape[0]
    if _tuple(a.shape) != _tuple(b.shape) or b._ind.shape[0] != sd:
        raise RuntimeError("sparse_binary_op_intersection_cpu(): expects sparse inputs with equal dimensionality, number of sparse dimensions, and shape of sparse dimensions")
    dt = torch._promote_types(a.dtype, b.dtype)
    # PyTorch's intersection kernel: the "source" operand is coalesced and
    # looked up; the result has the other ("probe") operand's entries in
    # its order. With both coalesced only the shared entries remain (and
    # the result is coalesced); otherwise every probe entry stays, zero
    # where the source has none.
    both = a._coal and b._coal
    if a._coal != b._coal:
        source = a if a._coal else b
    else:
        source = a if a._ind.shape[1] >= b._ind.shape[1] else b
    probe = b if source is a else a
    src = _coalesce_raw(source)
    sizes = _tuple(a.shape[:sd])
    n = probe._ind.shape[1]
    pv = _values_as(probe, dt)
    tail = _tuple(pv.shape[1:])
    if src._ind.shape[1] == 0 or n == 0:
        pos = torch.full((n,), -1, dtype=_int64)
    else:
        pos = _dense(_k.sp_search(_keys(src._ind, sizes)._s, _keys(probe._ind, sizes)._s), (n,), _int64)
    found = pos >= 0
    sv = _values_as(src, dt)
    picked = torch.index_select(sv, 0, torch.clamp(pos, 0)) if sv.shape[0] else torch.zeros((n,) + tail, dtype=dt)
    prod = torch.mul(picked, pv) if probe is b else torch.mul(pv, picked)
    if both:
        keep = _compact(found)
        out = _make_coo(torch.index_select(_plain(probe._ind), 1, keep), torch.index_select(prod, 0, keep), a.shape, True)
    else:
        mask = found if not tail else found.reshape((n,) + (1,) * _len(tail))
        out = _make_coo(_plain(probe._ind), torch.where(mask, prod, torch.zeros((), dtype=dt)), a.shape, False)
    if _grad_on(a, b):
        ad, bd = _detached(a), _detached(b)
        _record(out, (a, b), lambda g: (torch.mul(g, bd) if a.requires_grad else None, torch.mul(g, ad) if b.requires_grad else None), "Mul")
    return out


def _detached(s):
    """s without its history (shares indices and values)."""
    if s.__class__ is not _S:
        return _plain(s)
    if s.layout is _coo:
        return _make_coo(s._ind, s._val, s.shape, s._coal, s._vnc)
    return _make_compressed(s.layout, s._crow, s._col, s._val, s.shape)


def _mul_sd(s, o):
    """sparse * (number, 0-d tensor or dense tensor): s's entries, scaled."""
    layout = s.layout
    if layout in _BLOCKED:
        raise _unsupported_layout(repr(layout))
    if layout is not _coo:
        return _as_layout(_mul_sd(_as_coo(s), o), layout)
    vals = _plain(s._val)
    if _is_number(o) or (_isinstance(o, _T) and not o.shape):
        k = o if not _isinstance(o, _T) else _plain(o)
        out = _make_coo(s._ind, torch.mul(vals, k), s.shape, s._coal)
        if _grad_on(s, o):
            sd = _detached(s)

            def backward(g):
                gs = torch.mul(g, k) if s.requires_grad else None
                go = None
                if _isinstance(o, _T) and o.requires_grad:
                    go = torch.sum(torch.mul(_grad_values(g, sd), vals))
                return (gs, go)
            _record(out, (s, o if _isinstance(o, _T) else None), backward, "Mul")
        return out
    if not _isinstance(o, _T):
        o = torch.tensor(o)
    os_, ss_ = _list(o.shape), _list(s.shape)
    n = _b.max(_len(os_), _len(ss_))
    os_, ss_ = [1] * (n - _len(os_)) + os_, [1] * (n - _len(ss_)) + ss_
    for i in _range(n):
        if os_[i] != ss_[i] and os_[i] != 1 and ss_[i] != 1:
            raise RuntimeError("The size of tensor a (%d) must match the size of tensor b (%d) at non-singleton dimension %d" % (os_[i], ss_[i], i))
    if _tuple(torch.broadcast_shapes(o.shape, s.shape)) != _tuple(s.shape):
        raise NotImplementedError("multiplying a sparse tensor by a dense tensor of a larger broadcast shape is not supported on Zipp")
    oe = _plain(o) if _tuple(o.shape) == _tuple(s.shape) else _plain(o).expand(*s.shape)
    out = _make_coo(s._ind, torch.mul(vals, _gather(oe, s)), s.shape, s._coal)
    if _grad_on(s, o):
        sdet = _detached(s)
        op = _plain(o)

        def backward(g):
            gs = torch.mul(g, op) if s.requires_grad else None
            go = None
            if o.requires_grad:
                go = torch.mul(g, sdet)
                if _tuple(o.shape) != _tuple(s.shape):
                    go = torch._unbroadcast(to_dense(go), o.shape)
            return (gs, go)
        _record(out, (s, o), backward, "Mul")
    return out


def _div_s(s, o):
    layout = s.layout
    if layout is not _coo:
        raise RuntimeError("unsupported tensor layout: %s" % _layout_name(s))
    k = o if not _isinstance(o, _T) else _plain(o)
    vals = _plain(s._val)
    out = _make_coo(s._ind, torch.div(vals, k), s.shape, s._coal)
    if _grad_on(s, o):
        sd = _detached(s)

        def backward(g):
            gs = torch.div(g, k) if s.requires_grad else None
            go = None
            if _isinstance(o, _T) and o.requires_grad:
                go = torch.neg(torch.sum(torch.mul(_grad_values(g, sd), vals))) / (k * k)
            return (gs, go)
        _record(out, (s, o if _isinstance(o, _T) else None), backward, "Div")
    return out


def _pow_s(s, e):
    layout = s.layout
    if layout is not _coo:
        return _as_layout(_pow_s(_as_coo(s), e), layout)
    return _valuewise(coalesce(s), lambda v: torch.pow(v, e), "Pow")


def _valuewise(c, f, name):
    """A COO tensor with c's indices and f(c's values); its gradient reaches
    c through f's own backward, at c's entries."""
    vals = _plain(c._val)
    if not _grad_on(c):
        return _make_coo(c._ind, f(vals), c.shape, c._coal)
    v = _T(vals._s, vals.shape, vals.dtype)
    v.requires_grad = True
    with torch.enable_grad():
        ov = f(v)
    out = _make_coo(c._ind, _plain(ov), c.shape, c._coal)

    def backward(g):
        gv = _grad_values(g, out)
        dv = torch.autograd.grad(ov, v, gv, retain_graph=True)[0]
        return (_make_coo(c._ind, dv, c.shape, c._coal),)
    return _record(out, (c,), backward, name)


# PyTorch's sparse unary ops: they keep zeros zero, so they act on the
# values (coalesced first, except the linear `neg`).
_ZERO_PRESERVING = frozenset(["neg", "abs", "sin", "sinh", "tan", "tanh", "asin", "asinh", "atan", "atanh", "sqrt", "log1p", "expm1",
                              "floor", "ceil", "trunc", "round", "frac", "sign", "sgn", "relu", "erf", "erfinv", "isnan", "isinf",
                              "isposinf", "isneginf", "signbit", "square", "deg2rad", "rad2deg", "nan_to_num"])


def _unary(op, a, name, backward, p1, p2, saves):
    if op not in _ZERO_PRESERVING:
        raise _no_kernel("aten::" + op, a)
    layout = a.layout
    if layout in _BLOCKED:
        raise _unsupported_layout(repr(layout))
    if layout is not _coo:
        return _as_layout(_unary(op, _as_coo(a), name, backward, p1, p2, saves), layout)

    def f(v):
        return torch._unary(op, v, name, backward, p1, p2, saves)
    if op == "neg":
        out = _make_coo(a._ind, f(_plain(a._val)), a.shape, a._coal)
        if _grad_on(a):
            _record(out, (a,), lambda g: (torch.neg(g),), "Neg")
        return out
    return _valuewise(coalesce(a), f, name)


def _unary_nograd(op, a, p1, p2):
    if op not in _ZERO_PRESERVING:
        raise _no_kernel("aten::" + op, a)
    layout = a.layout
    if layout is not _coo:
        return _regrad(_unary_nograd(op, _compressed_to_coo_raw(a), p1, p2), layout)
    c = _coalesce_raw(a)
    return _make_coo(c._ind, torch._unary_nograd(op, _plain(c._val), p1, p2), c.shape, c._coal)


# ---- reductions --------------------------------------------------------------------------
def _dims_list(dim, rank):
    if _isinstance(dim, (_list, _tuple)):
        dims = [torch._norm_dim(d, rank) for d in dim]
    else:
        dims = [torch._norm_dim(dim, rank)]
    out = []
    for d in dims:
        if d in out:
            raise RuntimeError("dim %d appears multiple times in the list of dims" % d)
        out.append(d)
    return sorted(out)


def sum(input, dim=None, dtype=None):
    """torch.sparse.sum: the sum over all elements (a dense 0-d tensor), or
    over `dim`, sparse while sparse dims remain."""
    if input.__class__ is not _S:
        return torch.sum(input, dtype=dtype) if dim is None else torch.sum(input, dim, dtype=dtype)
    s = input
    if s.layout is not _coo:
        if dim is not None:
            raise RuntimeError("reduction operations on CSR tensors with keepdim=False is unsupported")
        s = _as_coo(s)
    if dtype is not None:
        s = _to(s, dtype)
    c = _coalesce_raw(s)
    vals = _plain(c._val)
    sd = c._ind.shape[0]
    if dim is None:
        out = torch.sum(vals)
        if _grad_on(s):
            _record(out, (s,), lambda g: (_make_coo(c._ind, g.expand(*vals.shape), s.shape, True, True),), "Sum")
        return out
    dims = _dims_list(dim, _len(s.shape))
    sdims = [d for d in dims if d < sd]
    ddims = [d - sd + 1 for d in dims if d >= sd]
    v = torch.sum(vals, ddims) if ddims else vals
    keep = [d for d in _range(sd) if d not in sdims]
    rest = [s.shape[d] for d in _range(sd, _len(s.shape)) if d not in dims]
    if not keep:
        out = torch.sum(v, 0)
    elif not sdims:
        out = _make_coo(c._ind, v, [s.shape[d] for d in keep] + rest, True)
    else:
        proj = torch.index_select(c._ind, 0, torch.tensor(keep, dtype=_int64))
        out = _coalesce_raw(_make_coo(proj, v, [s.shape[d] for d in keep] + rest, False))
    if _grad_on(s):
        def backward(g):
            if not keep:
                gv = g.unsqueeze(0).expand(*((vals.shape[0],) + _tuple(g.shape)))
            else:
                proj_ind = c._ind if not sdims else torch.index_select(c._ind, 0, torch.tensor(keep, dtype=_int64))
                gv = _grad_values(g, _make_coo(proj_ind, v, out.shape))
            for d in ddims:
                gv = gv.unsqueeze(d)
            if ddims:
                gv = gv.expand(*vals.shape)
            return (_make_coo(c._ind, gv, s.shape, True, (not keep) or _bool(ddims)),)
        _record(out, (s,), backward, "SparseSum")
    return out


def _tensor_sum(a, dim, keepdim, dtype):
    """torch.sum / Tensor.sum of a sparse tensor."""
    if dim is None or (_isinstance(dim, (_list, _tuple)) and not dim):
        return sum(a, None, dtype)
    if a.layout is not _coo:
        if not keepdim:
            raise RuntimeError("reduction operations on CSR tensors with keepdim=False is unsupported")
        raise _unsupported_layout("a keepdim reduction of a compressed tensor")
    out = sum(a, dim, dtype)
    if not keepdim:
        return out
    dims = _dims_list(dim, _len(a.shape))
    sd = a._ind.shape[0]
    if out.__class__ is not _S:
        raise _unsupported_layout("keepdim=True over every sparse dimension")
    rows, k = [], 0
    for d in _range(sd):
        if d in dims:
            rows.append(torch.zeros(out._ind.shape[1], dtype=_int64))
        else:
            rows.append(out._ind[k])
            k += 1
    vals = out._val
    for d in dims:
        if d >= sd:
            vals = vals.unsqueeze(d - sd + 1)
    shape = [1 if d in dims else a.shape[d] for d in _range(_len(a.shape))]
    res = _make_coo(torch.stack(rows) if rows else out._ind, _plain(vals), shape, False)
    if _grad_on(out):
        res_shape = out.shape
        _record(res, (out,), lambda g: (_make_coo(out._ind, _grad_values(g, res).reshape((out._ind.shape[1],) + _tuple(out._val.shape[1:])), res_shape, out._coal),), "View")
    return res


def softmax(input, dim, dtype=None):
    """torch.sparse.softmax: the softmax of the nonzeros along `dim` (an
    unspecified element counts as -inf, not 0)."""
    return _softmax(input, dim, dtype, False)


def log_softmax(input, dim, dtype=None):
    return _softmax(input, dim, dtype, True)


def _softmax(s, dim, dtype, log):
    if s.__class__ is not _S:
        return torch.log_softmax(s, dim, dtype) if log else torch.softmax(s, dim, dtype)
    if s.layout is not _coo:
        raise _no_kernel("aten::_sparse_log_softmax" if log else "aten::_sparse_softmax", s)
    if dtype is not None:
        s = _to(s, dtype)
    if not s.dtype.is_floating_point:
        raise NotImplementedError("\"%s\" not implemented for '%s'" % ("log_softmax" if log else "softmax", torch._CAST_NAME[s.dtype.name]))
    c = _coalesce_raw(s)
    sd = c._ind.shape[0]
    rank = _len(s.shape)
    d = torch._norm_dim(dim, rank)
    vals = _plain(c._val)
    block = _block(c)
    pools = None
    if d >= sd:
        dd = d - sd + 1
        ov = torch.log_softmax(vals, dd) if log else torch.softmax(vals, dd)
    else:
        others = [r for r in _range(sd) if r != d]
        pind = torch.index_select(c._ind, 0, torch.tensor(others, dtype=_int64)) if others else torch.zeros((0, c._ind.shape[1]), dtype=_int64)
        pools = _keys(pind, [s.shape[r] for r in others])
        ov = _dense(_k.sp_pool(1 if log else 0, pools._s, vals._s, block), vals.shape, vals.dtype)
    out = _make_coo(c._ind, ov, s.shape, True)
    if _grad_on(s):
        def pool_sum(x):
            if pools is None:
                return torch.sum(x, d - sd + 1, True)
            return _dense(_k.sp_pool(2, pools._s, _plain(x)._s, block), x.shape, x.dtype)

        def backward(g):
            gv = _grad_values(g, out)
            if log:
                gi = gv - torch.exp(ov) * pool_sum(gv)
            else:
                gi = ov * (gv - pool_sum(gv * ov))
            return (_make_coo(c._ind, gi, s.shape, True),)
        _record(out, (s,), backward, "SparseLogSoftmax" if log else "SparseSoftmax")
    return out


def _norm(s, p="fro", dim=None, keepdim=False, dtype=None):
    if dim is not None:
        raise _no_kernel("aten::linalg_vector_norm", s)
    c = coalesce(_as_coo(s))
    v = _with_values_grad(c)
    return torch.norm(v, 2 if p == "fro" else p, dtype=dtype)


def _with_values_grad(c):
    """c's values, differentiable back to c (Tensor.values())."""
    v = c._val
    if _grad_on(c):
        v = _T(v._s, v.shape, v.dtype)
        _record(v, (c,), lambda g: (_make_coo(c._ind, g, c.shape, c._coal),), "Values")
    return v


# ---- products ----------------------------------------------------------------------------
def _coo_parts(s):
    """(COO form, rows, cols, values) of a 2-D sparse matrix, no history."""
    c = s if s.layout is _coo else _compressed_to_coo_raw(s)
    ind = _plain(c._ind)
    return c, ind[0], ind[1], _plain(c._val)


def _spmm_raw(rows, cols, vals, m, d):
    """The [m, n] product of the sparse matrix (rows, cols, vals) and the
    dense [k, n] matrix d (complex as four real products)."""
    if vals.dtype.is_complex:
        vr, vi, dr, di = torch.real(vals), torch.imag(vals), torch.real(d), torch.imag(d)
        re = _spmm_raw(rows, cols, vr, m, dr) - _spmm_raw(rows, cols, vi, m, di)
        im = _spmm_raw(rows, cols, vr, m, di) + _spmm_raw(rows, cols, vi, m, dr)
        return torch.complex(re, im)
    n = d.shape[1]
    return _dense(_k.sp_spmm(rows._s, cols._s, vals._s, m, _plain(d)._s, n), (m, n), vals.dtype)


def _check_matrix(s):
    if s.layout in _BLOCKED:
        raise _unsupported_layout(repr(s.layout))
    if s.layout is _coo and (s._ind.shape[0] != 2 or _len(s.shape) != 2):
        raise RuntimeError("addmm: matrices expected, got %dD tensor" % s._ind.shape[0])
    if _len(s.shape) != 2:
        raise _unsupported_layout("a batched compressed tensor")


def _check_dense_operand(s, d, k):
    if _len(d.shape) != 2:
        raise RuntimeError("addmm: matrices expected, got %dD tensor" % _len(d.shape))
    if d.shape[0] != k:
        raise RuntimeError("addmm: Argument #3 (dense): Expected dim 0 size %d, got %d" % (k, d.shape[0]))
    if s.dtype is not d.dtype:
        raise RuntimeError("expected scalar type %s but found %s" % (torch._CAST_NAME[s.dtype.name], torch._CAST_NAME[d.dtype.name]))
    if s.dtype is torch.bool:
        raise NotImplementedError("\"addmm_sparse_dense\" not implemented for 'Bool'")


def _conj(t):
    return torch.conj(t) if t.dtype.is_complex else t


def _spmm(s, d, name, sparse_grad, beta=0, mat=None, alpha=1):
    """beta * mat + alpha * (s @ d) for a sparse s and a dense 2-D d."""
    _check_matrix(s)
    if not _isinstance(d, _T):
        d = torch.tensor(d)
    m, k = s.shape[0], s.shape[1]
    _check_dense_operand(s, d, k)
    c, rows, cols, vals = _coo_parts(s)
    dp = _plain(d)
    out = _spmm_raw(rows, cols, vals, m, dp)
    if alpha != 1:
        out = torch.mul(out, alpha)
    if mat is not None and beta != 0:
        mp = _plain(mat)
        out = torch.add(out, mp if beta == 1 else torch.mul(mp, beta))
    if _grad_on(s, d, mat):
        layout = s.layout

        def backward(g):
            gm = gs = gd = None
            if mat is not None and mat.requires_grad:
                gm = torch._unbroadcast(g if beta == 1 else torch.mul(g, beta), mat.shape)
            if s.requires_grad:
                if sparse_grad:
                    cc = _coalesce_raw(c)
                    ci = _plain(cc._ind)
                    gv = torch.sum(torch.mul(torch.index_select(g, 0, ci[0]), _conj(torch.index_select(dp, 0, ci[1]))), 1)
                    if alpha != 1:
                        gv = torch.mul(gv, alpha)
                    gs = _regrad(_make_coo(ci, gv, s.shape, True), layout)
                else:
                    gs = torch.matmul(g, _conj(dp).t())
                    if alpha != 1:
                        gs = torch.mul(gs, alpha)
            if d.requires_grad:
                gd = _spmm_raw(cols, rows, _conj(vals), k, g)
                if alpha != 1:
                    gd = torch.mul(gd, alpha)
            return (gm, gs, gd)
        _record(out, (mat, s, d), backward, name)
    return out


def _dsmm(d, s, name, sparse_grad):
    """d @ s for a dense 2-D d and a sparse s, as (s^T @ d^T)^T."""
    _check_matrix(s)
    if not _isinstance(d, _T):
        d = torch.tensor(d)
    k, n = s.shape[0], s.shape[1]
    if _len(d.shape) != 2:
        raise RuntimeError("addmm: matrices expected, got %dD tensor" % _len(d.shape))
    if d.shape[1] != k:
        raise RuntimeError("mat1 and mat2 shapes cannot be multiplied (%dx%d and %dx%d)" % (d.shape[0], d.shape[1], k, n))
    if s.dtype is not d.dtype:
        raise RuntimeError("expected scalar type %s but found %s" % (torch._CAST_NAME[s.dtype.name], torch._CAST_NAME[d.dtype.name]))
    c, rows, cols, vals = _coo_parts(s)
    dp = _plain(d)
    out = _spmm_raw(cols, rows, vals, n, dp.t()).t()
    if _grad_on(d, s):
        layout = s.layout

        def backward(g):
            gd = gs = None
            if d.requires_grad:
                gd = _spmm_raw(rows, cols, _conj(vals), k, g.t()).t()
            if s.requires_grad:
                if sparse_grad:
                    cc = _coalesce_raw(c)
                    ci = _plain(cc._ind)
                    dt_, gt_ = _conj(dp).t(), g.t()
                    gv = torch.sum(torch.mul(torch.index_select(dt_, 0, ci[0]), torch.index_select(gt_, 0, ci[1])), 1)
                    gs = _regrad(_make_coo(ci, gv, s.shape, True), layout)
                else:
                    gs = torch.matmul(_conj(dp).t(), g)
            return (gd, gs)
        _record(out, (d, s), backward, name)
    return out


def _ssmm_raw(a, b, dt, discovery=False):
    """The coalesced COO product of two 2-D sparse matrices (no history).
    `discovery`: each row's entries in the order the product first reaches
    their columns instead (PyTorch's compressed matmul), not flagged."""
    ac = _coalesce_raw(a if a.layout is _coo else _compressed_to_coo_raw(a))
    bc = _coalesce_raw(b if b.layout is _coo else _compressed_to_coo_raw(b))
    m, k, n = a.shape[0], a.shape[1], b.shape[1]
    ai, bi = _plain(ac._ind), _plain(bc._ind)
    av, bv = _values_as(ac, dt), _values_as(bc, dt)
    empty = _make_coo(torch.zeros((2, 0), dtype=_int64), torch.zeros(0, dtype=dt), (m, n), True)
    if ai.shape[1] == 0 or bi.shape[1] == 0:
        return empty
    # Each nonzero (i, j) of a meets the nonzeros of b's row j.
    bcrow = _k.sp_compress(bi[0]._s, k)
    ar, acol, bcol = ai[0]._s, ai[1]._s, bi[1]._s

    def products(x, y):
        return _k.sp_spgemm(ar, acol, x._s, bcrow, bcol, y._s)
    if dt.is_complex:
        xr, xi, yr, yi = torch.real(av), torch.imag(av), torch.real(bv), torch.imag(bv)
        rr, ii, ri, ir = products(xr, yr), products(xi, yi), products(xr, yi), products(xi, yr)
        total = _k.size(rr[0])
        part = lambda q: _dense(q[2], (total,), xr.dtype)
        vals = torch.complex(part(rr) - part(ii), part(ri) + part(ir))
        rows, cols = rr[0], rr[1]
    else:
        rows, cols, vs = products(av, bv)
        total = _k.size(rows)
        vals = _dense(vs, (total,), dt)
    if total == 0:
        return empty
    ind = torch.stack([_dense(rows, (total,), _int64), _dense(cols, (total,), _int64)])
    if not discovery:
        return _coalesce_raw(_make_coo(ind, vals, (m, n), False))
    keys = _k.sp_keys(ind._s, total, (m, n))
    first, summed = _k.sp_coalesce(keys, vals._s, 1)
    g = _k.size(first)
    first = _dense(first, (g,), _int64)
    order = torch.argsort(first, stable=True)
    return _make_coo(torch.index_select(torch.index_select(ind, 1, first), 1, order), torch.index_select(_dense(summed, (g,), dt), 0, order), (m, n), False)


def _transposed_raw(s):
    c = s if s.layout is _coo else _compressed_to_coo_raw(s)
    ind = _plain(c._ind)
    return _make_coo(torch.stack([ind[1], ind[0]]), _plain(c._val), (s.shape[1], s.shape[0]), False)


def _zero_outside(g, like):
    """g (coalesced COO) with its values zeroed where `like` has no entry."""
    lc = _coalesce_raw(like if like.layout is _coo else _compressed_to_coo_raw(like))
    n = g._ind.shape[1]
    if n == 0:
        return g
    if lc._ind.shape[1] == 0:
        return _make_coo(g._ind, torch.zeros(g._val.shape, dtype=g.dtype), g.shape, g._coal)
    sizes = _tuple(g.shape[:2])
    pos = _dense(_k.sp_search(_keys(lc._ind, sizes)._s, _keys(g._ind, sizes)._s), (n,), _int64)
    return _make_coo(g._ind, torch.where(pos >= 0, g._val, torch.zeros((), dtype=g.dtype)), g.shape, g._coal)


def _ssmm(a, b, name="Mm", masked=False):
    """The sparse product of two sparse matrices, coalesced (in a's
    layout). Gradients are sparse products too: torch.mm's in full,
    torch.sparse.mm's zeroed where the input has no entry."""
    _check_matrix(a)
    _check_matrix(b)
    if a.shape[1] != b.shape[0]:
        raise RuntimeError("mat1 and mat2 shapes cannot be multiplied (%dx%d and %dx%d)" % (a.shape[0], a.shape[1], b.shape[0], b.shape[1]))
    dt = torch._promote_types(a.dtype, b.dtype)
    discovery = a.layout is not _coo
    out = _ssmm_raw(a, b, dt, discovery)
    if _grad_on(a, b):
        ad, bd = _detached(a), _detached(b)

        def backward(g):
            gs = g if g.__class__ is _S else _dense_to_coo(g)
            ga = gb = None
            if a.requires_grad:
                ga = _ssmm_raw(gs, _transposed_raw(bd), gs.dtype)
                if masked:
                    ga = _zero_outside(ga, ad)
                ga = _regrad(ga, a.layout)
            if b.requires_grad:
                gb = _ssmm_raw(_transposed_raw(ad), gs, gs.dtype)
                if masked:
                    gb = _zero_outside(gb, bd)
                gb = _regrad(gb, b.layout)
            return (ga, gb)
        _record(out, (a, b), backward, name)
    if not discovery:
        return out
    res = _coo_to_compressed_raw(out, _csr, _int64, True)
    if a.layout is not _csr:
        res = _coo_to_compressed_raw(_compressed_to_coo_raw(res), a.layout)
    if out._node is not None:
        _record(res, (out,), lambda g: (_regrad(g, _coo),), "ToSparseCsr")
    return res


def _mm(a, b):
    """torch.mm with a sparse operand (dense gradients for a sparse input)."""
    sa, sb = a.__class__ is _S, b.__class__ is _S
    if sa and sb:
        return _ssmm(a, b)
    if sa:
        return _spmm(a, b, "Mm", False)
    return _dsmm(a, b, "Mm", False)


def _matmul(a, b):
    """torch.matmul / @ with a sparse operand."""
    sa, sb = a.__class__ is _S, b.__class__ is _S
    if sa and sb:
        return _ssmm(a, b)
    if sa:
        if not _isinstance(b, _T):
            b = torch.tensor(b)
        if _len(b.shape) == 1:
            return torch.squeeze(_spmm(a, torch.unsqueeze(b, 1), "Mm", False), 1)
        return _spmm(a, b, "Mm", False)
    if _len(a.shape) == 1:
        return torch.squeeze(_dsmm(torch.unsqueeze(a, 0), b, "Mm", False), 0)
    return _dsmm(a, b, "Mm", False)


def mm(mat1, mat2, reduce="sum"):
    """torch.sparse.mm: sparse @ dense (sparse gradient for the sparse
    input, masked to its nonzeros), dense @ sparse, or sparse @ sparse."""
    if reduce != "sum":
        raise NotImplementedError("torch.sparse.mm: reduce=%r is not supported on Zipp" % (reduce,))
    sa, sb = mat1.__class__ is _S, mat2.__class__ is _S
    if sa and sb:
        return _ssmm(mat1, mat2, "SparseSparseMatmul", True)
    if sa:
        return _spmm(mat1, mat2, "SparseAddmm", True)
    if sb:
        return _dsmm(mat1, mat2, "Transpose", True)
    return torch.mm(mat1, mat2)


def addmm(mat, mat1, mat2, beta=1.0, alpha=1.0):
    """torch.sparse.addmm: beta * mat + alpha * (mat1 @ mat2) for a sparse
    mat1 and dense mat and mat2."""
    if mat1.__class__ is not _S:
        return torch.addmm(mat, mat1, mat2, beta=beta, alpha=alpha)
    return _spmm(mat1, mat2, "SparseAddmm", True, beta, mat, alpha)


def _smm(input, mat):
    """torch.smm: sparse @ dense as a sparse tensor (every column of each row
    the sparse input has)."""
    if input.__class__ is not _S:
        raise RuntimeError("smm: expected a sparse input")
    if _grad_on(input, mat):
        raise NotImplementedError("the gradient of torch.smm is not supported on Zipp")
    dense = _spmm(input, mat, "Mm", False)
    rows = _present_rows(input)
    n = dense.shape[1]
    r = rows.unsqueeze(1).expand(rows.shape[0], n).reshape(-1)
    cols = torch.arange(n, dtype=_int64).unsqueeze(0).expand(rows.shape[0], n).reshape(-1)
    vals = torch.index_select(dense, 0, rows).reshape(-1)
    return _make_coo(torch.stack([r, cols]), vals, dense.shape, False)


def _present_rows(s):
    """The rows of a 2-D sparse matrix that hold an entry, ascending."""
    c = _coalesce_raw(s if s.layout is _coo else _compressed_to_coo_raw(s))
    crow = _dense(_k.sp_compress(_plain(c._ind)[0]._s, s.shape[0]), (s.shape[0] + 1,), _int64)
    return _compact(crow[1:] - crow[:-1])


def _hspmm(mat1, mat2):
    """torch.hspmm: sparse @ dense as a hybrid COO tensor (one sparse dim)."""
    if mat1.__class__ is not _S:
        raise RuntimeError("hspmm: expected a sparse input")
    if _grad_on(mat1, mat2):
        raise NotImplementedError("the gradient of torch.hspmm is not supported on Zipp")
    dense = _spmm(mat1, mat2, "Mm", False)
    rows = _present_rows(mat1)
    return _make_coo(rows.reshape(1, -1), torch.index_select(dense, 0, rows), dense.shape, False)


def spdiags(diagonals, offsets, shape, layout=None):
    """torch.sparse.spdiags: row k of `diagonals` placed on the diagonal
    offsets[k] of a matrix of `shape` (element j at (j - offset, j), as
    SciPy's spdiags), in offset order. As PyTorch 2.11 does, a positive
    offset takes elements up to the column count and a non-positive one up
    to the row count plus the offset, each ignoring the other bound."""
    if _len(diagonals.shape) != 2 or _len(offsets.shape) != 1 or diagonals.shape[0] != offsets.shape[0]:
        raise RuntimeError("Number of diagonals (%d) does not match the number of offsets (%d)" % (diagonals.shape[0] if diagonals.shape else 0, offsets.shape[0] if offsets.shape else 0))
    rows_n, cols_n = _int(shape[0]), _int(shape[1])
    rows, cols, picks = [], [], []
    for k, off in enumerate(_to_int64(_plain(offsets)).tolist()):
        span = _range(off, _b.min(cols_n, diagonals.shape[1])) if off > 0 else _range(0, _b.min(rows_n + off, diagonals.shape[1]))
        for j in span:
            rows.append(j - off)
            cols.append(j)
            picks.append(k * diagonals.shape[1] + j)
    vals = torch.index_select(_plain(diagonals).reshape(-1), 0, torch.tensor(picks, dtype=_int64))
    out = _make_coo(torch.tensor([rows, cols], dtype=_int64).reshape(2, _len(rows)), vals, (rows_n, cols_n), False)
    if layout is None or layout is _coo:
        return out
    if layout in _COMPRESSED:
        return _coo_to_compressed_raw(out, layout)
    raise _unsupported_layout(repr(layout))


def sampled_addmm(input, mat1, mat2, beta=1.0, alpha=1.0):
    """torch.sparse.sampled_addmm: beta * input + alpha * (mat1 @ mat2)
    sampled at input's (a CSR matrix's) entries."""
    if input.__class__ is not _S or input.layout is not _csr:
        raise RuntimeError("sampled_addmm: Expected self to be a sparse CSR tensor")
    if _grad_on(input, mat1, mat2):
        raise NotImplementedError("the gradient of torch.sparse.sampled_addmm is not supported on Zipp")
    c = _compressed_to_coo_raw(input)
    ci = _plain(c._ind)
    prod = torch.sum(torch.mul(torch.index_select(_plain(mat1), 0, ci[0]), torch.index_select(_plain(mat2).t(), 0, ci[1])), 1)
    vals = torch.add(torch.mul(_plain(c._val), beta), torch.mul(prod, alpha))
    return _make_compressed(_csr, input._crow, input._col, vals, input.shape)


def _legacy(dtype_name):
    def build(*args, **kwargs):
        dt = getattr(torch, dtype_name)
        if _len(args) >= 2 and _isinstance(args[0], _T) and _isinstance(args[1], _T):
            return _sparse_coo_tensor(args[0], args[1], args[2] if _len(args) > 2 else None, dtype=dt)
        if _len(args) == 1 and _isinstance(args[0], (_tuple, _list, _Size)):
            args = _tuple(args[0])
        shape = _tuple(_int(a) for a in args) if args else (0,)
        return _make_coo(torch.zeros((_len(shape), 0), dtype=_int64), torch.zeros(0, dtype=dt), shape, True)
    build.__name__ = build.__qualname__ = torch._CAST_NAME[dtype_name] + "Tensor"
    return build


FloatTensor = _legacy("float32")
DoubleTensor = _legacy("float64")
HalfTensor = _legacy("float16")
BFloat16Tensor = _legacy("bfloat16")
LongTensor = _legacy("int64")
IntTensor = _legacy("int32")
ShortTensor = _legacy("int16")
CharTensor = _legacy("int8")
ByteTensor = _legacy("uint8")


# ---- shape ops ---------------------------------------------------------------------------
def _transpose(s, dim0, dim1, name="Transpose"):
    rank = _len(s.shape)
    d0, d1 = torch._norm_dim(dim0, rank), torch._norm_dim(dim1, rank)
    layout = s.layout
    if layout in _BLOCKED:
        raise _unsupported_layout(repr(layout))
    if layout in _COMPRESSED:
        if d0 == d1:
            return s
        if (d0, d1) not in ((0, 1), (1, 0)) or rank != 2 + _dense_dim(s):
            raise RuntimeError("transpose(): hybrid sparse compressed tensors with dense dimensions are not supported")
        other = _csc if layout is _csr else _csr
        out = _make_compressed(other, s._crow, s._col, s._val, (s.shape[1], s.shape[0]) + _tuple(s.shape[2:]))
    else:
        sd = s._ind.shape[0]
        if d0 == d1:
            return s
        if d0 >= sd or d1 >= sd:
            raise RuntimeError("sparse transpose: transposed dimensions must be sparse Got sparse_dim: %d, d0: %d, d1: %d" % (sd, d0, d1))
        perm = _list(_range(sd))
        perm[d0], perm[d1] = perm[d1], perm[d0]
        shape = _list(s.shape)
        shape[d0], shape[d1] = shape[d1], shape[d0]
        out = _make_coo(torch.index_select(_plain(s._ind), 0, torch.tensor(perm, dtype=_int64)), s._val, shape, False, s._vnc)
    if _grad_on(s):
        _record(out, (s,), lambda g: (torch.transpose(g, d0, d1),), name)
    return out


def _t(s):
    if s.layout is _coo:
        sd, dd = s._ind.shape[0], _dense_dim(s)
        if sd > 2 or dd:
            raise RuntimeError("t() expects a tensor with <= 2 sparse and 0 dense dimensions, but got %d sparse and %d dense dimensions" % (sd, dd))
        if sd < 2:
            return s
    return _transpose(s, 0, 1, "T")


def _index_select(s, dim, index):
    layout = s.layout
    if layout in _BLOCKED:
        raise _unsupported_layout(repr(layout))
    if layout is not _coo:
        return _as_layout(_index_select(_as_coo(s), dim, index), layout)
    rank = _len(s.shape)
    d = torch._norm_dim(dim, rank)
    idx = _to_int64(_plain(index)).reshape(-1)
    size = s.shape[d]
    for v in idx.tolist():
        if v < -size or v >= size:
            raise IndexError("index_select(): index contains %d that is out of range for tensor of size %s at dimension %d" % (v, _list(s.shape), d))
    sd = s._ind.shape[0]
    shape = _list(s.shape)
    shape[d] = idx.shape[0]
    vals = _plain(s._val)
    if d < sd:
        ind = _plain(s._ind)
        pos, nv = _k.sp_index_select(ind[d]._s, idx._s, size)
        n = _k.size(pos)
        pos = _dense(pos, (n,), _int64)
        rows = [_dense(nv, (n,), _int64) if r == d else torch.index_select(ind[r], 0, pos) for r in _range(sd)]
        out = _make_coo(torch.stack(rows), torch.index_select(vals, 0, pos), shape, n <= 1)
        if _grad_on(s):
            def backward(g):
                gv = _grad_values(g, out)
                gin = torch.index_add(torch.zeros(vals.shape, dtype=gv.dtype), 0, pos, gv)
                return (_make_coo(s._ind, gin, s.shape, s._coal),)
            _record(out, (s,), backward, "IndexSelect")
        return out
    dd = d - sd + 1
    out = _make_coo(s._ind, torch.index_select(vals, dd, idx), shape, s._coal)
    if _grad_on(s):
        def backward_dense(g):
            gv = _grad_values(g, out)
            return (_make_coo(s._ind, torch.index_add(torch.zeros(vals.shape, dtype=gv.dtype), dd, idx, gv), s.shape, s._coal),)
        _record(out, (s,), backward_dense, "IndexSelect")
    return out


def _select0(s, i):
    """s[i] along the first dim."""
    if s.layout is not _coo:
        s = _as_coo(s)
    size = s.shape[0]
    if i < -size or i >= size:
        raise IndexError("select(): index %d out of range for tensor of size %s at dimension 0" % (i, _list(s.shape)))
    if i < 0:
        i += size
    sd = s._ind.shape[0]
    if sd == 1:
        # The result is dense: the sum of the values at index i.
        if _grad_on(s):
            return to_dense(s)[i]
        vals = _plain(s._val)
        keep = _compact(_plain(s._ind)[0] == i)
        return torch.sum(torch.index_select(vals, 0, keep), 0)
    ind = _plain(s._ind)
    pos, _nv = _k.sp_index_select(ind[0]._s, torch.tensor([i], dtype=_int64)._s, size)
    n = _k.size(pos)
    pos = _dense(pos, (n,), _int64)
    out = _make_coo(torch.index_select(ind[1:], 1, pos), torch.index_select(_plain(s._val), 0, pos), s.shape[1:], s._coal or n <= 1)
    if _grad_on(s):
        def backward(g):
            gv = _grad_values(g, out)
            gin = torch.index_add(torch.zeros(s._val.shape, dtype=gv.dtype), 0, pos, gv)
            return (_make_coo(s._ind, gin, s.shape, s._coal),)
        _record(out, (s,), backward, "Select")
    return out


def _getitem(s, key):
    keys = key if _isinstance(key, _tuple) else (key,)
    out = s
    for k in keys:
        if _isinstance(k, _T) and not k.shape and not k.dtype._inexact:
            k = _int(k.item())
        if not _isinstance(k, _int) or _isinstance(k, _bool):
            raise _no_kernel("aten::as_strided", s)
        if out.__class__ is not _S:
            out = out[k]
        else:
            out = _select0(out, k)
    return out


def _cat(tensors, dim):
    first = tensors[0]
    for t in tensors:
        if t.__class__ is not _S or t.layout is not _coo:
            raise _unsupported_layout("torch.cat of sparse tensors that are not all sparse COO")
    rank = _len(first.shape)
    d = torch._norm_dim(dim, rank)
    sd = first._ind.shape[0]
    if d >= sd:
        raise _unsupported_layout("torch.cat of sparse tensors along a dense dimension")
    if _grad_on(*tensors):
        raise NotImplementedError("the gradient of torch.cat of sparse tensors is not supported on Zipp")
    dt = first.dtype
    for t in tensors[1:]:
        dt = torch._promote_types(dt, t.dtype)
    inds, vals, offset = [], [], 0
    for t in tensors:
        if t._ind.shape[0] != sd or _len(t.shape) != rank:
            raise RuntimeError("concatenating sparse tensors, but at least one tensor has a different shape")
        for j in _range(rank):
            if j != d and t.shape[j] != first.shape[j]:
                raise RuntimeError("concatenating sparse tensors, but at least one tensor has a different shape")
        ind = _plain(t._ind)
        if offset:
            shift = [0] * sd
            shift[d] = offset
            ind = ind + torch.tensor(shift, dtype=_int64).reshape(sd, 1)
        inds.append(ind)
        vals.append(_values_as(t, dt))
        offset += t.shape[d]
    shape = _list(first.shape)
    shape[d] = offset
    return _make_coo(torch.cat(inds, 1), torch.cat(vals, 0), shape, False)


def _stack(tensors, dim):
    first = tensors[0]
    rank = _len(first.shape) + 1
    d = torch._norm_dim(dim, rank)
    parts = []
    for j, t in enumerate(tensors):
        if t.__class__ is not _S or t.layout is not _coo:
            raise _unsupported_layout("torch.stack of sparse tensors that are not all sparse COO")
        if _tuple(t.shape) != _tuple(first.shape):
            raise RuntimeError("stack expects each tensor to be equal size, but got %s at entry 0 and %s at entry %d" % (_list(first.shape), _list(t.shape), j))
        sd = t._ind.shape[0]
        if d > sd:
            raise _unsupported_layout("torch.stack of sparse tensors along a dense dimension")
        ind = _plain(t._ind)
        rows = [ind[r] for r in _range(sd)]
        rows.insert(d, torch.zeros(ind.shape[1], dtype=_int64))
        parts.append(_make_coo(torch.stack(rows), t._val, _list(t.shape[:d]) + [1] + _list(t.shape[d:]), t._coal))
    return _cat(parts, d)


def _zeros_like(s, dtype=None, requires_grad=False):
    dt = s.dtype if dtype is None else dtype
    if s.layout is not _coo:
        raise _unsupported_layout("zeros_like of a compressed tensor")
    sd = s._ind.shape[0]
    out = _make_coo(torch.zeros((sd, 0), dtype=_int64), torch.zeros((0,) + _tuple(s._val.shape[1:]), dtype=dt), s.shape, True)
    if requires_grad:
        out.requires_grad = True
    return out


def _to(s, dt):
    """s with values of dtype dt (differentiable)."""
    if dt is s.dtype:
        return s
    vals = _plain(s._val).to(dt)
    out = _make_coo(s._ind, vals, s.shape, s._coal) if s.layout is _coo else _make_compressed(s.layout, s._crow, s._col, vals, s.shape)
    if _grad_on(s) and dt._inexact and s.dtype._inexact:
        src = s.dtype
        _record(out, (s,), lambda g: (g.to(src),), "ToCopy")
    return out


# ---- autograd plumbing -------------------------------------------------------------------
def _assign(dst, src):
    """dst takes src's layout, shape and contents (an in-place result)."""
    dst._layout = src._layout
    dst.shape = src.shape
    dst.dtype = src.dtype
    dst._val = src._val
    dst._coal = src._coal
    if src.layout is _coo:
        dst._ind = src._ind
        dst._vnc = src._vnc
    else:
        dst._crow = src._crow
        dst._col = src._col


def _accumulate_grad(t, g):
    """AccumulateGrad with a sparse gradient or a sparse .grad: a first
    gradient is kept (its is_coalesced flag reset, as PyTorch's shallow copy
    does, unless several gradients were summed into it); a sparse .grad
    adds the next in place (the sum is dense if that one is)."""
    old = t.grad
    if old is None:
        if g.__class__ is _S:
            if g.layout is _coo:
                keep = g._shared or g._vnc
                t.grad = _make_coo(g._ind, g._val, g.shape, g._coal if keep else g._ind.shape[1] <= 1)
            else:
                t.grad = _make_compressed(g.layout, g._crow, g._col, g._val, g.shape)
        else:
            t.grad = _T(_k.copy(g._s), g.shape, g.dtype)
        return
    if old.__class__ is _S and g.__class__ is not _S:
        t.grad = torch.add(g, old)
    elif old.__class__ is not _S:
        old._write(torch.add(old, g))
    else:
        _assign(old, torch.add(old, g))


def _buffer_add(prior, g):
    """Two gradients for one tensor summed (PyTorch's InputBuffer: the new
    one first when the earlier one is sparse)."""
    if prior.__class__ is _S:
        out = torch.add(g, prior)
    else:
        out = torch.add(prior, g)
    if out.__class__ is _S:
        out._shared = True
    return out


def _dense_add_(d, s, alpha):
    """d.add_(s, alpha=alpha) for a dense d: each of s's nonzeros added
    into d's storage in turn (PyTorch's add_out_dense_sparse_cpu in place)."""
    if (torch._grad_enabled and (d.requires_grad or s.requires_grad)) or d._untracked or s.layout in _BLOCKED:
        return d._inplace_op(torch.add, s, alpha)
    if _tuple(d.shape) != _tuple(s.shape):
        raise RuntimeError("add: expected 'self' and 'other' to have same size, but self has size %s while other has size %s (FYI: dense-sparse addition does not currently support broadcasting)" % (_list(d.shape), _list(s.shape)))
    dt = torch._promote_types(d.dtype, s.dtype)
    if dt is not d.dtype:
        if torch._CATEGORY[dt.name] > torch._CATEGORY[d.dtype.name]:
            raise RuntimeError("result type %s can't be cast to the desired output type %s" % (torch._CAST_NAME[dt.name], torch._CAST_NAME[d.dtype.name]))
        return d._inplace_op(torch.add, s, alpha)
    c = s if s.layout is _coo else _compressed_to_coo_raw(s)
    nnz = c._ind.shape[1]
    if nnz:
        sd = c._ind.shape[0]
        sizes = _tuple(s.shape[:sd])
        vals = _values_as(c, dt)
        if alpha != 1:
            vals = torch.mul(vals, alpha)
        _k.sp_scatter_add(d._s, _keys(c._ind, sizes)._s, vals._s, _prod(s.shape[sd:]))
    return d


def _embedding(weight, idx, padding_idx=None, scale_grad_by_freq=False):
    """F.embedding(..., sparse=True): the rows of `weight` at idx, whose
    gradient for weight is a sparse COO tensor of one row per lookup
    (lookups of padding_idx left out), as PyTorch's EmbeddingBackward."""
    flat = _to_int64(_plain(idx)).reshape(-1)
    dim = weight.shape[1]
    out = torch.index_select(_plain(weight), 0, flat).reshape(_tuple(idx.shape) + (dim,))
    if _grad_on(weight):
        def backward(g):
            rows = g.reshape(-1, dim)
            ind = flat
            if scale_grad_by_freq:
                counts = {}
                for v in flat.tolist():
                    counts[v] = counts.get(v, 0) + 1
                rows = rows * torch.tensor([1.0 / counts[v] for v in flat.tolist()], dtype=rows.dtype).unsqueeze(1)
            if padding_idx is not None:
                keep = _compact(flat != padding_idx)
                ind, rows = torch.index_select(flat, 0, keep), torch.index_select(rows, 0, keep)
            return (_make_coo(_T(_k.copy(ind._s), (1, ind.shape[0]), _int64), _plain(rows), weight.shape, ind.shape[0] <= 1),)
        _record(out, (weight,), backward, "Embedding")
    return out


# ---- Tensor methods ----------------------------------------------------------------------
def _values_ref(s, name):
    """s's values tensor, differentiable back to s when s requires grad."""
    v = s._val
    if not _grad_on(s):
        return v
    out = _T(v._s, v.shape, v.dtype)
    layout = s.layout
    if layout is _coo:
        def backward(g):
            gs = _make_coo(s._ind, g, s.shape, s._coal)
            gs._shared = True
            return (gs,)
        return _record(out, (s,), backward, name)
    return _record(out, (s,), lambda g: (_make_compressed(layout, s._crow, s._col, g, s.shape),), name)


def values(s):
    if s.__class__ is not _S:
        raise RuntimeError("values expected sparse tensor layout but got Strided")
    if s.layout is _coo and not s._coal:
        raise RuntimeError("Cannot get values on an uncoalesced tensor, please call .coalesce() first")
    return _values_ref(s, "Values")


def _m_values(self):
    if self.layout is not _coo:
        return _values_ref(self, "Values")
    return self._val


def indices(s):
    _need_coo(s, "indices")
    if not s._coal:
        raise RuntimeError("Cannot get indices on an uncoalesced tensor, please call .coalesce() first")
    return s._ind


def _m_indices(self):
    _need_coo(self, "_indices")
    return self._ind


def _compressed_part(s, name, layouts, which):
    if s.__class__ is not _S or s.layout not in layouts:
        kind = "row" if layouts[0] is _csr else "column"
        raise RuntimeError("%s expected sparse %s compressed tensor layout but got %s" % (name, kind, _layout_name(s)))
    return s._crow if which == 0 else s._col


def _m_crow(self):
    return _compressed_part(self, "crow_indices", (_csr, torch.sparse_bsr), 0)


def _m_col(self):
    return _compressed_part(self, "col_indices", (_csr, torch.sparse_bsr), 1)


def _m_ccol(self):
    return _compressed_part(self, "ccol_indices", (_csc, torch.sparse_bsc), 0)


def _m_row(self):
    return _compressed_part(self, "row_indices", (_csc, torch.sparse_bsc), 1)


def _m_nnz(self):
    if self.__class__ is not _S:
        raise NotImplementedError("Could not run 'aten::_nnz' with arguments from the 'CPU' backend. This could be because the operator doesn't exist for this backend, or was omitted during the selective/custom build process (if using custom build).")
    return _nnz_of(self)


def _m_sparse_dim(self):
    return _sparse_dim(self) if self.__class__ is _S else 0


def _m_dense_dim(self):
    return _dense_dim(self) if self.__class__ is _S else _len(self.shape)


def _m_is_coalesced(self):
    _need_coo(self, "is_coalesced")
    return self._coal


def _m_coalesced_(self, coalesced):
    _need_coo(self, "_coalesced_")
    self._coal = _bool(coalesced)
    return self


def _m_to(self, *args, **kwargs):
    torch._check_cpu_device(kwargs.get("device"))
    target = kwargs.get("dtype")
    for a in args:
        if _isinstance(a, (_b.str, torch.device)):
            torch._check_cpu_device(a)
        elif _isinstance(a, torch.dtype):
            target = a
        elif _isinstance(a, _T):
            target = a.dtype
    if target is None or target is self.dtype:
        return _m_clone(self) if kwargs.get("copy", False) else self
    return _to(self, target)


def _m_type(self, dtype=None, non_blocking=False):
    if self.layout is not _coo:
        raise RuntimeError("Unimplemented backend %sCPU" % _layout_name(self))
    if dtype is None:
        return "torch.sparse." + torch._CAST_NAME[self.dtype.name] + "Tensor"
    if _isinstance(dtype, _b.str):
        name = dtype.rsplit(".", 1)[-1]
        if name not in torch._LEGACY_TYPES:
            raise RuntimeError("invalid type: '%s'" % dtype)
        dtype = torch._LEGACY_TYPES[name]
    return _m_to(self, dtype)


def _m_clone(self, memory_format=None):
    if self.layout is _coo:
        out = _make_coo(_plain(self._ind).clone(), _plain(self._val).clone(), self.shape, self._coal)
    else:
        out = _make_compressed(self.layout, _plain(self._crow).clone(), _plain(self._col).clone(), _plain(self._val).clone(), self.shape)
    if _grad_on(self):
        _record(out, (self,), lambda g: (g,), "Clone")
    return out


def _m_detach(self):
    return _detached(self)


def _m_detach_(self):
    self._node = None
    self.requires_grad = False
    return self


def _m_deepcopy(self, memo):
    """copy.deepcopy: a copy of a leaf's indices and values, requires_grad
    and .grad with it (PyTorch refuses a non-leaf)."""
    if self._node is not None:
        raise RuntimeError("Only Tensors created explicitly by the user (graph leaves) support the deepcopy protocol at the moment.  If you were attempting to deepcopy a module, this may be because of a torch.nn.utils.weight_norm usage, see https://github.com/pytorch/pytorch/pull/103001")
    key = id(self)
    if key in memo:
        return memo[key]
    out = _m_clone(_detached(self))
    out.requires_grad = self.requires_grad
    memo[key] = out
    if self.grad is not None:
        import copy
        out.grad = copy.deepcopy(self.grad, memo)
    return out


def _m_radd(self, other):
    if _isinstance(other, _T):
        return torch.add(other, self)
    return torch.add(self, other)


def _inplace(self, result):
    if torch._grad_enabled and self.requires_grad:
        if self._node is None:
            raise RuntimeError("a leaf Variable that requires grad is being used in an in-place operation.")
        raise NotImplementedError("in-place operations on a sparse tensor that requires grad are not supported on Zipp")
    if result.__class__ is not _S:
        raise RuntimeError("add(sparse, dense) is not supported. Use add(dense, sparse) instead.")
    if result.dtype is not self.dtype:
        if torch._CATEGORY[result.dtype.name] > torch._CATEGORY[self.dtype.name]:
            raise RuntimeError("result type %s can't be cast to the desired output type %s" % (torch._CAST_NAME[result.dtype.name], torch._CAST_NAME[self.dtype.name]))
        result = _to(_detached(result), self.dtype)
    if result.layout is not self.layout:
        result = _regrad(result, self.layout)
    _assign(self, result)
    return self


def _m_add_(self, other, alpha=1):
    if other.__class__ is not _S:
        raise RuntimeError("add(sparse, dense) is not supported. Use add(dense, sparse) instead.")
    return _inplace(self, torch.add(self, other, alpha=alpha))


def _m_sub_(self, other, alpha=1):
    if other.__class__ is not _S:
        raise RuntimeError("add(sparse, dense) is not supported. Use add(dense, sparse) instead.")
    return _inplace(self, torch.sub(self, other, alpha=alpha))


def _m_mul_(self, other):
    return _inplace(self, torch.mul(self, other))


def _m_div_(self, other, rounding_mode=None):
    return _inplace(self, torch.div(self, other, rounding_mode=rounding_mode))


def _m_neg_(self):
    return _inplace(self, torch.neg(self))


def _m_zero_(self):
    if torch._grad_enabled and self.requires_grad and self._node is None:
        raise RuntimeError("a leaf Variable that requires grad is being used in an in-place operation.")
    if self.layout is not _coo:
        raise _no_kernel("aten::zero_", self)
    sd = self._ind.shape[0]
    _assign(self, _make_coo(torch.zeros((sd, 0), dtype=_int64), torch.zeros((0,) + _tuple(self._val.shape[1:]), dtype=self.dtype), self.shape, True))
    return self


def _m_copy_(self, src, non_blocking=False):
    if src.__class__ is not _S:
        raise _no_kernel("aten::copy_", self)
    if _tuple(src.shape) != _tuple(self.shape):
        raise RuntimeError("copy_(): sizes of the source and destination must match")
    return _inplace(self, _m_clone(_detached(src)))


def _m_new(self, *args, **kwargs):
    dt = kwargs.get("dtype", self.dtype)
    if _len(args) >= 2 and _isinstance(args[0], _T) and _isinstance(args[1], _T):
        size = args[2] if _len(args) > 2 else kwargs.get("size")
        return _sparse_coo_tensor(args[0], args[1], size)
    if _len(args) == 1 and _isinstance(args[0], (_tuple, _list, _Size)):
        args = _tuple(args[0])
    shape = _tuple(_int(a) for a in args) if args else (0,)
    return _make_coo(torch.zeros((_len(shape), 0), dtype=_int64), torch.zeros(0, dtype=dt), shape, True)


def _m_resize_as_(self, other, memory_format=None):
    if other.__class__ is not _S or other.layout is not _coo:
        raise RuntimeError("resize_as_: expected a sparse COO tensor to resize like")
    sd = other._ind.shape[0]
    _assign(self, _make_coo(torch.zeros((sd, 0), dtype=_int64), torch.zeros((0,) + _tuple(other._val.shape[1:]), dtype=self.dtype), other.shape, True))
    return self


def _m_norm(self, p="fro", dim=None, keepdim=False, dtype=None):
    return _norm(self, p, dim, keepdim, dtype)


def _m_getitem(self, key):
    return _getitem(self, key)


def _m_view(self, *shape):
    raise _no_kernel("aten::view", self)


def _m_reshape(self, *shape):
    raise RuntimeError("reshape is not implemented for sparse tensors")


def _no_storage(self, *args, **kwargs):
    raise RuntimeError("Cannot access data pointer of Tensor that doesn't have storage")


def _m_storage(self):
    raise NotImplementedError("Cannot access storage of %s" % ("SparseTensorImpl" if self.layout is _coo else "SparseCsrTensorImpl"))


def _m_numpy(self, force=False):
    raise TypeError("can't convert %s layout tensor to numpy. Use Tensor.to_dense() first." % ("Sparse" if self.layout is _coo else _layout_name(self)))


def _m_item(self):
    n = _prod(self.shape)
    if n != 1:
        raise RuntimeError("a Tensor with %d elements cannot be converted to Scalar" % n)
    return _to_dense_raw(self).item()


def _m_stride(self, dim=None):
    if self.layout is not _coo:
        raise RuntimeError("Sparse %s tensors do not have strides" % ("CSR" if self.layout is _csr else "CSC"))
    s = (0,) * _len(self.shape)
    return s if dim is None else s[torch._norm_dim(dim, _len(self.shape))]


def _m_is_contiguous(self, memory_format=None):
    return False


def _m_contiguous(self, memory_format=None):
    raise RuntimeError("unsupported memory format option Contiguous")


def _m_format(self, spec):
    if spec:
        raise TypeError("unsupported format string passed to Tensor.__format__")
    return _repr(self)


def _m_data(self):
    return _detached(self)


def _m_softmax(self, dim=-1, dtype=None):
    raise _no_kernel("aten::_softmax", self)


def _dense_coalesce(self):
    if self.__class__ is not _S:
        raise RuntimeError("coalesce expected sparse coordinate tensor layout but got Strided")
    return coalesce(self)


def _dense_is_coalesced(self):
    if self.__class__ is not _S:
        raise RuntimeError("is_coalesced expected sparse coordinate tensor layout but got Strided")
    return _m_is_coalesced(self)


def _dense_values(self):
    return values(self)


def _dense_indices(self):
    return indices(self)


def _dense_to_sparse(self, sparseDims=None, layout=None, blocksize=None, dense_dim=None):
    if blocksize is not None or layout in _BLOCKED:
        return _to_blocked(self, blocksize, dense_dim)
    return to_sparse(self, sparseDims, layout, blocksize, dense_dim)


def _install():
    sparse_methods = {
        "__repr__": _repr, "__str__": _repr, "__format__": _m_format, "__getitem__": _m_getitem, "__radd__": _m_radd,
        "__deepcopy__": _m_deepcopy,
        "_nnz": _m_nnz, "sparse_dim": _m_sparse_dim, "dense_dim": _m_dense_dim, "indices": indices, "values": values,
        "_indices": _m_indices, "_values": _m_values, "crow_indices": _m_crow, "col_indices": _m_col, "ccol_indices": _m_ccol,
        "row_indices": _m_row, "coalesce": coalesce, "is_coalesced": _m_is_coalesced, "_coalesced_": _m_coalesced_,
        "to_dense": to_dense, "to_sparse": _dense_to_sparse, "to_sparse_coo": to_sparse_coo, "to_sparse_csr": to_sparse_csr,
        "to_sparse_csc": to_sparse_csc, "to_sparse_bsr": _to_blocked, "to_sparse_bsc": _to_blocked, "to": _m_to, "type": _m_type,
        "clone": _m_clone, "detach": _m_detach, "detach_": _m_detach_, "add_": _m_add_, "sub_": _m_sub_, "mul_": _m_mul_,
        "div_": _m_div_, "neg_": _m_neg_, "negative_": _m_neg_, "zero_": _m_zero_, "copy_": _m_copy_, "new": _m_new,
        "resize_as_": _m_resize_as_, "norm": _m_norm, "view": _m_view, "reshape": _m_reshape, "tolist": _no_storage,
        "data_ptr": _no_storage, "storage": _m_storage, "untyped_storage": _m_storage, "numpy": _m_numpy, "item": _m_item,
        "stride": _m_stride, "is_contiguous": _m_is_contiguous, "contiguous": _m_contiguous, "softmax": _m_softmax,
        "sparse_mask": lambda self, mask: sparse_mask(self, mask),
    }
    for name in sparse_methods:
        setattr(_S, name, sparse_methods[name])
    _S.layout = property(lambda self: self._layout)
    _S.is_sparse = property(lambda self: self._layout is _coo)
    _S.is_sparse_csr = property(lambda self: self._layout is _csr)
    _S.data = property(_m_data)
    _S._vnc = False
    _S._shared = False
    dense_methods = {
        "to_dense": to_dense, "to_sparse": _dense_to_sparse, "to_sparse_coo": to_sparse_coo, "to_sparse_csr": to_sparse_csr,
        "to_sparse_csc": to_sparse_csc, "to_sparse_bsr": _to_blocked, "to_sparse_bsc": _to_blocked, "sparse_mask": sparse_mask,
        "sparse_dim": _m_sparse_dim, "dense_dim": _m_dense_dim, "_nnz": _m_nnz, "coalesce": _dense_coalesce,
        "is_coalesced": _dense_is_coalesced, "indices": _dense_indices, "values": _dense_values, "crow_indices": _m_crow,
        "col_indices": _m_col, "ccol_indices": _m_ccol, "row_indices": _m_row,
    }
    for name in dense_methods:
        if not hasattr(_T, name):
            setattr(_T, name, dense_methods[name])


_install()
