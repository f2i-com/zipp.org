"""A familiar Torch model; Zipp records its inference as a compute graph.

Choose WebGL2 or WebGPU in the playground. Only submission/readback is
Zipp-specific: submit(callback) returns a regular CPU torch.Tensor asynchronously.
"""
import torch
from torch import nn
import ui

model = nn.Sequential(nn.Linear(2, 4), nn.ReLU(), nn.Linear(4, 1))
with torch.no_grad():
    for parameter in model.parameters():
        parameter.fill_(0.5)
model.eval()
inputs = torch.tensor([[0.0, 0.0], [1.0, 0.0], [0.0, 2.0], [2.0, 2.0]])
with torch.no_grad():
    expected = model(inputs)

prediction = torch.compile(model)(inputs)
values = []
status = "Submitting a Linear / ReLU / Linear graph..."
ui.canvas(520, 310)


def ready(output):
    global values, status
    values = output.flatten().tolist()
    status = "Torch inference: " + prediction.backend
    print(status)
    print("predictions:", output.tolist())
    print("matches eager:", torch.allclose(output, expected))


def failed(error):
    global status
    status = "Inference failed: " + str(error)
    print(status)


prediction.submit(ready, failed)


def draw():
    ui.clear("#0b0e14")
    ui.font(18)
    ui.text(20, 30, "Python + torch.nn running on Zipp WASM", "#e6edf3")
    ui.font(13)
    ui.text(20, 55, status, "#72dfbb")
    for index, value in enumerate(values):
        top = 85 + index * 48
        ui.rect(20, top, value * 36, 28, "#66c6ff")
        ui.text(28 + value * 36, top + 20, str(round(value, 2)), "#e6edf3")
    ui.text(20, 292, "Same model, same weights; selected backend computes inference.", "#9caabc")
