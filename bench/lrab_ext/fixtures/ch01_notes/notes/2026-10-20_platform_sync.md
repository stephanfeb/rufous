# Platform sync, 20 October 2026

Attendees: Priya Natarajan, Tomasz Wieczorek, Mei Lin, Daniel Okafor

## Discussion
- Priya's staging dry-run of the Postgres cut-over took 41 minutes; the plan allows 90. She
  confirmed the cut-over stays on 14 November 2026.
- Seven services write to the orders database; two still use the old connection string.
- Tomasz: the OpenSearch cluster is ordered, delivery expected 3 November.

## Decisions
DECISION: Freeze non-essential deploys for the cut-over week, 10 to 14 November 2026; owner Priya Natarajan.

## Actions
- Daniel: update the two services with the old connection string by 31 October.
- Mei: add the deploy freeze to the November rota note.
