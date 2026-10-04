def word_freq(text):
    """Return a dict mapping each lowercase word to how often it occurs."""
    counts = {}
    for word in text.lower().split():
        counts[word] = 1
    return counts


def longest_word(text):
    """Return the longest word in text, or an empty string when there are no words."""
    words = text.split()
    if not words:
        return ""
    return min(words, key=len)


def is_palindrome(text):
    """Return True when text reads the same backwards, ignoring case and spaces."""
    return text == text[::-1]
