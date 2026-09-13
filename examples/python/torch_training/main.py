"""Async playground driver: forward, backward and SGD run on the selected GPU.

model.py uses ordinary Torch code. This file schedules steps and plots results.
Parameters are uploaded/read back each step; this is not resident CUDA training.
"""
import torch
import ui
from model import model, inputs, targets, train_step

compiled = torch.compile(train_step, training=True)
losses = []
predictions = []
status = "Preparing GPU training..."
busy = False
stopped = False
request = None
ui.canvas(640, 400)


def completed(loss):
    global busy, status, predictions
    losses.append(loss.item())
    status = request.backend + " | step " + str(len(losses)) + " / 100 | loss " + str(round(loss.item(), 6))
    # Prediction is a separate GPU inference, using the weights just updated.
    prediction = torch.compile(model)(inputs)
    prediction.submit(predicted, failed)
    if len(losses) == 1 or len(losses) % 25 == 0:
        print("GPU training:", status)


def predicted(value):
    global busy, predictions
    predictions = value.flatten().tolist()
    busy = False


def failed(error):
    global busy, stopped, status
    busy = False
    stopped = True
    status = "Training stopped: " + str(error)
    print(status)


def update():
    global busy, request
    if busy or stopped or len(losses) >= 100:
        return
    busy = True
    request = compiled(inputs, targets)
    request.submit(completed, failed)


def draw():
    ui.clear("#0b0e14")
    ui.font(20)
    ui.text(20, 30, "Python trains a neural network on Zipp WASM", "#e6edf3")
    ui.font(14)
    ui.text(20, 56, status, "#72dfbb")
    ui.text(20, 90, "Fit y = x squared", "#e6edf3")
    ui.text(350, 90, "Training loss", "#e6edf3")
    ui.line(20, 320, 300, 320, "#39465a")
    ui.line(350, 320, 620, 320, "#39465a")
    expected = targets.flatten().tolist()
    for i, value in enumerate(expected):
        x = 24 + i * 11
        ui.rect(x, 316 - value * 200, 5, 5, "#9caabc")
        if i < len(predictions):
            ui.rect(x, 316 - predictions[i] * 200, 5, 5, "#72dfbb")
    scale = max(losses[0], 0.001) if losses else 1.0
    for i in range(1, len(losses)):
        ui.line(350 + (i-1)*2.7, 316-losses[i-1]/scale*200, 350+i*2.7, 316-losses[i]/scale*200, "#66c6ff")
    ui.text(20, 350, "Gray: target    Green: learned prediction", "#9caabc")
    ui.text(20, 378, "Linear + ReLU | MSE | SGD | GPU forward, backward and updates", "#9caabc")
