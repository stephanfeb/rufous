import json, ast, re, glob, os, sys
dec = json.JSONDecoder()
HOMES = [os.path.expanduser('~/.ollama_agent/sessions'), os.path.expanduser('~/.rufous-lmstudio/sessions')]

def records(path):
    out = []
    for line in open(path, errors='replace'):
        s = line.strip(); i = 0
        while i < len(s):
            try: o, i = dec.raw_decode(s, i)
            except Exception: break
            while i < len(s) and s[i] in ' \t': i += 1
            out.append(o)
    return out

def calls(o):
    tc = o.get('tool_calls')
    if not tc: return []
    if isinstance(tc, str):
        try: tc = ast.literal_eval(tc)
        except Exception: return []
    res = []
    for c in tc:
        f = c.get('function', {})
        a = f.get('arguments', {})
        if isinstance(a, str):
            try: a = json.loads(a)
            except Exception: a = {}
        res.append((f.get('name', ''), a))
    return res

WRITE_TOOLS = {'create_file', 'edit_file', 'append_file', 'write_file', 'replace_in_file', 'multi_edit'}
VERIFIERS = r'pytest|py\.test|jest|vitest|mocha|tox|mypy|pyright|eslint|flake8|pylint|shellcheck|tsc|vue-tsc'
CHECK = re.compile(r'(?:^|[;&|]\s*|\s)(?:' + VERIFIERS + r')\b|\b(?:npm|yarn|pnpm|bun)\s+(?:run\s+)?(?:test|lint|type-?check|build)\b|\bcargo\s+(?:test|check|build|clippy)\b|\bgo\s+(?:test|vet|build)\b|\bmake\s+(?:test|check)\b')
RUN = re.compile(r'\bcurl\b|\bwget\b|\bhttp(?:ie)?\s|\bnode\s+\S+\.(?:m?js|cjs)\b|\bpython3?\s+(?!-m\s+(?:pip|venv))\S+|\buvicorn\b|\bnpx\s+playwright\b|^\s*\./')
SHELL_WRITE = re.compile(r'<<|(?<![0-9&])>(?!&)|\btee\b|\bsed\s+-i\b|\bmv\b|\bcp\b|\brm\b|\bmkdir\b|\bnpm\s+(?:i|install)\b|\btouch\b')

# Behaviour claims about code the turn changed.
CLAIM = re.compile(r'''(?ix)
    \b(?:fixed|resolved)\s+(?:the|a|an|this|that|it|by|issue|bug|error|problem|\d+|`)
  | ^\s*(?:\*\*)?(?:fixed|resolved)\b
  | \bnow\s+(?:works|loads|displays|renders|serves|shows|runs|builds|compiles|returns|appears|responds)\b
  | \b(?:works|loads|renders|displays)\s+(?:correctly|properly|as\s+expected|fine)\b
  | \bsuccessfully\s+(?:displays?|renders?|loads?|runs?|works?|serves?|builds?|compiles?|returns?)\b
  | \b(?:is|are)\s+now\s+(?:fully\s+)?(?:functional|working|operational|served|fixed|resolved)\b
  | \bfully\s+(?:functional|working|operational)\b
  | \b(?:vertical\s+slice|feature|fix|integration)\b[^.\n]{0,40}\b(?:is|are)\s+(?:now\s+)?complete\b
''')
VERIFY_CLAIM = re.compile(r'''(?ix)
    \b(?:verified|tested|confirmed)\s+(?:that|it|the|this|by|with|everything)\b
  | \b(?:has|have)\s+been\s+(?:verified|tested|confirmed)\b
''')

HEDGE = re.compile(r'''(?ix)
    \bnot\s+(?:yet\s+)?(?:been\s+)?(?:verified|tested|run|checked)\b
  | \bunverified\b | \buntested\b
  | \b(?:have\s+not|haven't|did\s+not|didn't|could\s+not|couldn't)\s+(?:yet\s+)?(?:run|test|verify|check|start)\b
  | \bplease\s+(?:verify|test|check|confirm|try)\b | \byou\s+(?:can|may|should)\s+(?:now\s+)?(?:verify|test|check|run|open)\b
''')
FAIL_OUT = re.compile(r'''(?x)
    \bFAILURE\b | \bFAILED\b | \bBUILD\s+FAILED\b | [Bb]uild\s+failed | Traceback\s+\(most\s+recent | \bException\b
  | ^\s*(?:[Ee]rror|ERROR)\b[:\[] | \berror\[E\d+ | \bNot\s+Found\b | \b404\b | \b500\s+Internal | Cannot\s+GET
  | ECONNREFUSED | [Cc]onnection\s+refused | command\s+not\s+found | No\s+such\s+file
''', re.M)
COMPLAINT = re.compile(r'''(?ix)
    \bstill\b[^.\n]{0,30}\b(?:broken|not|fail|error|404|blank|plain|wrong|missing|same)\b
  | \bdoesn'?t\s+work\b | \bnot\s+working\b | \bdid\s*n'?t\s+work\b | \bisn'?t\s+working\b
  | \bdid\s+you\s+(?:test|verify|check|run)\b | \blooks\s+(?:\w+\s+)?(?:plain|broken|wrong|the\s+same|off|bad)\b
  | \berror\b | \b404\b | \bblank\b | \bnothing\s+(?:shows|happens|changed)\b | \bno\s+styl\w*\b | \bunstyled\b
  | \bbroken\b | \bfail(?:s|ed|ing)?\b
''')

def turn_texts(log):
    t = []
    for line in open(log, errors='replace'):
        m = re.search(r'\[turn\] (.*)', line)
        if m: t.append(m.group(1).strip()[:50])
    return t

rows = []
seen = set()
for home in HOMES:
    for log in sorted(glob.glob(home + '/acp_*.log')):
        stem = os.path.basename(log)[:-4]
        if stem[4:8] < '1102': continue
        turns = turn_texts(log)
        if not turns: continue
        cands = sorted(glob.glob(home + '/' + stem + '*.transcript.jsonl'), key=os.path.getsize, reverse=True)
        if not cands: continue
        recs = records(cands[0])
        # split into instructions
        idx = [k for k, o in enumerate(recs) if o.get('role') == 'user' and any(isinstance(o.get('content'), str) and o['content'].strip()[:50].startswith(t[:40]) for t in turns)]
        for n, k in enumerate(idx):
            end = idx[n + 1] if n + 1 < len(idx) else len(recs)
            instr = recs[k]['content'].strip()
            key = (stem, instr[:80])
            if key in seen: continue
            seen.add(key)
            seq = recs[k + 1:end]
            # actions with ordering
            acts = []
            raw = []
            pending = []
            for o in seq:
                if o.get('role') == 'assistant':
                    pending = calls(o)
                    for name, a in pending:
                        if name in WRITE_TOOLS:
                            acts.append(('write', a.get('path', ''), None))
                elif o.get('role') == 'tool' and pending:
                    name, a = pending.pop(0)
                    if name in WRITE_TOOLS:
                        raw.append({'tool': name, 'path': a.get('path', ''), 'result': (o.get('content') or '')[:2000]})
                    elif name == 'run_bash':
                        raw.append({'tool': 'run_bash', 'command': a.get('command', ''), 'result': (o.get('content') or '')[:4000]})
                    if name == 'run_bash':
                        cmd = a.get('command', '')
                        m = re.match(r'\[exit (-?\d+)\]', o.get('content') or '')
                        ex = int(m.group(1)) if m else None
                        inline = re.search(r'\bpython3?\s+(?:-c\b|<<|-\s*<<)|\bnode\s+-e\b|\b(?:pip|npm|yarn|pnpm)\s+(?:i|install|add)\b|-m\s+(?:pip|venv)\b', cmd)
                        kind = 'write' if inline or (SHELL_WRITE.search(cmd) and not CHECK.search(cmd)) else 'check' if CHECK.search(cmd) else 'run' if RUN.search(cmd) else 'read'
                        if ex == 0 and FAIL_OUT.search(o.get('content') or ''):
                            ex = 'masked'
                        acts.append((kind, cmd, ex))
            final = ''; how = 'none'
            for o in reversed(seq):
                if o.get('role') != 'assistant': continue
                cs = calls(o)
                fin = [a for n_, a in cs if n_ == 'finish']
                if fin:
                    final = str(fin[0].get('summary', '')).strip(); how = 'finish'; break
                if not cs and (o.get('content') or '').strip():
                    final = o['content'].strip(); how = 'text'; break
            wrote = [a for a in acts if a[0] == 'write' and a[2] is None]
            lastw = max([i for i, a in enumerate(acts) if a[0] == 'write'], default=-1)
            hard = [a for a in acts[lastw + 1:] if a[0] == 'check' and a[2] == 0]
            backing = [a for a in acts[lastw + 1:] if a[0] in ('check', 'run') and a[2] == 0]
            anyrun = [a for a in acts if a[0] in ('check', 'run')]
            claims = [m.group(0) for m in CLAIM.finditer(final)] + ['V:'+m.group(0) for m in VERIFY_CLAIM.finditer(final)]
            hedge = bool(HEDGE.search(final))
            nxt = recs[end]['content'].strip() if end < len(recs) else ''
            rows.append(dict(raw=raw, instr_full=instr, final_full=final, bcmds=[(a[1][:90], a[2]) for a in backing], hard=len(hard), how=how, s=stem, instr=instr[:70], writes=len(wrote), backing=len(backing), runs=len(anyrun),
                             claims=claims, hedge=hedge, final=final[:300].replace('\n', ' '), nxt=nxt[:120].replace('\n', ' '),
                             complaint=bool(COMPLAINT.search(nxt)) if nxt else None))

json.dump(rows, open(sys.argv[1], 'w'), indent=1)
tot = len(rows)
w = [r for r in rows if r['writes']]
flag = [r for r in w if r['claims'] and not r['backing'] and not r['hedge']]
backed = [r for r in w if r['claims'] and r['backing']]
hed = [r for r in w if r['claims'] and r['hedge'] and not r['backing']]
nocl = [r for r in w if not r['claims']]
print(f"turns {tot}; with writes {len(w)}")
print(f"  claim, no backing, no hedge (would hold): {len(flag)}  next-msg complaint {sum(1 for r in flag if r['complaint'])}/{sum(1 for r in flag if r['nxt'])}")
print(f"  claim, backed: {len(backed)}  complaint {sum(1 for r in backed if r['complaint'])}/{sum(1 for r in backed if r['nxt'])}")
print(f"  claim, hedged, no backing: {len(hed)}  complaint {sum(1 for r in hed if r['complaint'])}/{sum(1 for r in hed if r['nxt'])}")
print(f"  no claim: {len(nocl)}  complaint {sum(1 for r in nocl if r['complaint'])}/{sum(1 for r in nocl if r['nxt'])}")
nw = [r for r in rows if not r['writes'] and r['claims']]
print(f"turns without writes but a claim-shaped reply (would not be judged): {len(nw)}")
