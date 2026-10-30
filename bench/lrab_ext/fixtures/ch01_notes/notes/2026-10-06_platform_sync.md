# Platform sync, 6 October 2026

Attendees: Priya Natarajan, Tomasz Wieczorek, Mei Lin, Daniel Okafor

## Discussion
- The orders database is still on MySQL 5.7, which leaves support in March. Priya has a
  migration plan to Postgres 16 and has run the schema conversion twice on a copy.
- Search latency: Tomasz compared upgrading Elasticsearch with moving the index to OpenSearch.
  Licensing favours OpenSearch; the query DSL is compatible for our use.
- On-call: the fortnightly rota is leaving people tired by the second week.

## Decisions
DECISION: Migrate the orders database to Postgres 16; owner Priya Natarajan; cut-over Saturday 14 November 2026.
DECISION: Rebuild the search index on OpenSearch rather than upgrading Elasticsearch; owner Tomasz Wieczorek; done by 28 November 2026.

## Actions
- Priya: dry-run the cut-over in staging by 20 October.
- Tomasz: capacity estimate for the OpenSearch cluster by 13 October.
