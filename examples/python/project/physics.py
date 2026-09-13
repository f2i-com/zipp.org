# Integer physics helpers imported by main.py.

def step_ball(b, w, h):
    b[0] = b[0] + b[2]
    b[1] = b[1] + b[3]
    if b[0] < 8 or b[0] > w - 8:
        b[2] = -b[2]
    if b[1] < 8 or b[1] > h - 8:
        b[3] = -b[3]

def inside(px, py, x, y, w, h):
    return x <= px and px < x + w and y <= py and py < y + h
