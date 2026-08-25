"use strict";

// A host-free, React-shaped application kernel: function components, hook-like
// captured state, keyed children, alternating prop layouts, handler closures,
// immutable trees, and short-lived patch objects. It deliberately does not use
// JSX or a DOM so both engines execute identical JavaScript and host work.
(function main() {
  const renders = 4200;
  const itemCount = 28;

  function element(type, props, children, key) {
    return { type, key: key == null ? null : key, props, children };
  }

  function makeStore() {
    let selected = 0;
    let revision = 0;
    const listeners = [];
    return {
      getState: () => ({ selected, revision }),
      dispatch(action) {
        if (action.type === "select") selected = action.id | 0;
        else revision = (revision + action.delta) | 0;
        for (let i = 0; i < listeners.length; i++) listeners[i](selected, revision);
      },
      subscribe(listener) {
        listeners.push(listener);
        return () => listeners.splice(listeners.indexOf(listener), 1);
      }
    };
  }

  function Item({ id, active, revision, onSelect }) {
    const props = active
      ? { className: "item active", id: "row-" + id, onClick: onSelect, tabIndex: 0 }
      : (id & 1)
        ? { id: "row-" + id, className: "item", onClick: onSelect }
        : { onClick: onSelect, className: "item", id: "row-" + id, title: "r" + revision };
    return element("li", props, ["item " + id + ":" + revision], id);
  }

  function App({ store, tick }) {
    const state = store.getState();
    const children = new Array(itemCount);
    for (let id = 0; id < itemCount; id++) {
      const onSelect = () => store.dispatch({ type: "select", id });
      children[id] = Item({
        id,
        active: id === state.selected,
        revision: state.revision,
        onSelect
      });
    }
    if (tick & 1) children.reverse();
    return element("section", { className: "app", tick }, [
      element("h1", { className: "title" }, ["Revision " + state.revision], "title"),
      element("ul", { role: "list" }, children, "items")
    ], "root");
  }

  function diff(previous, next, patches) {
    if (previous === next) return;
    if (typeof previous !== typeof next || previous == null || next == null) {
      patches.push({ op: "replace", value: next });
      return;
    }
    if (typeof next !== "object") {
      if (previous !== next) patches.push({ op: "text", value: next });
      return;
    }
    if (previous.type !== next.type || previous.key !== next.key) {
      patches.push({ op: "replace", value: next });
      return;
    }

    const oldProps = previous.props;
    const newProps = next.props;
    for (const key of Object.keys(newProps)) {
      if (oldProps[key] !== newProps[key]) patches.push({ op: "prop", key, value: newProps[key] });
    }
    for (const key of Object.keys(oldProps)) {
      if (!(key in newProps)) patches.push({ op: "remove", key });
    }

    const count = Math.max(previous.children.length, next.children.length);
    for (let i = 0; i < count; i++) diff(previous.children[i], next.children[i], patches);
  }

  const store = makeStore();
  let observed = 0;
  store.subscribe((selected, revision) => {
    observed = (observed + selected * 17 + revision) | 0;
  });

  let tree = App({ store, tick: 0 });
  let patchCount = 0;
  let checksum = 0;
  for (let tick = 1; tick <= renders; tick++) {
    if ((tick & 3) === 0) store.dispatch({ type: "select", id: tick % itemCount });
    if ((tick & 7) === 0) store.dispatch({ type: "revision", delta: (tick & 15) + 1 });
    const next = App({ store, tick });
    const patches = [];
    diff(tree, next, patches);
    patchCount += patches.length;
    for (let i = 0; i < patches.length; i++) {
      checksum = (Math.imul(checksum ^ patches[i].op.length, 33) + i) | 0;
    }
    tree = next;
  }

  const state = store.getState();
  console.log("reactish-reconcile", checksum, patchCount, observed, state.selected, state.revision);
})();
