"""A torch.distributed launcher Zipp does not provide (elastic, launch,
run): importing it raises ImportError naming it. They start or coordinate
several processes, and Zipp runs one; torch.distributed itself (process
groups, collectives, DistributedDataParallel) works with world_size 1.
The other multi-process subpackages (fsdp, rpc, pipelining, ...) are not
bundled, so importing them fails as a missing module."""
raise ImportError("%s is not available on Zipp: Zipp runs a single process, so torch.distributed supports only a world_size=1 process group (init_process_group, the collectives and DistributedDataParallel work on it); launchers, elastic agents, RPC and sharded training need several processes" % __name__)
