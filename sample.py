class Cart:
    def __init__(self):
        self.items = []

    def add(self, item):
        validate(item)
        self.items.append(item)

    def total(self):
        return sum(price(i) for i in self.items)

def validate(item):
    if not item:
        raise ValueError("empty")

def price(item):
    return item * 2

def checkout():
    cart = Cart()
    cart.add(3)
    return cart.total()