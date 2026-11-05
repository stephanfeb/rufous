//! The claims hold (bead boubou-bcs) over recorded turns: reads the JSON that
//! `bench/claims_replay.py` writes (one row per turn: the instruction, the final reply and the
//! turn's tool records) and prints what the Rust detector decides for each turn that wrote.
//!
//! `cargo run -p rufous-kernel --example claims_replay -- rows.json`

use rufous_kernel::exec::ToolStatus;
use rufous_kernel::guards::integrity::{claim_backing, outcome_claim, ClaimBacking, CommandRecord};
use serde_json::Value;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: claims_replay <rows.json>");
    let rows: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    let (mut holds, mut backed, mut quiet) = (0, 0, 0);
    for r in &rows {
        if r["how"] == "none" {
            continue;
        }
        let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
        let cmds: Vec<CommandRecord> = r["raw"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| {
                let res = a["result"].as_str().unwrap_or("");
                if a["tool"] == "run_bash" {
                    CommandRecord::from_result(a["command"].as_str().unwrap_or(""), res)
                } else {
                    CommandRecord::file_write(
                        a["tool"].as_str().unwrap_or(""),
                        a["path"].as_str().unwrap_or(""),
                        ToolStatus::of_text(res) == ToolStatus::Ok,
                    )
                }
            })
            .collect();
        let Some(claim) = outcome_claim(&s("final_full")) else {
            quiet += 1;
            continue;
        };
        match claim_backing(&cmds, &s("instr_full")) {
            ClaimBacking::NoWrite => quiet += 1,
            ClaimBacking::Backed(by) => {
                backed += 1;
                println!("BACKED {} \"{claim}\" by {by}", s("s"));
            }
            ClaimBacking::Unbacked(why) => {
                holds += 1;
                println!("HOLD   {} \"{claim}\" ({why})", s("s"));
            }
        }
    }
    println!("holds {holds}, backed {backed}, no claim or no write {quiet}");
}
