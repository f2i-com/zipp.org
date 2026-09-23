# The torch_gpu / torch_gpu2 / torch_gpu3 drivers of zipp-vm's own tests
# (`case`, `params`, `batches`, `train_step`, `state`, `expected` come from
# the fixture), unchanged but for printing the backend: chained compiled
# calls, eager steps and one prepared session, compared with PyTorch 2.11.
compiled = torch.compile(train_step, training=True)
initial = [p.detach().clone() for p in params]
def reset():
    for p, value in zip(params, initial):
        p.data = value.clone()
        p.grad = None
    optimizer.state.clear()
def worst(actual, wanted):
    assert len(actual) == len(wanted), (len(actual), len(wanted))
    return max(abs(a - b) / (1.0 + abs(b)) for a, b in zip(actual, wanted))
chained, chained_losses = [], []
def took(loss):
    chained_losses.append(loss.item())
    chained.append(state(loss))
for x, y in batches:
    pending = compiled(x, y)
    pending.submit(took)
dc = max(worst(s, e) for s, e in zip(chained, expected))
reset()
eager = [state(train_step(x, y)) for x, y in batches]
de = max(worst(s, e) for s, e in zip(eager, expected))
reset()
prepared = compiled.prepare(*batches[0])
resident = []
prepared.step(lambda loss: resident.append(loss.item()), *batches[0])
prepared.steps(lambda results: resident.extend(r.item() for r in results), batches[1:])
prepared.sync(lambda p: None)
final = state(torch.tensor(resident[-1]))
backend = prepared.backend
prepared.dispose()
dp = worst(final, expected[-1])
assert max(dc, de, dp) < 1e-6, (dc, de, dp)
print(case, "compiled", repr(dc), "eager", repr(de), "prepared", repr(dp), resident == chained_losses, final == chained[-1],
      "backend", pending.backend, backend)
