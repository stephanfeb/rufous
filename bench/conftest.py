# Keep pytest out of the benchmark workdirs under results/ (they hold the tasks' own test files)
# and out of the context-pressure fixtures (their test_*.py are task material, run by the agent).
collect_ignore_glob = ["results/*", "lrab_ext/fixtures/*"]
