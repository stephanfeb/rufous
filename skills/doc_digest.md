---
name: doc_digest
description: Speed-read a long document: read it in chunks, produce a summary, key points and action items
---
# Speed-reading a long document
1. Find the file: list_dir, then read_file the start to confirm what it is and how long; for very large files use run_bash with python to count lines and slice by line range.
2. Read in chunks: 300-500 lines per chunk (read_file, or a slice via run_bash with python); from each chunk extract what it says, key figures, names / dates / conclusions.
3. Take notes as you go: one-sentence key point per chunk (kept in the conversation, no intermediate files).
4. Output: create_file to write the summary file (markdown):
   - One-paragraph overview (<= 150 words)
   - Key points list (one per line, with the source position, e.g. L120)
   - Table of key figures / dates / names
   - Action items (if the text contains to-dos or conclusions that point to actions)
   - Questions the text leaves unanswered
5. If the user only wants a verbal summary, do not create a file; finish directly.
Note: never skip key passages (figures, conclusions, tables); in the summary, distinguish "what the text says" from "what I infer".
