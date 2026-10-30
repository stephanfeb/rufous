# Platform sync, 13 October 2026

Attendees: Priya Natarajan, Tomasz Wieczorek, Mei Lin

## Discussion
- Tomasz's estimate: three data nodes, 32 GB each, for the OpenSearch cluster. Approved.
- Mei proposed a weekly on-call rota with a handover note every Monday.
- The Postgres migration stays with Priya; the deadline is unchanged at 14 November 2026.
  Daniel will cover the read replicas.

## Decisions
DECISION: Move to a weekly on-call rota from 1 November 2026, with a Monday handover note; owner Mei Lin.

## Actions
- Mei: publish the November rota by 24 October.
- Priya: list the services that write to the orders database.
