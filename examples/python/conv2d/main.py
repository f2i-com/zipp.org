"""Ordinary Torch CPU convolution and training, also supported by Zipp."""
import torch
from torch import nn
import torch.nn.functional as F

torch.manual_seed(7)
model = nn.Conv2d(1, 2, kernel_size=3, padding=1)
optimizer = torch.optim.SGD(model.parameters(), lr=0.1)
image = torch.arange(25, dtype=torch.float32).reshape(1, 1, 5, 5) / 25
target = torch.zeros(1, 2, 5, 5)

for step in range(10):
    optimizer.zero_grad()
    prediction = model(image)
    loss = F.mse_loss(prediction, target)
    loss.backward()
    optimizer.step()
    print("step", step, "loss", round(loss.item(), 6))

print("output shape:", model(image).shape)
