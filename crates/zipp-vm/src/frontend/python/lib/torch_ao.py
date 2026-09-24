"""torch.ao for Zipp: the package holding torch.ao.quantization and
torch.ao.nn (quantized, dynamic, intrinsic and QAT modules)."""
import torch._quant as _q

quantization = _q._LazyModule("torch.ao.quantization")
nn = _q._LazyModule("torch.ao.nn")
