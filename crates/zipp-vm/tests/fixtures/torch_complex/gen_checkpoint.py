# Writes complex.pt with CPU PyTorch 2.11 (python gen_checkpoint.py):
# complex64/complex128 tensors, a transposed and a sliced view (stored
# strided), a Parameter and complex dtypes, for python_torch_complex.rs.
import torch

z = torch.complex(torch.arange(6.0).reshape(2, 3), torch.arange(6.0).reshape(2, 3) * -0.5)
torch.save({"c64": z, "c128": z.to(torch.complex128) * 1.25, "T": z.T, "slice": z[:, 1:], "dt": [torch.complex64, torch.cdouble],
            "p": torch.nn.Parameter(z[0].clone())}, "complex.pt")
