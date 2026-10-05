import helpers
import report
from tools import fmt
from tools import money

import legacy


def test_total():
    assert report.total([1, 2]) == 3
    assert money(5) == fmt.money(5)
    assert helpers.ROWS
    assert legacy.OLD
