# Keep pytest out of the benchmark workdirs under results/ (they hold the tasks' own test files).
collect_ignore_glob = ["results/*"]
