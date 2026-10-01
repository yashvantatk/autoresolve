def check_item(item):
    if not item:
        raise ValueError("empty")

def average(nums):
    return sum(nums) / len(nums)

def last_item(items):
    return items[len(items)]

def add_all(cart, items, seen=[]):
    for i in items:
        check_item(i, strict=True)
        seen.append(i)
        cart.add(i)
    return seen