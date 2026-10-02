from store.billing import charge


def test_pay():
    assert charge.pay(1) == 1
