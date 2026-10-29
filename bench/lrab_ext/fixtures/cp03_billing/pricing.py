"""pricing: rate tables and tier logic for the billing service (generated fixture).
Do not reformat this file; edits must be surgical."""
from __future__ import annotations


def helper_00(value: float, factor: float = 1.00) -> float:
    """Helper 00: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1000:
        scaled = 1000
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_01(value: float, factor: float = 1.01) -> float:
    """Helper 01: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1037:
        scaled = 1037
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_02(value: float, factor: float = 1.02) -> float:
    """Helper 02: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1074:
        scaled = 1074
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_03(value: float, factor: float = 1.03) -> float:
    """Helper 03: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1111:
        scaled = 1111
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_04(value: float, factor: float = 1.04) -> float:
    """Helper 04: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1148:
        scaled = 1148
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)


def helper_05(value: float, factor: float = 1.05) -> float:
    """Helper 05: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1185:
        scaled = 1185
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_06(value: float, factor: float = 1.06) -> float:
    """Helper 06: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1222:
        scaled = 1222
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_07(value: float, factor: float = 1.07) -> float:
    """Helper 07: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1259:
        scaled = 1259
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_08(value: float, factor: float = 1.08) -> float:
    """Helper 08: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1296:
        scaled = 1296
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_09(value: float, factor: float = 1.09) -> float:
    """Helper 09: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1333:
        scaled = 1333
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)


def helper_10(value: float, factor: float = 1.10) -> float:
    """Helper 10: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1370:
        scaled = 1370
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_11(value: float, factor: float = 1.11) -> float:
    """Helper 11: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1407:
        scaled = 1407
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_12(value: float, factor: float = 1.12) -> float:
    """Helper 12: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1444:
        scaled = 1444
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_13(value: float, factor: float = 1.13) -> float:
    """Helper 13: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1481:
        scaled = 1481
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_14(value: float, factor: float = 1.14) -> float:
    """Helper 14: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1518:
        scaled = 1518
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)


def helper_15(value: float, factor: float = 1.15) -> float:
    """Helper 15: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1555:
        scaled = 1555
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_16(value: float, factor: float = 1.16) -> float:
    """Helper 16: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1592:
        scaled = 1592
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_17(value: float, factor: float = 1.17) -> float:
    """Helper 17: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1629:
        scaled = 1629
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_18(value: float, factor: float = 1.18) -> float:
    """Helper 18: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1666:
        scaled = 1666
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_19(value: float, factor: float = 1.19) -> float:
    """Helper 19: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1703:
        scaled = 1703
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)

RATES_EU_WEST = {
    "eu-west-000": {"base": 88.7, "per_unit": 0.48, "currency": "GBP"},
    "eu-west-001": {"base": 23.6, "per_unit": 0.89, "currency": "USD"},
    "eu-west-002": {"base": 65.3, "per_unit": 0.86, "currency": "EUR"},
    "eu-west-003": {"base": 40.4, "per_unit": 0.99, "currency": "USD"},
    "eu-west-004": {"base": 22.2, "per_unit": 0.80, "currency": "USD"},
    "eu-west-005": {"base": 49.9, "per_unit": 0.99, "currency": "EUR"},
    "eu-west-006": {"base": 47.2, "per_unit": 0.75, "currency": "USD"},
    "eu-west-007": {"base": 69.8, "per_unit": 0.18, "currency": "USD"},
    "eu-west-008": {"base": 36.2, "per_unit": 0.37, "currency": "GBP"},
    "eu-west-009": {"base": 72.1, "per_unit": 0.47, "currency": "GBP"},
    "eu-west-010": {"base": 8.1, "per_unit": 0.52, "currency": "EUR"},
    "eu-west-011": {"base": 82.0, "per_unit": 0.69, "currency": "EUR"},
    "eu-west-012": {"base": 50.6, "per_unit": 0.85, "currency": "EUR"},
    "eu-west-013": {"base": 75.8, "per_unit": 0.60, "currency": "USD"},
    "eu-west-014": {"base": 97.7, "per_unit": 0.68, "currency": "GBP"},
    "eu-west-015": {"base": 35.8, "per_unit": 0.61, "currency": "USD"},
    "eu-west-016": {"base": 17.2, "per_unit": 0.99, "currency": "USD"},
    "eu-west-017": {"base": 95.7, "per_unit": 0.12, "currency": "GBP"},
    "eu-west-018": {"base": 17.0, "per_unit": 0.44, "currency": "USD"},
    "eu-west-019": {"base": 17.9, "per_unit": 0.45, "currency": "USD"},
    "eu-west-020": {"base": 13.8, "per_unit": 0.96, "currency": "GBP"},
    "eu-west-021": {"base": 55.9, "per_unit": 0.36, "currency": "USD"},
    "eu-west-022": {"base": 59.8, "per_unit": 0.51, "currency": "GBP"},
    "eu-west-023": {"base": 98.6, "per_unit": 0.45, "currency": "GBP"},
    "eu-west-024": {"base": 86.4, "per_unit": 0.89, "currency": "EUR"},
    "eu-west-025": {"base": 69.1, "per_unit": 0.25, "currency": "USD"},
    "eu-west-026": {"base": 13.6, "per_unit": 0.44, "currency": "EUR"},
    "eu-west-027": {"base": 13.8, "per_unit": 0.04, "currency": "GBP"},
    "eu-west-028": {"base": 67.1, "per_unit": 0.34, "currency": "USD"},
    "eu-west-029": {"base": 52.5, "per_unit": 0.92, "currency": "USD"},
    "eu-west-030": {"base": 29.2, "per_unit": 0.55, "currency": "USD"},
    "eu-west-031": {"base": 74.3, "per_unit": 0.24, "currency": "GBP"},
    "eu-west-032": {"base": 86.0, "per_unit": 0.72, "currency": "GBP"},
    "eu-west-033": {"base": 35.7, "per_unit": 0.28, "currency": "GBP"},
    "eu-west-034": {"base": 53.0, "per_unit": 0.21, "currency": "GBP"},
    "eu-west-035": {"base": 1.5, "per_unit": 0.06, "currency": "GBP"},
    "eu-west-036": {"base": 48.6, "per_unit": 0.35, "currency": "EUR"},
    "eu-west-037": {"base": 82.0, "per_unit": 0.69, "currency": "GBP"},
    "eu-west-038": {"base": 62.8, "per_unit": 0.60, "currency": "GBP"},
    "eu-west-039": {"base": 58.1, "per_unit": 0.02, "currency": "GBP"},
    "eu-west-040": {"base": 48.1, "per_unit": 0.80, "currency": "EUR"},
    "eu-west-041": {"base": 42.5, "per_unit": 0.94, "currency": "GBP"},
    "eu-west-042": {"base": 2.6, "per_unit": 0.67, "currency": "USD"},
    "eu-west-043": {"base": 80.0, "per_unit": 0.46, "currency": "GBP"},
    "eu-west-044": {"base": 32.1, "per_unit": 0.18, "currency": "GBP"},
    "eu-west-045": {"base": 10.4, "per_unit": 0.52, "currency": "EUR"},
    "eu-west-046": {"base": 11.3, "per_unit": 0.50, "currency": "GBP"},
    "eu-west-047": {"base": 69.5, "per_unit": 0.18, "currency": "USD"},
    "eu-west-048": {"base": 21.2, "per_unit": 0.15, "currency": "GBP"},
    "eu-west-049": {"base": 38.1, "per_unit": 0.50, "currency": "USD"},
    "eu-west-050": {"base": 56.4, "per_unit": 0.78, "currency": "EUR"},
    "eu-west-051": {"base": 76.3, "per_unit": 0.98, "currency": "GBP"},
    "eu-west-052": {"base": 42.1, "per_unit": 0.31, "currency": "USD"},
    "eu-west-053": {"base": 78.0, "per_unit": 0.50, "currency": "EUR"},
    "eu-west-054": {"base": 57.3, "per_unit": 0.39, "currency": "USD"},
    "eu-west-055": {"base": 78.7, "per_unit": 0.30, "currency": "USD"},
    "eu-west-056": {"base": 47.3, "per_unit": 0.07, "currency": "GBP"},
    "eu-west-057": {"base": 76.4, "per_unit": 0.05, "currency": "EUR"},
    "eu-west-058": {"base": 39.1, "per_unit": 0.26, "currency": "EUR"},
    "eu-west-059": {"base": 14.5, "per_unit": 0.93, "currency": "GBP"},
    "eu-west-060": {"base": 46.4, "per_unit": 0.57, "currency": "USD"},
    "eu-west-061": {"base": 84.8, "per_unit": 0.49, "currency": "USD"},
    "eu-west-062": {"base": 44.5, "per_unit": 0.78, "currency": "GBP"},
    "eu-west-063": {"base": 35.3, "per_unit": 0.28, "currency": "GBP"},
    "eu-west-064": {"base": 71.1, "per_unit": 0.33, "currency": "USD"},
    "eu-west-065": {"base": 78.3, "per_unit": 0.36, "currency": "GBP"},
    "eu-west-066": {"base": 14.9, "per_unit": 0.49, "currency": "GBP"},
    "eu-west-067": {"base": 43.3, "per_unit": 0.47, "currency": "EUR"},
    "eu-west-068": {"base": 30.5, "per_unit": 0.39, "currency": "EUR"},
    "eu-west-069": {"base": 80.1, "per_unit": 0.22, "currency": "GBP"},
    "eu-west-070": {"base": 73.0, "per_unit": 0.34, "currency": "GBP"},
    "eu-west-071": {"base": 30.3, "per_unit": 0.08, "currency": "USD"},
    "eu-west-072": {"base": 32.6, "per_unit": 0.14, "currency": "USD"},
    "eu-west-073": {"base": 93.9, "per_unit": 0.11, "currency": "USD"},
    "eu-west-074": {"base": 55.3, "per_unit": 0.68, "currency": "GBP"},
    "eu-west-075": {"base": 13.5, "per_unit": 0.48, "currency": "EUR"},
    "eu-west-076": {"base": 34.3, "per_unit": 0.17, "currency": "EUR"},
    "eu-west-077": {"base": 60.4, "per_unit": 0.83, "currency": "EUR"},
    "eu-west-078": {"base": 60.9, "per_unit": 0.36, "currency": "GBP"},
    "eu-west-079": {"base": 42.3, "per_unit": 0.27, "currency": "GBP"},
    "eu-west-080": {"base": 51.4, "per_unit": 0.19, "currency": "GBP"},
    "eu-west-081": {"base": 70.1, "per_unit": 0.83, "currency": "USD"},
    "eu-west-082": {"base": 54.7, "per_unit": 0.18, "currency": "GBP"},
    "eu-west-083": {"base": 9.8, "per_unit": 0.40, "currency": "EUR"},
    "eu-west-084": {"base": 48.4, "per_unit": 0.98, "currency": "USD"},
    "eu-west-085": {"base": 16.7, "per_unit": 0.30, "currency": "USD"},
    "eu-west-086": {"base": 36.2, "per_unit": 0.21, "currency": "EUR"},
    "eu-west-087": {"base": 63.4, "per_unit": 0.26, "currency": "USD"},
    "eu-west-088": {"base": 60.5, "per_unit": 0.70, "currency": "GBP"},
    "eu-west-089": {"base": 52.3, "per_unit": 0.33, "currency": "EUR"},
    "eu-west-090": {"base": 29.0, "per_unit": 0.39, "currency": "GBP"},
    "eu-west-091": {"base": 6.0, "per_unit": 0.75, "currency": "USD"},
    "eu-west-092": {"base": 43.4, "per_unit": 0.20, "currency": "USD"},
    "eu-west-093": {"base": 86.2, "per_unit": 0.31, "currency": "EUR"},
    "eu-west-094": {"base": 73.0, "per_unit": 0.32, "currency": "USD"},
    "eu-west-095": {"base": 7.9, "per_unit": 0.70, "currency": "EUR"},
    "eu-west-096": {"base": 19.7, "per_unit": 0.89, "currency": "GBP"},
    "eu-west-097": {"base": 2.3, "per_unit": 0.01, "currency": "GBP"},
    "eu-west-098": {"base": 82.8, "per_unit": 0.84, "currency": "USD"},
    "eu-west-099": {"base": 87.4, "per_unit": 0.83, "currency": "GBP"},
    "eu-west-100": {"base": 63.2, "per_unit": 0.82, "currency": "GBP"},
    "eu-west-101": {"base": 5.8, "per_unit": 0.58, "currency": "GBP"},
    "eu-west-102": {"base": 75.5, "per_unit": 0.76, "currency": "EUR"},
    "eu-west-103": {"base": 27.3, "per_unit": 0.32, "currency": "GBP"},
    "eu-west-104": {"base": 39.9, "per_unit": 0.97, "currency": "EUR"},
    "eu-west-105": {"base": 26.5, "per_unit": 0.79, "currency": "GBP"},
    "eu-west-106": {"base": 16.8, "per_unit": 0.07, "currency": "EUR"},
    "eu-west-107": {"base": 64.9, "per_unit": 0.26, "currency": "GBP"},
    "eu-west-108": {"base": 38.8, "per_unit": 0.19, "currency": "EUR"},
    "eu-west-109": {"base": 59.0, "per_unit": 0.03, "currency": "GBP"},
    "eu-west-110": {"base": 55.1, "per_unit": 0.45, "currency": "USD"},
    "eu-west-111": {"base": 51.0, "per_unit": 0.90, "currency": "EUR"},
    "eu-west-112": {"base": 23.0, "per_unit": 0.19, "currency": "USD"},
    "eu-west-113": {"base": 97.3, "per_unit": 0.39, "currency": "EUR"},
    "eu-west-114": {"base": 4.2, "per_unit": 0.37, "currency": "GBP"},
    "eu-west-115": {"base": 68.0, "per_unit": 0.20, "currency": "USD"},
    "eu-west-116": {"base": 1.3, "per_unit": 0.53, "currency": "GBP"},
    "eu-west-117": {"base": 72.4, "per_unit": 0.31, "currency": "USD"},
    "eu-west-118": {"base": 8.8, "per_unit": 0.19, "currency": "GBP"},
    "eu-west-119": {"base": 57.1, "per_unit": 0.57, "currency": "EUR"},
}

RATES_EU_NORTH = {
    "eu-north-000": {"base": 49.9, "per_unit": 0.05, "currency": "EUR"},
    "eu-north-001": {"base": 56.8, "per_unit": 0.39, "currency": "USD"},
    "eu-north-002": {"base": 11.2, "per_unit": 0.61, "currency": "GBP"},
    "eu-north-003": {"base": 4.0, "per_unit": 0.26, "currency": "GBP"},
    "eu-north-004": {"base": 50.2, "per_unit": 0.29, "currency": "EUR"},
    "eu-north-005": {"base": 68.9, "per_unit": 0.60, "currency": "EUR"},
    "eu-north-006": {"base": 91.9, "per_unit": 0.27, "currency": "GBP"},
    "eu-north-007": {"base": 69.5, "per_unit": 0.17, "currency": "GBP"},
    "eu-north-008": {"base": 78.0, "per_unit": 0.07, "currency": "EUR"},
    "eu-north-009": {"base": 76.2, "per_unit": 0.69, "currency": "EUR"},
    "eu-north-010": {"base": 1.3, "per_unit": 0.17, "currency": "EUR"},
    "eu-north-011": {"base": 70.9, "per_unit": 0.32, "currency": "GBP"},
    "eu-north-012": {"base": 50.9, "per_unit": 0.44, "currency": "EUR"},
    "eu-north-013": {"base": 75.3, "per_unit": 0.26, "currency": "GBP"},
    "eu-north-014": {"base": 60.1, "per_unit": 0.88, "currency": "EUR"},
    "eu-north-015": {"base": 97.3, "per_unit": 0.57, "currency": "GBP"},
    "eu-north-016": {"base": 72.5, "per_unit": 0.02, "currency": "USD"},
    "eu-north-017": {"base": 79.9, "per_unit": 0.66, "currency": "USD"},
    "eu-north-018": {"base": 14.0, "per_unit": 0.82, "currency": "EUR"},
    "eu-north-019": {"base": 80.4, "per_unit": 0.17, "currency": "GBP"},
    "eu-north-020": {"base": 68.1, "per_unit": 0.59, "currency": "USD"},
    "eu-north-021": {"base": 58.0, "per_unit": 0.90, "currency": "USD"},
    "eu-north-022": {"base": 12.6, "per_unit": 0.17, "currency": "USD"},
    "eu-north-023": {"base": 79.7, "per_unit": 0.74, "currency": "USD"},
    "eu-north-024": {"base": 57.3, "per_unit": 0.17, "currency": "EUR"},
    "eu-north-025": {"base": 22.2, "per_unit": 0.61, "currency": "USD"},
    "eu-north-026": {"base": 91.2, "per_unit": 0.38, "currency": "EUR"},
    "eu-north-027": {"base": 93.2, "per_unit": 0.73, "currency": "GBP"},
    "eu-north-028": {"base": 47.8, "per_unit": 0.80, "currency": "EUR"},
    "eu-north-029": {"base": 57.7, "per_unit": 0.67, "currency": "EUR"},
    "eu-north-030": {"base": 51.5, "per_unit": 0.76, "currency": "USD"},
    "eu-north-031": {"base": 66.5, "per_unit": 0.66, "currency": "USD"},
    "eu-north-032": {"base": 95.5, "per_unit": 0.20, "currency": "GBP"},
    "eu-north-033": {"base": 85.5, "per_unit": 0.38, "currency": "GBP"},
    "eu-north-034": {"base": 46.1, "per_unit": 0.17, "currency": "EUR"},
    "eu-north-035": {"base": 18.6, "per_unit": 0.60, "currency": "EUR"},
    "eu-north-036": {"base": 55.3, "per_unit": 0.86, "currency": "USD"},
    "eu-north-037": {"base": 75.0, "per_unit": 0.04, "currency": "EUR"},
    "eu-north-038": {"base": 15.1, "per_unit": 0.32, "currency": "GBP"},
    "eu-north-039": {"base": 22.4, "per_unit": 0.02, "currency": "EUR"},
    "eu-north-040": {"base": 77.3, "per_unit": 0.88, "currency": "GBP"},
    "eu-north-041": {"base": 53.4, "per_unit": 0.41, "currency": "EUR"},
    "eu-north-042": {"base": 82.3, "per_unit": 0.17, "currency": "GBP"},
    "eu-north-043": {"base": 30.1, "per_unit": 0.91, "currency": "USD"},
    "eu-north-044": {"base": 91.0, "per_unit": 0.76, "currency": "USD"},
    "eu-north-045": {"base": 95.9, "per_unit": 0.01, "currency": "EUR"},
    "eu-north-046": {"base": 65.8, "per_unit": 0.60, "currency": "EUR"},
    "eu-north-047": {"base": 4.0, "per_unit": 0.68, "currency": "USD"},
    "eu-north-048": {"base": 10.3, "per_unit": 0.79, "currency": "USD"},
    "eu-north-049": {"base": 91.2, "per_unit": 0.08, "currency": "GBP"},
    "eu-north-050": {"base": 66.3, "per_unit": 0.26, "currency": "EUR"},
    "eu-north-051": {"base": 90.1, "per_unit": 0.80, "currency": "USD"},
    "eu-north-052": {"base": 60.2, "per_unit": 0.62, "currency": "EUR"},
    "eu-north-053": {"base": 21.0, "per_unit": 0.18, "currency": "EUR"},
    "eu-north-054": {"base": 21.6, "per_unit": 0.94, "currency": "EUR"},
    "eu-north-055": {"base": 16.9, "per_unit": 0.48, "currency": "EUR"},
    "eu-north-056": {"base": 92.2, "per_unit": 0.89, "currency": "EUR"},
    "eu-north-057": {"base": 61.5, "per_unit": 0.88, "currency": "EUR"},
    "eu-north-058": {"base": 35.8, "per_unit": 0.83, "currency": "USD"},
    "eu-north-059": {"base": 82.7, "per_unit": 0.02, "currency": "GBP"},
    "eu-north-060": {"base": 43.4, "per_unit": 0.41, "currency": "USD"},
    "eu-north-061": {"base": 1.3, "per_unit": 0.36, "currency": "GBP"},
    "eu-north-062": {"base": 16.7, "per_unit": 0.66, "currency": "USD"},
    "eu-north-063": {"base": 94.0, "per_unit": 0.70, "currency": "USD"},
    "eu-north-064": {"base": 4.7, "per_unit": 0.47, "currency": "USD"},
    "eu-north-065": {"base": 75.4, "per_unit": 0.65, "currency": "EUR"},
    "eu-north-066": {"base": 71.6, "per_unit": 0.79, "currency": "EUR"},
    "eu-north-067": {"base": 80.2, "per_unit": 0.51, "currency": "USD"},
    "eu-north-068": {"base": 97.3, "per_unit": 0.45, "currency": "USD"},
    "eu-north-069": {"base": 1.5, "per_unit": 0.91, "currency": "GBP"},
    "eu-north-070": {"base": 67.2, "per_unit": 0.89, "currency": "GBP"},
    "eu-north-071": {"base": 51.2, "per_unit": 0.05, "currency": "GBP"},
    "eu-north-072": {"base": 50.8, "per_unit": 0.58, "currency": "EUR"},
    "eu-north-073": {"base": 80.0, "per_unit": 0.27, "currency": "GBP"},
    "eu-north-074": {"base": 49.0, "per_unit": 0.25, "currency": "GBP"},
    "eu-north-075": {"base": 15.8, "per_unit": 0.20, "currency": "USD"},
    "eu-north-076": {"base": 84.4, "per_unit": 0.16, "currency": "USD"},
    "eu-north-077": {"base": 9.8, "per_unit": 0.25, "currency": "USD"},
    "eu-north-078": {"base": 10.3, "per_unit": 0.97, "currency": "GBP"},
    "eu-north-079": {"base": 59.4, "per_unit": 0.93, "currency": "GBP"},
    "eu-north-080": {"base": 58.3, "per_unit": 0.07, "currency": "GBP"},
    "eu-north-081": {"base": 36.6, "per_unit": 0.32, "currency": "EUR"},
    "eu-north-082": {"base": 61.0, "per_unit": 0.06, "currency": "USD"},
    "eu-north-083": {"base": 74.5, "per_unit": 0.71, "currency": "USD"},
    "eu-north-084": {"base": 77.1, "per_unit": 0.64, "currency": "GBP"},
    "eu-north-085": {"base": 85.8, "per_unit": 0.97, "currency": "USD"},
    "eu-north-086": {"base": 2.4, "per_unit": 0.76, "currency": "GBP"},
    "eu-north-087": {"base": 14.5, "per_unit": 0.61, "currency": "USD"},
    "eu-north-088": {"base": 50.2, "per_unit": 0.75, "currency": "GBP"},
    "eu-north-089": {"base": 50.0, "per_unit": 0.12, "currency": "GBP"},
    "eu-north-090": {"base": 89.6, "per_unit": 0.63, "currency": "USD"},
    "eu-north-091": {"base": 46.2, "per_unit": 0.42, "currency": "USD"},
    "eu-north-092": {"base": 70.3, "per_unit": 0.84, "currency": "USD"},
    "eu-north-093": {"base": 17.0, "per_unit": 0.36, "currency": "USD"},
    "eu-north-094": {"base": 51.7, "per_unit": 0.74, "currency": "EUR"},
    "eu-north-095": {"base": 53.5, "per_unit": 0.16, "currency": "GBP"},
    "eu-north-096": {"base": 80.3, "per_unit": 0.10, "currency": "GBP"},
    "eu-north-097": {"base": 2.9, "per_unit": 0.93, "currency": "GBP"},
    "eu-north-098": {"base": 42.1, "per_unit": 0.36, "currency": "USD"},
    "eu-north-099": {"base": 40.9, "per_unit": 0.45, "currency": "USD"},
    "eu-north-100": {"base": 61.5, "per_unit": 0.22, "currency": "USD"},
    "eu-north-101": {"base": 17.2, "per_unit": 0.73, "currency": "USD"},
    "eu-north-102": {"base": 64.9, "per_unit": 0.73, "currency": "EUR"},
    "eu-north-103": {"base": 52.9, "per_unit": 0.89, "currency": "GBP"},
    "eu-north-104": {"base": 62.7, "per_unit": 0.36, "currency": "USD"},
    "eu-north-105": {"base": 4.3, "per_unit": 0.08, "currency": "EUR"},
    "eu-north-106": {"base": 79.8, "per_unit": 0.04, "currency": "USD"},
    "eu-north-107": {"base": 15.5, "per_unit": 0.31, "currency": "GBP"},
    "eu-north-108": {"base": 24.2, "per_unit": 0.06, "currency": "EUR"},
    "eu-north-109": {"base": 16.1, "per_unit": 0.81, "currency": "GBP"},
    "eu-north-110": {"base": 75.8, "per_unit": 0.14, "currency": "GBP"},
    "eu-north-111": {"base": 42.0, "per_unit": 0.64, "currency": "GBP"},
    "eu-north-112": {"base": 48.1, "per_unit": 0.29, "currency": "USD"},
    "eu-north-113": {"base": 27.5, "per_unit": 0.17, "currency": "GBP"},
    "eu-north-114": {"base": 19.1, "per_unit": 0.16, "currency": "GBP"},
    "eu-north-115": {"base": 1.9, "per_unit": 0.11, "currency": "USD"},
    "eu-north-116": {"base": 60.0, "per_unit": 0.20, "currency": "USD"},
    "eu-north-117": {"base": 7.8, "per_unit": 0.28, "currency": "USD"},
    "eu-north-118": {"base": 9.2, "per_unit": 0.14, "currency": "EUR"},
    "eu-north-119": {"base": 47.8, "per_unit": 0.30, "currency": "EUR"},
}

RATES_US_EAST = {
    "us-east-000": {"base": 27.1, "per_unit": 0.75, "currency": "GBP"},
    "us-east-001": {"base": 38.9, "per_unit": 0.75, "currency": "EUR"},
    "us-east-002": {"base": 16.8, "per_unit": 0.89, "currency": "EUR"},
    "us-east-003": {"base": 77.3, "per_unit": 0.25, "currency": "EUR"},
    "us-east-004": {"base": 23.2, "per_unit": 0.79, "currency": "USD"},
    "us-east-005": {"base": 45.7, "per_unit": 0.57, "currency": "GBP"},
    "us-east-006": {"base": 52.1, "per_unit": 0.65, "currency": "USD"},
    "us-east-007": {"base": 82.5, "per_unit": 0.64, "currency": "GBP"},
    "us-east-008": {"base": 90.7, "per_unit": 0.24, "currency": "EUR"},
    "us-east-009": {"base": 65.5, "per_unit": 0.03, "currency": "GBP"},
    "us-east-010": {"base": 10.7, "per_unit": 0.34, "currency": "EUR"},
    "us-east-011": {"base": 49.2, "per_unit": 0.79, "currency": "GBP"},
    "us-east-012": {"base": 77.2, "per_unit": 0.44, "currency": "EUR"},
    "us-east-013": {"base": 10.9, "per_unit": 0.24, "currency": "USD"},
    "us-east-014": {"base": 52.3, "per_unit": 0.54, "currency": "GBP"},
    "us-east-015": {"base": 54.7, "per_unit": 0.70, "currency": "GBP"},
    "us-east-016": {"base": 72.0, "per_unit": 0.57, "currency": "EUR"},
    "us-east-017": {"base": 11.3, "per_unit": 0.69, "currency": "USD"},
    "us-east-018": {"base": 17.6, "per_unit": 0.58, "currency": "GBP"},
    "us-east-019": {"base": 36.4, "per_unit": 0.15, "currency": "EUR"},
    "us-east-020": {"base": 75.7, "per_unit": 0.01, "currency": "GBP"},
    "us-east-021": {"base": 2.4, "per_unit": 0.67, "currency": "EUR"},
    "us-east-022": {"base": 68.0, "per_unit": 0.65, "currency": "USD"},
    "us-east-023": {"base": 75.8, "per_unit": 0.31, "currency": "GBP"},
    "us-east-024": {"base": 11.4, "per_unit": 0.62, "currency": "USD"},
    "us-east-025": {"base": 11.6, "per_unit": 0.35, "currency": "GBP"},
    "us-east-026": {"base": 89.6, "per_unit": 0.71, "currency": "EUR"},
    "us-east-027": {"base": 44.6, "per_unit": 0.41, "currency": "GBP"},
    "us-east-028": {"base": 35.3, "per_unit": 0.56, "currency": "USD"},
    "us-east-029": {"base": 47.8, "per_unit": 0.49, "currency": "EUR"},
    "us-east-030": {"base": 13.4, "per_unit": 0.13, "currency": "EUR"},
    "us-east-031": {"base": 12.9, "per_unit": 0.38, "currency": "EUR"},
    "us-east-032": {"base": 91.6, "per_unit": 0.10, "currency": "EUR"},
    "us-east-033": {"base": 93.2, "per_unit": 0.19, "currency": "USD"},
    "us-east-034": {"base": 17.9, "per_unit": 0.25, "currency": "GBP"},
    "us-east-035": {"base": 59.5, "per_unit": 0.39, "currency": "GBP"},
    "us-east-036": {"base": 42.5, "per_unit": 0.59, "currency": "USD"},
    "us-east-037": {"base": 54.8, "per_unit": 0.20, "currency": "EUR"},
    "us-east-038": {"base": 8.1, "per_unit": 0.05, "currency": "EUR"},
    "us-east-039": {"base": 2.2, "per_unit": 0.30, "currency": "EUR"},
    "us-east-040": {"base": 95.5, "per_unit": 0.86, "currency": "EUR"},
    "us-east-041": {"base": 61.2, "per_unit": 0.59, "currency": "USD"},
    "us-east-042": {"base": 12.2, "per_unit": 0.03, "currency": "EUR"},
    "us-east-043": {"base": 41.2, "per_unit": 0.30, "currency": "USD"},
    "us-east-044": {"base": 43.8, "per_unit": 0.58, "currency": "EUR"},
    "us-east-045": {"base": 8.0, "per_unit": 0.57, "currency": "EUR"},
    "us-east-046": {"base": 27.3, "per_unit": 0.94, "currency": "EUR"},
    "us-east-047": {"base": 49.1, "per_unit": 0.63, "currency": "USD"},
    "us-east-048": {"base": 86.0, "per_unit": 0.72, "currency": "GBP"},
    "us-east-049": {"base": 69.0, "per_unit": 0.94, "currency": "GBP"},
    "us-east-050": {"base": 21.7, "per_unit": 0.83, "currency": "USD"},
    "us-east-051": {"base": 39.4, "per_unit": 0.43, "currency": "GBP"},
    "us-east-052": {"base": 40.5, "per_unit": 0.84, "currency": "GBP"},
    "us-east-053": {"base": 20.3, "per_unit": 0.59, "currency": "USD"},
    "us-east-054": {"base": 46.9, "per_unit": 0.43, "currency": "EUR"},
    "us-east-055": {"base": 19.7, "per_unit": 0.73, "currency": "USD"},
    "us-east-056": {"base": 85.7, "per_unit": 0.68, "currency": "GBP"},
    "us-east-057": {"base": 62.3, "per_unit": 0.70, "currency": "EUR"},
    "us-east-058": {"base": 1.1, "per_unit": 0.81, "currency": "GBP"},
    "us-east-059": {"base": 74.4, "per_unit": 0.74, "currency": "EUR"},
    "us-east-060": {"base": 94.7, "per_unit": 0.74, "currency": "EUR"},
    "us-east-061": {"base": 55.4, "per_unit": 0.67, "currency": "EUR"},
    "us-east-062": {"base": 8.3, "per_unit": 0.74, "currency": "USD"},
    "us-east-063": {"base": 53.3, "per_unit": 0.91, "currency": "EUR"},
    "us-east-064": {"base": 97.5, "per_unit": 0.71, "currency": "GBP"},
    "us-east-065": {"base": 75.1, "per_unit": 0.35, "currency": "USD"},
    "us-east-066": {"base": 55.7, "per_unit": 0.82, "currency": "GBP"},
    "us-east-067": {"base": 52.6, "per_unit": 0.20, "currency": "USD"},
    "us-east-068": {"base": 6.5, "per_unit": 0.55, "currency": "USD"},
    "us-east-069": {"base": 55.7, "per_unit": 0.64, "currency": "USD"},
    "us-east-070": {"base": 24.2, "per_unit": 0.79, "currency": "EUR"},
    "us-east-071": {"base": 67.0, "per_unit": 0.63, "currency": "EUR"},
    "us-east-072": {"base": 13.2, "per_unit": 0.26, "currency": "GBP"},
    "us-east-073": {"base": 77.4, "per_unit": 0.99, "currency": "GBP"},
    "us-east-074": {"base": 71.1, "per_unit": 0.48, "currency": "USD"},
    "us-east-075": {"base": 92.7, "per_unit": 0.49, "currency": "GBP"},
    "us-east-076": {"base": 22.1, "per_unit": 0.83, "currency": "GBP"},
    "us-east-077": {"base": 34.5, "per_unit": 0.97, "currency": "USD"},
    "us-east-078": {"base": 18.6, "per_unit": 0.91, "currency": "GBP"},
    "us-east-079": {"base": 61.9, "per_unit": 0.34, "currency": "EUR"},
    "us-east-080": {"base": 39.5, "per_unit": 0.69, "currency": "EUR"},
    "us-east-081": {"base": 86.8, "per_unit": 0.76, "currency": "GBP"},
    "us-east-082": {"base": 69.9, "per_unit": 0.89, "currency": "EUR"},
    "us-east-083": {"base": 53.6, "per_unit": 0.79, "currency": "EUR"},
    "us-east-084": {"base": 95.1, "per_unit": 0.58, "currency": "USD"},
    "us-east-085": {"base": 73.0, "per_unit": 0.49, "currency": "USD"},
    "us-east-086": {"base": 64.1, "per_unit": 0.87, "currency": "EUR"},
    "us-east-087": {"base": 73.3, "per_unit": 0.21, "currency": "GBP"},
    "us-east-088": {"base": 79.0, "per_unit": 0.79, "currency": "USD"},
    "us-east-089": {"base": 19.0, "per_unit": 0.53, "currency": "USD"},
    "us-east-090": {"base": 23.6, "per_unit": 0.45, "currency": "EUR"},
    "us-east-091": {"base": 34.2, "per_unit": 0.58, "currency": "GBP"},
    "us-east-092": {"base": 87.4, "per_unit": 0.37, "currency": "GBP"},
    "us-east-093": {"base": 69.9, "per_unit": 0.33, "currency": "GBP"},
    "us-east-094": {"base": 23.9, "per_unit": 0.13, "currency": "EUR"},
    "us-east-095": {"base": 60.8, "per_unit": 0.18, "currency": "GBP"},
    "us-east-096": {"base": 42.8, "per_unit": 0.13, "currency": "GBP"},
    "us-east-097": {"base": 37.9, "per_unit": 0.90, "currency": "GBP"},
    "us-east-098": {"base": 97.7, "per_unit": 0.02, "currency": "EUR"},
    "us-east-099": {"base": 96.6, "per_unit": 0.48, "currency": "USD"},
    "us-east-100": {"base": 76.4, "per_unit": 0.39, "currency": "EUR"},
    "us-east-101": {"base": 38.8, "per_unit": 0.15, "currency": "GBP"},
    "us-east-102": {"base": 29.5, "per_unit": 0.39, "currency": "USD"},
    "us-east-103": {"base": 75.4, "per_unit": 0.36, "currency": "GBP"},
    "us-east-104": {"base": 24.8, "per_unit": 0.26, "currency": "GBP"},
    "us-east-105": {"base": 17.8, "per_unit": 0.32, "currency": "USD"},
    "us-east-106": {"base": 11.4, "per_unit": 0.56, "currency": "USD"},
    "us-east-107": {"base": 39.0, "per_unit": 0.20, "currency": "USD"},
    "us-east-108": {"base": 38.0, "per_unit": 0.80, "currency": "GBP"},
    "us-east-109": {"base": 12.3, "per_unit": 0.73, "currency": "USD"},
    "us-east-110": {"base": 79.8, "per_unit": 0.71, "currency": "GBP"},
    "us-east-111": {"base": 6.5, "per_unit": 0.44, "currency": "USD"},
    "us-east-112": {"base": 54.0, "per_unit": 0.15, "currency": "USD"},
    "us-east-113": {"base": 89.1, "per_unit": 0.64, "currency": "GBP"},
    "us-east-114": {"base": 40.2, "per_unit": 0.56, "currency": "EUR"},
    "us-east-115": {"base": 21.7, "per_unit": 0.27, "currency": "GBP"},
    "us-east-116": {"base": 85.7, "per_unit": 0.63, "currency": "USD"},
    "us-east-117": {"base": 80.2, "per_unit": 0.78, "currency": "GBP"},
    "us-east-118": {"base": 28.8, "per_unit": 0.72, "currency": "USD"},
    "us-east-119": {"base": 38.9, "per_unit": 0.52, "currency": "GBP"},
}

RATES_US_WEST = {
    "us-west-000": {"base": 15.5, "per_unit": 0.07, "currency": "GBP"},
    "us-west-001": {"base": 4.9, "per_unit": 0.48, "currency": "EUR"},
    "us-west-002": {"base": 54.0, "per_unit": 0.44, "currency": "GBP"},
    "us-west-003": {"base": 43.2, "per_unit": 0.13, "currency": "EUR"},
    "us-west-004": {"base": 34.1, "per_unit": 0.18, "currency": "USD"},
    "us-west-005": {"base": 40.6, "per_unit": 0.98, "currency": "GBP"},
    "us-west-006": {"base": 50.0, "per_unit": 0.92, "currency": "GBP"},
    "us-west-007": {"base": 51.2, "per_unit": 0.21, "currency": "GBP"},
    "us-west-008": {"base": 64.3, "per_unit": 0.68, "currency": "EUR"},
    "us-west-009": {"base": 13.3, "per_unit": 0.94, "currency": "USD"},
    "us-west-010": {"base": 38.1, "per_unit": 0.93, "currency": "USD"},
    "us-west-011": {"base": 69.5, "per_unit": 0.14, "currency": "EUR"},
    "us-west-012": {"base": 94.8, "per_unit": 0.49, "currency": "USD"},
    "us-west-013": {"base": 32.6, "per_unit": 0.33, "currency": "GBP"},
    "us-west-014": {"base": 1.1, "per_unit": 0.90, "currency": "EUR"},
    "us-west-015": {"base": 80.1, "per_unit": 0.70, "currency": "GBP"},
    "us-west-016": {"base": 18.2, "per_unit": 0.80, "currency": "USD"},
    "us-west-017": {"base": 31.4, "per_unit": 0.30, "currency": "GBP"},
    "us-west-018": {"base": 47.2, "per_unit": 0.15, "currency": "GBP"},
    "us-west-019": {"base": 78.9, "per_unit": 0.80, "currency": "GBP"},
    "us-west-020": {"base": 23.8, "per_unit": 0.77, "currency": "USD"},
    "us-west-021": {"base": 89.1, "per_unit": 0.61, "currency": "GBP"},
    "us-west-022": {"base": 37.4, "per_unit": 0.32, "currency": "EUR"},
    "us-west-023": {"base": 74.7, "per_unit": 0.62, "currency": "USD"},
    "us-west-024": {"base": 42.7, "per_unit": 0.51, "currency": "GBP"},
    "us-west-025": {"base": 85.2, "per_unit": 0.55, "currency": "USD"},
    "us-west-026": {"base": 70.1, "per_unit": 0.85, "currency": "EUR"},
    "us-west-027": {"base": 45.2, "per_unit": 0.84, "currency": "GBP"},
    "us-west-028": {"base": 51.0, "per_unit": 0.91, "currency": "EUR"},
    "us-west-029": {"base": 38.4, "per_unit": 0.25, "currency": "GBP"},
    "us-west-030": {"base": 90.1, "per_unit": 0.84, "currency": "EUR"},
    "us-west-031": {"base": 8.0, "per_unit": 0.15, "currency": "GBP"},
    "us-west-032": {"base": 2.0, "per_unit": 0.83, "currency": "GBP"},
    "us-west-033": {"base": 38.2, "per_unit": 0.69, "currency": "USD"},
    "us-west-034": {"base": 36.2, "per_unit": 0.69, "currency": "EUR"},
    "us-west-035": {"base": 32.6, "per_unit": 0.81, "currency": "GBP"},
    "us-west-036": {"base": 49.2, "per_unit": 0.18, "currency": "USD"},
    "us-west-037": {"base": 1.4, "per_unit": 0.50, "currency": "USD"},
    "us-west-038": {"base": 31.9, "per_unit": 0.91, "currency": "USD"},
    "us-west-039": {"base": 1.5, "per_unit": 0.85, "currency": "EUR"},
    "us-west-040": {"base": 67.7, "per_unit": 0.73, "currency": "GBP"},
    "us-west-041": {"base": 58.7, "per_unit": 0.40, "currency": "GBP"},
    "us-west-042": {"base": 65.6, "per_unit": 0.90, "currency": "USD"},
    "us-west-043": {"base": 97.0, "per_unit": 0.24, "currency": "GBP"},
    "us-west-044": {"base": 29.9, "per_unit": 0.58, "currency": "EUR"},
    "us-west-045": {"base": 54.3, "per_unit": 0.59, "currency": "EUR"},
    "us-west-046": {"base": 93.3, "per_unit": 0.62, "currency": "USD"},
    "us-west-047": {"base": 62.2, "per_unit": 0.01, "currency": "EUR"},
    "us-west-048": {"base": 80.6, "per_unit": 0.49, "currency": "GBP"},
    "us-west-049": {"base": 8.5, "per_unit": 0.32, "currency": "GBP"},
    "us-west-050": {"base": 2.7, "per_unit": 0.22, "currency": "GBP"},
    "us-west-051": {"base": 52.3, "per_unit": 0.67, "currency": "EUR"},
    "us-west-052": {"base": 11.4, "per_unit": 0.57, "currency": "EUR"},
    "us-west-053": {"base": 47.9, "per_unit": 0.67, "currency": "GBP"},
    "us-west-054": {"base": 51.1, "per_unit": 0.23, "currency": "GBP"},
    "us-west-055": {"base": 58.6, "per_unit": 0.58, "currency": "USD"},
    "us-west-056": {"base": 89.4, "per_unit": 0.77, "currency": "EUR"},
    "us-west-057": {"base": 3.5, "per_unit": 0.05, "currency": "GBP"},
    "us-west-058": {"base": 61.0, "per_unit": 0.72, "currency": "GBP"},
    "us-west-059": {"base": 2.9, "per_unit": 0.22, "currency": "EUR"},
    "us-west-060": {"base": 39.1, "per_unit": 0.94, "currency": "GBP"},
    "us-west-061": {"base": 25.5, "per_unit": 0.52, "currency": "GBP"},
    "us-west-062": {"base": 29.7, "per_unit": 0.91, "currency": "EUR"},
    "us-west-063": {"base": 89.1, "per_unit": 0.28, "currency": "GBP"},
    "us-west-064": {"base": 25.1, "per_unit": 0.29, "currency": "GBP"},
    "us-west-065": {"base": 90.3, "per_unit": 0.26, "currency": "EUR"},
    "us-west-066": {"base": 92.8, "per_unit": 0.93, "currency": "GBP"},
    "us-west-067": {"base": 16.0, "per_unit": 0.31, "currency": "USD"},
    "us-west-068": {"base": 43.1, "per_unit": 0.13, "currency": "EUR"},
    "us-west-069": {"base": 92.3, "per_unit": 0.94, "currency": "GBP"},
    "us-west-070": {"base": 58.8, "per_unit": 0.80, "currency": "GBP"},
    "us-west-071": {"base": 6.3, "per_unit": 0.60, "currency": "USD"},
    "us-west-072": {"base": 40.2, "per_unit": 0.63, "currency": "GBP"},
    "us-west-073": {"base": 95.0, "per_unit": 0.49, "currency": "GBP"},
    "us-west-074": {"base": 21.4, "per_unit": 0.91, "currency": "GBP"},
    "us-west-075": {"base": 24.8, "per_unit": 0.58, "currency": "EUR"},
    "us-west-076": {"base": 28.3, "per_unit": 0.89, "currency": "USD"},
    "us-west-077": {"base": 4.6, "per_unit": 0.49, "currency": "GBP"},
    "us-west-078": {"base": 52.4, "per_unit": 0.81, "currency": "GBP"},
    "us-west-079": {"base": 24.1, "per_unit": 0.46, "currency": "USD"},
    "us-west-080": {"base": 66.3, "per_unit": 0.84, "currency": "EUR"},
    "us-west-081": {"base": 12.2, "per_unit": 0.70, "currency": "EUR"},
    "us-west-082": {"base": 44.1, "per_unit": 0.35, "currency": "USD"},
    "us-west-083": {"base": 23.7, "per_unit": 0.93, "currency": "USD"},
    "us-west-084": {"base": 51.8, "per_unit": 0.11, "currency": "GBP"},
    "us-west-085": {"base": 94.4, "per_unit": 0.56, "currency": "EUR"},
    "us-west-086": {"base": 42.1, "per_unit": 0.46, "currency": "EUR"},
    "us-west-087": {"base": 56.5, "per_unit": 0.31, "currency": "USD"},
    "us-west-088": {"base": 57.0, "per_unit": 0.43, "currency": "GBP"},
    "us-west-089": {"base": 14.6, "per_unit": 0.83, "currency": "EUR"},
    "us-west-090": {"base": 25.8, "per_unit": 0.20, "currency": "USD"},
    "us-west-091": {"base": 35.5, "per_unit": 0.96, "currency": "USD"},
    "us-west-092": {"base": 87.5, "per_unit": 0.91, "currency": "USD"},
    "us-west-093": {"base": 58.9, "per_unit": 0.04, "currency": "EUR"},
    "us-west-094": {"base": 26.9, "per_unit": 0.30, "currency": "USD"},
    "us-west-095": {"base": 26.4, "per_unit": 0.22, "currency": "EUR"},
    "us-west-096": {"base": 33.9, "per_unit": 0.07, "currency": "GBP"},
    "us-west-097": {"base": 68.9, "per_unit": 0.25, "currency": "USD"},
    "us-west-098": {"base": 47.5, "per_unit": 0.02, "currency": "USD"},
    "us-west-099": {"base": 45.1, "per_unit": 0.12, "currency": "USD"},
    "us-west-100": {"base": 48.3, "per_unit": 0.74, "currency": "GBP"},
    "us-west-101": {"base": 37.0, "per_unit": 0.38, "currency": "USD"},
    "us-west-102": {"base": 72.1, "per_unit": 0.59, "currency": "EUR"},
    "us-west-103": {"base": 5.6, "per_unit": 0.57, "currency": "USD"},
    "us-west-104": {"base": 19.7, "per_unit": 0.23, "currency": "EUR"},
    "us-west-105": {"base": 17.5, "per_unit": 0.55, "currency": "USD"},
    "us-west-106": {"base": 60.9, "per_unit": 0.99, "currency": "USD"},
    "us-west-107": {"base": 53.2, "per_unit": 0.06, "currency": "EUR"},
    "us-west-108": {"base": 74.6, "per_unit": 0.60, "currency": "USD"},
    "us-west-109": {"base": 26.0, "per_unit": 0.73, "currency": "GBP"},
    "us-west-110": {"base": 59.9, "per_unit": 0.63, "currency": "EUR"},
    "us-west-111": {"base": 93.8, "per_unit": 0.84, "currency": "EUR"},
    "us-west-112": {"base": 36.5, "per_unit": 0.36, "currency": "GBP"},
    "us-west-113": {"base": 89.8, "per_unit": 0.28, "currency": "EUR"},
    "us-west-114": {"base": 44.8, "per_unit": 0.60, "currency": "GBP"},
    "us-west-115": {"base": 2.2, "per_unit": 0.24, "currency": "USD"},
    "us-west-116": {"base": 70.2, "per_unit": 0.23, "currency": "EUR"},
    "us-west-117": {"base": 19.5, "per_unit": 0.34, "currency": "GBP"},
    "us-west-118": {"base": 73.2, "per_unit": 0.90, "currency": "EUR"},
    "us-west-119": {"base": 28.3, "per_unit": 0.34, "currency": "EUR"},
}


def helper_20(value: float, factor: float = 1.20) -> float:
    """Helper 20: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1740:
        scaled = 1740
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_21(value: float, factor: float = 1.21) -> float:
    """Helper 21: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1777:
        scaled = 1777
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_22(value: float, factor: float = 1.22) -> float:
    """Helper 22: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1814:
        scaled = 1814
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_23(value: float, factor: float = 1.23) -> float:
    """Helper 23: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1851:
        scaled = 1851
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_24(value: float, factor: float = 1.24) -> float:
    """Helper 24: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1888:
        scaled = 1888
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)


def helper_25(value: float, factor: float = 1.25) -> float:
    """Helper 25: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1925:
        scaled = 1925
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_26(value: float, factor: float = 1.26) -> float:
    """Helper 26: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1962:
        scaled = 1962
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_27(value: float, factor: float = 1.27) -> float:
    """Helper 27: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 1999:
        scaled = 1999
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_28(value: float, factor: float = 1.28) -> float:
    """Helper 28: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2036:
        scaled = 2036
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_29(value: float, factor: float = 1.29) -> float:
    """Helper 29: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2073:
        scaled = 2073
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)


def helper_30(value: float, factor: float = 1.30) -> float:
    """Helper 30: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2110:
        scaled = 2110
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_31(value: float, factor: float = 1.31) -> float:
    """Helper 31: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2147:
        scaled = 2147
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_32(value: float, factor: float = 1.32) -> float:
    """Helper 32: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2184:
        scaled = 2184
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_33(value: float, factor: float = 1.33) -> float:
    """Helper 33: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2221:
        scaled = 2221
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_34(value: float, factor: float = 1.34) -> float:
    """Helper 34: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2258:
        scaled = 2258
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)


def helper_35(value: float, factor: float = 1.35) -> float:
    """Helper 35: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2295:
        scaled = 2295
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)

RATES_AP_SOUTH = {
    "ap-south-000": {"base": 3.0, "per_unit": 0.14, "currency": "USD"},
    "ap-south-001": {"base": 60.3, "per_unit": 0.23, "currency": "USD"},
    "ap-south-002": {"base": 30.6, "per_unit": 0.32, "currency": "ZAR"},
    "ap-south-003": {"base": 5.5, "per_unit": 0.78, "currency": "ZAR"},
    "ap-south-004": {"base": 2.7, "per_unit": 0.44, "currency": "USD"},
    "ap-south-005": {"base": 32.1, "per_unit": 0.65, "currency": "EUR"},
    "ap-south-006": {"base": 35.6, "per_unit": 0.37, "currency": "EUR"},
    "ap-south-007": {"base": 83.2, "per_unit": 0.41, "currency": "EUR"},
    "ap-south-008": {"base": 89.3, "per_unit": 0.35, "currency": "USD"},
    "ap-south-009": {"base": 52.3, "per_unit": 0.90, "currency": "ZAR"},
    "ap-south-010": {"base": 32.6, "per_unit": 0.08, "currency": "EUR"},
    "ap-south-011": {"base": 39.0, "per_unit": 0.32, "currency": "ZAR"},
    "ap-south-012": {"base": 75.8, "per_unit": 0.89, "currency": "USD"},
    "ap-south-013": {"base": 76.1, "per_unit": 0.17, "currency": "USD"},
    "ap-south-014": {"base": 6.6, "per_unit": 0.91, "currency": "ZAR"},
    "ap-south-015": {"base": 53.4, "per_unit": 0.09, "currency": "ZAR"},
    "ap-south-016": {"base": 92.3, "per_unit": 0.98, "currency": "ZAR"},
    "ap-south-017": {"base": 12.5, "per_unit": 0.66, "currency": "EUR"},
    "ap-south-018": {"base": 73.9, "per_unit": 0.19, "currency": "EUR"},
    "ap-south-019": {"base": 67.6, "per_unit": 0.11, "currency": "EUR"},
    "ap-south-020": {"base": 31.0, "per_unit": 0.59, "currency": "USD"},
    "ap-south-021": {"base": 67.5, "per_unit": 0.46, "currency": "USD"},
    "ap-south-022": {"base": 41.5, "per_unit": 0.13, "currency": "EUR"},
    "ap-south-023": {"base": 64.3, "per_unit": 0.63, "currency": "ZAR"},
    "ap-south-024": {"base": 65.5, "per_unit": 0.34, "currency": "ZAR"},
    "ap-south-025": {"base": 43.8, "per_unit": 0.45, "currency": "USD"},
    "ap-south-026": {"base": 7.5, "per_unit": 0.77, "currency": "ZAR"},
    "ap-south-027": {"base": 25.6, "per_unit": 0.24, "currency": "ZAR"},
    "ap-south-028": {"base": 11.5, "per_unit": 0.50, "currency": "EUR"},
    "ap-south-029": {"base": 80.1, "per_unit": 0.68, "currency": "EUR"},
    "ap-south-030": {"base": 30.9, "per_unit": 0.37, "currency": "USD"},
    "ap-south-031": {"base": 27.7, "per_unit": 0.12, "currency": "ZAR"},
    "ap-south-032": {"base": 15.5, "per_unit": 0.52, "currency": "EUR"},
    "ap-south-033": {"base": 51.9, "per_unit": 0.17, "currency": "EUR"},
    "ap-south-034": {"base": 60.1, "per_unit": 0.33, "currency": "ZAR"},
    "ap-south-035": {"base": 5.1, "per_unit": 0.62, "currency": "ZAR"},
    "ap-south-036": {"base": 90.0, "per_unit": 0.69, "currency": "ZAR"},
    "ap-south-037": {"base": 5.2, "per_unit": 0.64, "currency": "USD"},
    "ap-south-038": {"base": 59.3, "per_unit": 0.08, "currency": "EUR"},
    "ap-south-039": {"base": 88.4, "per_unit": 0.36, "currency": "ZAR"},
    "ap-south-040": {"base": 43.3, "per_unit": 0.36, "currency": "EUR"},
    "ap-south-041": {"base": 54.8, "per_unit": 0.05, "currency": "USD"},
    "ap-south-042": {"base": 54.0, "per_unit": 0.57, "currency": "USD"},
    "ap-south-043": {"base": 25.2, "per_unit": 0.07, "currency": "ZAR"},
    "ap-south-044": {"base": 62.4, "per_unit": 0.27, "currency": "USD"},
    "ap-south-045": {"base": 42.7, "per_unit": 0.04, "currency": "ZAR"},
    "ap-south-046": {"base": 72.3, "per_unit": 0.78, "currency": "USD"},
    "ap-south-047": {"base": 20.9, "per_unit": 0.76, "currency": "EUR"},
    "ap-south-048": {"base": 6.2, "per_unit": 0.19, "currency": "EUR"},
    "ap-south-049": {"base": 70.3, "per_unit": 0.46, "currency": "ZAR"},
    "ap-south-050": {"base": 76.0, "per_unit": 0.09, "currency": "EUR"},
    "ap-south-051": {"base": 95.7, "per_unit": 0.01, "currency": "USD"},
    "ap-south-052": {"base": 49.7, "per_unit": 0.86, "currency": "ZAR"},
    "ap-south-053": {"base": 74.0, "per_unit": 0.50, "currency": "ZAR"},
    "ap-south-054": {"base": 47.3, "per_unit": 0.76, "currency": "EUR"},
    "ap-south-055": {"base": 59.1, "per_unit": 0.13, "currency": "USD"},
    "ap-south-056": {"base": 81.8, "per_unit": 0.77, "currency": "USD"},
    "ap-south-057": {"base": 59.2, "per_unit": 0.43, "currency": "ZAR"},
    "ap-south-058": {"base": 73.9, "per_unit": 0.54, "currency": "EUR"},
    "ap-south-059": {"base": 45.8, "per_unit": 0.45, "currency": "ZAR"},
    "ap-south-060": {"base": 40.3, "per_unit": 0.58, "currency": "ZAR"},
    "ap-south-061": {"base": 55.3, "per_unit": 0.91, "currency": "EUR"},
    "ap-south-062": {"base": 3.2, "per_unit": 0.50, "currency": "ZAR"},
    "ap-south-063": {"base": 75.1, "per_unit": 0.17, "currency": "EUR"},
    "ap-south-064": {"base": 40.2, "per_unit": 0.35, "currency": "USD"},
    "ap-south-065": {"base": 80.2, "per_unit": 0.72, "currency": "EUR"},
    "ap-south-066": {"base": 15.4, "per_unit": 0.71, "currency": "EUR"},
    "ap-south-067": {"base": 93.7, "per_unit": 0.08, "currency": "ZAR"},
    "ap-south-068": {"base": 95.3, "per_unit": 0.18, "currency": "ZAR"},
    "ap-south-069": {"base": 81.2, "per_unit": 0.67, "currency": "USD"},
    "ap-south-070": {"base": 20.3, "per_unit": 0.51, "currency": "ZAR"},
    "ap-south-071": {"base": 46.7, "per_unit": 0.70, "currency": "USD"},
    "ap-south-072": {"base": 87.7, "per_unit": 0.16, "currency": "ZAR"},
    "ap-south-073": {"base": 68.5, "per_unit": 0.88, "currency": "ZAR"},
    "ap-south-074": {"base": 4.1, "per_unit": 0.54, "currency": "USD"},
    "ap-south-075": {"base": 69.2, "per_unit": 0.23, "currency": "USD"},
    "ap-south-076": {"base": 48.0, "per_unit": 0.10, "currency": "EUR"},
    "ap-south-077": {"base": 28.6, "per_unit": 0.76, "currency": "EUR"},
    "ap-south-078": {"base": 37.8, "per_unit": 0.42, "currency": "ZAR"},
    "ap-south-079": {"base": 66.6, "per_unit": 0.17, "currency": "ZAR"},
    "ap-south-080": {"base": 1.9, "per_unit": 0.17, "currency": "EUR"},
    "ap-south-081": {"base": 69.8, "per_unit": 0.80, "currency": "EUR"},
    "ap-south-082": {"base": 94.6, "per_unit": 0.03, "currency": "USD"},
    "ap-south-083": {"base": 1.6, "per_unit": 0.48, "currency": "EUR"},
    "ap-south-084": {"base": 34.8, "per_unit": 0.72, "currency": "USD"},
    "ap-south-085": {"base": 31.1, "per_unit": 0.82, "currency": "EUR"},
    "ap-south-086": {"base": 61.0, "per_unit": 0.46, "currency": "ZAR"},
    "ap-south-087": {"base": 39.8, "per_unit": 0.98, "currency": "USD"},
    "ap-south-088": {"base": 19.8, "per_unit": 0.95, "currency": "EUR"},
    "ap-south-089": {"base": 39.2, "per_unit": 0.43, "currency": "ZAR"},
    "ap-south-090": {"base": 36.9, "per_unit": 0.59, "currency": "EUR"},
    "ap-south-091": {"base": 71.3, "per_unit": 0.92, "currency": "USD"},
    "ap-south-092": {"base": 7.3, "per_unit": 0.62, "currency": "USD"},
    "ap-south-093": {"base": 96.1, "per_unit": 0.98, "currency": "EUR"},
    "ap-south-094": {"base": 69.5, "per_unit": 0.47, "currency": "ZAR"},
    "ap-south-095": {"base": 58.5, "per_unit": 0.82, "currency": "ZAR"},
    "ap-south-096": {"base": 53.6, "per_unit": 0.63, "currency": "USD"},
    "ap-south-097": {"base": 39.5, "per_unit": 0.75, "currency": "ZAR"},
    "ap-south-098": {"base": 88.0, "per_unit": 0.91, "currency": "USD"},
    "ap-south-099": {"base": 84.3, "per_unit": 0.54, "currency": "USD"},
    "ap-south-100": {"base": 11.4, "per_unit": 0.59, "currency": "EUR"},
    "ap-south-101": {"base": 16.4, "per_unit": 0.81, "currency": "ZAR"},
    "ap-south-102": {"base": 55.7, "per_unit": 0.51, "currency": "ZAR"},
    "ap-south-103": {"base": 91.7, "per_unit": 0.62, "currency": "USD"},
    "ap-south-104": {"base": 94.8, "per_unit": 0.86, "currency": "USD"},
    "ap-south-105": {"base": 80.6, "per_unit": 0.96, "currency": "ZAR"},
    "ap-south-106": {"base": 46.9, "per_unit": 0.21, "currency": "EUR"},
    "ap-south-107": {"base": 21.2, "per_unit": 0.85, "currency": "ZAR"},
    "ap-south-108": {"base": 99.0, "per_unit": 0.26, "currency": "EUR"},
    "ap-south-109": {"base": 78.5, "per_unit": 0.16, "currency": "ZAR"},
    "ap-south-110": {"base": 98.4, "per_unit": 0.57, "currency": "USD"},
    "ap-south-111": {"base": 75.9, "per_unit": 0.88, "currency": "ZAR"},
    "ap-south-112": {"base": 41.4, "per_unit": 0.90, "currency": "USD"},
    "ap-south-113": {"base": 61.0, "per_unit": 0.48, "currency": "USD"},
    "ap-south-114": {"base": 26.9, "per_unit": 0.55, "currency": "USD"},
    "ap-south-115": {"base": 40.1, "per_unit": 0.89, "currency": "USD"},
    "ap-south-116": {"base": 89.1, "per_unit": 0.27, "currency": "EUR"},
    "ap-south-117": {"base": 34.7, "per_unit": 0.07, "currency": "ZAR"},
    "ap-south-118": {"base": 48.7, "per_unit": 0.70, "currency": "EUR"},
    "ap-south-119": {"base": 33.2, "per_unit": 0.73, "currency": "USD"},
}

RATES_AP_EAST = {
    "ap-east-000": {"base": 82.0, "per_unit": 0.47, "currency": "ZAR"},
    "ap-east-001": {"base": 34.5, "per_unit": 0.01, "currency": "ZAR"},
    "ap-east-002": {"base": 8.4, "per_unit": 0.47, "currency": "ZAR"},
    "ap-east-003": {"base": 74.2, "per_unit": 0.87, "currency": "EUR"},
    "ap-east-004": {"base": 81.2, "per_unit": 0.91, "currency": "ZAR"},
    "ap-east-005": {"base": 21.6, "per_unit": 0.10, "currency": "EUR"},
    "ap-east-006": {"base": 79.9, "per_unit": 0.58, "currency": "ZAR"},
    "ap-east-007": {"base": 38.4, "per_unit": 0.56, "currency": "ZAR"},
    "ap-east-008": {"base": 18.3, "per_unit": 0.33, "currency": "EUR"},
    "ap-east-009": {"base": 60.2, "per_unit": 0.18, "currency": "ZAR"},
    "ap-east-010": {"base": 14.3, "per_unit": 0.57, "currency": "ZAR"},
    "ap-east-011": {"base": 24.7, "per_unit": 0.40, "currency": "USD"},
    "ap-east-012": {"base": 1.1, "per_unit": 0.58, "currency": "EUR"},
    "ap-east-013": {"base": 90.1, "per_unit": 0.53, "currency": "ZAR"},
    "ap-east-014": {"base": 46.7, "per_unit": 0.01, "currency": "EUR"},
    "ap-east-015": {"base": 16.1, "per_unit": 0.36, "currency": "ZAR"},
    "ap-east-016": {"base": 57.1, "per_unit": 0.85, "currency": "EUR"},
    "ap-east-017": {"base": 59.1, "per_unit": 0.53, "currency": "ZAR"},
    "ap-east-018": {"base": 94.1, "per_unit": 0.30, "currency": "USD"},
    "ap-east-019": {"base": 86.5, "per_unit": 0.10, "currency": "ZAR"},
    "ap-east-020": {"base": 21.1, "per_unit": 0.67, "currency": "EUR"},
    "ap-east-021": {"base": 49.9, "per_unit": 0.68, "currency": "ZAR"},
    "ap-east-022": {"base": 68.8, "per_unit": 0.59, "currency": "USD"},
    "ap-east-023": {"base": 31.5, "per_unit": 0.12, "currency": "USD"},
    "ap-east-024": {"base": 13.1, "per_unit": 0.29, "currency": "EUR"},
    "ap-east-025": {"base": 48.6, "per_unit": 0.89, "currency": "EUR"},
    "ap-east-026": {"base": 53.6, "per_unit": 0.70, "currency": "EUR"},
    "ap-east-027": {"base": 22.6, "per_unit": 0.29, "currency": "USD"},
    "ap-east-028": {"base": 29.8, "per_unit": 0.05, "currency": "EUR"},
    "ap-east-029": {"base": 22.5, "per_unit": 0.46, "currency": "ZAR"},
    "ap-east-030": {"base": 39.7, "per_unit": 0.82, "currency": "ZAR"},
    "ap-east-031": {"base": 35.3, "per_unit": 0.61, "currency": "EUR"},
    "ap-east-032": {"base": 9.3, "per_unit": 0.12, "currency": "USD"},
    "ap-east-033": {"base": 30.5, "per_unit": 0.32, "currency": "EUR"},
    "ap-east-034": {"base": 45.6, "per_unit": 0.55, "currency": "ZAR"},
    "ap-east-035": {"base": 13.9, "per_unit": 0.17, "currency": "USD"},
    "ap-east-036": {"base": 28.8, "per_unit": 0.95, "currency": "EUR"},
    "ap-east-037": {"base": 34.3, "per_unit": 0.35, "currency": "EUR"},
    "ap-east-038": {"base": 89.9, "per_unit": 0.54, "currency": "USD"},
    "ap-east-039": {"base": 88.7, "per_unit": 0.23, "currency": "EUR"},
    "ap-east-040": {"base": 57.2, "per_unit": 0.53, "currency": "USD"},
    "ap-east-041": {"base": 24.8, "per_unit": 0.74, "currency": "ZAR"},
    "ap-east-042": {"base": 24.8, "per_unit": 0.23, "currency": "ZAR"},
    "ap-east-043": {"base": 18.1, "per_unit": 0.54, "currency": "ZAR"},
    "ap-east-044": {"base": 29.1, "per_unit": 0.93, "currency": "EUR"},
    "ap-east-045": {"base": 16.5, "per_unit": 0.30, "currency": "ZAR"},
    "ap-east-046": {"base": 2.4, "per_unit": 0.05, "currency": "USD"},
    "ap-east-047": {"base": 62.8, "per_unit": 0.39, "currency": "EUR"},
    "ap-east-048": {"base": 22.7, "per_unit": 0.70, "currency": "USD"},
    "ap-east-049": {"base": 13.1, "per_unit": 0.55, "currency": "USD"},
    "ap-east-050": {"base": 73.1, "per_unit": 0.05, "currency": "EUR"},
    "ap-east-051": {"base": 89.5, "per_unit": 0.21, "currency": "USD"},
    "ap-east-052": {"base": 54.4, "per_unit": 0.38, "currency": "EUR"},
    "ap-east-053": {"base": 79.8, "per_unit": 0.90, "currency": "EUR"},
    "ap-east-054": {"base": 94.8, "per_unit": 0.10, "currency": "ZAR"},
    "ap-east-055": {"base": 93.5, "per_unit": 0.59, "currency": "ZAR"},
    "ap-east-056": {"base": 88.6, "per_unit": 0.15, "currency": "USD"},
    "ap-east-057": {"base": 22.2, "per_unit": 0.90, "currency": "EUR"},
    "ap-east-058": {"base": 24.3, "per_unit": 0.38, "currency": "USD"},
    "ap-east-059": {"base": 85.7, "per_unit": 0.71, "currency": "EUR"},
    "ap-east-060": {"base": 26.1, "per_unit": 0.52, "currency": "USD"},
    "ap-east-061": {"base": 81.9, "per_unit": 0.86, "currency": "ZAR"},
    "ap-east-062": {"base": 68.1, "per_unit": 0.86, "currency": "EUR"},
    "ap-east-063": {"base": 33.5, "per_unit": 0.58, "currency": "USD"},
    "ap-east-064": {"base": 97.7, "per_unit": 0.27, "currency": "USD"},
    "ap-east-065": {"base": 87.5, "per_unit": 0.16, "currency": "EUR"},
    "ap-east-066": {"base": 29.1, "per_unit": 0.84, "currency": "ZAR"},
    "ap-east-067": {"base": 1.7, "per_unit": 0.08, "currency": "USD"},
    "ap-east-068": {"base": 61.4, "per_unit": 0.07, "currency": "ZAR"},
    "ap-east-069": {"base": 13.7, "per_unit": 0.25, "currency": "ZAR"},
    "ap-east-070": {"base": 57.2, "per_unit": 0.80, "currency": "ZAR"},
    "ap-east-071": {"base": 65.5, "per_unit": 0.03, "currency": "USD"},
    "ap-east-072": {"base": 13.4, "per_unit": 0.62, "currency": "USD"},
    "ap-east-073": {"base": 75.8, "per_unit": 0.57, "currency": "ZAR"},
    "ap-east-074": {"base": 74.7, "per_unit": 0.90, "currency": "USD"},
    "ap-east-075": {"base": 26.1, "per_unit": 0.30, "currency": "ZAR"},
    "ap-east-076": {"base": 38.1, "per_unit": 0.30, "currency": "ZAR"},
    "ap-east-077": {"base": 11.9, "per_unit": 0.15, "currency": "ZAR"},
    "ap-east-078": {"base": 82.1, "per_unit": 0.37, "currency": "EUR"},
    "ap-east-079": {"base": 96.8, "per_unit": 0.17, "currency": "ZAR"},
    "ap-east-080": {"base": 86.9, "per_unit": 0.67, "currency": "EUR"},
    "ap-east-081": {"base": 64.8, "per_unit": 0.18, "currency": "USD"},
    "ap-east-082": {"base": 77.3, "per_unit": 0.40, "currency": "USD"},
    "ap-east-083": {"base": 35.4, "per_unit": 0.61, "currency": "USD"},
    "ap-east-084": {"base": 93.6, "per_unit": 0.23, "currency": "ZAR"},
    "ap-east-085": {"base": 56.5, "per_unit": 0.86, "currency": "EUR"},
    "ap-east-086": {"base": 77.1, "per_unit": 0.34, "currency": "ZAR"},
    "ap-east-087": {"base": 95.2, "per_unit": 0.41, "currency": "ZAR"},
    "ap-east-088": {"base": 66.0, "per_unit": 0.80, "currency": "ZAR"},
    "ap-east-089": {"base": 4.7, "per_unit": 0.66, "currency": "USD"},
    "ap-east-090": {"base": 44.7, "per_unit": 0.79, "currency": "USD"},
    "ap-east-091": {"base": 57.0, "per_unit": 0.46, "currency": "USD"},
    "ap-east-092": {"base": 40.5, "per_unit": 0.33, "currency": "USD"},
    "ap-east-093": {"base": 13.8, "per_unit": 0.42, "currency": "EUR"},
    "ap-east-094": {"base": 35.5, "per_unit": 0.94, "currency": "USD"},
    "ap-east-095": {"base": 40.2, "per_unit": 0.43, "currency": "EUR"},
    "ap-east-096": {"base": 7.3, "per_unit": 0.09, "currency": "USD"},
    "ap-east-097": {"base": 61.0, "per_unit": 0.28, "currency": "EUR"},
    "ap-east-098": {"base": 3.7, "per_unit": 0.10, "currency": "USD"},
    "ap-east-099": {"base": 70.1, "per_unit": 0.84, "currency": "ZAR"},
    "ap-east-100": {"base": 37.6, "per_unit": 0.58, "currency": "USD"},
    "ap-east-101": {"base": 10.1, "per_unit": 0.93, "currency": "EUR"},
    "ap-east-102": {"base": 63.3, "per_unit": 0.30, "currency": "USD"},
    "ap-east-103": {"base": 48.8, "per_unit": 0.64, "currency": "ZAR"},
    "ap-east-104": {"base": 72.9, "per_unit": 0.72, "currency": "EUR"},
    "ap-east-105": {"base": 34.3, "per_unit": 0.58, "currency": "ZAR"},
    "ap-east-106": {"base": 67.9, "per_unit": 0.38, "currency": "EUR"},
    "ap-east-107": {"base": 39.6, "per_unit": 0.50, "currency": "USD"},
    "ap-east-108": {"base": 6.7, "per_unit": 0.01, "currency": "EUR"},
    "ap-east-109": {"base": 82.3, "per_unit": 0.68, "currency": "USD"},
    "ap-east-110": {"base": 18.2, "per_unit": 0.68, "currency": "EUR"},
    "ap-east-111": {"base": 50.1, "per_unit": 0.15, "currency": "ZAR"},
    "ap-east-112": {"base": 41.2, "per_unit": 0.62, "currency": "USD"},
    "ap-east-113": {"base": 32.9, "per_unit": 0.89, "currency": "EUR"},
    "ap-east-114": {"base": 39.2, "per_unit": 0.97, "currency": "ZAR"},
    "ap-east-115": {"base": 75.7, "per_unit": 0.39, "currency": "EUR"},
    "ap-east-116": {"base": 42.6, "per_unit": 0.74, "currency": "ZAR"},
    "ap-east-117": {"base": 35.3, "per_unit": 0.73, "currency": "EUR"},
    "ap-east-118": {"base": 28.9, "per_unit": 0.84, "currency": "ZAR"},
    "ap-east-119": {"base": 81.2, "per_unit": 0.69, "currency": "EUR"},
}

RATES_SA_EAST = {
    "sa-east-000": {"base": 38.1, "per_unit": 0.06, "currency": "ZAR"},
    "sa-east-001": {"base": 71.6, "per_unit": 0.23, "currency": "USD"},
    "sa-east-002": {"base": 87.3, "per_unit": 0.23, "currency": "EUR"},
    "sa-east-003": {"base": 34.8, "per_unit": 0.70, "currency": "EUR"},
    "sa-east-004": {"base": 81.8, "per_unit": 0.59, "currency": "EUR"},
    "sa-east-005": {"base": 37.4, "per_unit": 0.45, "currency": "USD"},
    "sa-east-006": {"base": 29.9, "per_unit": 0.16, "currency": "ZAR"},
    "sa-east-007": {"base": 50.6, "per_unit": 0.49, "currency": "EUR"},
    "sa-east-008": {"base": 57.5, "per_unit": 0.19, "currency": "USD"},
    "sa-east-009": {"base": 21.3, "per_unit": 0.19, "currency": "USD"},
    "sa-east-010": {"base": 96.8, "per_unit": 0.91, "currency": "USD"},
    "sa-east-011": {"base": 81.4, "per_unit": 0.27, "currency": "ZAR"},
    "sa-east-012": {"base": 18.0, "per_unit": 0.85, "currency": "EUR"},
    "sa-east-013": {"base": 7.1, "per_unit": 0.98, "currency": "EUR"},
    "sa-east-014": {"base": 29.7, "per_unit": 0.08, "currency": "EUR"},
    "sa-east-015": {"base": 43.5, "per_unit": 0.03, "currency": "USD"},
    "sa-east-016": {"base": 12.3, "per_unit": 0.08, "currency": "USD"},
    "sa-east-017": {"base": 16.3, "per_unit": 0.41, "currency": "ZAR"},
    "sa-east-018": {"base": 47.0, "per_unit": 0.68, "currency": "EUR"},
    "sa-east-019": {"base": 38.3, "per_unit": 0.26, "currency": "ZAR"},
    "sa-east-020": {"base": 51.5, "per_unit": 0.49, "currency": "USD"},
    "sa-east-021": {"base": 95.9, "per_unit": 0.36, "currency": "ZAR"},
    "sa-east-022": {"base": 23.8, "per_unit": 0.10, "currency": "USD"},
    "sa-east-023": {"base": 42.5, "per_unit": 0.41, "currency": "EUR"},
    "sa-east-024": {"base": 79.2, "per_unit": 0.09, "currency": "USD"},
    "sa-east-025": {"base": 15.0, "per_unit": 0.78, "currency": "EUR"},
    "sa-east-026": {"base": 15.3, "per_unit": 0.21, "currency": "ZAR"},
    "sa-east-027": {"base": 5.9, "per_unit": 0.93, "currency": "ZAR"},
    "sa-east-028": {"base": 99.0, "per_unit": 0.52, "currency": "EUR"},
    "sa-east-029": {"base": 41.8, "per_unit": 0.04, "currency": "EUR"},
    "sa-east-030": {"base": 70.8, "per_unit": 0.20, "currency": "EUR"},
    "sa-east-031": {"base": 86.3, "per_unit": 0.23, "currency": "ZAR"},
    "sa-east-032": {"base": 49.1, "per_unit": 0.10, "currency": "ZAR"},
    "sa-east-033": {"base": 94.3, "per_unit": 0.88, "currency": "EUR"},
    "sa-east-034": {"base": 46.7, "per_unit": 0.47, "currency": "USD"},
    "sa-east-035": {"base": 35.2, "per_unit": 0.51, "currency": "EUR"},
    "sa-east-036": {"base": 98.1, "per_unit": 0.51, "currency": "EUR"},
    "sa-east-037": {"base": 43.1, "per_unit": 0.92, "currency": "USD"},
    "sa-east-038": {"base": 30.0, "per_unit": 0.70, "currency": "EUR"},
    "sa-east-039": {"base": 96.2, "per_unit": 0.95, "currency": "EUR"},
    "sa-east-040": {"base": 25.9, "per_unit": 0.12, "currency": "ZAR"},
    "sa-east-041": {"base": 20.0, "per_unit": 0.14, "currency": "ZAR"},
    "sa-east-042": {"base": 26.5, "per_unit": 0.95, "currency": "ZAR"},
    "sa-east-043": {"base": 38.7, "per_unit": 0.49, "currency": "EUR"},
    "sa-east-044": {"base": 75.1, "per_unit": 0.37, "currency": "EUR"},
    "sa-east-045": {"base": 95.9, "per_unit": 0.89, "currency": "EUR"},
    "sa-east-046": {"base": 33.3, "per_unit": 0.44, "currency": "ZAR"},
    "sa-east-047": {"base": 38.7, "per_unit": 0.25, "currency": "USD"},
    "sa-east-048": {"base": 75.0, "per_unit": 0.03, "currency": "ZAR"},
    "sa-east-049": {"base": 68.6, "per_unit": 0.18, "currency": "EUR"},
    "sa-east-050": {"base": 9.9, "per_unit": 0.35, "currency": "EUR"},
    "sa-east-051": {"base": 29.3, "per_unit": 0.99, "currency": "ZAR"},
    "sa-east-052": {"base": 77.8, "per_unit": 0.18, "currency": "EUR"},
    "sa-east-053": {"base": 52.4, "per_unit": 0.82, "currency": "ZAR"},
    "sa-east-054": {"base": 6.6, "per_unit": 0.39, "currency": "ZAR"},
    "sa-east-055": {"base": 11.9, "per_unit": 0.82, "currency": "USD"},
    "sa-east-056": {"base": 24.8, "per_unit": 0.55, "currency": "ZAR"},
    "sa-east-057": {"base": 13.8, "per_unit": 0.48, "currency": "EUR"},
    "sa-east-058": {"base": 62.7, "per_unit": 0.44, "currency": "EUR"},
    "sa-east-059": {"base": 60.7, "per_unit": 0.20, "currency": "EUR"},
    "sa-east-060": {"base": 8.6, "per_unit": 0.45, "currency": "USD"},
    "sa-east-061": {"base": 17.4, "per_unit": 0.07, "currency": "EUR"},
    "sa-east-062": {"base": 23.0, "per_unit": 0.62, "currency": "ZAR"},
    "sa-east-063": {"base": 66.6, "per_unit": 0.59, "currency": "ZAR"},
    "sa-east-064": {"base": 24.0, "per_unit": 0.94, "currency": "EUR"},
    "sa-east-065": {"base": 4.5, "per_unit": 0.69, "currency": "ZAR"},
    "sa-east-066": {"base": 57.4, "per_unit": 0.54, "currency": "ZAR"},
    "sa-east-067": {"base": 71.9, "per_unit": 0.78, "currency": "EUR"},
    "sa-east-068": {"base": 13.5, "per_unit": 0.74, "currency": "ZAR"},
    "sa-east-069": {"base": 5.8, "per_unit": 0.70, "currency": "USD"},
    "sa-east-070": {"base": 69.2, "per_unit": 0.27, "currency": "EUR"},
    "sa-east-071": {"base": 28.5, "per_unit": 0.03, "currency": "USD"},
    "sa-east-072": {"base": 83.5, "per_unit": 0.08, "currency": "USD"},
    "sa-east-073": {"base": 49.9, "per_unit": 0.11, "currency": "ZAR"},
    "sa-east-074": {"base": 69.3, "per_unit": 0.80, "currency": "EUR"},
    "sa-east-075": {"base": 74.6, "per_unit": 0.46, "currency": "EUR"},
    "sa-east-076": {"base": 69.1, "per_unit": 0.77, "currency": "USD"},
    "sa-east-077": {"base": 90.7, "per_unit": 0.21, "currency": "USD"},
    "sa-east-078": {"base": 70.9, "per_unit": 0.15, "currency": "USD"},
    "sa-east-079": {"base": 51.3, "per_unit": 0.65, "currency": "USD"},
    "sa-east-080": {"base": 60.9, "per_unit": 0.09, "currency": "USD"},
    "sa-east-081": {"base": 26.2, "per_unit": 0.16, "currency": "USD"},
    "sa-east-082": {"base": 80.0, "per_unit": 0.87, "currency": "ZAR"},
    "sa-east-083": {"base": 52.5, "per_unit": 0.59, "currency": "EUR"},
    "sa-east-084": {"base": 45.9, "per_unit": 0.63, "currency": "EUR"},
    "sa-east-085": {"base": 69.3, "per_unit": 0.38, "currency": "USD"},
    "sa-east-086": {"base": 38.8, "per_unit": 0.68, "currency": "ZAR"},
    "sa-east-087": {"base": 31.8, "per_unit": 0.67, "currency": "USD"},
    "sa-east-088": {"base": 47.6, "per_unit": 0.33, "currency": "USD"},
    "sa-east-089": {"base": 91.9, "per_unit": 0.09, "currency": "ZAR"},
    "sa-east-090": {"base": 33.2, "per_unit": 0.16, "currency": "ZAR"},
    "sa-east-091": {"base": 23.7, "per_unit": 0.99, "currency": "USD"},
    "sa-east-092": {"base": 83.1, "per_unit": 0.44, "currency": "EUR"},
    "sa-east-093": {"base": 94.5, "per_unit": 0.05, "currency": "EUR"},
    "sa-east-094": {"base": 55.1, "per_unit": 0.32, "currency": "EUR"},
    "sa-east-095": {"base": 29.4, "per_unit": 0.98, "currency": "EUR"},
    "sa-east-096": {"base": 4.5, "per_unit": 0.59, "currency": "ZAR"},
    "sa-east-097": {"base": 91.3, "per_unit": 0.57, "currency": "USD"},
    "sa-east-098": {"base": 82.4, "per_unit": 0.44, "currency": "ZAR"},
    "sa-east-099": {"base": 66.6, "per_unit": 0.01, "currency": "EUR"},
    "sa-east-100": {"base": 42.6, "per_unit": 0.29, "currency": "EUR"},
    "sa-east-101": {"base": 67.7, "per_unit": 0.30, "currency": "ZAR"},
    "sa-east-102": {"base": 66.7, "per_unit": 0.46, "currency": "ZAR"},
    "sa-east-103": {"base": 51.0, "per_unit": 0.45, "currency": "ZAR"},
    "sa-east-104": {"base": 27.5, "per_unit": 0.43, "currency": "ZAR"},
    "sa-east-105": {"base": 5.2, "per_unit": 0.06, "currency": "EUR"},
    "sa-east-106": {"base": 44.8, "per_unit": 0.55, "currency": "USD"},
    "sa-east-107": {"base": 77.1, "per_unit": 0.70, "currency": "ZAR"},
    "sa-east-108": {"base": 25.1, "per_unit": 0.67, "currency": "EUR"},
    "sa-east-109": {"base": 83.8, "per_unit": 0.13, "currency": "USD"},
    "sa-east-110": {"base": 79.8, "per_unit": 0.88, "currency": "ZAR"},
    "sa-east-111": {"base": 22.9, "per_unit": 0.77, "currency": "EUR"},
    "sa-east-112": {"base": 90.6, "per_unit": 0.90, "currency": "ZAR"},
    "sa-east-113": {"base": 1.8, "per_unit": 0.78, "currency": "EUR"},
    "sa-east-114": {"base": 86.2, "per_unit": 0.81, "currency": "USD"},
    "sa-east-115": {"base": 26.1, "per_unit": 0.62, "currency": "USD"},
    "sa-east-116": {"base": 21.7, "per_unit": 0.63, "currency": "EUR"},
    "sa-east-117": {"base": 97.7, "per_unit": 0.41, "currency": "EUR"},
    "sa-east-118": {"base": 35.0, "per_unit": 0.15, "currency": "EUR"},
    "sa-east-119": {"base": 63.0, "per_unit": 0.51, "currency": "ZAR"},
}

RATES_AF_SOUTH = {
    "af-south-000": {"base": 46.0, "per_unit": 0.19, "currency": "USD"},
    "af-south-001": {"base": 18.2, "per_unit": 0.08, "currency": "USD"},
    "af-south-002": {"base": 31.6, "per_unit": 0.98, "currency": "EUR"},
    "af-south-003": {"base": 11.6, "per_unit": 0.59, "currency": "USD"},
    "af-south-004": {"base": 40.4, "per_unit": 0.45, "currency": "EUR"},
    "af-south-005": {"base": 72.6, "per_unit": 0.73, "currency": "USD"},
    "af-south-006": {"base": 48.7, "per_unit": 0.36, "currency": "EUR"},
    "af-south-007": {"base": 3.5, "per_unit": 0.92, "currency": "USD"},
    "af-south-008": {"base": 12.4, "per_unit": 0.50, "currency": "EUR"},
    "af-south-009": {"base": 79.6, "per_unit": 0.73, "currency": "EUR"},
    "af-south-010": {"base": 27.4, "per_unit": 0.95, "currency": "USD"},
    "af-south-011": {"base": 4.2, "per_unit": 0.09, "currency": "USD"},
    "af-south-012": {"base": 6.5, "per_unit": 0.08, "currency": "EUR"},
    "af-south-013": {"base": 89.4, "per_unit": 0.30, "currency": "ZAR"},
    "af-south-014": {"base": 14.4, "per_unit": 0.37, "currency": "ZAR"},
    "af-south-015": {"base": 51.4, "per_unit": 0.98, "currency": "ZAR"},
    "af-south-016": {"base": 26.7, "per_unit": 0.10, "currency": "EUR"},
    "af-south-017": {"base": 71.3, "per_unit": 0.82, "currency": "EUR"},
    "af-south-018": {"base": 56.3, "per_unit": 0.28, "currency": "ZAR"},
    "af-south-019": {"base": 48.5, "per_unit": 0.88, "currency": "EUR"},
    "af-south-020": {"base": 1.9, "per_unit": 0.93, "currency": "USD"},
    "af-south-021": {"base": 79.4, "per_unit": 0.14, "currency": "ZAR"},
    "af-south-022": {"base": 29.2, "per_unit": 0.14, "currency": "ZAR"},
    "af-south-023": {"base": 81.9, "per_unit": 0.19, "currency": "USD"},
    "af-south-024": {"base": 92.5, "per_unit": 0.65, "currency": "EUR"},
    "af-south-025": {"base": 87.8, "per_unit": 0.44, "currency": "USD"},
    "af-south-026": {"base": 78.2, "per_unit": 0.79, "currency": "ZAR"},
    "af-south-027": {"base": 13.6, "per_unit": 0.66, "currency": "ZAR"},
    "af-south-028": {"base": 53.3, "per_unit": 0.24, "currency": "USD"},
    "af-south-029": {"base": 6.4, "per_unit": 0.88, "currency": "EUR"},
    "af-south-030": {"base": 17.9, "per_unit": 0.29, "currency": "ZAR"},
    "af-south-031": {"base": 85.7, "per_unit": 0.66, "currency": "EUR"},
    "af-south-032": {"base": 48.4, "per_unit": 0.02, "currency": "USD"},
    "af-south-033": {"base": 24.3, "per_unit": 0.53, "currency": "USD"},
    "af-south-034": {"base": 90.3, "per_unit": 0.31, "currency": "USD"},
    "af-south-035": {"base": 2.3, "per_unit": 0.57, "currency": "USD"},
    "af-south-036": {"base": 83.5, "per_unit": 0.93, "currency": "USD"},
    "af-south-037": {"base": 90.6, "per_unit": 0.12, "currency": "EUR"},
    "af-south-038": {"base": 13.0, "per_unit": 0.33, "currency": "ZAR"},
    "af-south-039": {"base": 21.2, "per_unit": 0.63, "currency": "USD"},
    "af-south-040": {"base": 81.2, "per_unit": 0.48, "currency": "USD"},
    "af-south-041": {"base": 49.9, "per_unit": 0.60, "currency": "ZAR"},
    "af-south-042": {"base": 57.6, "per_unit": 0.04, "currency": "EUR"},
    "af-south-043": {"base": 24.2, "per_unit": 0.75, "currency": "USD"},
    "af-south-044": {"base": 98.7, "per_unit": 0.98, "currency": "USD"},
    "af-south-045": {"base": 96.3, "per_unit": 0.10, "currency": "ZAR"},
    "af-south-046": {"base": 37.5, "per_unit": 0.67, "currency": "EUR"},
    "af-south-047": {"base": 97.9, "per_unit": 0.26, "currency": "ZAR"},
    "af-south-048": {"base": 4.2, "per_unit": 0.47, "currency": "EUR"},
    "af-south-049": {"base": 33.1, "per_unit": 0.78, "currency": "ZAR"},
    "af-south-050": {"base": 49.4, "per_unit": 0.75, "currency": "USD"},
    "af-south-051": {"base": 33.1, "per_unit": 0.92, "currency": "EUR"},
    "af-south-052": {"base": 1.8, "per_unit": 0.75, "currency": "USD"},
    "af-south-053": {"base": 50.2, "per_unit": 0.34, "currency": "ZAR"},
    "af-south-054": {"base": 65.4, "per_unit": 0.48, "currency": "EUR"},
    "af-south-055": {"base": 53.7, "per_unit": 0.65, "currency": "ZAR"},
    "af-south-056": {"base": 51.2, "per_unit": 0.47, "currency": "USD"},
    "af-south-057": {"base": 64.6, "per_unit": 0.37, "currency": "EUR"},
    "af-south-058": {"base": 92.2, "per_unit": 0.61, "currency": "ZAR"},
    "af-south-059": {"base": 65.2, "per_unit": 0.72, "currency": "ZAR"},
    "af-south-060": {"base": 94.6, "per_unit": 0.24, "currency": "ZAR"},
    "af-south-061": {"base": 60.1, "per_unit": 0.96, "currency": "USD"},
    "af-south-062": {"base": 70.4, "per_unit": 0.76, "currency": "ZAR"},
    "af-south-063": {"base": 63.2, "per_unit": 0.41, "currency": "EUR"},
    "af-south-064": {"base": 83.8, "per_unit": 0.82, "currency": "EUR"},
    "af-south-065": {"base": 95.7, "per_unit": 0.39, "currency": "EUR"},
    "af-south-066": {"base": 66.1, "per_unit": 0.43, "currency": "EUR"},
    "af-south-067": {"base": 35.8, "per_unit": 0.59, "currency": "EUR"},
    "af-south-068": {"base": 11.2, "per_unit": 0.34, "currency": "EUR"},
    "af-south-069": {"base": 84.6, "per_unit": 0.44, "currency": "EUR"},
    "af-south-070": {"base": 36.2, "per_unit": 0.44, "currency": "ZAR"},
    "af-south-071": {"base": 28.0, "per_unit": 0.13, "currency": "ZAR"},
    "af-south-072": {"base": 58.4, "per_unit": 0.65, "currency": "ZAR"},
    "af-south-073": {"base": 94.1, "per_unit": 0.50, "currency": "ZAR"},
    "af-south-074": {"base": 8.5, "per_unit": 0.04, "currency": "ZAR"},
    "af-south-075": {"base": 39.0, "per_unit": 0.87, "currency": "USD"},
    "af-south-076": {"base": 56.6, "per_unit": 0.99, "currency": "EUR"},
    "af-south-077": {"base": 39.1, "per_unit": 0.70, "currency": "ZAR"},
    "af-south-078": {"base": 93.2, "per_unit": 0.78, "currency": "USD"},
    "af-south-079": {"base": 51.5, "per_unit": 0.60, "currency": "EUR"},
    "af-south-080": {"base": 33.8, "per_unit": 0.45, "currency": "EUR"},
    "af-south-081": {"base": 8.3, "per_unit": 0.39, "currency": "ZAR"},
    "af-south-082": {"base": 51.5, "per_unit": 0.96, "currency": "USD"},
    "af-south-083": {"base": 9.2, "per_unit": 0.93, "currency": "EUR"},
    "af-south-084": {"base": 66.4, "per_unit": 0.72, "currency": "EUR"},
    "af-south-085": {"base": 71.0, "per_unit": 0.49, "currency": "EUR"},
    "af-south-086": {"base": 89.6, "per_unit": 0.54, "currency": "EUR"},
    "af-south-087": {"base": 9.3, "per_unit": 0.90, "currency": "ZAR"},
    "af-south-088": {"base": 35.2, "per_unit": 0.31, "currency": "USD"},
    "af-south-089": {"base": 92.9, "per_unit": 0.22, "currency": "EUR"},
    "af-south-090": {"base": 14.9, "per_unit": 0.31, "currency": "EUR"},
    "af-south-091": {"base": 37.3, "per_unit": 0.90, "currency": "USD"},
    "af-south-092": {"base": 72.8, "per_unit": 0.72, "currency": "USD"},
    "af-south-093": {"base": 45.8, "per_unit": 0.88, "currency": "EUR"},
    "af-south-094": {"base": 97.9, "per_unit": 0.75, "currency": "ZAR"},
    "af-south-095": {"base": 26.6, "per_unit": 0.15, "currency": "EUR"},
    "af-south-096": {"base": 55.1, "per_unit": 0.49, "currency": "USD"},
    "af-south-097": {"base": 31.1, "per_unit": 0.17, "currency": "EUR"},
    "af-south-098": {"base": 84.0, "per_unit": 0.18, "currency": "ZAR"},
    "af-south-099": {"base": 12.3, "per_unit": 0.81, "currency": "ZAR"},
    "af-south-100": {"base": 21.1, "per_unit": 0.78, "currency": "EUR"},
    "af-south-101": {"base": 78.3, "per_unit": 0.75, "currency": "EUR"},
    "af-south-102": {"base": 74.2, "per_unit": 0.14, "currency": "EUR"},
    "af-south-103": {"base": 11.2, "per_unit": 0.65, "currency": "USD"},
    "af-south-104": {"base": 62.8, "per_unit": 0.08, "currency": "ZAR"},
    "af-south-105": {"base": 97.4, "per_unit": 0.75, "currency": "USD"},
    "af-south-106": {"base": 58.6, "per_unit": 0.86, "currency": "EUR"},
    "af-south-107": {"base": 30.7, "per_unit": 0.64, "currency": "EUR"},
    "af-south-108": {"base": 78.3, "per_unit": 0.25, "currency": "USD"},
    "af-south-109": {"base": 23.0, "per_unit": 0.11, "currency": "USD"},
    "af-south-110": {"base": 5.0, "per_unit": 0.65, "currency": "USD"},
    "af-south-111": {"base": 17.6, "per_unit": 0.32, "currency": "EUR"},
    "af-south-112": {"base": 47.3, "per_unit": 0.15, "currency": "USD"},
    "af-south-113": {"base": 93.7, "per_unit": 0.98, "currency": "USD"},
    "af-south-114": {"base": 13.4, "per_unit": 0.97, "currency": "ZAR"},
    "af-south-115": {"base": 34.7, "per_unit": 0.24, "currency": "ZAR"},
    "af-south-116": {"base": 55.8, "per_unit": 0.35, "currency": "EUR"},
    "af-south-117": {"base": 73.9, "per_unit": 0.81, "currency": "USD"},
    "af-south-118": {"base": 87.4, "per_unit": 0.99, "currency": "USD"},
    "af-south-119": {"base": 20.9, "per_unit": 0.39, "currency": "USD"},
}


TIER_THRESHOLDS = {"silver": 100, "gold": 1000, "platinum": 10000}


def tier_for_volume(units: int) -> str:
    """Pricing tier for a monthly unit volume.

    0-99 units -> "bronze"; 100-999 -> "silver"; 1000-9999 -> "gold"; 10000 and
    above -> "platinum". The lower bound of each tier is inclusive.
    """
    if units < 0:
        raise ValueError("units must be non-negative")
    if units > TIER_THRESHOLDS["platinum"]:
        return "platinum"
    if units > TIER_THRESHOLDS["gold"]:
        return "gold"
    if units > TIER_THRESHOLDS["silver"]:
        return "silver"
    return "bronze"


def discount_for_tier(tier: str) -> float:
    return {"bronze": 0.0, "silver": 0.05, "gold": 0.12, "platinum": 0.2}[tier]


def monthly_cost(region_key: str, units: int) -> float:
    table = None
    for name, value in globals().items():
        if name.startswith("RATES_") and region_key in value:
            table = value[region_key]
            break
    if table is None:
        raise KeyError(region_key)
    gross = table["base"] + table["per_unit"] * units
    return round(gross * (1 - discount_for_tier(tier_for_volume(units))), 2)


def helper_36(value: float, factor: float = 1.36) -> float:
    """Helper 36: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2332:
        scaled = 2332
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_37(value: float, factor: float = 1.37) -> float:
    """Helper 37: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2369:
        scaled = 2369
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_38(value: float, factor: float = 1.38) -> float:
    """Helper 38: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2406:
        scaled = 2406
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)


def helper_39(value: float, factor: float = 1.39) -> float:
    """Helper 39: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2443:
        scaled = 2443
    step = 0.05
    units = int(scaled / step)
    return round(units * step, 2)


def helper_40(value: float, factor: float = 1.40) -> float:
    """Helper 40: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2480:
        scaled = 2480
    step = 0.01
    units = int(scaled / step)
    return round(units * step, 2)


def helper_41(value: float, factor: float = 1.41) -> float:
    """Helper 41: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2517:
        scaled = 2517
    step = 0.02
    units = int(scaled / step)
    return round(units * step, 2)


def helper_42(value: float, factor: float = 1.42) -> float:
    """Helper 42: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2554:
        scaled = 2554
    step = 0.03
    units = int(scaled / step)
    return round(units * step, 2)


def helper_43(value: float, factor: float = 1.43) -> float:
    """Helper 43: scale, clamp and round a value (generated)."""
    if value < 0:
        raise ValueError("value must be non-negative")
    scaled = value * factor
    if scaled > 2591:
        scaled = 2591
    step = 0.04
    units = int(scaled / step)
    return round(units * step, 2)

