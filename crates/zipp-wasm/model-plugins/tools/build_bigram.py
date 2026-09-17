#!/usr/bin/env python3
"""Regenerate the tiny deterministic table fixture after manifest updates."""
from pathlib import Path
import hashlib,json,struct
r=Path(__file__).resolve().parents[1]
f=r/'examples/bigram';f.mkdir(exist_ok=True)
vocab=['<bos>','<eos>','<unk>','a','b','c','.'];v=len(vocab)
w=[-8.0]*(v*v)
for a,b in [(0,3),(3,4),(4,5),(5,6),(6,1),(1,1),(2,1)]:w[a*v+b]=8.0
header=json.dumps({'transition_logits':{'dtype':'F32','shape':[v,v],'data_offsets':[0,v*v*4]}},separators=(',',':')).encode();header+=b' '*((-len(header))%8)
raw=struct.pack('<Q',len(header))+header+struct.pack('<'+'f'*len(w),*w)
(f/'weights.safetensors').write_bytes(raw)
p=(r/'plugins/bigram/plugin.json').read_bytes()
model={'format':'zipp.local-model','version':1,'architecture':{'id':'org.zipp.bigram','version':'0.1.0','sha256':hashlib.sha256(p).hexdigest()},'config':{'vocab_size':v,'context_length':32},'tokenizer':{'type':'character-v1','vocab':vocab,'bos_token_id':0,'eos_token_id':1,'unk_token_id':2},'weights':[{'path':'weights.safetensors','sha256':hashlib.sha256(raw).hexdigest()}],'license':'CC0-1.0; hand-authored deterministic integration fixture, not a trained LLM'}
(f/'model.json').write_text(json.dumps(model,indent=2)+'\n')
print('Bigram:',len(raw),'bytes; expected generation abc.')
