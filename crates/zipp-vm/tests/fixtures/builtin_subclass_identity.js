(() => {
  for (const Base of [Array, Boolean, Number, String, Date, RegExp, Map, Set, Promise]) {
    const shared = {tag: Base.name};
    let calls = 0, target;
    function Parent() { calls++; target = new.target; return shared; }
    Parent.prototype = Base.prototype;
    class Child extends Parent { field = 42; }
    const result = new Child();
    console.log('user:' + Base.name + ':' + (result === shared) + ':' +
      calls + ':' + (target === Child) + ':' + shared.field);
  }
})();
