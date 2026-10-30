"""Referee for CH-03: python check_tidy.py (run from the workdir)."""
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
INBOX = os.path.join(HERE, "inbox")
EXPECT = json.loads("{\"IMG_2041.jpg\": {\"sub\": \"images\", \"sha256\": \"e7f0971f9e28f7e383a23f080f065359df455d2fa965b67feb97bed23b98e355\"}, \"screenshot 2026-10-02.png\": {\"sub\": \"images\", \"sha256\": \"218fa02630351c4137899632b953a4d9ecb43d96cc04405dd872e9699d10ec40\"}, \"logo.png\": {\"sub\": \"images\", \"sha256\": \"4837bcbe1ce24a07f74c00563efccd808317f348a7db721c4a87dae9b3309917\"}, \"offer_letter.md\": {\"sub\": \"docs\", \"sha256\": \"018f6344aaa7876cf5677ee722522ddd5ca2a1181447d94b7b89a3436fb00b98\"}, \"notes from call.txt\": {\"sub\": \"docs\", \"sha256\": \"277efa20ca5329114c688ab69c362aa9dcd110f99f3804bcb51a8583295267b5\"}, \"agenda.md\": {\"sub\": \"docs\", \"sha256\": \"90381e8a1ca9e73ab68faf26720060d65c73840ba107039db62eff6ffc7874ab\"}, \"q3_numbers.csv\": {\"sub\": \"data\", \"sha256\": \"ed615701dcd2efc909649f2a3fd8959817429028c194b415d0deea1be58c10fa\"}, \"contacts.json\": {\"sub\": \"data\", \"sha256\": \"6a993f7ec112777702bc2b6bb4e0c5580cffc6b4d93d4ba1893b8d5b1a0ffb59\"}, \"export.csv\": {\"sub\": \"data\", \"sha256\": \"71c8c546d014bd01bc290b6cd091534d2122e9119e0c3a680b44ccfdabf410f7\"}}")

problems = []
for name, want in EXPECT.items():
    p = os.path.join(INBOX, want["sub"], name)
    if not os.path.isfile(p):
        problems.append(f"{name} not under {want['sub']}/")
        continue
    if hashlib.sha256(open(p, "rb").read()).hexdigest() != want["sha256"]:
        problems.append(f"{name} changed")
loose = [n for n in os.listdir(INBOX) if os.path.isfile(os.path.join(INBOX, n))] if os.path.isdir(INBOX) else ["(inbox missing)"]
if loose:
    problems.append("still loose in inbox/: " + ", ".join(sorted(loose)))
if problems:
    print("TIDY FAIL:", "; ".join(problems))
    sys.exit(1)
print("TIDY OK")
