import torch
from torch import nn
import torch.nn.functional as F

model = nn.Sequential(nn.Linear(2, 3), nn.ReLU(), nn.Linear(3, 1))
with torch.no_grad():
    model[0].weight.copy_(torch.tensor([[0.5, -0.25], [-0.75, 0.5], [0.25, 0.25]]))
    model[0].bias.copy_(torch.tensor([0.0, -0.25, 0.0]))
    model[2].weight.copy_(torch.tensor([[0.5, -0.5, 0.25]]))
    model[2].bias.copy_(torch.tensor([0.125]))
inputs = torch.tensor([[1.0, 2.0], [-1.0, 0.5], [0.0, 0.0]])
targets = torch.tensor([[0.75], [-0.5], [0.0]])
optimizer = torch.optim.SGD(model.parameters(), lr=0.1, weight_decay=0.01)


def train_step(x, y):
    optimizer.zero_grad()
    loss = F.mse_loss(model(x), y)
    loss.backward()
    optimizer.step()
    return loss


def state(loss):
    return [loss.item()] + [value for p in model.parameters() for value in p.grad.flatten().tolist()] + [value for p in model.parameters() for value in p.detach().flatten().tolist()]
