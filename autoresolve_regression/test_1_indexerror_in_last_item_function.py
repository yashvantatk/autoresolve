import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))

from buggy import last_item

def test_last_item():
    items = [1, 2, 3]
    # The function should return 3, but it currently raises IndexError
    assert last_item(items) == 3

if __name__ == "__main__":
    test_last_item()

