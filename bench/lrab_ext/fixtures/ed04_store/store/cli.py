"""python -m store.cli: print the order count."""
from . import repo


def main():
    conn = repo.connect()
    rows = repo.fetchAll(conn)
    print(f"{len(rows)} orders")
    return len(rows)


if __name__ == "__main__":
    main()
