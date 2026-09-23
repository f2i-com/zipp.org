# fixtures/torch_training.py (`expected_text` defined before) as ordinary
# synchronous code: five chained compiled training steps against PyTorch
# 2.11, a graph whose result is not finite, and an inference call.
import json
expected_steps = json.loads(expected_text)
compiled = torch.compile(train_step, training=True)
deviations = []
for i in range(5):
    pending = compiled(inputs, targets)
    pending.submit(lambda loss: deviations.append(max(abs(a - b) for a, b in zip(state(loss), expected_steps[i]))))
assert len(deviations) == 5 and max(deviations) < 2e-6, deviations
print("training steps", len(deviations), "max-dev", repr(max(deviations)), "backend", pending.backend)
before = [p.detach().clone() for p in model.parameters()]
try:
    torch.compile(lambda x: torch.log(x))(torch.tensor([-1.0, 2.0])).submit(print)
    print("non-finite ACCEPTED")
except Exception as error:
    print("non-finite readback", type(error).__name__, error.code, "model unchanged",
          all(torch.equal(a, b) for a, b in zip(before, model.parameters())))
model.eval()
with torch.no_grad():
    eager = model(inputs)
pending = torch.compile(model)(inputs)
pending.submit(lambda out: print("inference", pending.backend, "max-dev", repr((out - eager).abs().max().item())))
