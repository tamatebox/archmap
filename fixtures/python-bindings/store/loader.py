import importlib


def rates():
    return importlib.import_module("store.billing.rates")


def relative():
    return importlib.import_module(".rates", package="store.billing")


def stdlib():
    return __import__("json")


def computed(name):
    return importlib.import_module(f"store.{name}")
