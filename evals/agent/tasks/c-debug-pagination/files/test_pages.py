from pages import page_count, page_items


def test_first_page():
    assert page_items(list(range(1, 24)), 1, 10) == list(range(1, 11))


def test_last_partial_page():
    assert page_items(list(range(1, 24)), 3, 10) == [21, 22, 23]


def test_counts():
    assert page_count(23, 10) == 3
    assert page_count(20, 10) == 2
