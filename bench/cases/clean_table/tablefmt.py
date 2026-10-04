def format_table(rows):
    """Return rows (lists of values, all the same length) as aligned text lines joined by newlines."""
    if not rows:
        return ""
    if any(len(row) != len(rows[0]) for row in rows):
        raise ValueError("all rows must have the same length")
    cells = [[str(value) for value in row] for row in rows]
    widths = [max(len(row[i]) for row in cells) for i in range(len(cells[0]))]
    lines = []
    for row in cells:
        lines.append("  ".join(value.ljust(widths[i]) for i, value in enumerate(row)).rstrip())
    return "\n".join(lines)
