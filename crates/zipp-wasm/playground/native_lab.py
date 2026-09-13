"""Fixed local NCA workloads. JSON-lines stdout is consumed by native-lab.cjs.

The model implementations and supplied checkpoints come from NCA_LAB_DIR.
Each process owns one selected CUDA device (or explicitly selected CPU).
"""
import argparse
import json
import os
from pathlib import Path
import sys
import time


def emit(kind, **data):
    print(json.dumps(dict(type=kind, **data), allow_nan=False), flush=True)


def probe():
    import torch
    devices = []
    for i in range(torch.cuda.device_count()):
        row = dict(id=f"cuda:{i}", name=torch.cuda.get_device_name(i),
                   uuid=str(getattr(torch.cuda.get_device_properties(i), 'uuid', '')),
                   totalMiB=round(torch.cuda.get_device_properties(i).total_memory / 2**20))
        try:
            with torch.cuda.device(i):
                x = torch.arange(16, device=f"cuda:{i}", dtype=torch.float32).reshape(4, 4)
                assert (x @ x.T).sum().item() == 3680
                torch.cuda.synchronize(i)
            row['usable'] = True
        except Exception as error:
            row.update(usable=False, error=str(error))
        devices.append(row)
    emit('probe', torch=str(torch.__version__), cuda=torch.version.cuda, devices=devices,
         lab=str(Path(os.environ['NCA_LAB_DIR']).resolve()),
         labAvailable=(Path(os.environ['NCA_LAB_DIR']) / 'cellular_memory.py').is_file())


def move_episode(ep, device):
    from dataclasses import fields
    import torch
    for field in fields(ep):
        value = getattr(ep, field.name)
        if isinstance(value, torch.Tensor):
            setattr(ep, field.name, value.to(device))
    return ep


def grid(tensor):
    x = tensor.detach().float().cpu()
    return dict(width=x.shape[-1], height=x.numel() // x.shape[-1], values=x.reshape(-1).tolist())


def memory(a, lab, out):
    import torch
    import torch.nn.functional as F
    from cellular_memory import CellularMemory, MemoryConfig
    from tasks import sample, ingest
    from run_memory import fingerprint, score
    if a.mode == 'memory-train':
        model = CellularMemory(MemoryConfig()).to(a.device)
        opt = torch.optim.AdamW(model.parameters(), lr=.003, weight_decay=0.)
        rng = torch.Generator().manual_seed(10000 + a.seed)
        with (out / 'training.jsonl').open('w', encoding='utf-8') as log:
            for step in range(1, a.steps + 1):
                model.train()
                ep = move_episode(sample(a.batch, 8, 8, rng, overwrite=step % 3 == 0), a.device)
                opt.zero_grad(set_to_none=True)
                state = ingest(model, ep)
                logits = model.query(state, ep.query_symbol, ep.query_origin)
                loss = F.binary_cross_entropy_with_logits(logits, ep.target)
                if not torch.isfinite(loss):
                    raise RuntimeError('Nonfinite memory loss')
                loss.backward()
                torch.nn.utils.clip_grad_norm_(model.parameters(), 1.)
                opt.step()
                row = dict(step=step, loss=loss.item(), **score(logits.detach(), ep.target))
                log.write(json.dumps(row) + '\n')
                if step == 1 or step % max(1, a.steps // 100) == 0 or step == a.steps:
                    emit('frame', phase='training', **row, grid=grid(state[0]))
        torch.save(dict(model=model.state_dict(), config=model.config_dict()), out / 'model.pt')
    else:
        saved = torch.load(lab / 'results/fast_seed0/model.pt', map_location='cpu', weights_only=True)
        model = CellularMemory(MemoryConfig(**saved['config'])).to(a.device)
        model.load_state_dict(saved['model'])
    model.eval()
    model.requires_grad_(False)
    before = fingerprint(model)
    ep = move_episode(sample(1, 8, 8, torch.Generator().manual_seed(92312 + a.seed)), a.device)
    with torch.no_grad():
        state = model.initial_memory(1, ep.cells)
        for j in range(ep.symbols.shape[1]):
            state = model.write_event(state, ep.symbols[:, j], ep.values[:, j], ep.positions[:, j])
            emit('frame', phase='write', step=j + 1, cell=int(ep.positions[0, j]),
                 key=int(ep.symbols[0, j]), grid=grid(state[0]))
            time.sleep(a.frame_ms / 1000)
        logits, trace = model.query(state, ep.query_symbol, ep.query_origin, return_trace=True)
        for i, packet in enumerate(trace):
            emit('frame', phase='query', step=i + 1, cell=int(packet['valid'][0, :, 0].argmax()),
                 key=int(ep.query_symbol[0]), grid=grid(state[0]), packet=grid(packet['acc'][0]))
            time.sleep(a.frame_ms / 1000)
    bits = lambda x: ''.join(str(int(v)) for v in x)
    result = dict(observed=bits(ep.target[0]), answer=bits(logits[0] > 0),
                  writer=int(ep.source_position[0]), reader=int(ep.query_origin[0]),
                  sharedWeightsUnchanged=before == fingerprint(model), **score(logits, ep.target))
    assert result['sharedWeightsUnchanged']
    emit('result', **result)
    return result


def language(a, lab, out):
    import torch
    import torch.nn.functional as F
    from causal_lm import CausalNCALM, LMConfig
    from train_language import make_batch, evaluate
    validation_result = None
    last_state = [None]
    def build(config):
        model = CausalNCALM(config).to(a.device)
        model.head.register_forward_pre_hook(lambda module, args: last_state.__setitem__(0, args[0].detach()))
        return model
    if a.mode == 'language-train':
        tr_bytes = (lab / 'toy_text/train.txt').read_bytes()
        va_bytes = (lab / 'toy_text/validation.txt').read_bytes()
        if tr_bytes == va_bytes or min(len(tr_bytes), len(va_bytes)) <= a.length:
            raise ValueError('Need distinct training/validation files longer than the sequence')
        train = torch.tensor(list(tr_bytes), dtype=torch.long)
        validation = torch.tensor(list(va_bytes), dtype=torch.long)
        model = build(LMConfig())
        opt = torch.optim.AdamW(model.parameters(), lr=.001, weight_decay=.01)
        rng = torch.Generator().manual_seed(1000 + a.seed)
        with (out / 'training.jsonl').open('w', encoding='utf-8') as log:
            for step in range(1, a.steps + 1):
                model.train()
                x, y = make_batch(train, a.batch, a.length, rng, a.device)
                opt.zero_grad(set_to_none=True)
                logits = model(x)
                loss = F.cross_entropy(logits.reshape(-1, 256), y.reshape(-1))
                if not torch.isfinite(loss):
                    raise RuntimeError('Nonfinite language loss')
                loss.backward()
                torch.nn.utils.clip_grad_norm_(model.parameters(), 1.)
                opt.step()
                row = dict(step=step, loss=loss.item(), bitsPerByte=loss.item() / 0.6931471805599453)
                log.write(json.dumps(row) + '\n')
                if step == 1 or step % max(1, a.steps // 100) == 0 or step == a.steps:
                    # The head's input hook captures actual final recurrent activations.
                    emit('frame', phase='training', **row, grid=grid(last_state[0][0]))
        torch.save(dict(model=model.state_dict(), config=model.config_dict()), out / 'model.pt')
        validation_result = evaluate(model, validation, a.length)
        emit('validation', **validation_result)
    else:
        saved = torch.load(lab / 'language_toy_longer/model.pt', map_location='cpu', weights_only=True)
        model = build(LMConfig(**saved['config']))
        model.load_state_dict(saved['model'])
    model.eval()
    rng = torch.Generator(device=a.device).manual_seed(a.seed)
    cache = None
    text = bytearray(a.prompt.encode('utf-8'))
    with torch.no_grad():
        for byte in text:
            logits, cache = model.stream_step(torch.tensor([byte], device=a.device), cache)
        for i in range(a.bytes):
            token = torch.multinomial(torch.softmax(logits / a.temperature, -1), 1, generator=rng).flatten()
            text.append(int(token.item()))
            logits, cache = model.stream_step(token, cache)
            emit('frame', phase='generation', step=i + 1, text=text.decode('utf-8', errors='replace'),
                 grid=grid(torch.stack([stage[0, :, -1] for stage in cache])))
            time.sleep(a.frame_ms / 1000)
    result = dict(text=text.decode('utf-8', errors='replace'), maxContextBytes=model.max_context_bytes(),
                  note='Template-trained byte model; memory and language are separate experiments.')
    if validation_result is not None:
        result['validation'] = validation_result
    emit('result', **result)
    return result


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--probe', action='store_true')
    p.add_argument('--mode', choices=['memory', 'memory-train', 'language', 'language-train'])
    p.add_argument('--device', default='cuda:0')
    p.add_argument('--steps', type=int, default=100)
    p.add_argument('--batch', type=int, default=32)
    p.add_argument('--length', type=int, default=96)
    p.add_argument('--seed', type=int, default=0)
    p.add_argument('--bytes', type=int, default=160)
    p.add_argument('--prompt', default='The ')
    p.add_argument('--temperature', type=float, default=.3)
    p.add_argument('--frame-ms', type=int, default=80)
    p.add_argument('--out', type=Path)
    a = p.parse_args()
    if a.probe:
        return probe()
    if not a.mode or not a.out or not a.prompt:
        p.error('mode, out and nonempty prompt are required')
    import torch
    lab = Path(os.environ['NCA_LAB_DIR']).resolve()
    sys.path.insert(0, str(lab))
    torch.set_num_threads(2)
    torch.manual_seed(a.seed)
    if a.device.startswith('cuda:'):
        torch.cuda.set_device(a.device)
        # Exercise a kernel before claiming that a GPU job has started.
        torch.ones(1, device=a.device).add_(1).item()
    a.out.mkdir(parents=True, exist_ok=False)
    emit('started', device=a.device, name=torch.cuda.get_device_name(a.device) if a.device != 'cpu' else 'CPU',
         mode=a.mode, seed=a.seed, output=str(a.out), torch=str(torch.__version__))
    start = time.perf_counter()
    result = memory(a, lab, a.out) if a.mode.startswith('memory') else language(a, lab, a.out)
    if a.device != 'cpu':
        torch.cuda.synchronize(a.device)
    result.update(device=a.device, mode=a.mode, seed=a.seed, seconds=time.perf_counter() - start,
                  torch=str(torch.__version__), lab=str(lab),
                  options={key: value for key, value in vars(a).items() if key not in ('out', 'probe')})
    (a.out / 'metrics.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
    emit('done', **result)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        emit('error', message=str(error))
        raise
