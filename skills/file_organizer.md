---
name: file_organizer
description: Tidy a folder: group by type / date, batch rename, find duplicates
---
# File organising
1. See the current state first: list_dir the directory; with many files work in batches (<= 200 per batch).
2. Clarify the rules (if the user gave none, use the defaults): group by extension (images / documents / archives / installers / other) or by year-month.
3. Group and move: prefer run_bash with python (using shutil.move), which is faster and more atomic than one command per file; create the target subdirectory first if it does not exist.
4. Batch rename: one consistent rule (prefix_number, date_originalname); before renaming, print an "old name -> new name" list for the user to confirm, then run os.rename.
5. Find duplicates: same name in different directories, or match on "size + content hash" (run_bash with python, reading files with hashlib); report only, deletion needs the user's confirmation.
6. Finish: output the statistics (N files in total, X moved, Y renamed, Z suspected duplicates) and list the files that could not be handled, with reasons.
Red lines: never move or delete paths outside the working directory; never delete, only move; do not touch "suspected duplicates" until the user confirms.
