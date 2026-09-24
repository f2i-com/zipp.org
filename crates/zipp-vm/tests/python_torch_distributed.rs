//! torch.distributed on one process, checked against CPU PyTorch 2.11.
//! `fixtures/torch_distributed/dist_cases.py` runs unchanged under both;
//! `gen.py` writes PyTorch's output to `text_expected.txt`: availability,
//! errors before initialization, init_process_group's refusals (nccl, mpi,
//! an unknown backend, missing env:// variables or tcp:// rank) and init
//! methods (env://, tcp://, file://, a store), every collective on a
//! one-rank gloo group with its argument errors, new_group,
//! destroy_process_group, DistributedDataParallel on the initialized group
//! (outputs and gradients identical to the bare module), and
//! DistributedSampler's index order.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run(source: &str) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &[], &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(2_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

#[test]
fn distributed_single_process_matches_pytorch() {
    let source = format!(
        "{}\nfor line in text('pg_store'):\n    print(line)\n",
        include_str!("fixtures/torch_distributed/dist_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("\n"));
    let expected = lines(include_str!("fixtures/torch_distributed/text_expected.txt"));
    for (i, (g, e)) in out.iter().zip(&expected).enumerate() {
        assert_eq!(g, e, "line {i}");
    }
    assert_eq!(out.len(), expected.len(), "{out:#?}");
}

/// What only Zipp does: a world_size above 1 (or a rank above 0) is
/// refused with an explanation instead of waiting for peers that cannot
/// exist; point-to-point to another rank or from any source raises; the
/// multi-process subpackages raise ImportError naming themselves;
/// DistributedDataParallel works without a process group (PyTorch requires
/// one); torch.distributed is reachable as an attribute after `import
/// torch`, and distributed_c10d names the same objects.
#[test]
fn distributed_zipp_specific_behaviour() {
    let out = run(r#"
import os
import torch
import torch.nn as nn

def err(fn):
    try:
        fn()
    except Exception as e:
        return "%s: %s" % (type(e).__name__, str(e))
    return "ok"

dist = torch.distributed
print(dist.is_available(), dist.is_initialized())
os.environ["MASTER_ADDR"] = "127.0.0.1"
os.environ["MASTER_PORT"] = "29500"
print(err(lambda: dist.init_process_group("gloo", rank=0, world_size=2)))
print(err(lambda: dist.init_process_group("gloo", rank=1, world_size=1)))
print(err(lambda: dist.init_process_group("gloo", init_method="tcp://10.0.0.1:23456?rank=0&world_size=4")))
print(dist.is_initialized())
ddp = nn.parallel.DistributedDataParallel(nn.Linear(2, 2))
print(ddp.process_group, err(lambda: ddp(torch.ones(1, 2)).sum().backward()))
print(err(lambda: torch.utils.data.DistributedSampler(list(range(4)))))
dist.init_process_group("gloo", init_method="tcp://127.0.0.1:23456?rank=0&world_size=1")
print(dist.get_world_size(), dist.get_backend())
t = torch.ones(3)
print(err(lambda: dist.send(t, 1)))
print(err(lambda: dist.recv(t, 0)))
print(err(lambda: dist.recv(t)))
print(err(lambda: dist.isend(t, 2)))
ddp = nn.parallel.DistributedDataParallel(nn.Linear(2, 2))
print(ddp.process_group is dist.group.WORLD)
import torch.distributed.distributed_c10d as c10d
print(c10d.get_rank is dist.get_rank, c10d.ReduceOp.SUM is dist.ReduceOp.SUM)
for name in ("torch.distributed.elastic", "torch.distributed.launch", "torch.distributed.run"):
    try:
        __import__(name)
        print(name, "imported")
    except ImportError as e:
        print(str(e).split(":")[0])
dist.destroy_process_group()
print(dist.is_initialized())
"#)
    .unwrap();
    assert_eq!(
        out,
        [
            "True False",
            "RuntimeError: torch.distributed on Zipp runs a single process: world_size must be 1 (rank 0), got world_size=2. Zipp cannot start or reach other processes; run with world_size=1 or use PyTorch for multi-process training.",
            "RuntimeError: torch.distributed on Zipp runs a single process: rank must be 0, got rank=1",
            "RuntimeError: torch.distributed on Zipp runs a single process: world_size must be 1 (rank 0), got world_size=4. Zipp cannot start or reach other processes; run with world_size=1 or use PyTorch for multi-process training.",
            "False",
            "None ok",
            "ValueError: Default process group has not been initialized, please make sure to call init_process_group.",
            "1 gloo",
            "ValueError: Invalid destination rank 1: Zipp runs one process (world_size 1), so rank 1 does not exist",
            "ValueError: Invalid source rank: source rank should not be the same as the rank of the current process.",
            "RuntimeError: recv from any source cannot complete: Zipp runs one process, so no other rank exists",
            "ValueError: Invalid destination rank 2: Zipp runs one process (world_size 1), so rank 2 does not exist",
            "True",
            "True True",
            "torch.distributed.elastic is not available on Zipp",
            "torch.distributed.launch is not available on Zipp",
            "torch.distributed.run is not available on Zipp",
            "False",
        ]
    );
}
