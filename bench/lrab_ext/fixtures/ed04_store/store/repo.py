"""Repository: rows from the orders table (an in-memory list here)."""

ROWS = [
    (1, "widget", 3, 9.99),
    (2, "gadget", 1, 24.50),
    (3, "doohickey", 12, 0.75),
]


def fetchAll(conn):
    """Every order row as a tuple (id, sku, qty, unit_price)."""
    return list(conn["rows"])


def fetch_one(conn, order_id):
    for row in conn["rows"]:
        if row[0] == order_id:
            return row
    return None


def connect():
    return {"rows": ROWS}
