# Spec: expense export service

## Summary
A command-line tool that reads `expenses.csv`, validates each row, converts amounts to the
report currency and writes a monthly summary.

## Features
1. Load `expenses.csv` (date, merchant, amount, currency) and reject malformed rows with a
   line number in the error.
2. Convert every amount to EUR using the rates in `rates.json`; unknown currencies are an
   error naming the currency.
3. Group by calendar month and total per month.
4. Write `summary.md` with one table (month, total EUR, row count).
5. A `--year` flag that limits the summary to one year.
