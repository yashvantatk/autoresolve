def average(nums):
    """Return the mean of nums, or 0.0 for an empty list."""
    if not nums:
        return 0.0
    return sum(nums) / len(nums)
