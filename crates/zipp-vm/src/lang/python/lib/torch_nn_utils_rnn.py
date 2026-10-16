"""torch.nn.utils.rnn for Zipp: padding and packing variable-length sequences.

A PackedSequence holds the time-major concatenation of the sequences sorted
by decreasing length (`data`), the batch size at each time step
(`batch_sizes`) and the permutations to and from the caller's order, as in
PyTorch. Recurrent layers accept one and run it with per-row masking.
"""
import torch
from torch import Tensor
import torch.nn.functional as F


def invert_permutation(permutation):
    if permutation is None:
        return None
    n = permutation.numel()
    inverse = [0] * n
    for i, p in enumerate(permutation.tolist()):
        inverse[int(p)] = i
    return torch.tensor(inverse, dtype=torch.int64)


class PackedSequence(tuple):
    def __new__(cls, data, batch_sizes=None, sorted_indices=None, unsorted_indices=None):
        if unsorted_indices is None:
            unsorted_indices = invert_permutation(sorted_indices)
        if batch_sizes is not None:
            if batch_sizes.dtype != torch.int64:
                raise ValueError("batch_sizes should always be on CPU and of type torch.int64")
        else:
            # PackedSequence(data_and_batch_sizes_tuple) form.
            data, batch_sizes = data[0], data[1]
        return tuple.__new__(cls, (data, batch_sizes, sorted_indices, unsorted_indices))

    @property
    def data(self):
        return self[0]

    @property
    def batch_sizes(self):
        return self[1]

    @property
    def sorted_indices(self):
        return self[2]

    @property
    def unsorted_indices(self):
        return self[3]

    def to(self, *args, **kwargs):
        data = self.data.to(*args, **kwargs)
        if data is self.data:
            return self
        return type(self)(data, self.batch_sizes, self.sorted_indices, self.unsorted_indices)

    def cpu(self, *args, **kwargs):
        return self

    def double(self):
        return self.to(dtype=torch.float64)

    def float(self):
        return self.to(dtype=torch.float32)

    def long(self):
        return self.to(dtype=torch.int64)

    def int(self):
        return self.to(dtype=torch.int32)

    @property
    def is_cuda(self):
        return False

    def is_pinned(self):
        return False

    def __repr__(self):
        return "PackedSequence(data=%r, batch_sizes=%r, sorted_indices=%r, unsorted_indices=%r)" % tuple(self)


def _lengths_list(lengths):
    if isinstance(lengths, Tensor):
        return [int(v) for v in lengths.reshape(-1).tolist()]
    return [int(v) for v in lengths]


def pack_padded_sequence(input, lengths, batch_first=False, enforce_sorted=True):
    lens = _lengths_list(lengths)
    if enforce_sorted:
        if any(lens[i] < lens[i + 1] for i in range(len(lens) - 1)):
            raise RuntimeError("`lengths` array must be sorted in decreasing order when `enforce_sorted` is True. You can pass `enforce_sorted=False` to pack_padded_sequence and/or pack_sequence to sidestep this requirement if you do not need ONNX exportability.")
        sorted_indices = None
    else:
        # A stable descending sort, as PyTorch's.
        order = sorted(range(len(lens)), key=lambda i: -lens[i])
        sorted_indices = torch.tensor(order, dtype=torch.int64)
        lens = [lens[i] for i in order]
        input = input.index_select(0 if batch_first else 1, sorted_indices)
    if any(v <= 0 for v in lens):
        raise RuntimeError("Length of all samples has to be greater than 0, but found an element in 'lengths' that is <= 0")
    x = input.transpose(0, 1) if batch_first else input
    if lens and lens[0] > x.shape[0]:
        raise RuntimeError("Expected sequence length to be larger than or equal to the largest length %d, got %d" % (lens[0], x.shape[0]))
    batch_sizes = [sum(1 for v in lens if v > t) for t in range(lens[0] if lens else 0)]
    times = F._unbind(x, 0)
    steps = [times[t].narrow(0, 0, b) for t, b in enumerate(batch_sizes)]
    data = torch.cat(steps, 0)
    return PackedSequence(data, torch.tensor(batch_sizes, dtype=torch.int64), sorted_indices, None)


def _pad_packed_sorted(sequence, batch_first=False, padding_value=0.0, total_length=None):
    """The padded (T, B, *) tensor and lengths in the packed (sorted) order."""
    data = sequence.data
    sizes = [int(v) for v in sequence.batch_sizes.tolist()]
    batch = sizes[0] if sizes else 0
    steps = len(sizes) if total_length is None else total_length
    tail = tuple(data.shape[1:])
    pieces = F._unbind(data, 0)
    rows, offset = [], 0
    for t in range(steps):
        b = sizes[t] if t < len(sizes) else 0
        part = torch.stack(pieces[offset:offset + b], 0) if b else torch.zeros((0,) + tail, dtype=data.dtype)
        offset += b
        if b < batch:
            part = torch.cat([part, torch.full((batch - b,) + tail, padding_value, dtype=data.dtype)], 0)
        rows.append(part)
    padded = torch.stack(rows, 0)
    lengths = [sum(1 for s in sizes if s > i) for i in range(batch)]
    if batch_first:
        padded = padded.transpose(0, 1)
    return padded, torch.tensor(lengths, dtype=torch.int64)


def pad_packed_sequence(sequence, batch_first=False, padding_value=0.0, total_length=None):
    max_len = sequence.batch_sizes.shape[0]
    if total_length is not None:
        if total_length < max_len:
            raise ValueError("Expected total_length to be at least the length of the longest sequence in input, but got total_length=%d and max sequence length being %d" % (total_length, max_len))
    padded, lengths = _pad_packed_sorted(sequence, batch_first, padding_value, total_length)
    unsorted = sequence.unsorted_indices
    if unsorted is not None:
        padded = padded.index_select(0 if batch_first else 1, unsorted)
        lengths = lengths[unsorted]
    return padded, lengths


def _pack_sorted(output, batch_sizes, sorted_indices, unsorted_indices):
    """Re-pack a time-major (T, B, *) output whose rows are in sorted order."""
    sizes = [int(v) for v in batch_sizes.tolist()]
    times = F._unbind(output, 0)
    rows = [times[t].narrow(0, 0, b) for t, b in enumerate(sizes)]
    return PackedSequence(torch.cat(rows, 0), batch_sizes, sorted_indices, unsorted_indices)


def pad_sequence(sequences, batch_first=False, padding_value=0.0, padding_side="right"):
    if padding_side not in ("left", "right"):
        raise ValueError("Expected padding_side to be one of left or right, but got %s." % padding_side)
    sequences = list(sequences) if not isinstance(sequences, Tensor) else list(sequences.unbind(0))
    if not sequences:
        raise RuntimeError("received an empty list of sequences")
    max_len = max(s.shape[0] for s in sequences)
    padded = []
    for s in sequences:
        extra = max_len - s.shape[0]
        if extra:
            fill = torch.full((extra,) + tuple(s.shape[1:]), padding_value, dtype=s.dtype)
            s = torch.cat([s, fill] if padding_side == "right" else [fill, s], 0)
        padded.append(s)
    return torch.stack(padded, 0 if batch_first else 1)


def unpad_sequence(padded_sequences, lengths, batch_first=False):
    x = padded_sequences if batch_first else padded_sequences.transpose(0, 1)
    return [x[i].narrow(0, 0, n) for i, n in enumerate(_lengths_list(lengths))]


def pack_sequence(sequences, enforce_sorted=True):
    lengths = [s.shape[0] for s in sequences]
    return pack_padded_sequence(pad_sequence(sequences), lengths, enforce_sorted=enforce_sorted)


def unpack_sequence(packed_sequences):
    padded, lengths = pad_packed_sequence(packed_sequences, batch_first=True)
    return unpad_sequence(padded, lengths, batch_first=True)
