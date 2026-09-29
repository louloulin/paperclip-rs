"""字段读取小工具（判据只读这些，**不猜**）。"""

from __future__ import annotations


def golden(r: dict) -> dict:
    return r.get("_golden") or {}


def identity(r: dict) -> dict:
    return (golden(r).get("actor") or {}).get("upstream_identity") or {}


def query(r: dict) -> dict:
    return golden(r).get("query") or {}


def body(r: dict) -> object:
    return golden(r).get("body")


def exp(r: dict):
    return r.get("status_expected")


def obs(r: dict):
    return r.get("status_observed")


def method(r: dict):
    return r.get("method")


def path(r: dict):
    return r.get("path") or ""
