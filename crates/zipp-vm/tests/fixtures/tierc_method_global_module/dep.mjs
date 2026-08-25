let moduleState = 0;

function moduleRandom() {
  moduleState = (moduleState + 1) | 0;
  return moduleState;
}

// These functions are installed through the ordinary eval/new-Function
// pipeline, not through the module loader's immutable function ranges. Their
// direct global access makes them tempting to the transactional method lane,
// but the exact jit_func_eligible boundary must reject both.
const evalRandom = (0, eval)(
  "(function evalRandom(){ Number = Number; return 23; })",
);
const dynamicRandom = Function("Number = Number; return 29;");

const moduleObj = { random: moduleRandom };
const evalObj = { random: evalRandom };
const dynamicObj = { random: dynamicRandom };

function invokeModule() {
  return moduleObj.random();
}

function invokeEval() {
  return evalObj.random();
}

function invokeDynamic() {
  return dynamicObj.random();
}

export function exercise() {
  let moduleValue = 0;
  let evalValue = 0;
  let dynamicValue = 0;
  for (let i = 0; i < 256; i++) moduleValue = invokeModule();
  for (let i = 0; i < 256; i++) evalValue = invokeEval();
  for (let i = 0; i < 256; i++) dynamicValue = invokeDynamic();
  return moduleValue + "|" + moduleState + "|" + evalValue + "|" + dynamicValue;
}
