# fixtures/torch_prepared.py (`case`, `expected` defined before) as ordinary
# synchronous code: six distinct batches as one prepared session (two single
# steps, the rest in one run, then sync), compared with PyTorch 2.11.
compiled = torch.compile(train_step, training=True)
losses = []
if optimizer_kind == "momentum":
    # Dampening: PyTorch's first step clones the gradient into the buffer.
    compiled(*batches[0]).submit(lambda loss: losses.append(loss.item()))
prepared = compiled.prepare(*batches[len(losses)])
prepared.step(lambda loss: losses.append(loss.item()), *batches[len(losses)])
prepared.step(lambda loss: losses.append(loss.item()), *batches[len(losses)])
prepared.steps(lambda results: losses.extend(r.item() for r in results), batches[len(losses):])
worst_loss = max(abs(l - expected[i][0]) for i, l in enumerate(losses))
synced = []
prepared.sync(synced.append)
assert synced == [prepared]
actual = state(torch.tensor(losses[-1]))
worst = max(abs(a - b) for a, b in zip(actual, expected[-1]))
assert worst_loss < 2e-6 and worst < 2e-6, (worst_loss, worst)
backend = prepared.backend
executed = prepared.executed
prepared.dispose()
print(case, "losses", len(losses), "loss-dev", repr(worst_loss), "state-dev", repr(worst), "executed", executed, "backend", backend)
