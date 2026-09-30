def add_item(item, bucket=[]):
    bucket.append(item)
    return bucket

def risky(x):
    try:
        return 1 / x
    except:
        return None

if risky(0) == None:
    print("failed")