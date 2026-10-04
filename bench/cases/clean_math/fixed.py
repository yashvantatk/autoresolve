import math


def circle_area(radius):
    """Return the area of a circle; a negative radius is an error."""
    if radius < 0:
        raise ValueError("radius must not be negative")
    return math.pi * radius * radius


def clamp(value, low, high):
    """Return value limited to the range low..high."""
    return max(low, min(value, high))
