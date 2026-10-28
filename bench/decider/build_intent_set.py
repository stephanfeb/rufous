#!/usr/bin/env python3
"""Build bench/decider/intent_set.jsonl: the labelled intent set for the decider comparison
(docs/system-one-decider.md section 5; bead boubou-acy.2).

Labels, by what the loop must do, not by surface form:
  question   satisfiable on the Q&A path: a text answer, possibly after read_file / list_dir /
             web_search / web_fetch (face::CHAT_TOOLS); nothing in the workspace changes.
  task       needs the task loop: run_bash, files written or edited, a deliverable, a plan.
  follow_up  a short steer on work already under way (continuation, correction, confirmation).

Sources: candidates.json (collected from the user's sessions, LRAB prompts, probe prompts and
the test corpora; labelled here by index) plus authored cases that over-sample the heuristic's
0.5 defaults (short messages, long cue-free messages) and its known blind spots (questions that
contain task verbs). `uncertain` marks cases where a reasonable labeller could disagree.
"""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
cands = json.load(open(os.path.join(HERE, "candidates.json"), encoding="utf-8"))

Q, T, F = "question", "task", "follow_up"
# index in candidates.json -> (label, uncertain)
collected = {
    0: (Q, False), 1: (T, False), 2: (Q, True), 3: (T, False), 4: (T, False), 5: (T, False),
    6: (T, False), 7: (Q, False), 8: (Q, True), 9: (F, False), 10: (Q, True), 11: (Q, False),
    12: (Q, False), 13: (Q, False), 14: (Q, False), 15: (Q, False), 16: (Q, False), 17: (T, False),
    18: (Q, False), 19: (F, True), 20: (T, False), 21: (Q, False), 22: (Q, False), 23: (T, False),
    24: (T, False), 25: (T, False),
    **{i: (T, False) for i in range(26, 64)},          # LRAB prompts, probe tasks
    64: (T, False), 65: (Q, True), 66: (Q, False), 67: (T, False), 68: (T, False), 69: (T, False),
    70: (T, False), 71: (Q, False), 72: (Q, True), 73: (T, False), 74: (T, False), 75: (T, True),
    76: (T, False), 77: (Q, False), 78: (Q, False), 79: (Q, False), 80: (Q, False), 81: (F, False),
    # 82, 84, 85, 86 are harness-note fixtures, skipped
    83: (T, True), 87: (F, True), 88: (T, False), 89: (T, True), 90: (Q, False), 91: (Q, False),
    92: (T, False), 93: (T, False), 94: (Q, False), 95: (T, False), 96: (T, False), 97: (F, False),
    98: (Q, False), 99: (T, False), 100: (Q, False), 102: (F, True), 103: (T, False),
}
# progress flag for the test-corpus follow-ups whose fixture carried it
progress_override = {81: True, 97: True, 102: True, 87: True}

authored = []
def a(text, label, has_progress=False, uncertain=False, group=""):
    authored.append({"text": text, "label": label, "has_progress": has_progress,
                     "uncertain": uncertain, "source": "authored:" + group})

# --- follow-ups on work under way (heuristic: short → FollowUp 0.5, or a verb hint) ---
for t in ["yes", "ok", "go ahead", "do it", "yes please", "no", "not that one", "the other file",
          "use python instead", "try again", "skip that step", "carry on", "continue",
          "same for the second file", "make it csv instead", "also add a header row",
          "now the tests", "and the README too", "one more: rename it to final.md",
          "that's wrong, the total should be 42", "no, lowercase everything", "keep the old name",
          "fine, finish up", "that's enough, wrap it up", "stop there", "put it in docs/ instead",
          "two decimal places please", "sort by date, newest first", "drop the last column",
          "use tabs not spaces", "the file is called data2.csv, not data.csv", "ignore the warnings",
          "and commit nothing", "retry with sudo", "only the first ten rows", "actually, all of them",
          "undo that", "add a docstring", "run it again", "what about the tests?", "and the other two?",
          "shorter", "in British spelling", "yes, overwrite it", "no, the one in src"]:
    a(t, F, has_progress=True, group="followup_progress")
# follow-ups where the hint list fires a task verb (heuristic right for the wrong reason)
for t in ["now run them", "fix the import too", "write that to notes.md as well", "check the second file",
          "delete the temp files when you're done", "count them again"]:
    a(t, F, has_progress=True, group="followup_verb")
# status questions during work: text answer suffices
for t in ["did it pass?", "why did that fail?", "how many are left?", "what did the test say?",
          "is that the final version?", "which file did you change?"]:
    a(t, Q, has_progress=True, group="question_during_work")
# follow-ups with no recorded progress (a Q&A conversation being steered)
a("No, you should look up beads and openspec because they are not flutter libraries.", F, uncertain=True, group="followup_noprogress")
a("shorter please", F, uncertain=True, group="followup_noprogress")
a("again, but for Rust", F, uncertain=True, group="followup_noprogress")

# --- short questions, no progress (heuristic: Question 0.5 unless a hint) ---
for t in ["hello", "hi there", "hey", "good afternoon", "who are you", "what can you do",
          "which model are you", "any ideas?", "status?", "ok thanks", "cheers", "what time is it",
          "where is the config", "is this repo rust or go", "how big is this project",
          "what does this repo do", "any tests in here", "which version of python is this",
          "?", "so what now", "and then", "what's in src", "licence?", "summary please",
          "are you there", "still with me?", "that all?", "meaning?"]:
    a(t, Q, group="short_question")

# --- long cue-free questions (heuristic: Task 0.5) ---
for t in [
    "I was wondering whether this repository has anything to do with authentication tokens",
    "I'd like to understand what the difference is between the two config loaders in here before I decide anything",
    "Something I never understood about async in Rust is why a future does nothing until it is polled",
    "Before I touch anything I want your opinion on whether the current layout of the crates makes sense",
    "I keep seeing the words borrow of moved value in my logs and I am not sure what it means in this context",
    "There are two README files in this project and I am curious which one is the current one",
    "My colleague says the ledger module is unused but the import is still there, so I am puzzled",
    "It is unclear to me from the code comments who is supposed to own the session files on disk",
    "The time budget wording in the task text seems to accept half an hour as well as 30m, is that right",
    "Just so I understand the shape of this thing, the web UI and the CLI share one core crate",
    "I am trying to decide between a trait object and an enum for the backends and would value a view",
    "The last run took eight minutes and I am not sure whether that is the model or the harness",
    "Not sure the profile table in the docs matches what the code actually does for gemma4 12b",
    "Out of interest, roughly how many lines of Rust are in this workspace",
    "Apparently the corrector only fires after two identical failures, which seems late to me",
    "I noticed the integrity gate text mentions a stem match and I am unsure what counts as a stem",
    "Thinking about the memory footprint on a sixteen gigabyte laptop with the 12b model loaded",
    "Curious whether the safety gate treats a find over the home directory as an escape",
    "Wondering if the skills directory is scanned on every turn or once at the start",
    "From the README it is hard to tell whether offline mode disables the web tools entirely",
    "The pre-delivery check seems to re-inject the whole task, which feels expensive on long briefs",
    "My understanding is that children never see the plan gates, correct me if that is outdated",
    "I am new to this codebase and would appreciate a tour of where the agent loop lives",
    "Reading the loop probe results, the forced end counter is a mystery to me",
    "Honestly unsure if the heuristic treats a trailing question mark differently from a mid-sentence one",
]:
    a(t, Q, group="long_question_nocue")

# --- long cue-free tasks (heuristic: Task 0.5; right answer, no rule) ---
for t in [
    "The invoices from March need to end up in one spreadsheet with a total per client",
    "Every markdown file under docs should get a table of contents at the top",
    "All the images in assets are too large; I want them under two hundred kilobytes each",
    "The deploy script keeps timing out and I need that sorted before tonight",
    "There is a dataset.csv in here and I want a histogram of the value column as a png",
    "The three CSV exports in data/ belong together as one table with a source column",
    "Each function in util.py needs a docstring in the Google style",
    "The failing test in test_parse.py has to pass before the release",
    "This repository needs a CHANGELOG covering the last five commits",
    "The notes in meeting_notes.txt should become a one page summary in notes.md",
    "I want the logs in logs/ reduced to the error lines only, one file per day",
    "The shell script in bin/ should also work on Linux, not only macOS",
    "A requirements.txt matching the imports in this project is missing",
    "The config example in the README is out of date compared to config.rs",
    "The report needs a figure comparing wall time per arm, saved under figures/",
    "All occurrences of the old product name in docs/ should say Boubou",
    "The dockerfile is broken since the base image changed",
    "The two JSON schemas in schema/ should be merged into one with shared definitions",
    "Line endings in the templates are mixed and should be LF throughout",
    "The ten largest files in this tree, with sizes, into a file called big_files.txt",
    "Somebody left debug prints all over app.py and they need to go",
    "The unit tests take too long; the slow ones should be marked and skipped by default",
    "A nightly cron entry that backs up the sessions directory to backups/",
    "The CSV has a BOM and the parser chokes on it",
    "Tabs to four spaces across the whole src directory",
]:
    a(t, T, group="long_task_nocue")

# --- questions containing a task verb (heuristic blind spot: hint → Task 1.0) ---
for t in [
    "How do I write a Rust macro?", "What is the best way to run tests in a cargo workspace?",
    "Can you explain what this build error means?", "Which command would create a new git branch?",
    "Is it safe to delete the target directory?", "Should I fix this with a trait or an enum?",
    "What does cargo check do compared to cargo build?", "Do you know how to count lines in a file from the shell?",
    "Why would someone generate code instead of writing it by hand?", "When should I refactor rather than rewrite?",
    "What does it mean to install a crate with the vendored feature?", "How does Ollama download a model?",
    "Is there a reason to compute the hash twice here?", "What would you change in this function and why?",
    "How do people usually test async code in Rust?", "Does edit_file make a backup before it changes anything?",
    "Can you tell me what the plot in figures/ shows?", "Who should review a change to the safety gate?",
    "How is a profile configured for a model, roughly?", "What is the difference between debug and release builds?",
]:
    a(t, Q, group="question_with_verb")

# --- tasks phrased as questions or ending in ? (heuristic: verb hint wins → Task 1.0) ---
for t in [
    "Can you write a script that parses the logs in this directory?", "Could you set the project up with a Makefile?",
    "Would you mind creating a README for this crate?", "Can we get the tests passing?",
    "Is it possible to convert all these docx files to markdown? Please do it.", "Any chance you could rename the modules to snake case?",
    "Could you run the benchmark and save the numbers to results.md?", "Can you fix the typo in the title of report.md?",
    "Mind generating a .gitignore for a Rust project?", "Can you install the dependencies and run the tests?",
    "Would you translate README.md into French and save it as README.fr.md?", "Can you count the rows in data.csv and put the number in count.txt?",
]:
    a(t, T, group="task_as_question")

# --- plain tasks ---
for t in [
    "Write a haiku about rain", "Create a file called hello.txt containing the word hello",
    "Run the tests and fix whatever fails", "Summarise the three PDFs in docs/ into summary.md",
    "Refactor parse.py so the CSV reader is its own function", "Set up a Python virtual environment and install pytest",
    "Build the release binary and report its size", "Generate a table of the ten most common words in corpus.txt",
    "Translate the error messages in messages.json into German", "Delete all .bak files in this directory",
    "Compare the two config files and write the differences to diff.md", "Plot the values in data.csv as a line chart",
    "Debug why main.py exits with code 2", "Install ripgrep with brew and confirm the version",
    "Organise the photos in pics/ into folders by year", "Export the sheet in budget.xlsx to CSV",
]:
    a(t, T, group="task_plain")

# --- reads and lookups that the Q&A path can serve ---
for t in ["Read the file README.md", "List the files in the current directory", "Show me what is inside the directory src",
          "What is in config.toml?", "Open notes.txt and tell me the first line", "Search the web for the Ollama systemone API",
          "Look up the current Rust stable version", "Fetch https://example.com and tell me the title",
          "Find out who maintains the regex crate", "Which files mention the word corrector?"]:
    a(t, Q, uncertain=(t.startswith("Which files")), group="readonly_lookup")

# --- commands the Q&A path cannot run ---
for t in ["Run ls -la and show me the output", "Execute python -m pytest -q", "Run cargo build and tell me if it compiles",
          "Start the dev server on port 8000", "Check the git status", "Run the linter on src/"]:
    a(t, T, group="command")

# --- small talk with progress ---
for t in ["thanks, that's all", "great work", "bye", "perfect", "lovely, thank you"]:
    a(t, Q, has_progress=True, group="smalltalk_progress")

rows = []
n = 0
for i, c in enumerate(cands):
    if i not in collected:
        continue
    label, unc = collected[i]
    n += 1
    rows.append({"id": f"c{i:03d}", "text": c["text"], "has_progress": progress_override.get(i, c["has_progress"]),
                 "label": label, "uncertain": unc, "source": c["source"]})
for j, c in enumerate(authored):
    rows.append({"id": f"a{j:03d}", **c})

out = os.path.join(HERE, "intent_set.jsonl")
with open(out, "w", encoding="utf-8") as f:
    for r in rows:
        f.write(json.dumps(r, ensure_ascii=False) + "\n")
import collections
print(f"{len(rows)} cases ({n} collected, {len(authored)} authored) -> {out}")
print("labels:", dict(collections.Counter(r["label"] for r in rows)))
print("uncertain:", sum(r["uncertain"] for r in rows))
print("groups:", dict(collections.Counter(r["source"].split(":")[0] + (":" + r["source"].split(":")[1] if r["source"].startswith("authored") else "") for r in rows)))
