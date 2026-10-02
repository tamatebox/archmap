from store.billing import rates


def test_rate():
    assert rates.rate() == 1
