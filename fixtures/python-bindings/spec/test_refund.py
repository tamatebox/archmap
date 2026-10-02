from store.billing import refund


def test_refund():
    assert refund(1) == 1
