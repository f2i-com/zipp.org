// Internal errors retain ordinary property/prototype behavior with exact storage.
'use strict';
const assert=require('node:assert/strict');
const vm=require('node:vm');
const {Engine}=require('./pkg/zipp_wasm.js');
const source=String.raw`
  const cases=[
    [RangeError,function(){new Array(-1);}],
    [TypeError,function(){Object.defineProperty(null,'x',{});}],
    [RangeError,function(){new Uint8Array(-1);}],
    [SyntaxError,function(){JSON.parse('{');}],
    [ReferenceError,function(){return missingInternalErrorTestName;}]
  ];
  for(const pair of cases){
    const C=pair[0], old=Object.getOwnPropertyDescriptor(C.prototype,'name');
    let reads=0;
    Object.defineProperty(C.prototype,'name',{configurable:true,get(){reads++;return 'observed';}});
    let e;try{pair[1]();}catch(caught){e=caught;}
    const d=Object.getOwnPropertyDescriptor(e,'message');
    print(reads,e instanceof C,Object.getPrototypeOf(e)===C.prototype,Object.hasOwn(e,'name'),typeof d.value,d.writable,d.enumerable,d.configurable);
    print(e.name,reads);
    e.message='replacement'; e.extra=17;
    print(e.toString(),e.extra);
    delete e.message;print(e.message);
    Object.defineProperty(C.prototype,'name',old);
  }
  const effects=[];
  const message={toString(){effects.push('message');return 'text';}};
  const options={get cause(){effects.push('cause');return 7;}};
  const e=new TypeError(message,options);
  const cause=Object.getOwnPropertyDescriptor(e,'cause');
  print(effects.join(','),e.message,cause.value,cause.writable,cause.enumerable,cause.configurable);
  print(Object.hasOwn(new Error(),'message'),Object.hasOwn(new Error(undefined),'message'),new Error(null).message);
  class Custom extends Error{}
  const custom=new Custom('custom',{cause:9});
  print(custom instanceof Custom,Object.getPrototypeOf(custom)===Custom.prototype,custom.message,custom.cause);
`;
const expected=[];
vm.runInNewContext(source,{print:(...args)=>expected.push(args.join(' '))});
const e=new Engine();
try{e.initScript(source);assert.deepEqual(e.takeOutput(),expected);}
finally{e.dispose();e.free();}
console.log('internal error properties, prototype effects and general constructors match Node');
