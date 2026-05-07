import os
from collections import OrderedDict

class User:
    def __init__(self, name):
        self.name = name

    @staticmethod
    def from_dict(data):
        return User(data["name"])

    def greet(self):
        return f"Hello, {self.name}"

def process(items):
    return items
