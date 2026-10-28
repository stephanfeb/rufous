//! Compatibility with Mingbird's on-disk formats, using fixtures copied from the Python repo.

use std::path::Path;

use boubou_core::config::{Config, SlimOutput};
use boubou_core::mcp;
use boubou_core::ollama::{ChatChunk, ChatRequest, Message, NdjsonDecoder, Options};
use boubou_core::skills::{Skill, SkillSet};
use boubou_core::tools::{self, ToolInvocation, CORE_TOOL_NAMES};
use serde_json::json;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn config_example_parses_with_defaults() {
    let cfg = Config::load_from(&fixture("config.json")).expect("fixture parses");
    assert_eq!(cfg.ollama_host, "http://127.0.0.1:11434");
    assert!(cfg.ollama_exe.is_empty());
    assert_eq!(cfg.slim_output, SlimOutput::Auto);
    assert!(!cfg.offline_mode);
}

#[test]
fn missing_config_is_defaults_and_unknown_keys_are_ignored() {
    let cfg = Config::load_from(Path::new("/definitely/not/here.json")).expect("defaults");
    assert_eq!(cfg, Config::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("config.json");
    std::fs::write(
        &p,
        r#"{"ollama_host":"http://gpu-box:11434","some_future_key":1,"slim_output":"on"}"#,
    )
    .expect("write");
    let cfg = Config::load_from(&p).expect("parses");
    assert_eq!(cfg.ollama_host, "http://gpu-box:11434");
    assert_eq!(cfg.slim_output, SlimOutput::On);
}

#[test]
fn config_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("nested/config.json");
    let mut cfg = Config::default();
    cfg.models.insert("fast".into(), "gemma4:e2b".into());
    cfg.save_to(&p).expect("save");
    let back = Config::load_from(&p).expect("load");
    assert_eq!(back, cfg);
    assert_eq!(back.display_name("gemma4:e2b"), "fast");
    assert_eq!(back.display_name("qwen3:0.6b"), "qwen3:0.6b");
}

#[test]
fn core_tool_schemas_match_mingbird() {
    let core = tools::core_tools();
    let names: Vec<&str> = core
        .iter()
        .map(|t| t["function"]["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, CORE_TOOL_NAMES);
    // Minimal schema: no per-parameter descriptions anywhere (prefill budget).
    for t in core.iter().chain(tools::advanced_tools().iter()) {
        let props = t["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties");
        for (_, p) in props {
            assert!(p.get("description").is_none(), "{t}");
        }
        assert!(t["function"]["parameters"]["required"].is_array());
    }
    assert_eq!(
        tools::advanced_tools().len(),
        tools::ADVANCED_TOOL_NAMES.len()
    );
}

#[test]
fn tool_schemas_add_only_enabled_advanced_tools() {
    let all = tools::tool_schemas(&["web_fetch".to_string(), "nonsense".to_string()]);
    assert_eq!(all.len(), 10);
    assert_eq!(all[9]["function"]["name"], "web_fetch");
}

#[test]
fn tool_calls_decode_into_typed_variants() {
    let inv = ToolInvocation::parse("create_file", &json!({"path": "a.txt", "content": "hi"}))
        .expect("valid");
    assert_eq!(
        inv,
        ToolInvocation::CreateFile {
            path: "a.txt".into(),
            content: "hi".into(),
            replace: false
        }
    );
    assert_eq!(inv.name(), "create_file");

    // Stringified JSON arguments, as small models often emit.
    let inv = ToolInvocation::parse(
        "todo",
        &json!(r#"{"action":"update","index":2,"done":true}"#),
    )
    .expect("string args accepted");
    match inv {
        ToolInvocation::Todo(args) => {
            assert_eq!(args.index, vec![2]);
            assert_eq!(args.done, Some(true));
        }
        other => panic!("expected todo, got {other:?}"),
    }
}

#[test]
fn web_search_accepts_a_list_of_queries() {
    // gemma4 sent `{"query": ["a", "b"]}` three times in one session and lost each turn to
    // "invalid type: sequence"; a list now runs as a multi-search.
    let inv = ToolInvocation::parse(
        "web_search",
        &json!({"query": ["Flint model", "Recursive AI Flint"]}),
    )
    .expect("list");
    assert_eq!(
        inv,
        ToolInvocation::WebSearchMulti {
            queries: vec!["Flint model".into(), "Recursive AI Flint".into()],
            max_results: None
        }
    );
    let inv =
        ToolInvocation::parse("web_search", &json!({"query": "Flint model"})).expect("string");
    assert_eq!(
        inv,
        ToolInvocation::WebSearch {
            query: "Flint model".into()
        }
    );
    let err = ToolInvocation::parse("web_search", &json!({"query": 7})).expect_err("number");
    assert!(err.to_string().contains("argument `query`"), "{err}");
}

#[test]
fn tool_call_errors_are_actionable() {
    let err = ToolInvocation::parse("edit_file", &json!({"path": "a"})).expect_err("missing");
    assert_eq!(
        err.to_string(),
        "tool `edit_file`: missing required argument `old`"
    );
    let err = ToolInvocation::parse("todo", &json!({"action": "destroy"})).expect_err("enum");
    assert!(err.to_string().contains("argument `action`"));
    let err = ToolInvocation::parse("teleport", &json!({})).expect_err("unknown");
    assert_eq!(err.to_string(), "unknown tool `teleport`");
}

#[test]
fn request_omits_tristate_fields_when_unset() {
    let req = ChatRequest {
        model: "gemma4:e2b".into(),
        messages: vec![Message::system("x"), Message::user("y")],
        tools: tools::core_tools(),
        stream: true,
        think: None,
        keep_alive: None,
        options: Options {
            num_ctx: 8192,
            num_predict: 2048,
            temperature: None,
        },
    };
    let v = serde_json::to_value(&req).expect("serialize");
    assert!(v.get("think").is_none());
    assert!(v["options"].get("temperature").is_none());
    let with = ChatRequest {
        think: Some(false),
        ..req
    };
    let v = serde_json::to_value(&with).expect("serialize");
    assert_eq!(v["think"], false);
}

#[test]
fn ndjson_decoder_handles_split_lines_and_tool_calls() {
    let mut dec = NdjsonDecoder::default();
    dec.push(br#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"list_dir","arguments":{"path":"."}}}]},"done":false}"#);
    dec.push(b"\n\n{\"message\":{\"role\":\"assist");
    let first: ChatChunk = dec.next_value().expect("one line").expect("parses");
    assert_eq!(first.message.tool_calls[0].function.name, "list_dir");
    assert!(
        dec.next_value::<ChatChunk>().is_none(),
        "partial line waits"
    );
    dec.push(br#"ant","content":"done"},"done":true,"done_reason":"stop","eval_count":7}"#);
    assert!(dec.next_value::<ChatChunk>().is_none(), "still no newline");
    let last: ChatChunk = dec.finish().expect("flushed").expect("parses");
    assert!(last.done);
    assert_eq!(last.eval_count, Some(7));
}

#[test]
fn mingbird_skill_file_parses_and_loads() {
    let skill = Skill::from_path(&fixture("architecture.md")).expect("frontmatter");
    assert_eq!(skill.name, "architecture");
    assert!(skill.description.starts_with("System design"));
    let body = skill.load().expect("body");
    assert!(body.starts_with("# Architecture design"));
    assert!(!body.contains("---\nname:"));
}

#[test]
fn skill_lookup_order_shadows_and_skips_readme() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hi = dir.path().join("work/skills");
    let lo = dir.path().join("home/skills");
    std::fs::create_dir_all(&hi).expect("mkdir");
    std::fs::create_dir_all(&lo).expect("mkdir");
    std::fs::write(
        hi.join("a.md"),
        "---\nname: a\ndescription: high\n---\nbody",
    )
    .expect("w");
    std::fs::write(lo.join("a.md"), "---\nname: a\ndescription: low\n---\nbody").expect("w");
    std::fs::write(
        lo.join("b.md"),
        "---\nname: b\ndescription: only low\n---\nbody",
    )
    .expect("w");
    std::fs::write(lo.join("README.md"), "# not a skill").expect("w");
    std::fs::write(lo.join("broken.md"), "no frontmatter").expect("w");
    let set = SkillSet::discover([&hi, &lo]);
    assert_eq!(set.len(), 2);
    assert_eq!(set.get("a").expect("a").description, "high");
    assert_eq!(set.listing(), "a: high\nb: only low");
    assert!(set.get("zzz").is_err());
}

#[test]
fn mcp_config_example_parses_with_category_overrides() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mcp.json.example");
    let cfg = mcp::load_from(&root).expect("example parses");
    let big = &cfg["big_server"];
    assert_eq!(big.categories_for("search_web"), vec!["web"]);
    assert_eq!(big.categories_for("save_note"), vec!["memory", "files"]);
    assert_eq!(big.categories_for("anything_else"), vec!["files"]);
    assert!(big.unknown_categories().is_empty());
    assert!(!cfg["utils"].is_http());
    assert_eq!(mcp::qualified_name("utils", "calculate"), "utils.calculate");
}

#[test]
fn model_dedupe_prefers_mapped_then_non_latest_then_shorter() {
    use boubou_core::config::Config;
    use boubou_core::ollama::ModelInfo;
    let mi = |name: &str, digest: &str| ModelInfo {
        name: name.into(),
        size: 0,
        digest: digest.into(),
        details: serde_json::Value::Null,
    };
    let mut cfg = Config::default();
    cfg.models.insert("Gemma".into(), "gemma4:e2b".into());
    let list = [
        mi("gemma4:latest", "d1"),
        mi("gemma4:e2b", "d1"), // mapped wins over :latest and over the alias below
        mi("my-gemma-alias", "d1"),
        mi("qwen3:8b", "d2"),
        mi("q", "d2"),         // shorter wins among unmapped non-latest
        mi("solo:latest", ""), // no digest → keyed by name
    ];
    let out = cfg.dedupe_model_entries(&list);
    assert_eq!(
        out,
        vec![
            ("Gemma".to_string(), "gemma4:e2b".to_string()),
            ("q".to_string(), "q".to_string()),
            ("solo:latest".to_string(), "solo:latest".to_string()),
        ]
    );
}
