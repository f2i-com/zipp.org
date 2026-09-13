"""Async training regressions. Works as an eager PyTorch reference as well."""
import torch
from torch import nn

p = nn.Parameter(torch.tensor([[2.0]]))
q = nn.Parameter(torch.tensor([[3.0]]))
p.grad = torch.tensor([[11.0]])
q.grad = torch.tensor([[7.0]])
x = torch.tensor([[1.0]])
optimizer = torch.optim.SGD([{"params": [p]}, {"params": []}], lr=0.1)


def step(x):
    optimizer.zero_grad()
    with torch.no_grad():
        detached_branch = (x @ p) * 100.0
    loss = (x @ p).sum() + detached_branch.sum()
    loss.backward()
    optimizer.step()
    return loss


def values():
    return [p.detach().item(), q.detach().item(),
            None if p.grad is None else p.grad.item(),
            None if q.grad is None else q.grad.item()]


def change_optimizer(kind):
    group = optimizer.param_groups[0]
    if kind in ['lr', 'weight_decay', 'momentum', 'dampening']:
        group[kind] = 0.5
    elif kind in ['maximize', 'nesterov']:
        group[kind] = True
    elif kind == 'append':
        group['params'].append(q)
    elif kind == 'replace':
        group['params'] = [q]
    elif kind == 'remove':
        group['params'].clear()
    elif kind == 'add_group':
        optimizer.add_param_group({'params': [q]})
    elif kind == 'reorder_groups':
        optimizer.param_groups.reverse()
    elif kind == 'remove_group':
        optimizer.param_groups.pop(0)
    elif kind == 'remove_option':
        del group['lr']
    else:
        raise ValueError(kind)
