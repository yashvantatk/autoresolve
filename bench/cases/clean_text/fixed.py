def title_case(text):
    """Capitalize the first letter of every word and lowercase the rest."""
    return " ".join(word[:1].upper() + word[1:].lower() for word in text.split())


def count_words(text):
    """Return how many words text contains."""
    return len(text.split())
