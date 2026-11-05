//! `RunParams`: the environment switches Mingbird's launch scripts set, read the way
//! Mingbird reads them (a typo never stops the agent), and `config.json` `slim_output`
//! applied under the environment's override.

use rufous_kernel::agent::{parse_switch, parse_temp, parse_think, resolve_slim_output, RunParams};
use rufous_kernel::config::{Config, SlimOutput};

#[test]
fn think_is_a_tri_state_and_a_typo_is_off_not_fatal() {
    assert_eq!(parse_think(None), None);
    assert_eq!(parse_think(Some("")), None);
    assert_eq!(parse_think(Some("  ")), None);
    assert_eq!(parse_think(Some("1")), Some(true));
    assert_eq!(parse_think(Some("true")), Some(true));
    assert_eq!(parse_think(Some("On")), Some(true));
    assert_eq!(parse_think(Some("0")), Some(false));
    assert_eq!(parse_think(Some("false")), Some(false));
    assert_eq!(
        parse_think(Some("maybe")),
        Some(false),
        "Mingbird: anything but on is off"
    );
}

#[test]
fn a_temperature_that_is_not_a_number_is_unset_not_fatal() {
    assert_eq!(parse_temp(None), None);
    assert_eq!(parse_temp(Some("")), None);
    assert_eq!(parse_temp(Some("0")), Some(0.0));
    assert_eq!(parse_temp(Some(" 0.7 ")), Some(0.7));
    assert_eq!(parse_temp(Some("abc")), None);
    assert_eq!(parse_temp(Some("nan")), None);
}

#[test]
fn slim_output_follows_env_then_config_then_the_context_size() {
    assert_eq!(parse_switch("X", Some("on")), Some(true));
    assert_eq!(parse_switch("X", Some("0")), Some(false));
    assert_eq!(parse_switch("X", Some("sometimes")), None);
    assert_eq!(parse_switch("X", None), None);
    // The environment wins.
    assert!(resolve_slim_output(Some(true), SlimOutput::Off, 131_072));
    assert!(!resolve_slim_output(Some(false), SlimOutput::On, 8_192));
    // Then the config.
    assert!(resolve_slim_output(None, SlimOutput::On, 131_072));
    assert!(!resolve_slim_output(None, SlimOutput::Off, 8_192));
    // Then the context size.
    assert!(resolve_slim_output(None, SlimOutput::Auto, 32_768));
    assert!(!resolve_slim_output(None, SlimOutput::Auto, 32_769));
}

#[test]
fn apply_config_reads_the_slim_output_key_that_used_to_be_ignored() {
    let cfg = Config {
        slim_output: SlimOutput::On,
        ..Config::default()
    };
    let mut p = RunParams::plain("m", "/tmp");
    assert!(!p.slim_output);
    p.apply_config(&cfg);
    assert!(p.slim_output, "config.json slim_output=on was not applied");
    let mut p = RunParams::plain("m", "/tmp");
    p.num_ctx = 8_192;
    p.apply_config(&Config {
        slim_output: SlimOutput::Off,
        ..Config::default()
    });
    assert!(
        !p.slim_output,
        "config.json slim_output=off was not applied"
    );
    let mut p = RunParams::plain("m", "/tmp");
    p.num_ctx = 8_192;
    p.slim_output_env = Some(false);
    p.apply_config(&cfg);
    assert!(!p.slim_output, "the environment must win over the config");
}

#[test]
fn the_standard_agent_runs_its_executor_in_the_configured_slim_mode() {
    // The production wiring, with a host nothing is sent to while building.
    let cfg = Config {
        ollama_host: "http://127.0.0.1:9".into(),
        slim_output: SlimOutput::On,
        ..Config::default()
    };
    let mut p = RunParams::plain("m", std::env::temp_dir());
    p.num_ctx = 131_072;
    let agent = rufous_kernel::agent::Agent::standard(p, &cfg).expect("agent");
    assert!(
        agent.tools.slim_output,
        "config.json slim_output=on did not reach the executor"
    );
}

#[test]
fn the_home_line_is_left_out_when_there_is_no_user_home() {
    // A sandboxed helper's home is its host's container (boubou-f8d).
    use rufous_kernel::agent::prompt::{fill_base_with, TASK_BASE};
    let named = fill_base_with(TASK_BASE, Some("/Users/ada"));
    assert!(named.contains("- Home directory: /Users/ada. To save"));
    assert!(named.contains("/Users/ada/Desktop"));
    let none = fill_base_with(TASK_BASE, None);
    assert!(!none.contains("Home directory") && !none.contains("{USER_HOME}"));
    assert_eq!(
        named.lines().count(),
        none.lines().count() + 1,
        "only that line goes"
    );
}

#[test]
fn no_start_engine_makes_the_run_fail_fast_on_an_unreachable_engine() {
    let mut p = RunParams::plain("m", ".");
    p.apply_config(&Config::default());
    assert!(!p.engine_unmanaged);
    p.apply_config(&Config {
        no_start_engine: true,
        ..Config::default()
    });
    assert!(p.engine_unmanaged);
}
