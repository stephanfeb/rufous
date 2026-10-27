# Writes its pid to argv[1] and never answers: the probe must time out and kill it.
import os, sys, time
open(sys.argv[1], "w").write(str(os.getpid()))
time.sleep(300)
