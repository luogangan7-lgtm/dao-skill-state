def page_count(total, size):
    """Number of pages needed for `total` items, `size` per page."""
    return total // size + 1


def page_items(items, page, size):
    """Items on 1-based `page`; pages outside the range are empty."""
    if page < 1:
        return []
    start = page * size
    return items[start:start + size]
