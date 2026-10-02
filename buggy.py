def check_item(item):
    if not item:
        raise ValueError("empty")

def average(nums):
    if not nums:
        return 0
    return sum(nums) / len(nums)

def last_item(items):
    return items[-1]

def add_all(cart, items, seen=[]):
    for i in items:
        check_item(i, strict=True)
        seen.append(i)
        cart.add(i)
    return seen