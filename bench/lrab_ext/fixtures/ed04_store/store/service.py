"""Service layer: dict-shaped orders for the API."""
from .repo import connect, fetchAll

CONN = connect()


def row_to_dict(row):
    order_id, sku, qty, unit_price = row
    return {"id": order_id, "sku": sku, "qty": qty, "total": round(qty * unit_price, 2)}


def load_all():
    return [row_to_dict(r) for r in fetchAll(CONN)]


def grand_total():
    return round(sum(o["total"] for o in load_all()), 2)
