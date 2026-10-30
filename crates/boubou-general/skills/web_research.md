---
name: web_research
description: Deep research: multi-query search + cross-checking, summarised into a report file
---
# Deep research and report
1. Break the question down: split the user's question into 3-5 specific queries (web_search_multi runs them in parallel at once).
2. Search: web_search_multi(queries=[...]) returns title + snippet + link; pick the 2-4 most relevant links.
3. Read the sources: web_fetch the full text (saved automatically under sources/); key facts must come from the source text, not the snippets.
4. Cross-check: every key figure / conclusion needs at least two independent sources; when they disagree, write both down and flag the disagreement.
5. Write it up: create_file a markdown report in the working directory (file name includes the topic and date):
   - One-sentence conclusion (first)
   - Key findings (each: fact + source link)
   - Disagreements and uncertainties
   - Source list
6. Wrap up: in finish give the one-sentence conclusion plus the report file path.
Note: search queries must differ from one another (repeating the same query gets blocked by the anti-loop guard); if the information cannot be found, say so plainly; do not invent it.
