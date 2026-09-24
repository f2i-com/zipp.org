# torch.distributed parity cases for one process: availability, errors
# before initialization, init_process_group's refusals and init methods
# (env://, tcp://, file://, a store), queries, every collective on a
# one-rank gloo group (sync and async_op), their argument errors, new_group,
# destroy_process_group, DistributedDataParallel on an initialized group,
# and DistributedSampler's index order (shuffle, seed, epoch, drop_last,
# padding, explicit replicas). Runs unchanged under PyTorch 2.11 (gen.py
# writes text_expected.txt) and under Zipp (python_torch_distributed.rs).
# `setup(path)` gets the scratch path file:// uses.
import math
import os

import torch
import torch.distributed as dist
import torch.nn as nn
from torch.utils.data import DataLoader, DistributedSampler, TensorDataset

try:
    import warnings

    warnings.simplefilter("ignore")
except ImportError:
    pass

PORT = [29561]


def err(fn):
    try:
        r = fn()
    except Exception as e:
        return "ERR %s: %s" % (type(e).__name__, str(e).split("\n")[0])
    return "ok %s" % (r if not isinstance(r, torch.Tensor) else r.tolist(),)


def env(addr=True):
    for k in ("MASTER_ADDR", "MASTER_PORT", "RANK", "WORLD_SIZE", "LOCAL_RANK"):
        os.environ.pop(k, None)
    if addr:
        os.environ["MASTER_ADDR"] = "127.0.0.1"
        PORT[0] += 1
        os.environ["MASTER_PORT"] = str(PORT[0])


def vals(n, seed):
    return [math.sin(0.9 * i + seed) + 0.3 * math.cos(2.3 * i + 2 * seed) for i in range(n)]


def text(scratch):
    T = []
    p = T.append
    p("%s %s %s %s %s" % (dist.is_available(), dist.is_initialized(), dist.is_gloo_available(), dist.is_nccl_available(), dist.is_mpi_available()))
    p("%s %s" % (dist.is_torchelastic_launched(), dist.Backend("GLOO")))
    for op in (dist.ReduceOp.SUM, dist.ReduceOp.AVG, dist.ReduceOp.PRODUCT, dist.ReduceOp.MIN, dist.ReduceOp.MAX, dist.ReduceOp.BAND,
               dist.ReduceOp.BOR, dist.ReduceOp.BXOR):
        p("%r %s" % (op, op))
    p("%s %s" % (dist.ReduceOp.SUM == dist.ReduceOp.SUM, dist.ReduceOp.SUM == dist.ReduceOp.MAX))
    for f in (dist.get_rank, dist.get_world_size, dist.get_backend, dist.barrier, lambda: dist.all_reduce(torch.ones(2)),
              lambda: dist.new_group([0]), lambda: dist.destroy_process_group()):
        p(err(f))
    env(addr=False)
    p(err(lambda: dist.init_process_group("gloo", rank=0, world_size=1)))
    os.environ["MASTER_ADDR"] = "127.0.0.1"
    p(err(lambda: dist.init_process_group("gloo", rank=0, world_size=1)))
    p(err(lambda: dist.init_process_group("gloo", world_size=1)))
    p(err(lambda: dist.init_process_group("gloo", rank=0)))
    env()
    p(err(lambda: dist.init_process_group("nccl", rank=0, world_size=1)))
    p(err(lambda: dist.init_process_group("mpi", rank=0, world_size=1)))
    p(err(lambda: dist.init_process_group("bogus", rank=0, world_size=1)))
    p(err(lambda: dist.init_process_group("gloo", init_method="tcp://127.0.0.1:1", world_size=1)))
    p(str(dist.is_initialized()))
    # env:// with the variables
    env()
    os.environ["RANK"] = "0"
    os.environ["WORLD_SIZE"] = "1"
    p(err(lambda: dist.init_process_group("gloo")))
    p("%s %s %s %s" % (dist.is_initialized(), dist.get_rank(), dist.get_world_size(), dist.get_backend()))
    p(err(lambda: dist.init_process_group("gloo", rank=0, world_size=1)))
    p(type(dist.group.WORLD).__name__)
    p("%s %s" % (dist.get_process_group_ranks(dist.group.WORLD), dist.get_global_rank(dist.group.WORLD, 0)))
    # collectives
    t = torch.tensor(vals(5, 0.3))
    for op in (dist.ReduceOp.SUM, dist.ReduceOp.AVG, dist.ReduceOp.PRODUCT, dist.ReduceOp.MIN, dist.ReduceOp.MAX):
        x = t.clone()
        r = dist.all_reduce(x, op=op)
        p("all_reduce %s %s %s" % (op, r, x.tolist()))
    i = torch.tensor([6, 3, 5])
    for op in (dist.ReduceOp.BAND, dist.ReduceOp.BOR, dist.ReduceOp.BXOR, dist.ReduceOp.PRODUCT):
        x = i.clone()
        dist.all_reduce(x, op=op)
        p("all_reduce int %s %s" % (op, x.tolist()))
    p(err(lambda: dist.all_reduce(t.clone(), op=dist.ReduceOp.BAND)))
    x = t.clone()
    w = dist.all_reduce(x, async_op=True)
    p("async %s %s %s %s" % (type(w).__name__, w.wait(), w.is_completed(), x.tolist()))
    x = t.clone()
    p("broadcast %s %s" % (dist.broadcast(x, 0), x.tolist()))
    w = dist.broadcast(x, src=0, async_op=True)
    p("broadcast async %s" % (w.wait(),))
    p(err(lambda: dist.broadcast(t.clone(), 1)))
    out = [torch.zeros(5)]
    p("all_gather %s %s" % (dist.all_gather(out, t), [o.tolist() for o in out]))
    p(err(lambda: dist.all_gather([torch.zeros(5), torch.zeros(5)], t)))
    o2 = torch.zeros(5)
    p("all_gather_into_tensor %s %s" % (dist.all_gather_into_tensor(o2, t), o2.tolist()))
    p(err(lambda: dist.all_gather_into_tensor(torch.zeros(6), t)))
    objs = [None]
    dist.all_gather_object(objs, {"a": [1, 2], "b": "c"})
    p("all_gather_object %s" % (objs,))
    o3 = torch.zeros(5)
    p("reduce_scatter %s %s" % (dist.reduce_scatter(o3, [t.clone()]), o3.tolist()))
    o4 = torch.zeros(5)
    p("reduce_scatter_tensor %s %s" % (dist.reduce_scatter_tensor(o4, t.clone()), o4.tolist()))
    x = t.clone()
    p("reduce %s %s" % (dist.reduce(x, 0), x.tolist()))
    g = [torch.zeros(5)]
    p("gather %s %s" % (dist.gather(t, g, dst=0), [v.tolist() for v in g]))
    p(err(lambda: dist.gather(t, None, dst=0)))
    s = torch.zeros(5)
    p("scatter %s %s" % (dist.scatter(s, [t * 2], src=0), s.tolist()))
    p(err(lambda: dist.scatter(torch.zeros(5), None, src=0)))
    p(err(lambda: dist.all_to_all([torch.zeros(5)], [t.clone()])))
    o5 = torch.zeros(5)
    p("all_to_all_single %s %s" % (dist.all_to_all_single(o5, t.clone()), o5.tolist()))
    lst = [1, "x", {"k": 2}]
    p("broadcast_object_list %s %s" % (dist.broadcast_object_list(lst, src=0), lst))
    gl = [None]
    dist.gather_object({"z": 1}, gl, dst=0)
    p("gather_object %s" % (gl,))
    so = [None]
    dist.scatter_object_list(so, ["only"], src=0)
    p("scatter_object_list %s" % (so,))
    p(err(lambda: dist.send(t, 0)))
    p("barrier %s %s" % (dist.barrier(), dist.monitored_barrier()))
    w = dist.barrier(async_op=True)
    p("barrier async %s" % (w.wait(),))
    pg = dist.new_group([0])
    p("new_group %s %s %s %s" % (type(pg).__name__, dist.get_rank(pg), dist.get_world_size(pg), dist.get_backend(pg)))
    x = t.clone()
    dist.all_reduce(x, group=pg)
    p("group all_reduce %s" % (x.tolist(),))
    p(err(lambda: dist.new_group([0, 1])))
    p("local rank %s" % (dist.get_node_local_rank(fallback_rank=0),))
    # DistributedDataParallel on the initialized group
    torch.manual_seed(0)
    lin = nn.Sequential(nn.Linear(4, 3), nn.ReLU(), nn.Linear(3, 2))
    with torch.no_grad():
        for k, prm in enumerate(lin.parameters()):
            prm.copy_(torch.tensor(vals(prm.numel(), k + 0.5)).reshape(prm.shape) * 0.4)
    ref = nn.Sequential(nn.Linear(4, 3), nn.ReLU(), nn.Linear(3, 2))
    ref.load_state_dict(lin.state_dict())
    ddp = nn.parallel.DistributedDataParallel(lin)
    x = torch.tensor(vals(8, 1.7)).reshape(2, 4)
    y = ddp(x)
    y.sum().backward()
    ref(x).sum().backward()
    p("ddp %s %s %s %s" % (ddp.module is lin, torch.equal(y, ref(x)), all(torch.equal(a.grad, b.grad) for a, b in zip(lin.parameters(), ref.parameters())), sorted(ddp.state_dict().keys())))
    with ddp.no_sync():
        ddp(x).sum().backward()
    p("ddp no_sync %s" % ([round(v, 6) for v in lin[0].bias.grad.tolist()],))
    opt = torch.optim.SGD(ddp.parameters(), lr=0.1)
    opt.step()
    p("ddp step %s" % ([round(v, 6) for v in lin[2].weight.reshape(-1).tolist()],))
    # samplers
    for n, replicas, rank, shuffle, seed, epoch, drop in ((10, 3, 0, False, 0, 0, False), (10, 3, 2, False, 0, 0, False), (10, 3, 1, False, 0, 0, True),
                                                           (10, 4, 3, True, 0, 0, False), (10, 4, 3, True, 0, 1, False), (10, 4, 3, True, 7, 1, False),
                                                           (7, 5, 4, True, 3, 2, True), (3, 8, 7, False, 0, 0, False), (3, 8, 5, True, 1, 0, False),
                                                           (100, 6, 2, True, 42, 5, False), (5, 1, 0, True, 0, 0, False)):
        sm = DistributedSampler(list(range(n)), num_replicas=replicas, rank=rank, shuffle=shuffle, seed=seed, drop_last=drop)
        sm.set_epoch(epoch)
        p("sampler %d %d %d %s %d %d %s -> %d %s" % (n, replicas, rank, shuffle, seed, epoch, drop, len(sm), list(sm)))
    sm = DistributedSampler(list(range(9)))
    p("sampler default %s %s %s" % (sm.num_replicas, sm.rank, list(sm)))
    p(err(lambda: DistributedSampler(list(range(4)), num_replicas=2, rank=2)))
    ds = TensorDataset(torch.arange(12.0).reshape(6, 2), torch.arange(6))
    loader = DataLoader(ds, batch_size=2, sampler=DistributedSampler(ds, seed=3))
    p("loader %s" % ([b[1].tolist() for b in loader],))
    # teardown and the other init methods
    p("destroy %s %s" % (dist.destroy_process_group(), dist.is_initialized()))
    for f in (dist.get_rank, lambda: dist.all_reduce(t.clone()), lambda: dist.destroy_process_group()):
        p(err(f))
    env()
    PORT[0] += 1
    p(err(lambda: dist.init_process_group("gloo", init_method="tcp://127.0.0.1:%d" % PORT[0], rank=0, world_size=1)))
    p("tcp %s %s %s" % (dist.get_rank(), dist.get_world_size(), dist.get_backend()))
    dist.destroy_process_group()
    p(err(lambda: dist.init_process_group("gloo", init_method="file://" + scratch, rank=0, world_size=1)))
    p("file %s %s %s" % (dist.get_rank(), dist.get_world_size(), dist.get_backend()))
    dist.destroy_process_group()
    p(err(lambda: dist.init_process_group("gloo", store=dist.FileStore(scratch + "_s1", 1), rank=0, world_size=1)))
    p("store %s %s %s" % (dist.get_rank(), dist.get_world_size(), dist.get_backend()))
    dist.destroy_process_group()
    env()
    p(err(lambda: dist.init_process_group(rank=0, world_size=1)))
    p("default backend %s" % (dist.get_backend(),))
    dist.destroy_process_group()
    st = dist.FileStore(scratch + "_s2", 1)
    st.set("a", "1")
    p("hashstore %s %s %s %s" % (st.get("a"), st.add("n", 3), st.add("n", 2), st.num_keys()))
    return T
