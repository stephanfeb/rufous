# Keep pytest out of the benchmark workdirs under results/ (they hold the tasks' own test files),
# the context-pressure fixtures (their test_*.py are task material, run by the agent) and the
# hidden scorer overlays (run by the scorer, in the finished workdir).
collect_ignore_glob = ["results/*", "lrab_ext/fixtures/*", "lrab_ext/hidden/*"]
