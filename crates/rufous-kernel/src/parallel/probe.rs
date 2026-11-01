//! Hardware probe (`parallel_probe.py`): on unified-memory machines the real budget is
//! physical RAM.
//!
//! - memory: `sysinfo` (total / available bytes); on macOS the available figure comes from
//!   `vm_stat` first (psutil's formula, see [`read_system_memory`]);
//! - loaded models: `GET /api/ps` → model / size / size_vram;
//! - capacity: `usable = available × cap`, `budget = usable − reserve`,
//!   `max_parallel = floor(budget / per_child_cost)`, then min with `hard_cap`;
//! - parent coexistence gate: a different loaded model would be evicted → refused by default.
//!
//! Both readers are injectable, so tests never touch the machine.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use super::config::ParallelConfig;
use crate::guards::model_params_b;
use crate::ollama::{Client, LoadedModel};

pub const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// `parse_params_b`: `family:e2b` → 2.0, `x:7b-instruct` → 7.0; `None` when unparseable (treated as large).
pub fn parse_params_b(model: &str) -> Option<f64> {
    model_params_b(model)
}

/// `(total_bytes, available_bytes)` or `None` when the reading failed.
///
/// On macOS `sysinfo` computes available as free + inactive + purgeable **minus the
/// compressor's pages**; with a 7 GB model resident the compressor outgrows the rest and the
/// figure saturates to 0 (2026-10-29, 38 GB Mac, a 9B model loaded: sysinfo 0 B, `vm_stat`
/// free 0.3 GB + inactive 3.9 GB; bead boubou-wh5). That told the capacity planner there was
/// no room exactly when a model was loaded, which is when dispatch is asked. Mingbird reads
/// psutil, whose macOS available is free + inactive, and the cap/reserve formula was tuned
/// against that, so macOS reads `vm_stat` first and falls back to `sysinfo` only when the
/// command fails. Other platforms use `sysinfo` as before.
pub fn read_system_memory() -> Option<(f64, f64)> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total = sys.total_memory() as f64;
    if total <= 0.0 {
        return None;
    }
    let avail = vm_stat_available()
        .filter(|a| *a > 0.0 && *a <= total)
        .unwrap_or(sys.available_memory() as f64);
    Some((total, avail))
}

/// macOS only: `vm_stat`'s free + inactive pages × page size, psutil's "available".
fn vm_stat_available() -> Option<f64> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("vm_stat").output().ok()?;
    if !out.status.success() {
        return None;
    }
    available_from_vm_stat(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `vm_stat` output: `Mach Virtual Memory Statistics: (page size of 16384 bytes)`,
/// then `Pages free:` and `Pages inactive:` rows (trailing dots). Public for the tests.
pub fn available_from_vm_stat(text: &str) -> Option<f64> {
    let page = text
        .lines()
        .find_map(|l| {
            let (_, rest) = l.split_once("page size of ")?;
            rest.split_whitespace().next()?.parse::<f64>().ok()
        })
        .filter(|p| *p > 0.0)?;
    let count = |key: &str| -> Option<f64> {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_end_matches('.').parse::<f64>().ok())?
        })
    };
    let free = count("Pages free")?;
    let inactive = count("Pages inactive")?;
    Some((free + inactive) * page)
}

/// `read_loaded_models`: `/api/ps`. `None` = unreachable (capacity cannot be confirmed →
/// no dispatch); `Some(vec![])` = Ollama up with nothing loaded (fine to dispatch).
pub fn read_loaded_models(host: &str, timeout: Duration) -> Option<Vec<LoadedModel>> {
    let host = host.to_string();
    let work = move || -> Option<Vec<LoadedModel>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async {
            tokio::time::timeout(timeout, Client::new(host).loaded_models())
                .await
                .ok()?
                .ok()
        })
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        // Never start a runtime inside a runtime thread.
        std::thread::scope(|s| s.spawn(work).join().ok().flatten())
    } else {
        work()
    }
}

/// KV-cache estimate: the tier whose limit the parameter count fits under; `None` above
/// the largest tier (children must be small models).
pub fn kv_bytes_for(params_b: Option<f64>, ctx_tokens: u64, cfg: &ParallelConfig) -> Option<f64> {
    let tiers = cfg.kv_tiers();
    if tiers.is_empty() {
        return None;
    }
    for (limit, per_1k) in tiers {
        if params_b.is_none_or(|p| p <= limit) {
            return Some(per_1k * (ctx_tokens as f64 / 1000.0));
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct CostBreakdown {
    pub params_b: Option<f64>,
    pub weights_bytes: f64,
    pub kv_bytes: f64,
    pub runtime_bytes: f64,
    pub process_bytes: f64,
    pub total_bytes: f64,
    pub ctx: u64,
}

/// Why a child model was refused by [`estimate_child_cost`].
#[derive(Debug, Clone, PartialEq)]
pub struct CostRejection {
    pub reason: &'static str,
    pub params_b: Option<f64>,
}

/// Memory cost of one child, bytes. `loaded_size` (from `/api/ps`) beats the estimate.
pub fn estimate_child_cost(
    child_model: &str,
    cfg: &ParallelConfig,
    loaded_size: Option<f64>,
) -> Result<(f64, CostBreakdown), CostRejection> {
    let params_b = parse_params_b(child_model);
    let weights = match loaded_size.filter(|s| *s > 0.0) {
        Some(s) => s,
        None => {
            let Some(p) = params_b else {
                return Err(CostRejection {
                    reason: "child_model_unparsable",
                    params_b: None,
                });
            };
            if p > cfg.max_child_params_b {
                return Err(CostRejection {
                    reason: "child_model_too_large",
                    params_b,
                });
            }
            p * cfg.bytes_per_param
        }
    };
    if params_b.is_some_and(|p| p > cfg.max_child_params_b) {
        return Err(CostRejection {
            reason: "child_model_too_large",
            params_b,
        });
    }
    let Some(kv) = kv_bytes_for(params_b, cfg.child_ctx, cfg) else {
        return Err(CostRejection {
            reason: "kv_table_missing_for_params",
            params_b,
        });
    };
    let total = weights * cfg.weight_overhead
        + kv
        + cfg.runtime_overhead_bytes
        + cfg.process_overhead_bytes;
    Ok((
        total,
        CostBreakdown {
            params_b,
            weights_bytes: weights,
            kv_bytes: kv,
            runtime_bytes: cfg.runtime_overhead_bytes,
            process_bytes: cfg.process_overhead_bytes,
            total_bytes: total,
            ctx: cfg.child_ctx,
        },
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub total_ram: f64,
    pub available_ram: f64,
    pub loaded: Vec<LoadedModel>,
    pub models_ok: bool,
}

/// The capacity plan. `max_parallel == 0` always comes with human-readable reasons.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CapacityPlan {
    pub max_parallel: usize,
    pub per_child_cost_bytes: Option<f64>,
    pub breakdown: Option<CostBreakdown>,
    pub reasons: Vec<String>,
    pub snapshot: Option<Snapshot>,
}

impl CapacityPlan {
    pub fn none(reason: impl Into<String>) -> Self {
        Self {
            reasons: vec![reason.into()],
            ..Default::default()
        }
    }
}

/// Anything that can say how many children fit (the probe, or a test stub).
pub trait CapacityPlanner: Send + Sync {
    fn plan_capacity(&self, child_model: &str, cfg: &ParallelConfig) -> CapacityPlan;
}

type MemReader = Box<dyn Fn() -> Option<(f64, f64)> + Send + Sync>;
type ModelsReader = Box<dyn Fn() -> Option<Vec<LoadedModel>> + Send + Sync>;

/// `ResourceProbe`: the machine probe with injectable readers.
pub struct ResourceProbe {
    mem: MemReader,
    models: ModelsReader,
}

impl fmt::Debug for ResourceProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceProbe").finish_non_exhaustive()
    }
}

impl ResourceProbe {
    /// Real readers against this machine and the given Ollama host.
    pub fn new(ollama_host: impl Into<String>) -> Self {
        let host = ollama_host.into();
        Self {
            mem: Box::new(read_system_memory),
            models: Box::new(move || read_loaded_models(&host, Duration::from_secs(4))),
        }
    }

    pub fn with_readers(mem: MemReader, models: ModelsReader) -> Self {
        Self { mem, models }
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        let (total, avail) = (self.mem)()?;
        let models = (self.models)();
        Some(Snapshot {
            total_ram: total,
            available_ram: avail,
            models_ok: models.is_some(),
            loaded: models.unwrap_or_default(),
        })
    }
}

impl CapacityPlanner for ResourceProbe {
    fn plan_capacity(&self, child_model: &str, cfg: &ParallelConfig) -> CapacityPlan {
        let Some(snap) = self.snapshot() else {
            return CapacityPlan::none(
                "memory_probe_failed(could not determine physical memory; treated as no dispatch)",
            );
        };
        let mut reasons = Vec::new();
        let available = snap.available_ram;
        let usable = available * cfg.ram_utilization_cap;
        let budget = (usable - cfg.ram_reserve_bytes).max(0.0);
        reasons.push(format!(
            "available={:.1}GB cap={:.2} reserve={:.1}GB budget={:.1}GB",
            available / GIB,
            cfg.ram_utilization_cap,
            cfg.ram_reserve_bytes / GIB,
            budget / GIB
        ));
        let stop = |reasons: Vec<String>, cost: Option<f64>, snap: Snapshot| CapacityPlan {
            max_parallel: 0,
            per_child_cost_bytes: cost,
            breakdown: None,
            reasons,
            snapshot: Some(snap),
        };
        if !snap.models_ok {
            reasons.push(
                "ollama_ps_unreachable(capacity cannot be confirmed; no dispatch)".to_string(),
            );
            return stop(reasons, None, snap);
        }
        // Parent coexistence: swapping models on one instance evicts the parent → thrash.
        let others: Vec<&str> = snap
            .loaded
            .iter()
            .map(|m| m.model.as_str())
            .filter(|m| !m.is_empty() && *m != child_model)
            .collect();
        if !others.is_empty() && !cfg.allow_parent_eviction {
            reasons.push(format!(
                "parent_model_would_be_evicted(loaded: {}; a different tag from the child model, and swapping models on one instance thrashes) -> 0",
                others.iter().take(3).copied().collect::<Vec<_>>().join(", ")
            ));
            return stop(reasons, None, snap);
        }
        let loaded_size = snap
            .loaded
            .iter()
            .find(|m| m.model == child_model)
            .map(|m| m.size);
        let (cost, br) = match estimate_child_cost(child_model, cfg, loaded_size) {
            Ok(x) => x,
            Err(rej) => {
                reasons.push(format!(
                    "child_model_rejected({}, params_b={})",
                    rej.reason,
                    rej.params_b
                        .map(crate::text::fmt_g)
                        .unwrap_or_else(|| "None".to_string())
                ));
                return stop(reasons, None, snap);
            }
        };
        if cost <= 0.0 {
            reasons.push("child_cost_non_positive(configuration error)".to_string());
            return stop(reasons, Some(cost), snap);
        }
        let mut n = (budget / cost).floor() as usize;
        reasons.push(format!(
            "per_child={:.1}GB -> floor({:.1}/{:.1})={n}",
            cost / GIB,
            budget / GIB,
            cost / GIB
        ));
        if n > cfg.hard_cap {
            reasons.push(format!("clamped_to_hard_cap={}", cfg.hard_cap));
            n = cfg.hard_cap;
        }
        if n < 1 {
            reasons.push(
                "no_headroom_for_even_one_child(no room for even one small model)".to_string(),
            );
        }
        CapacityPlan {
            max_parallel: n,
            per_child_cost_bytes: Some(cost),
            breakdown: Some(br),
            reasons,
            snapshot: Some(snap),
        }
    }
}

/// `format_capacity_table`: the human-readable plan for `rufous plan` and the log.
pub fn format_capacity_table(plan: &CapacityPlan, _cfg: &ParallelConfig) -> String {
    let mut lines = vec!["[parallel] capacity plan".to_string()];
    for r in &plan.reasons {
        lines.push(format!("  - {r}"));
    }
    if plan.per_child_cost_bytes.is_some_and(|c| c > 0.0) {
        if let Some(br) = &plan.breakdown {
            lines.push(format!(
                "  - child_ctx={} weights={:.1}GB kv={:.1}GB runtime={:.1}GB proc={:.1}GB",
                br.ctx,
                br.weights_bytes / GIB,
                br.kv_bytes / GIB,
                br.runtime_bytes / GIB,
                br.process_bytes / GIB
            ));
        }
    }
    lines.push(format!("  => max_parallel = {}", plan.max_parallel));
    lines.join("\n")
}

/// Lenient `/api/ps` parsing shared with the Ollama client tests.
pub fn parse_ps(v: &Value) -> Vec<LoadedModel> {
    crate::ollama::parse_ps(v)
}
