# Langton's ant, written the way a desktop Python program would be: classes,
# a rule table, and a grid. The drawing goes through the playground's `ui`
# module instead of Tk. Run it natively with `zipp py examples/python/langtons_ant`
# (the top level only) or load the folder in crates/zipp-wasm/playground.
import ui
from rules import Compass, RULES, turn


class Ant:
    def __init__(self, grid):
        self.grid = grid
        self.reset()

    def reset(self):
        self.x = self.grid.size // 2
        self.y = self.grid.size // 2
        self.direction = Compass.EAST
        self.steps = 0

    def step(self):
        state = self.grid.cells[self.y][self.x]
        rule = RULES[state]
        self.direction = turn(self.direction, rule.turn_right)
        self.grid.cells[self.y][self.x] = (state + 1) % len(RULES)
        if self.direction == Compass.NORTH:
            self.y -= 1
        elif self.direction == Compass.SOUTH:
            self.y += 1
        elif self.direction == Compass.EAST:
            self.x += 1
        else:
            self.x -= 1
        # The grid wraps around, as in the original.
        self.x %= self.grid.size
        self.y %= self.grid.size
        self.steps += 1


class Grid:
    def __init__(self, size, cell):
        self.size = size
        self.cell = cell
        self.cells = [[0] * size for _ in range(size)]
        self.ant = Ant(self)
        self.running = False
        self.speed = 50

    def clear(self):
        for row in self.cells:
            for i in range(len(row)):
                row[i] = 0
        self.ant.reset()

    def draw(self):
        ui.clear("#ffffff")
        c = self.cell
        for y, row in enumerate(self.cells):
            for x, state in enumerate(row):
                if state:
                    ui.rect(x * c, y * c, c - 1, c - 1, RULES[state].color)
        ax, ay = self.ant.x * c, self.ant.y * c
        ui.rect(ax, ay, c - 1, c - 1, "#e6194b")


SIZE = 100
CELL = 5
grid = Grid(SIZE, CELL)
ui.canvas(SIZE * CELL, SIZE * CELL + 40)
print(f"Langton's ant on a {SIZE}x{SIZE} grid with {len(RULES)} rules: " + ", ".join(r.color for r in RULES))


def on_key(key):
    if key == " ":
        grid.running = not grid.running
    elif key == "ArrowUp":
        grid.speed = min(500, grid.speed * 2)
    elif key == "ArrowDown":
        grid.speed = max(1, grid.speed // 2)
    elif key == "r":
        grid.clear()


def update():
    if grid.running:
        for _ in range(grid.speed):
            grid.ant.step()


def draw():
    grid.draw()
    base = SIZE * CELL
    ui.rect(0, base, base, 40, "#10141c")
    ui.font(14)
    label = "running" if grid.running else "paused"
    ui.text(8, base + 25, f"{label}   steps: {grid.ant.steps}   {grid.speed} steps/frame", "#e6edf3")
    if ui.button(base - 190, base + 8, 60, 24, "start" if not grid.running else "stop"):
        grid.running = not grid.running
    if ui.button(base - 125, base + 8, 60, 24, "reset"):
        grid.clear()
    if ui.button(base - 60, base + 8, 56, 24, "faster"):
        grid.speed = min(500, grid.speed * 2)
