// Reduced from a declarative screen builder: the whole layout of a screen is
// one object literal, because the nesting IS the tree it describes. There is no
// flatter way to write a panel that contains a row that contains a list whose
// rows are built by a callback -- splitting it into named parts would only move
// the same depth into a chain of variables the reader has to reassemble.
//
// The completed-AST validator is what this file leans on: literals are parsed
// with a mix of recursion and iteration, but the tree the compiler and capture
// analysis then walk is exactly as deep as the layout is.

function officeScreen(state) {
  return {
    kind: "screen",
    id: "office",
    style: { fill: state.dark ? "#101014" : "#f5f5f2", pad: 12 },
    children: [
      {
        kind: "panel",
        id: "sidebar",
        style: { width: 220, gap: 8 },
        children: [
          {
            kind: "list",
            id: "tasks",
            items: state.tasks.filter(function (t) { return !t.hidden; }),
            row: function (task, index) {
              return {
                kind: "row",
                id: "task_" + index,
                style: { gap: 6, opacity: task.done ? 0.5 : 1 },
                children: [
                  {
                    kind: "icon",
                    glyph: task.done ? "check" : (task.blocked ? "lock" : "dot"),
                    style: { tint: task.blocked ? "#c04040" : "#606060" }
                  },
                  {
                    kind: "column",
                    children: [
                      { kind: "text", text: task.title, style: { weight: 600 } },
                      {
                        kind: "text",
                        text: task.due ? ("due " + formatDay(task.due)) : "no date",
                        style: { size: 11, tint: task.overdue ? "#c04040" : "#909090" }
                      }
                    ]
                  }
                ]
              };
            }
          }
        ]
      },
      {
        kind: "panel",
        id: "board",
        children: state.columns.map(function (column) {
          return {
            kind: "column",
            id: "col_" + column.key,
            children: [
              { kind: "text", text: column.title },
              {
                kind: "list",
                items: column.cards,
                row: function (card) {
                  return {
                    kind: "card",
                    id: card.id,
                    style: {
                      border: card.flagged
                        ? { width: 2, tint: "#c0a040" }
                        : { width: 1, tint: "#d0d0d0" }
                    },
                    children: [
                      { kind: "text", text: card.title },
                      {
                        kind: "row",
                        children: card.tags.map(function (tag) {
                          return { kind: "chip", text: tag, style: { tint: tagTint(tag) } };
                        })
                      }
                    ]
                  };
                }
              }
            ]
          };
        })
      }
    ]
  };
}
