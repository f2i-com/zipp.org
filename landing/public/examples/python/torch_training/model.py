"""Ordinary Torch model and training step; also runs in native PyTorch."""
import torch
from torch import nn
import torch.nn.functional as F

model = nn.Sequential(nn.Linear(1, 8), nn.ReLU(), nn.Linear(8, 1))
# Explicit initialization makes this tiny regression demo reproducible.
with torch.no_grad():
    model[0].weight.copy_(torch.tensor([[1.0], [-1.0], [0.5], [-0.5], [1.5], [-1.5], [0.75], [-0.75]]))
    model[0].bias.fill_(0.25)
    model[2].weight.fill_(0.1)
    model[2].bias.fill_(0.0)
inputs = torch.linspace(-1.0, 1.0, 24).reshape(24, 1)
targets = inputs * inputs
optimizer = torch.optim.SGD(model.parameters(), lr=0.15)


def train_step(x, y):
    optimizer.zero_grad()
    loss = F.mse_loss(model(x), y)
    loss.backward()
    optimizer.step()
    return loss
