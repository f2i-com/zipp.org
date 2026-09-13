"""Conway's Game of Life using ordinary PyTorch operations.

The grid wraps at every edge. No custom GPU API is needed for these rules.
"""
import torch


def neighbor_matrix(size):
    # Each row selects its two neighboring rows, including wraparound.
    identity = torch.eye(size)
    return torch.roll(identity, 1, 0) + torch.roll(identity, -1, 0)


def exactly(count, value):
    # A triangular pulse: 1 at value, 0 at every other integer count.
    return (torch.relu(count - (value - 1))
            - 2 * torch.relu(count - value)
            + torch.relu(count - (value + 1)))


def step(cells, neighbors):
    # Matrix products count horizontal, vertical and diagonal neighbors.
    horizontal = cells @ neighbors
    count = horizontal + neighbors @ cells + neighbors @ horizontal

    # Birth with three neighbors; survival with two or three.
    return exactly(count, 3) + cells * exactly(count, 2)
