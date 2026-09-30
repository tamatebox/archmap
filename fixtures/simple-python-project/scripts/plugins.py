import importlib


def load(name):
    return importlib.import_module(f"shop.plugins.{name}")
