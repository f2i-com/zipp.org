"""torch.ao.nn for Zipp: the package holding the quantized
(torch.ao.nn.quantized, .quantized.dynamic), fused (torch.ao.nn.intrinsic)
and quantization-aware training (torch.ao.nn.qat) modules."""
import torch._quant as _q

quantized = _q._LazyModule("torch.ao.nn.quantized")
intrinsic = _q._LazyModule("torch.ao.nn.intrinsic")
qat = _q._LazyModule("torch.ao.nn.qat")
