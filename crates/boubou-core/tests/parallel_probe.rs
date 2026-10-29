//! Port of `tests/test_parallel_probe.py`: cost formula and capacity plan with injected
//! readers; `/api/ps` parsing; the real memory reader on this machine.

use boubou_core::ollama::{parse_ps, LoadedModel};
use boubou_core::parallel::probe::{
    available_from_vm_stat, read_loaded_models, read_system_memory, GIB,
};
use boubou_core::parallel::{
    estimate_child_cost, format_capacity_table, CapacityPlanner, ParallelConfig, ResourceProbe,
};
use serde_json::json;

fn cfg() -> ParallelConfig {
    ParallelConfig::default()
}

fn probe(available_gib: f64, loaded: Vec<LoadedModel>, models_ok: bool) -> ResourceProbe {
    ResourceProbe::with_readers(
        Box::new(move || Some((32.0 * GIB, available_gib * GIB))),
        Box::new(move || models_ok.then(|| loaded.clone())),
    )
}

fn lm(model: &str, size_gib: f64) -> LoadedModel {
    LoadedModel {
        model: model.into(),
        size: size_gib * GIB,
        size_vram: size_gib * GIB,
    }
}

#[test]
fn e2b_8k_is_conservative_and_tiers_scale() {
    let (cost, br) = estimate_child_cost("gemma4:e2b", &cfg(), None).expect("cost");
    assert!(3.0 * GIB < cost && cost < 5.0 * GIB, "{cost}");
    assert_eq!(br.params_b, Some(2.0));
    assert_eq!(br.ctx, 8192);
    let (c4, _) = estimate_child_cost("qwen3.5:4b", &cfg(), None).expect("cost");
    assert!(c4 > cost * 1.5);
    let mut big = cfg();
    big.child_ctx = 32768;
    let (c32, _) = estimate_child_cost("gemma4:e2b", &big, None).expect("cost");
    assert!(c32 > cost);
}

#[test]
fn too_large_or_unparsable_models_rejected() {
    let rej = estimate_child_cost("ornith-1.5:35b", &cfg(), None).expect_err("reject");
    assert_eq!(rej.reason, "child_model_too_large");
    assert!(estimate_child_cost("some-model", &cfg(), None).is_err());
}

#[test]
fn loaded_size_preferred_over_estimate() {
    let (_, br) = estimate_child_cost("gemma4:e2b", &cfg(), Some(1.4 * GIB)).expect("cost");
    assert!((br.weights_bytes - 1.4 * GIB).abs() < 1.0);
}

#[test]
fn two_children_on_20gb_free_clamped_to_hard_cap() {
    let plan = probe(20.0, vec![], true).plan_capacity("gemma4:e2b", &cfg());
    assert_eq!(plan.max_parallel, 2); // floor(12.5/3.8)=3 → hard_cap 2
    assert!(plan
        .reasons
        .iter()
        .any(|r| r.contains("clamped_to_hard_cap")));
    assert!(plan.reasons.iter().any(|r| r.contains("budget=")));
    assert!(plan.reasons.iter().any(|r| r.contains("per_child=")));
    assert!(format_capacity_table(&plan, &cfg()).contains("max_parallel = 2"));
    let mut c = cfg();
    c.hard_cap = 2;
    assert!(
        probe(31.0, vec![], true)
            .plan_capacity("gemma4:e2b", &c)
            .max_parallel
            <= 2
    );
}

#[test]
fn no_headroom_means_zero() {
    let plan = probe(1.0, vec![], true).plan_capacity("gemma4:e2b", &cfg());
    assert_eq!(plan.max_parallel, 0);
    assert!(plan.reasons.iter().any(|r| r.contains("no_headroom")));
}

#[test]
fn probe_failures_are_conservative() {
    let p = ResourceProbe::with_readers(Box::new(|| None), Box::new(|| Some(vec![])));
    let plan = p.plan_capacity("gemma4:e2b", &cfg());
    assert_eq!(plan.max_parallel, 0);
    assert!(plan.reasons[0].contains("memory_probe_failed"));

    let plan = probe(20.0, vec![], false).plan_capacity("gemma4:e2b", &cfg());
    assert_eq!(plan.max_parallel, 0);
    assert!(plan
        .reasons
        .iter()
        .any(|r| r.contains("ollama_ps_unreachable")));
}

#[test]
fn parent_eviction_gate() {
    let loaded = vec![lm("ornith-1.5:35b", 20.0)];
    let plan = probe(20.0, loaded.clone(), true).plan_capacity("gemma4:e2b", &cfg());
    assert_eq!(plan.max_parallel, 0);
    assert!(plan
        .reasons
        .iter()
        .any(|r| r.contains("parent_model_would_be_evicted")));
    let mut c = cfg();
    c.allow_parent_eviction = true;
    assert!(
        probe(20.0, loaded, true)
            .plan_capacity("gemma4:e2b", &c)
            .max_parallel
            >= 1
    );
}

#[test]
fn same_model_as_parent_counts_full_cost() {
    let plan = probe(20.0, vec![lm("gemma4:e2b", 2.0)], true).plan_capacity("gemma4:e2b", &cfg());
    assert_eq!(plan.max_parallel, 2); // no same-model discount (over-estimate direction)
}

#[test]
fn ps_parsing_is_lenient() {
    let v = json!({"models": [
        {"model": "gemma4:e2b", "size": 2147483648u64, "size_vram": 2000000000u64},
        {"name": "other", "size": "1024"}
    ]});
    let out = parse_ps(&v);
    assert_eq!(out[0].model, "gemma4:e2b");
    assert_eq!(out[0].size, 2147483648.0);
    assert_eq!(out[0].size_vram, 2000000000.0);
    assert_eq!(out[1].model, "other");
    assert_eq!(out[1].size, 1024.0);
    assert!(parse_ps(&json!({})).is_empty());
}

#[test]
fn unreachable_ollama_reads_none() {
    // A closed port: None (probe failed), never Some(vec![]).
    let r = read_loaded_models("http://127.0.0.1:9", std::time::Duration::from_secs(2));
    assert!(r.is_none());
}

#[test]
fn real_memory_reader_works_here() {
    let (total, avail) = read_system_memory().expect("memory reading");
    eprintln!("memory: total={total} avail={avail}");
    assert!(
        total > 0.0 && avail > 0.0 && avail <= total,
        "total={total} avail={avail}"
    );
}

#[test]
fn vm_stat_available_is_free_plus_inactive_pages() {
    // The readings of 2026-10-29 with qwen3.5:9b resident, where sysinfo said 0.
    let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
Pages free:                                17022.\n\
Pages active:                             261894.\n\
Pages inactive:                           255351.\n\
Pages speculative:                          6554.\n\
Pages throttled:                               0.\n\
Pages wired down:                        1172275.\n\
Pages purgeable:                           13107.\n\
\"Translation faults\":                 123456789.\n\
Pages stored in compressor:               900000.\n";
    let avail = available_from_vm_stat(text).expect("parsed");
    assert_eq!(avail, (17022.0 + 255351.0) * 16384.0);
    assert!((avail / GIB - 4.15).abs() < 0.05, "{}", avail / GIB);
    assert!(available_from_vm_stat("garbage").is_none());
    assert!(available_from_vm_stat(
        "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages free: 10.\n"
    )
    .is_none());
}
