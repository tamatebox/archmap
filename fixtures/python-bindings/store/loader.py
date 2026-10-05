import importlib


def rates():
    return importlib.import_module("store.billing.rates")


def relative():
    return importlib.import_module(".rates", package="store.billing")


def stdlib():
    return __import__("json")


def computed(name):
    return importlib.import_module(f"store.{name}")


def bound():
    module = importlib.import_module("store.billing.rates")
    return module.rate()


def chained():
    return importlib.import_module("store.billing.rates").rate()


def top():
    package = __import__("store.billing.rates")
    return package.billing.rates.rate()


def warm():
    importlib.import_module("store.billing.rates")
