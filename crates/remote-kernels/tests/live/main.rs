//! Live lifecycle tests — SPENDS REAL MONEY. Every scenario drives the real
//! binary as a separate process over stdio MCP and checks what happened with
//! the provider's own API, so the same scenarios run on old and new binaries:
//! no internal files, and message wording only where the scenario is about what
//! the agent gets told.
//!
//! ```sh
//! # One scenario, with its step timings. --exact matters: a bare live_l1 would
//! # also run live_l10 to live_l13.
//! cargo test -p remote-kernels --test live -- --ignored --nocapture --exact live_l3
//! # Everything (four at a time is fine: scenarios share nothing):
//! cargo test -p remote-kernels --test live -- --ignored --nocapture --test-threads=4 live_
//! # An older build (L12 needs one to upgrade from):
//! RK_LIVE_BINARY=/path/to/old cargo test ...
//! RK_LIVE_OLD_BINARY=/path/to/released cargo test ... --exact live_l12
//! # After anything: the results table, the balance, a last sweep.
//! cargo test -p remote-kernels --test live -- --ignored --nocapture live_summary
//! cargo test -p remote-kernels --test live -- --ignored --nocapture live_reap
//! ```
//!
//! Keys come from the process environment or the repo's `.env.local`
//! (`RUNPOD_API_KEY`, `VAST_API_KEY`) and reach the server through its
//! environment only. `RK_LIVE_DIR` holds logs, scratch projects, the ledger and
//! results (default: `$TMPDIR/rk-live`). All live runs together stop at
//! `RK_LIVE_SPEND_LIMIT` dollars (default 2.50): the ledger in that directory
//! counts across runs, and scenarios refuse to start, or end their machines,
//! past it.
//!
//! Machines are named `rklive-<run>-<scenario>...`. Each scenario's guard
//! terminates its own prefix on the way out, including on panic, and verifies
//! the provider lists nothing left; `live_reap` does the same for every run
//! recorded in `RK_LIVE_DIR`. Nothing without that prefix is ever touched.

mod harness;
mod mcp;
mod others;
mod providers;
mod proxy;
mod scenarios;

use std::fmt::Write as _;

use serde_json::Value;

use harness::{now_epoch, read_jsonl, run_prefix, spend_limit, total_spend};
use providers::Provider;

macro_rules! live {
    ($($test:ident => $body:path, $why:literal;)*) => {
        $(
            #[test]
            #[ignore = $why]
            fn $test() {
                $body();
            }
        )*
    };
}

live! {
    live_r1 => scenarios::r1, "spends real money on RunPod";
    live_l1 => scenarios::l1, "spends real money on RunPod";
    live_l2 => scenarios::l2, "spends real money on RunPod";
    live_l3 => scenarios::l3, "spends real money on RunPod";
    live_l4 => scenarios::l4, "spends real money on RunPod; creates and deletes a network volume";
    live_l5 => scenarios::l5, "spends real money on RunPod";
    live_l6 => scenarios::l6, "spends real money on RunPod";
    live_l7 => scenarios::l7, "spends real money on RunPod";
    live_l8 => scenarios::l8, "spends real money on RunPod";
    live_l8_create => scenarios::l8_create, "spends real money on RunPod";
    live_l9 => scenarios::l9, "spends real money on RunPod";
    live_l10 => scenarios::l10, "spends real money on RunPod";
    live_l11 => scenarios::l11, "spends real money on RunPod";
    live_l12 => scenarios::l12, "spends real money on RunPod; needs RK_LIVE_OLD_BINARY";
    live_l13 => scenarios::l13, "spends real money on RunPod";
    live_v1 => others::v1, "spends real money on vast.ai";
    live_v2 => others::v2, "spends real money on vast.ai";
    live_v3 => others::v3, "spends real money on vast.ai";
    live_c1 => others::c1, "spends real money on RunPod and uses the Claude subscription";
}

fn recorded_runs() -> Vec<String> {
    let mut runs: Vec<String> = read_jsonl("runs.jsonl")
        .iter()
        .filter_map(|row| row.get("run")?.as_str().map(String::from))
        .collect();
    runs.dedup();
    runs
}

/// Terminate everything carrying a recorded run's prefix (or the one run in
/// `RK_LIVE_RUN`), delete its volumes, and verify the inventory is clean.
/// Machines with any other name are listed for information, and left alone.
#[test]
#[ignore = "reads provider inventories; terminates this harness's own machines"]
fn live_reap() {
    let runs = std::env::var("RK_LIVE_RUN").map_or_else(|_| recorded_runs(), |run| vec![run]);
    let mut left = Vec::new();
    for run in &runs {
        let prefix = run_prefix(run);
        for provider in [Provider::Runpod, Provider::Vast] {
            for machine in providers::list(provider, &prefix).expect("inventory") {
                eprintln!("reaping {}", machine.describe());
                if let Err(error) = providers::destroy(&machine) {
                    eprintln!("destroy failed: {error}");
                }
                left.push(machine);
            }
        }
        for (id, name) in providers::network_volumes(&prefix).expect("volumes") {
            eprintln!("deleting network volume {id} {name:?}");
            providers::delete_network_volume(&id).expect("delete volume");
        }
    }
    if !left.is_empty() {
        std::thread::sleep(std::time::Duration::from_secs(15));
    }
    let mut remaining = Vec::new();
    for run in &runs {
        let prefix = run_prefix(run);
        for provider in [Provider::Runpod, Provider::Vast] {
            remaining.extend(providers::list(provider, &prefix).expect("inventory"));
        }
        let removed = providers::vast_remove_scratch_keys(&harness::live_dir().join(run))
            .expect("vast ssh keys");
        if removed > 0 {
            eprintln!("removed {removed} scratch ssh key(s) from the vast account");
        }
    }
    for provider in [Provider::Runpod, Provider::Vast] {
        let others = providers::list(provider, "").expect("inventory");
        eprintln!(
            "{} inventory now: {:?}",
            provider.name(),
            others
                .iter()
                .map(providers::Machine::describe)
                .collect::<Vec<_>>()
        );
    }
    assert!(
        remaining.is_empty(),
        "machines left: {:?}",
        remaining
            .iter()
            .map(providers::Machine::describe)
            .collect::<Vec<_>>()
    );
    eprintln!(
        "reaped {} machine(s) across {} run(s); none left",
        left.len(),
        runs.len()
    );
}

/// The results table, spend against the limit, and the provider's balances
/// and bills. Spends nothing.
#[test]
#[ignore = "reads provider balances and bills"]
fn live_summary() {
    let mut table = String::from(
        "| scenario | result | time | est. cost | billed | failures |\n|---|---|---|---|---|---|\n",
    );
    for row in read_jsonl("results.jsonl") {
        let text = |key: &str| {
            row.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let num = |key: &str| row.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let billed: f64 = row
            .get("machines")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|machine| machine.get("provider").and_then(Value::as_str) == Some("runpod"))
            .filter_map(|machine| machine.get("id")?.as_str())
            .filter_map(|id| providers::runpod_billed(id).ok())
            .sum();
        let failures = row
            .get("failures")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        let inconclusive = text("inconclusive");
        let _ = writeln!(
            table,
            "| {} | {} | {:.0}s | ${:.3} | ${billed:.3} | {} |",
            text("scenario"),
            text("result"),
            num("secs"),
            num("usd"),
            if inconclusive.is_empty() {
                failures
            } else {
                inconclusive
            },
        );
    }
    eprintln!("{table}");
    eprintln!(
        "ledger total ${:.3} of ${:.2}; RunPod balance ${:?}; vast credit ${:?}; at {}",
        total_spend(),
        spend_limit(),
        providers::runpod_balance().ok(),
        providers::vast_credit().ok(),
        now_epoch(),
    );
}

/// Record spend that happened outside a scenario, so the limit counts it:
/// `RK_LIVE_ADD_SPEND=0.05 RK_LIVE_ADD_WHAT=spikes cargo test ... live_add_spend`.
#[test]
#[ignore = "writes to the spend ledger"]
fn live_add_spend() {
    let usd: f64 = std::env::var("RK_LIVE_ADD_SPEND")
        .expect("RK_LIVE_ADD_SPEND")
        .parse()
        .expect("a dollar amount");
    let what = std::env::var("RK_LIVE_ADD_WHAT").unwrap_or_else(|_| "manual".to_string());
    harness::add_to_ledger(&what, usd);
    eprintln!("ledger total now ${:.3}", total_spend());
}
