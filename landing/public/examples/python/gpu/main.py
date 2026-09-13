# GPU compute from Python: a float32 graph library (`zipp_gpu`) whose
# programs the playground runs through WebGPU, WebGL2, compiled WebAssembly
# or a JavaScript reference, in that order of preference (or the backend
# picked in the toolbar). Python records the graph; the host executes it and
# calls back with the named outputs. Run it natively with
# `zipp py examples/python/gpu` and the same graphs evaluate on the CPU.
import ui
from zipp_gpu import Graph

SIZE = 96
CELL = 4
ui.canvas(SIZE * CELL, SIZE * CELL + 40)


def show(label):
    def on_result(result):
        outputs = result["outputs"]
        for name in sorted(outputs):
            print(f"{label}: {name} = {outputs[name]['data']}  [{result['backend']}]")
    return on_result


# A vector expression: every operation records a node, one request runs them all.
g = Graph()
a = g.tensor([1, 2, 3, 4])
b = g.tensor([10, 20, 30, 40])
c = (a * b + 4).relu()
g.submit(show("vector"), result=c, total=c.sum())

# Matrix multiplication and a tiny explicit-weight network.
m = Graph()
x = m.tensor([[1, 2, 3], [4, 5, 6]])
y = m.tensor([[7, 8], [9, 10], [11, 12]])
m.submit(show("matmul"), result=x @ y)

n = Graph()
x = n.tensor([[1, -2, 3], [0, 1, 2]])
w1 = n.tensor([[0.5, -1, 2, 0], [1, 0, -0.5, 1], [-1, 1, 0, 0.5]])
hidden = (x @ w1 + 0.25).relu()
w2 = n.tensor([[1, 0], [0.5, -0.5], [1, 1], [-1, 2]])
n.submit(show("mlp"), result=hidden @ w2)


# Conway's life, one generation per frame, computed by the compute backend:
# `update` submits the next step when the previous one has arrived and
# `draw` paints whatever generation is current.
class Life:
    def __init__(self):
        self.cells = [0.0] * (SIZE * SIZE)
        # A persistent glider gun, pulsar, acorns and a busy central patch.
        # Patterns share one toroidal world, so their debris and gliders meet.
        def stamp(x, y, rows):
            for dy, row in enumerate(rows):
                for dx, cell in enumerate(row):
                    if cell == "O":
                        self.cells[((y + dy) % SIZE) * SIZE + (x + dx) % SIZE] = 1.0

        stamp(3, 3, [
            "........................O...........",
            "......................O.O...........",
            "............OO......OO............OO",
            "...........O...O....OO............OO",
            "OO........O.....O...OO..............",
            "OO........O...O.OO....O.O...........",
            "..........O.....O.......O...........",
            "...........O...O....................",
            "............OO......................",
        ])
        stamp(67, 7, [
            "..OOO...OOO..", ".............", "O....O.O....O",
            "O....O.O....O", "O....O.O....O", "..OOO...OOO..",
            ".............", "..OOO...OOO..", "O....O.O....O",
            "O....O.O....O", "O....O.O....O", ".............", "..OOO...OOO..",
        ])
        for x, y in [(12, 55), (58, 65), (76, 45)]:
            stamp(x, y, [".O.....", "...O...", "OO..OOO"])
        for x, y in [(28, 29), (62, 32), (30, 72)]:
            stamp(x, y, [".OO", "OO.", ".O."])
        seed = 19
        for y in range(34, 52):
            for x in range(34, 60):
                seed = (seed * 1103515245 + 12345) % 2147483648
                if seed % 100 < 28:
                    self.cells[y * SIZE + x] = 1.0
        self.generation = 0
        self.pending = False
        self.backend = "?"
        self.alive = sum(self.cells)
        self.failed = None

    def step(self):
        if self.pending or self.failed:
            return
        graph = Graph()
        state = graph.tensor(self.cells, shape=(SIZE, SIZE)).life()
        self.pending = True
        graph.submit(self.arrived, self.failure, result=state, alive=state.sum())

    def arrived(self, result):
        self.cells = result["outputs"]["result"]["data"]
        self.alive = result["outputs"]["alive"]["data"][0]
        self.backend = result["backend"]
        self.generation += 1
        self.pending = False

    def failure(self, error):
        self.failed = str(error)
        self.pending = False
        print("life stopped:", error)

    def draw(self):
        ui.clear("#0b0e14")
        for i, v in enumerate(self.cells):
            if v > 0.5:
                ui.rect((i % SIZE) * CELL, (i // SIZE) * CELL, CELL - 1, CELL - 1, "#2ecc71")
        base = SIZE * CELL
        ui.rect(0, base, base, 40, "#10141c")
        ui.font(14)
        ui.text(8, base + 25, f"generation {self.generation}   alive {int(self.alive)}   backend {self.backend}", "#e6edf3")


life = Life()


def update():
    life.step()


def draw():
    life.draw()
