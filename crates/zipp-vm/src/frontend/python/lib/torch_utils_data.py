"""torch.utils.data for Zipp: datasets, samplers, `default_collate` and a
DataLoader that follow PyTorch's semantics, run in-process.

`num_workers` is accepted and ignored: every batch is loaded in the calling
program (so `get_worker_info()` is None and `worker_init_fn` is not called);
`pin_memory`, `timeout`, `prefetch_factor` and `persistent_workers` change
nothing. Shuffling draws from `torch`'s generators the way PyTorch's samplers
do (one 64-bit seed per iterator, then `randperm`), so `torch.manual_seed` or
a `generator=` makes the batch order reproducible from run to run; whether it
equals PyTorch's order depends on the generator's random stream matching."""
import copy
import math
import collections.abc as _abc
import torch

__all__ = ["BatchSampler", "ChainDataset", "ConcatDataset", "DataLoader", "Dataset", "IterableDataset", "RandomSampler", "Sampler",
           "SequentialSampler", "StackDataset", "Subset", "SubsetRandomSampler", "TensorDataset", "WeightedRandomSampler",
           "default_collate", "default_convert", "get_worker_info", "random_split"]


def get_worker_info():
    return None


def _random_seed(generator=None):
    """PyTorch's `torch.empty((), dtype=torch.int64).random_(generator=g)`:
    one 64-bit draw (two 32-bit words, high first) reduced to [0, 2**63)."""
    hi, lo = torch.randint(0, 2 ** 32, (2,), generator=generator).tolist()
    return ((int(hi) << 32) | int(lo)) % (1 << 63)


# ---- datasets ----------------------------------------------------------------------------
class Dataset:
    def __getitem__(self, index):
        raise NotImplementedError("Subclasses of Dataset should implement __getitem__.")

    def __add__(self, other):
        return ConcatDataset([self, other])


class IterableDataset(Dataset):
    def __iter__(self):
        raise NotImplementedError("Subclasses of IterableDataset should implement __iter__.")

    def __add__(self, other):
        return ChainDataset([self, other])


class TensorDataset(Dataset):
    def __init__(self, *tensors):
        if any(tensors[0].size(0) != tensor.size(0) for tensor in tensors):
            raise AssertionError("Size mismatch between tensors")
        self.tensors = tensors

    def __getitem__(self, index):
        return tuple(tensor[index] for tensor in self.tensors)

    def __len__(self):
        return self.tensors[0].size(0)


class StackDataset(Dataset):
    def __init__(self, *args, **kwargs):
        if args:
            if kwargs:
                raise ValueError("Supported either ``tuple``- (via ``args``) or``dict``- (via ``kwargs``) like input/output, but both types are given.")
            self._length = len(args[0])
            if any(self._length != len(dataset) for dataset in args):
                raise ValueError("Size mismatch between datasets")
            self.datasets = args
        elif kwargs:
            values = list(kwargs.values())
            self._length = len(values[0])
            if any(self._length != len(dataset) for dataset in values):
                raise ValueError("Size mismatch between datasets")
            self.datasets = kwargs
        else:
            raise ValueError("At least one dataset should be passed")

    def __getitem__(self, index):
        if isinstance(self.datasets, dict):
            return {k: dataset[index] for k, dataset in self.datasets.items()}
        return tuple(dataset[index] for dataset in self.datasets)

    def __len__(self):
        return self._length


class ConcatDataset(Dataset):
    @staticmethod
    def cumsum(sequence):
        r, s = [], 0
        for e in sequence:
            s += len(e)
            r.append(s)
        return r

    def __init__(self, datasets):
        self.datasets = list(datasets)
        if len(self.datasets) == 0:
            raise AssertionError("datasets should not be an empty iterable")
        for d in self.datasets:
            if isinstance(d, IterableDataset):
                raise AssertionError("ConcatDataset does not support IterableDataset")
        self.cumulative_sizes = self.cumsum(self.datasets)

    def __len__(self):
        return self.cumulative_sizes[-1]

    def __getitem__(self, idx):
        if idx < 0:
            if -idx > len(self):
                raise ValueError("absolute value of index should not exceed dataset length")
            idx = len(self) + idx
        dataset_idx = _bisect_right(self.cumulative_sizes, idx)
        sample_idx = idx if dataset_idx == 0 else idx - self.cumulative_sizes[dataset_idx - 1]
        return self.datasets[dataset_idx][sample_idx]


class ChainDataset(IterableDataset):
    def __init__(self, datasets):
        self.datasets = datasets

    def __iter__(self):
        for d in self.datasets:
            if not isinstance(d, IterableDataset):
                raise AssertionError("ChainDataset only supports IterableDataset")
            for x in d:
                yield x

    def __len__(self):
        total = 0
        for d in self.datasets:
            if not isinstance(d, IterableDataset):
                raise AssertionError("ChainDataset only supports IterableDataset")
            total += len(d)
        return total


class Subset(Dataset):
    def __init__(self, dataset, indices):
        self.dataset = dataset
        self.indices = indices

    def __getitem__(self, idx):
        if isinstance(idx, list):
            return self.dataset[[self.indices[i] for i in idx]]
        return self.dataset[self.indices[idx]]

    def __getitems__(self, indices):
        getitems = getattr(self.dataset, "__getitems__", None)
        if callable(getitems):
            return getitems([self.indices[idx] for idx in indices])
        return [self.dataset[self.indices[idx]] for idx in indices]

    def __len__(self):
        return len(self.indices)


def _bisect_right(values, x):
    lo, hi = 0, len(values)
    while lo < hi:
        mid = (lo + hi) // 2
        if x < values[mid]:
            hi = mid
        else:
            lo = mid + 1
    return lo


def random_split(dataset, lengths, generator=None):
    """Split `dataset` into non-overlapping Subsets of the given lengths
    (counts, or fractions summing to 1); `generator` defaults to torch's."""
    lengths = list(lengths)
    total = sum(lengths)
    if math.isclose(total, 1) and total <= 1:
        subset_lengths = []
        for i, frac in enumerate(lengths):
            if frac < 0 or frac > 1:
                raise ValueError("Fraction at index %d is not between 0 and 1" % i)
            subset_lengths.append(int(math.floor(len(dataset) * frac)))
        remainder = len(dataset) - sum(subset_lengths)
        for i in range(remainder):
            subset_lengths[i % len(subset_lengths)] += 1
        lengths = subset_lengths
    if sum(lengths) != len(dataset):
        raise ValueError("Sum of input lengths does not equal the length of the input dataset!")
    indices = torch.randperm(sum(lengths), generator=generator).tolist()
    out, offset = [], 0
    for length in lengths:
        offset += length
        out.append(Subset(dataset, indices[offset - length:offset]))
    return out


# ---- samplers ----------------------------------------------------------------------------
class Sampler:
    def __init__(self, data_source=None):
        pass

    def __iter__(self):
        raise NotImplementedError


class SequentialSampler(Sampler):
    def __init__(self, data_source):
        self.data_source = data_source

    def __iter__(self):
        return iter(range(len(self.data_source)))

    def __len__(self):
        return len(self.data_source)


class RandomSampler(Sampler):
    def __init__(self, data_source, replacement=False, num_samples=None, generator=None):
        self.data_source = data_source
        self.replacement = replacement
        self._num_samples = num_samples
        self.generator = generator
        if not isinstance(self.replacement, bool):
            raise TypeError("replacement should be a boolean value, but got replacement=%s" % (self.replacement,))
        if not isinstance(self.num_samples, int) or isinstance(self.num_samples, bool) or self.num_samples <= 0:
            raise ValueError("num_samples should be a positive integer value, but got num_samples=%s" % (self.num_samples,))

    @property
    def num_samples(self):
        if self._num_samples is None:
            return len(self.data_source)
        return self._num_samples

    def __iter__(self):
        n = len(self.data_source)
        if self.generator is None:
            generator = torch.Generator()
            generator.manual_seed(_random_seed())
        else:
            generator = self.generator
        if self.replacement:
            for _ in range(self.num_samples // 32):
                for i in torch.randint(0, n, (32,), generator=generator).tolist():
                    yield i
            for i in torch.randint(0, n, (self.num_samples % 32,), generator=generator).tolist():
                yield i
        else:
            for _ in range(self.num_samples // n):
                for i in torch.randperm(n, generator=generator).tolist():
                    yield i
            for i in torch.randperm(n, generator=generator).tolist()[:self.num_samples % n]:
                yield i

    def __len__(self):
        return self.num_samples


class SubsetRandomSampler(Sampler):
    def __init__(self, indices, generator=None):
        self.indices = indices
        self.generator = generator

    def __iter__(self):
        for i in torch.randperm(len(self.indices), generator=self.generator).tolist():
            yield self.indices[i]

    def __len__(self):
        return len(self.indices)


class WeightedRandomSampler(Sampler):
    def __init__(self, weights, num_samples, replacement=True, generator=None):
        if not isinstance(num_samples, int) or isinstance(num_samples, bool) or num_samples <= 0:
            raise ValueError("num_samples should be a positive integer value, but got num_samples=%s" % (num_samples,))
        if not isinstance(replacement, bool):
            raise ValueError("replacement should be a boolean value, but got replacement=%s" % (replacement,))
        weights = torch.as_tensor(weights, dtype=torch.float64)
        if len(weights.shape) != 1:
            raise ValueError("weights should be a 1d sequence but given weights have shape %s" % (tuple(weights.shape),))
        self.weights = weights
        self.num_samples = num_samples
        self.replacement = replacement
        self.generator = generator

    def __iter__(self):
        for i in torch.multinomial(self.weights, self.num_samples, self.replacement, generator=self.generator).tolist():
            yield i

    def __len__(self):
        return self.num_samples


class BatchSampler(Sampler):
    def __init__(self, sampler, batch_size, drop_last):
        if not isinstance(batch_size, int) or isinstance(batch_size, bool) or batch_size <= 0:
            raise ValueError("batch_size should be a positive integer value, but got batch_size=%s" % (batch_size,))
        if not isinstance(drop_last, bool):
            raise ValueError("drop_last should be a boolean value, but got drop_last=%s" % (drop_last,))
        self.sampler = sampler
        self.batch_size = batch_size
        self.drop_last = drop_last

    def __iter__(self):
        batch = []
        for idx in self.sampler:
            batch.append(idx)
            if len(batch) == self.batch_size:
                yield batch
                batch = []
        if batch and not self.drop_last:
            yield batch

    def __len__(self):
        if self.drop_last:
            return len(self.sampler) // self.batch_size
        return (len(self.sampler) + self.batch_size - 1) // self.batch_size


class _InfiniteConstantSampler(Sampler):
    def __iter__(self):
        while True:
            yield None


# ---- collation ---------------------------------------------------------------------------
default_collate_err_msg_format = "default_collate: batch must contain tensors, numpy arrays, numbers, dicts or lists; found {}"


def _is_mapping(value):
    return isinstance(value, dict) or isinstance(value, _abc.Mapping)


def _is_sequence(value):
    if isinstance(value, (str, bytes, bytearray, dict, torch.Tensor)):
        return False
    return isinstance(value, (list, tuple, range)) or isinstance(value, _abc.Sequence)


def _is_namedtuple(value):
    return isinstance(value, tuple) and hasattr(value, "_fields")


def default_collate(batch):
    """Stack a list of samples into a batch: tensors are stacked on a new
    first dimension, Python floats become a float64 tensor and ints an
    int64 one, strings stay a list, and dicts, namedtuples, tuples and
    lists are collated field by field."""
    elem = batch[0]
    if isinstance(elem, torch.Tensor):
        return torch.stack(list(batch), 0)
    if isinstance(elem, float):
        return torch.tensor(list(batch), dtype=torch.float64)
    if isinstance(elem, int):
        return torch.tensor(list(batch))
    if isinstance(elem, (str, bytes)):
        return batch
    if _is_mapping(elem):
        collated = [(key, default_collate([d[key] for d in batch])) for key in elem]
        if isinstance(elem, dict):
            clone = copy.copy(elem)
            clone.update(collated)
            return clone
        try:
            return type(elem)(dict(collated))
        except TypeError:
            return dict(collated)
    if _is_namedtuple(elem):
        return type(elem)(*(default_collate(samples) for samples in zip(*batch)))
    if _is_sequence(elem):
        elem_size = len(elem)
        if not all(len(e) == elem_size for e in batch):
            raise RuntimeError("each element in list of batch should be of equal size")
        transposed = list(zip(*batch))
        if isinstance(elem, tuple):
            return [default_collate(samples) for samples in transposed]
        if isinstance(elem, list):
            clone = copy.copy(elem)
            for i, samples in enumerate(transposed):
                clone[i] = default_collate(samples)
            return clone
        try:
            return type(elem)([default_collate(samples) for samples in transposed])
        except TypeError:
            return [default_collate(samples) for samples in transposed]
    raise TypeError(default_collate_err_msg_format.format(type(elem)))


def default_convert(data):
    """The collate_fn when automatic batching is off (`batch_size=None`):
    samples pass through; plain tuples become lists, as in PyTorch."""
    if isinstance(data, (torch.Tensor, str, bytes)):
        return data
    if _is_mapping(data):
        converted = [(key, default_convert(data[key])) for key in data]
        if isinstance(data, dict):
            clone = copy.copy(data)
            clone.update(converted)
            return clone
        try:
            return type(data)(dict(converted))
        except TypeError:
            return dict(converted)
    if _is_namedtuple(data):
        return type(data)(*(default_convert(d) for d in data))
    if isinstance(data, tuple):
        return [default_convert(d) for d in data]
    if isinstance(data, list):
        return [default_convert(d) for d in data]
    return data


# ---- the loader --------------------------------------------------------------------------
class DataLoader:
    def __init__(self, dataset, batch_size=1, shuffle=None, sampler=None, batch_sampler=None, num_workers=0, collate_fn=None,
                 pin_memory=False, drop_last=False, timeout=0, worker_init_fn=None, multiprocessing_context=None, generator=None, *,
                 prefetch_factor=None, persistent_workers=False, pin_memory_device="", in_order=True):
        if num_workers < 0:
            raise ValueError("num_workers option should be non-negative; use num_workers=0 to disable multiprocessing.")
        if timeout < 0:
            raise ValueError("timeout option should be non-negative")
        if num_workers == 0 and prefetch_factor is not None:
            raise ValueError("prefetch_factor option could only be specified in multiprocessing.let num_workers > 0 to enable "
                             "multiprocessing, otherwise set prefetch_factor to None.")
        if prefetch_factor is not None and prefetch_factor < 0:
            raise ValueError("prefetch_factor option should be non-negative")
        if persistent_workers and num_workers == 0:
            raise ValueError("persistent_workers option needs num_workers > 0")
        self.dataset = dataset
        self.num_workers = num_workers
        self.prefetch_factor = prefetch_factor
        self.pin_memory = pin_memory
        self.pin_memory_device = pin_memory_device
        self.timeout = timeout
        self.worker_init_fn = worker_init_fn
        self.multiprocessing_context = multiprocessing_context
        self.persistent_workers = persistent_workers
        self.in_order = in_order
        self._iterable = isinstance(dataset, IterableDataset)
        if self._iterable:
            if shuffle not in (False, None):
                raise ValueError("DataLoader with IterableDataset: expected unspecified shuffle option, but got shuffle=%s" % (shuffle,))
            if sampler is not None:
                raise ValueError("DataLoader with IterableDataset: expected unspecified sampler option, but got sampler=%s" % (sampler,))
            if batch_sampler is not None:
                raise ValueError("DataLoader with IterableDataset: expected unspecified batch_sampler option, but got batch_sampler=%s"
                                 % (batch_sampler,))
        shuffle = bool(shuffle)
        if sampler is not None and shuffle:
            raise ValueError("sampler option is mutually exclusive with shuffle")
        if batch_sampler is not None:
            if batch_size != 1 or shuffle or sampler is not None or drop_last:
                raise ValueError("batch_sampler option is mutually exclusive with batch_size, shuffle, sampler, and drop_last")
            batch_size = None
            drop_last = False
        elif batch_size is None:
            if drop_last:
                raise ValueError("batch_size=None option disables auto-batching and is mutually exclusive with drop_last")
        if sampler is None:
            if self._iterable:
                sampler = _InfiniteConstantSampler()
            elif shuffle:
                sampler = RandomSampler(dataset, generator=generator)
            else:
                sampler = SequentialSampler(dataset)
        if batch_size is not None and batch_sampler is None:
            batch_sampler = BatchSampler(sampler, batch_size, drop_last)
        self.batch_size = batch_size
        self.drop_last = drop_last
        self.sampler = sampler
        self.batch_sampler = batch_sampler
        self.generator = generator
        if collate_fn is None:
            collate_fn = default_collate if batch_sampler is not None else default_convert
        self.collate_fn = collate_fn

    @property
    def _auto_collation(self):
        return self.batch_sampler is not None

    @property
    def _index_sampler(self):
        return self.batch_sampler if self._auto_collation else self.sampler

    def __len__(self):
        if self._iterable:
            length = len(self.dataset)
            if self.batch_size is not None:
                length = length // self.batch_size if self.drop_last else int(math.ceil(length / self.batch_size))
            return length
        return len(self._index_sampler)

    def __iter__(self):
        return _DataLoaderIter(self)

    def check_worker_number_rationality(self):
        return None


class _DataLoaderIter:
    def __init__(self, loader):
        self._loader = loader
        self._dataset = loader.dataset
        self._auto_collation = loader._auto_collation
        self._drop_last = loader.drop_last
        self._collate_fn = loader.collate_fn
        self._index_sampler = loader._index_sampler
        self._sampler_iter = iter(self._index_sampler)
        # Every PyTorch loader iterator draws a base seed for its workers,
        # advancing the generator even with shuffle=False and no workers.
        self._base_seed = _random_seed(loader.generator)
        self._dataset_iter = iter(self._dataset) if loader._iterable else None
        self._ended = False
        self._num_yielded = 0

    def __iter__(self):
        return self

    def __len__(self):
        return len(self._loader)

    def _fetch(self, index):
        if self._dataset_iter is not None:
            if self._ended:
                raise StopIteration
            if self._auto_collation:
                data = []
                for _ in index:
                    try:
                        data.append(next(self._dataset_iter))
                    except StopIteration:
                        self._ended = True
                        break
                if len(data) == 0 or (self._drop_last and len(data) < len(index)):
                    raise StopIteration
            else:
                data = next(self._dataset_iter)
            return self._collate_fn(data)
        if self._auto_collation:
            getitems = getattr(self._dataset, "__getitems__", None)
            if callable(getitems):
                data = getitems(index)
            else:
                data = [self._dataset[i] for i in index]
        else:
            data = self._dataset[index]
        return self._collate_fn(data)

    def __next__(self):
        index = next(self._sampler_iter)
        data = self._fetch(index)
        self._num_yielded += 1
        return data


class DistributedSampler(Sampler):
    """PyTorch's DistributedSampler: the dataset's indices (shuffled from
    seed + epoch when `shuffle`), padded by repeating from the start (or cut
    with `drop_last`) to a multiple of num_replicas, then every
    num_replicas-th index from `rank`. num_replicas and rank default to the
    process group's (one process on Zipp); explicit values select any
    replica's share."""

    def __init__(self, dataset, num_replicas=None, rank=None, shuffle=True, seed=0, drop_last=False):
        if num_replicas is None or rank is None:
            import torch.distributed as dist
            if not dist.is_available():
                raise RuntimeError("Requires distributed package to be available")
            if num_replicas is None:
                num_replicas = dist.get_world_size()
            if rank is None:
                rank = dist.get_rank()
        if rank >= num_replicas or rank < 0:
            raise ValueError("Invalid rank %d, rank should be in the interval [0, %d]" % (rank, num_replicas - 1))
        self.dataset = dataset
        self.num_replicas = num_replicas
        self.rank = rank
        self.epoch = 0
        self.drop_last = drop_last
        if self.drop_last and len(self.dataset) % self.num_replicas != 0:
            self.num_samples = math.ceil((len(self.dataset) - self.num_replicas) / self.num_replicas)
        else:
            self.num_samples = math.ceil(len(self.dataset) / self.num_replicas)
        self.total_size = self.num_samples * self.num_replicas
        self.shuffle = shuffle
        self.seed = seed

    def __iter__(self):
        if self.shuffle:
            g = torch.Generator()
            g.manual_seed(self.seed + self.epoch)
            indices = torch.randperm(len(self.dataset), generator=g).tolist()
        else:
            indices = list(range(len(self.dataset)))
        if not self.drop_last:
            padding_size = self.total_size - len(indices)
            if padding_size <= len(indices):
                indices += indices[:padding_size]
            else:
                indices += (indices * math.ceil(padding_size / len(indices)))[:padding_size]
        else:
            indices = indices[:self.total_size]
        if len(indices) != self.total_size:
            raise AssertionError("Number of indices (%d) does not match total_size (%d)" % (len(indices), self.total_size))
        indices = indices[self.rank:self.total_size:self.num_replicas]
        if len(indices) != self.num_samples:
            raise AssertionError("Number of subsampled indices (%d) does not match num_samples (%d)" % (len(indices), self.num_samples))
        return iter(indices)

    def __len__(self):
        return self.num_samples

    def set_epoch(self, epoch):
        self.epoch = epoch


__all__.append("DistributedSampler")
