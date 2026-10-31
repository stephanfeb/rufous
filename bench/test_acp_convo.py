"""Tests for the ACP conversation rig (bead boubou-6gq): the checker on synthetic turns, and
the driver end to end against a fake ACP agent that speaks the wire format, asks one
permission and writes a session log. No model, no Ollama.

Run: uv run --with pytest --python 3.13 python -m pytest bench/ -q
"""
import json
import os
import stat
import sys
import textwrap

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acp_convo as rig  # noqa: E402

FAKE_AGENT = textwrap.dedent('''
    import json, os, sys
    home = os.environ["BOUBOU_HOME"]
    os.makedirs(os.path.join(home, "sessions"), exist_ok=True)
    log = open(os.path.join(home, "sessions", "acp_fake.log"), "a", encoding="utf-8")
    cwd = None
    n = 0

    def out(msg):
        sys.stdout.write(json.dumps(msg) + "\\n"); sys.stdout.flush()

    def logline(s):
        log.write("2026-10-31 19:00:00 " + s + "\\n"); log.flush()

    def chunk(sid, text):
        out({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid,
             "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        m = msg.get("method")
        if m == "initialize":
            out({"jsonrpc": "2.0", "id": msg["id"], "result": {"protocolVersion": 1, "agentCapabilities": {}}})
        elif m == "session/new":
            cwd = msg["params"]["cwd"]
            out({"jsonrpc": "2.0", "id": msg["id"], "result": {"sessionId": "s1"}})
        elif m == "session/prompt":
            sid = msg["params"]["sessionId"]; text = msg["params"]["prompt"][0]["text"]; n += 1
            logline("[turn] " + text[:120])
            logline("[ctx: 1000/12000 = 8%]")
            if "tasks" in text:
                open(os.path.join(cwd, "tasks.md"), "w").write("# Tasks\\n- [ ] first\\n")
                logline('[0|+1s] ⚙ create_file {"path":"tasks.md","content":"# Tasks"} -> [created tasks.md]')
                chunk(sid, "Done.")
            elif "drift" in text:
                out({"jsonrpc": "2.0", "id": 900 + n, "method": "session/request_permission", "params": {
                    "sessionId": sid,
                    "toolCall": {"toolCallId": "c1", "title": "outside the instruction: lib/x.py", "rawInput": {"tool": "create_file"}},
                    "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "allow_all", "name": "Allow all", "kind": "allow_always"},
                                {"optionId": "deny", "name": "Deny", "kind": "reject_once"}]}})
                ans = json.loads(sys.stdin.readline())
                chosen = ans["result"]["outcome"].get("optionId")
                if chosen in ("allow", "allow_all"):
                    os.makedirs(os.path.join(cwd, "lib"), exist_ok=True)
                    open(os.path.join(cwd, "lib", "x.py"), "w").write("x = 1\\n")
                    logline('[0|+1s] ⚙ create_file {"path":"lib/x.py","content":"x = 1"} -> [created lib/x.py]')
                    chunk(sid, "wrote code")
                else:
                    logline('[0|+1s] ⚙ create_file {"path":"lib/x.py","content":"x = 1"} -> [scope gate: the instruction was: ...]')
                    chunk(sid, "refused: outside the instruction")
            else:
                chunk(sid, "OK " + text)
            out({"jsonrpc": "2.0", "id": msg["id"], "result": {"stopReason": "end_turn"}})
''')

SCRIPT = {
    "id": "fake_flow", "fixture": "ch05_hello",
    "turns": [
        {"say": "Make tasks", "expect": {"files_exist": ["tasks.md"], "no_source_writes": True,
                                        "reply_regex": "Done", "max_calls": 1}},
        {"say": "Please drift", "permission": "deny",
         "expect": {"permission_asked": True, "files_absent": ["lib/x.py"], "reply_regex": "refused"}},
        {"say": "Please drift", "permission": "allow",
         "expect": {"permission_asked": True, "files_exist": ["lib/x.py"], "reply_regex": "wrote code"}},
        {"say": "say hi", "expect": {"reply_regex": ["^OK", "hi"], "max_calls": 0, "permission_asked": False,
                                    "no_tool": ["run_bash"]}},
        {"say": "fail me", "expect": {"reply_regex": "never", "files_exist": ["nope.txt"]}},
    ],
}


def _fake_binary(tmp_path):
    fake = tmp_path / "fake_agent.py"
    fake.write_text(FAKE_AGENT, encoding="utf-8")
    wrapper = tmp_path / "fake_boubou"
    wrapper.write_text(f"#!/bin/sh\nexec {sys.executable} -I {fake} \"$@\"\n", encoding="utf-8")
    wrapper.chmod(wrapper.stat().st_mode | stat.S_IXUSR)
    return str(wrapper)


def test_the_driver_runs_a_scripted_conversation_against_a_fake_agent(tmp_path, capsys):
    binary = _fake_binary(tmp_path)
    cell = tmp_path / "results" / "fake_1b" / "fake_flow_r0"
    score = rig.run_cell(binary, "fake:1b", 8000, SCRIPT, str(cell), 60, str(tmp_path / "nohome"))
    assert score["turns"] == 5
    assert score["passed"] == 4, json.dumps(score["turn_results"], indent=1)
    t = score["turn_results"]
    assert t[0]["pass"] and t[0]["calls"] == 1 and t[0]["source_writes"] == []
    assert t[1]["pass"] and len(t[1]["asks"]) == 1 and t[1]["asks"][0]["answer"] == "deny"
    assert t[2]["pass"] and t[2]["source_writes"] == ["lib/x.py"]
    assert t[3]["pass"] and t[3]["reply"] == "OK say hi"
    assert not t[4]["pass"] and len(t[4]["fails"]) == 2
    assert score["asks"] == 2 and score["calls"] == 3
    assert (cell / "score.json").is_file() and (cell / "turns.jsonl").is_file()
    assert (cell / "home" / "sessions" / "acp_fake.log").is_file(), "the session log is read from the cell's home"
    # A second call is a cache hit; the summary prints one row.
    again = rig.run_cell(binary, "fake:1b", 8000, SCRIPT, str(cell), 60, str(tmp_path / "nohome"))
    assert again["passed"] == 4
    rig.summary(str(tmp_path / "results"))
    out = capsys.readouterr().out
    assert "fake:1b" in out and "4/5" in out and "t5: reply does not match /never/" in out


def test_session_log_turns_split_on_markers(tmp_path):
    home = tmp_path / "home"
    (home / "sessions").mkdir(parents=True)
    (home / "sessions" / "x.log").write_text(
        "2026-10-31 19:00:00 [turn] Break the spec into tasks\n"
        "2026-10-31 19:00:00 [scope] class=tracker guarded (Break the spec into tasks) (On)\n"
        "2026-10-31 19:00:01 [0|+1s] ⚙ read_file {\"path\":\"openspec/spec.md\"} -> # Spec\n"
        "2026-10-31 19:00:02 [ctx: 5000/12000 = 41%]\n"
        "2026-10-31 19:00:03 [1|+3s] ⚙ create_file {\"path\":\"tasks.md\",\"content\":\"x\"} -> [created]\n"
        "2026-10-31 19:00:04 [2|+5s] ⚙ edit_file {\"path\":\"lib/export.py\",\"old\":\"a\"} -> [the text may say anything] ⟂refused:scope:user\n"
        "2026-10-31 19:00:04 [3|+6s] ⚙ append_file {\"path\":\"lib/cli.py\",\"content\":\"a\"} -> [appended 1 line]\n"
        "2026-10-31 19:00:05 [context compaction L1: 10 -> 4 messages]\n"
        "2026-10-31 19:00:06 [turn] Which task first?\n"
        "2026-10-31 19:00:07 [ctx: 6000/12000 = 50%]\n", encoding="utf-8")
    turns = rig.session_log_turns(str(home))
    assert len(turns) == 2
    assert [c["tool"] for c in turns[0]["calls"]] == ["read_file", "create_file", "edit_file", "append_file"]
    # The refused edit is an attempt, not a write; the appended source file is.
    assert rig.source_writes(turns[0]["calls"]) == ["lib/cli.py"]
    assert turns[0]["scope_class"] == "tracker" and turns[0]["compactions"] == 1 and turns[0]["ctx_peak"] == 5000
    assert turns[1]["calls"] == [] and turns[1]["ctx_peak"] == 6000


def test_check_turn_reads_each_expectation(tmp_path):
    (tmp_path / "report.md").write_text("# Heron report\n## Risks\n", encoding="utf-8")
    turn = {"reply": "OK\n3", "asks": [], "stop": "end_turn", "error": None}
    log = {"calls": [{"tool": "read_file", "args": '{"path":"a.md"}', "result": ""}]}
    ok = {"reply_regex": ["^\\s*OK", "\\b3\\b"], "reply_max_lines": 2, "files_exist": ["report.md"],
          "file_regex": {"report.md": "## Risks", "report.md#title": "^# Heron report"},
          "file_not_regex": {"report.md": "## Owners"}, "no_source_writes": True,
          "no_tool": ["run_bash"], "max_calls": 1, "permission_asked": False, "stop_reason": "end_turn"}
    assert rig.check_turn(ok, turn, log, str(tmp_path)) == []
    bad = {"reply_regex": "never", "reply_max_lines": 1, "files_exist": ["x.md"], "files_absent": ["report.md"],
           "file_regex": {"report.md": "## Owners"}, "no_tool": ["read_file"], "max_calls": 0,
           "permission_asked": True, "stop_reason": "cancelled"}
    fails = rig.check_turn(bad, turn, log, str(tmp_path))
    assert len(fails) == 9, fails
    code_turn = {"calls": [{"tool": "create_file", "args": '{"path":"lib/x.dart","content":"c"}', "result": ""}]}
    assert rig.check_turn({"no_source_writes": True}, turn, code_turn, str(tmp_path)) == ["source writes: ['lib/x.dart']"]
    assert rig.check_turn({"reply_nonempty": True}, {"reply": "  ", "asks": [], "stop": None, "error": None}, None, str(tmp_path)) == ["empty reply"]


def test_every_script_loads_and_names_a_fixture():
    scripts = rig.load_scripts([])
    assert {s["id"] for s in scripts} >= {"tracker_drift", "follow_ups", "constraints", "longform", "permission"}
    for s in scripts:
        rig.find_fixture(s["fixture"])
        assert s["turns"], s["id"]
        for t in s["turns"]:
            assert t["say"].strip() and isinstance(t.get("expect", {}), dict), (s["id"], t)


def test_a_status_tag_decides_what_landed_and_old_logs_fall_back_to_the_opening():
    assert rig.split_status("[created x] ") == ("[created x] ", "")
    assert rig.split_status("[whatever the gate said] ⟂refused:boundary:user") == (
        "[whatever the gate said]", "refused:boundary:user")
    tagged = {"tool": "create_file", "args": '{"path":"lib/a.py"}', "result": "[anything]", "status": "refused:scope"}
    old = {"tool": "create_file", "args": '{"path":"lib/a.py"}', "result": "[scope gate: …]", "status": ""}
    landed = {"tool": "create_file", "args": '{"path":"lib/a.py"}', "result": "[created lib/a.py]", "status": ""}
    assert rig.source_writes([tagged, old, landed]) == ["lib/a.py"]
