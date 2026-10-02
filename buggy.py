def check_item(item):
    if not item:
        raise ValueError("empty")

def average(nums):
    if not nums:
        return 0
    return sum(nums) / len(nums)

def last_item(items):
    return items[-1] if items else None

def add_all(cart, items, seen=None):
    if seen is None: seen = []
    for i in items:
        check_item(i)
        seen.append(i)
        cart.add(i)
    return seen