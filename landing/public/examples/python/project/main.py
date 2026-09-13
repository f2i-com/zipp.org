# A small interactive project for the ZIPP playground. Run it natively with
# `zipp py examples/python/project` (top level only: no frames without a host)
# or load the folder in crates/zipp-wasm/playground to see it animate.
import ui
from physics import step_ball, inside

W = 480
H = 320
balls = []          # each ball is a list: [x, y, vx, vy]
paddle = [W // 2 - 40]
frames = [0]

def spawn(x, y):
    if len(balls) < 40:
        balls.append([x, y, 3, -4])

spawn(W // 2, H // 2)
ui.canvas(W, H)
print("balls ready:", len(balls))

def on_click(x, y):
    spawn(x, y)

def on_key(key):
    if key == " ":
        spawn(W // 2, 40)

def update():
    if ui.key("ArrowLeft"):
        paddle[0] = paddle[0] - 6
    if ui.key("ArrowRight"):
        paddle[0] = paddle[0] + 6
    for b in balls:
        step_ball(b, W, H)
        if inside(b[0], b[1], paddle[0], H - 24, 80, 12) and b[3] > 0:
            b[3] = -b[3]
    frames[0] = frames[0] + 1

def draw():
    ui.clear("#10141c")
    for b in balls:
        ui.circle(b[0], b[1], 8, "#ffcc00")
    ui.rect(paddle[0], H - 24, 80, 12, "#4cc2ff")
    ui.font(14)
    ui.text(10, 20, "balls: " + str(len(balls)) + "   frame: " + str(frames[0]), "#e6edf3")
    ui.text(10, H - 6, "click to add a ball, arrows move the paddle, space serves", "#8b949e")
    if ui.button(W - 90, 8, 82, 26, "reset"):
        while len(balls) > 1:
            balls.pop()
