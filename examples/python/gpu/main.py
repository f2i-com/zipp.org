# Playground adapter for the ordinary Torch rules in life.py.
# Open life.py to see the portable simulation code.
import torch
import ui
from life import neighbor_matrix, step

SIZE = 96
CELL = 4
ui.canvas(SIZE * CELL, SIZE * CELL + 40)
neighbors = neighbor_matrix(SIZE)
advance = torch.compile(step)
print("Life rules: ordinary Torch; browser GPU results arrive asynchronously.")


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
        state = torch.tensor(self.cells).reshape(SIZE, SIZE)
        self.request = advance(state, neighbors)
        self.pending = True
        self.request.submit(self.arrived, self.failure)

    def arrived(self, result):
        self.cells = result.flatten().tolist()
        self.alive = sum(self.cells)
        self.backend = self.request.backend
        self.generation += 1
        if self.generation == 1:
            print("Life generation 1:", int(self.alive), "alive", self.backend)
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
