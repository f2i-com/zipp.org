// Compare small, spilled, duplicate-key and reviver parses with Node.
'use strict';
const assert = require('node:assert/strict');
const vm = require('node:vm');
const {Engine} = require('./pkg/zipp_wasm.js');
const source = String.raw`
  for (const count of [0,1,2,3,4,5,6,7,8,9,10,16,257,4096]) {
    const fields = [];
    for (let i=0;i<count;i++) fields.push('"k'+i+'":'+i);
    const text = '{'+fields.join(',')+'}';
    const plain = JSON.parse(text);
    let hash=0;
    for (const key of Object.keys(plain)) hash=(hash*31+plain[key])%1000000007;
    let visits=0, sourceHash=0;
    const revived=JSON.parse(text,function(key,value,context){
      visits++;
      if(typeof value==='number') {
        if(context.source!==String(value)) throw Error('source:'+key);
        sourceHash=(sourceHash*31+value)%1000000007;
      }
      return value;
    });
    print(count,Object.keys(plain).length,hash,visits,sourceHash,JSON.stringify(plain)===JSON.stringify(revived));
  }
  const duplicate='{"b":1,"2":2,"a":3,"b":4,"1":5,"a":6,"c":7,"b":8}';
  const log=[];
  const dup=JSON.parse(duplicate,function(k,v,c){if(typeof v==='number')log.push(k+':'+c.source);return v;});
  print(JSON.stringify(dup),log.join('|'));
  const special=JSON.parse('{"__proto__":1,"constructor":2,"@@x":3,"a\u0062":4,"ab":5,"é☃":6}');
  print(Object.getPrototypeOf(special)===Object.prototype,JSON.stringify(special));
  const nested=JSON.parse('{"a":{"b":[{}, {"c":1,"d":2,"e":3,"f":4,"g":5}]}}');
  print(JSON.stringify(nested));
  for(const bad of ['{','{"a":1,','{"a":1,"b":2,"c":3,"d":4,"e":5,','{"a":1,"b":2,"c":3,"d":4,"e":5,"f":','{"a":1,}']) {
    for(const reviver of [undefined,function(k,v){return v;}]) {
      let threw=false;
      try{JSON.parse(bad,reviver);}catch(e){threw=e instanceof SyntaxError;}
      if(!threw)throw Error('invalid JSON accepted');
    }
  }
  print('malformed rejected');
`;
const expected=[];
vm.runInNewContext(source,{print:(...args)=>expected.push(args.join(' '))});
const e=new Engine();
try { e.initScript(source); assert.deepEqual(e.takeOutput(),expected); }
finally { e.dispose(); e.free(); }
const depthCheck=new Engine();
try {
  depthCheck.initScript(`
    const good='{"x":'.repeat(48)+'0'+'}'.repeat(48);
    const bad='{"x":'.repeat(256)+'0'+'}'.repeat(256);
    for(const reviver of [undefined,function(k,v){return v;}]) {
      let value=JSON.parse(good,reviver);
      for(let i=0;i<48;i++)value=value.x;
      if(value!==0)throw Error('nested value');
      let bounded=false;
      try{JSON.parse(bad,reviver);}catch(e){bounded=e instanceof RangeError;}
      if(!bounded)throw Error('missing depth bound');
    }
    print('bounded and usable',1+2);
  `);
  assert.deepEqual(depthCheck.takeOutput(),['bounded and usable 3']);
} finally { depthCheck.dispose(); depthCheck.free(); }
console.log('small/spilled JSON objects, duplicate keys and reviver sources match Node');
