"""Shipping rate bands for the order service."""

# (lower bound in GBP, band). A band starts at its lower bound, inclusive.
BANDS = [(0, "economy"), (250, "standard"), (1000, "premium")]

SURCHARGE = {"economy": 4.99, "standard": 2.49, "premium": 0.0}


def band_for(amount):
    """The band whose lower bound the amount has reached."""
    band = BANDS[0][1]
    for lower, name in BANDS:
        if amount > lower:
            band = name
    return band


def shipping_cost(amount):
    """The surcharge for an order of `amount`."""
    return SURCHARGE[band_for(amount)]
