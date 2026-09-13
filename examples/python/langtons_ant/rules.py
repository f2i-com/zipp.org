# The ant's rule table and compass: each cell state maps to a colour and a
# turn direction, cycling through the states on every visit.
from dataclasses import dataclass
from enum import Enum


class Compass(Enum):
    NORTH = 1
    EAST = 2
    SOUTH = 3
    WEST = 4


@dataclass(frozen=True)
class Rule:
    color: str
    turn_right: bool


# State 0 is blank (drawn as the background), then the coloured states.
RULES = [Rule("white", False), Rule("#222222", True), Rule("#2ecc71", True)]

ORDER = [Compass.NORTH, Compass.EAST, Compass.SOUTH, Compass.WEST]


def turn(direction, right):
    i = ORDER.index(direction)
    return ORDER[(i + (1 if right else -1)) % 4]
