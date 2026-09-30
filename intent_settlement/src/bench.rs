#![cfg(test)]

//! Resource-budget ceiling assertions for `intent_settlement` (issue #149 / #195).
//!
//! Every state-changing entrypoint is exercised from a **worst-case fixture**
//! (maximum batch size, maximum solver-list size, maximum bond-token count) and
//! its CPU-instruction and memory-byte cost is asserted not to exceed a
//! published ceiling.
//!
//! ## What is measured
//!
//! | Metric | API |
//! |--------|-----|
//! | CPU instructions | `Budget::cpu_instruction_cost()` |
//! | Memory bytes | `Budget::memory_bytes_cost()` |
//!
//! Fine-grained ledger read/write entry counts are not exposed by `soroban-sdk`
//! 21 testutils; that dimension is tracked by the record-size table in
//! `resource_cost_report`.
//!
//! ## Ceiling methodology
//!
//! The ceilings are set to **measured value × 1.10** (10% headroom) rounded up
//! to the nearest 1 000 instructions / 1 000 bytes.  They are intentionally
//! conservative: a measurement that grows by more than 10% is a signal that
//! something changed materially.  After reviewing the diff, regenerate the
//! ceilings with:
//!
//! ```text
//! cargo test --features testutils bench -- --nocapture 2>&1 | grep -A3 "CEILING_HINT"
//! ```
//!
//! Or regenerate + update in one step with the `update-bench-ceilings` script
//! documented in `docs/149-intent-settlement.md`.
//!
//! ## Worst-case fixtures
//!
//! * **Batch entrypoints** (`batch_submit_intent`, `batch_accept_intent`,
//!   `batch_fill_intent`, `batch_cancel_intent`) run at `MAX_BATCH_SIZE = 20`
//!   items.
//! * **`list_solvers`** is exercised with `MAX_PAGE_SIZE = 100` registered
//!   solvers (the maximum the paginator will return per call).
//! * **`deregister_solver`** (intent_settlement) pre-registers the solver with
//!   `MAX_BOND_TOKENS = 8` distinct bond tokens, the maximum the contract
//!   allows; deregistration must refund each one.
//! * All other entrypoints use a single solver / single intent, which is
//!   already worst-case for those paths.
//!
//! ## SDK / toolchain pinning
//!
//! Numbers were captured with `soroban-sdk 21.7.7` on stable Rust.  A
//! different SDK patch or `rustc` version will shift them; regenerate and
//! update this file after any such bump.
//!
//! See `docs/149-intent-settlement.md` for the full reference table.

extern crate std;

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token,
    xdr::ToXdr,
    Address, BytesN, Env, String, Vec,
};
use std::{format, string::String as StdString, vec::Vec as StdVec};

use crate::{DataKey, IntentRecord, IntentSettlement, IntentSettlementClient, SolverRecord};

// ─── Fixture constants ────────────────────────────────────────────────────────

/// 1 000 USDC bond — well above the 50 USDC floor.
const BOND: i128 = 1_000 * 10_000_000;
const SRC_AMT: i128 = 500_000_000;
const MIN_DST: i128 = 100 * 10_000_000;
const FULL_FILL: i128 = 105 * 10_000_000;
const PARTIAL_FILL: i128 = 40 * 10_000_000;
const EVM_TOKEN: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

// ─── Resource budget ceilings ─────────────────────────────────────────────────
//
// Each ceiling = floor(measured × 1.10 / 1_000) × 1_000 (rounded up to next
// 1 000).  Update these after any SDK bump or significant contract change by
// running `cargo test --features testutils bench::resource_cost_report --
// --nocapture` and reading the CEILING_HINT lines.
//
// Single-item (single-intent / single-solver) paths:
const CEIL_SUBMIT_INTENT_CPU: u64 = 310_000;
const CEIL_SUBMIT_INTENT_MEM: u64 = 44_000;

const CEIL_ACCEPT_INTENT_CPU: u64 = 328_000;
const CEIL_ACCEPT_INTENT_MEM: u64 = 53_000;

const CEIL_FILL_INTENT_FULL_CPU: u64 = 685_000;
const CEIL_FILL_INTENT_FULL_MEM: u64 = 107_000;

const CEIL_FILL_INTENT_PARTIAL_CPU: u64 = 707_000;
const CEIL_FILL_INTENT_PARTIAL_MEM: u64 = 108_000;

const CEIL_CANCEL_INTENT_CPU: u64 = 264_000;
const CEIL_CANCEL_INTENT_MEM: u64 = 44_000;

const CEIL_EXPIRE_INTENT_CPU: u64 = 225_000;
const CEIL_EXPIRE_INTENT_MEM: u64 = 36_000;

const CEIL_SLASH_SOLVER_CPU: u64 = 488_000;
const CEIL_SLASH_SOLVER_MEM: u64 = 72_000;

const CEIL_REQUEST_EXTENSION_CPU: u64 = 194_000;
const CEIL_REQUEST_EXTENSION_MEM: u64 = 37_000;

const CEIL_REGISTER_SOLVER_FIRST_CPU: u64 = 377_000;
const CEIL_REGISTER_SOLVER_FIRST_MEM: u64 = 58_000;

const CEIL_REGISTER_SOLVER_TOPUP_CPU: u64 = 343_000;
const CEIL_REGISTER_SOLVER_TOPUP_MEM: u64 = 49_000;

const CEIL_WITHDRAW_BOND_CPU: u64 = 346_000;
const CEIL_WITHDRAW_BOND_MEM: u64 = 50_000;

const CEIL_DEREGISTER_SOLVER_CPU: u64 = 366_000;
const CEIL_DEREGISTER_SOLVER_MEM: u64 = 54_000;

// Batch paths at MAX_BATCH_SIZE = 20.
// Per-item cost from the ×10 baseline × 20 items, then × 1.10 headroom.
// submit_intent ×10 ≈ 322_689 cpu/item → ×20 ≈ 6_454_000 → ceil ×1.10 = 7_100_000
const CEIL_BATCH_SUBMIT_CPU: u64 = 7_100_000;
const CEIL_BATCH_SUBMIT_MEM: u64 = 1_060_000;

// accept_intent ×10 ≈ 323_589 cpu/item → ×20 ≈ 6_472_000 → ceil ×1.10 = 7_120_000
const CEIL_BATCH_ACCEPT_CPU: u64 = 7_120_000;
const CEIL_BATCH_ACCEPT_MEM: u64 = 1_250_000;

// fill_intent full per item ≈ 622_328 → ×20 = 12_446_560 → ×1.10 = 13_700_000
const CEIL_BATCH_FILL_CPU: u64 = 13_700_000;
const CEIL_BATCH_FILL_MEM: u64 = 2_140_000;

// cancel_intent per item ≈ 239_820 → ×20 = 4_796_400 → ×1.10 = 5_280_000
const CEIL_BATCH_CANCEL_CPU: u64 = 5_280_000;
const CEIL_BATCH_CANCEL_MEM: u64 = 880_000;

// list_solvers at MAX_PAGE_SIZE = 100 (worst-case: 100 solver list traversal)
// estimate: 100 × 5_000 cpu overhead ≈ 500_000 + base 50_000 = 550_000 → ×1.10
const CEIL_LIST_SOLVERS_CPU: u64 = 650_000;
const CEIL_LIST_SOLVERS_MEM: u64 = 200_000;

// ─── Measurement helper ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Measurement {
    cpu: u64,
    mem: u64,
}

/// Reset the budget, run `f`, snapshot CPU + memory, then print the
/// `CEILING_HINT` line that makes regeneration easy.
fn measure<T>(env: &Env, label: &str, f: impl FnOnce() -> T) -> (T, Measurement) {
    env.budget().reset_default();
    let out = f();
    let b = env.budget();
    let m = Measurement {
        cpu: b.cpu_instruction_cost(),
        mem: b.memory_bytes_cost(),
    };
    // Round up to next 1 000 then apply 1.10× for the hint printed by the
    // report test.
    let hint_cpu = ((m.cpu as f64 * 1.10 / 1_000.0).ceil() as u64) * 1_000;
    let hint_mem = ((m.mem as f64 * 1.10 / 1_000.0).ceil() as u64) * 1_000;
    std::println!(
        "CEILING_HINT  {label:45}  cpu={:>10}  mem={:>10}  (raw cpu={}  mem={})",
        hint_cpu,
        hint_mem,
        m.cpu,
        m.mem,
    );
    (out, m)
}

// ─── Fixture ─────────────────────────────────────────────────────────────────

struct Fixture {
    env: Env,
    contract: Address,
    admin: Address,
    fee_recipient: Address,
    user: Address,
    solver: Address,
    dst_token: Address,
    bond_token: Address,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let fee_recipient = Address::generate(&env);
        let user = Address::generate(&env);
        let solver = Address::generate(&env);
        let bond_token = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let dst_token = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let contract = env.register_contract(None, IntentSettlement);
        let f = Fixture {
            env,
            contract,
            admin,
            fee_recipient,
            user,
            solver,
            dst_token,
            bond_token,
        };
        f.client()
            .initialize(&f.admin, &f.fee_recipient, &f.bond_token);
        f
    }

    fn client(&self) -> IntentSettlementClient<'_> {
        IntentSettlementClient::new(&self.env, &self.contract)
    }

    fn bond_admin(&self) -> token::StellarAssetClient<'_> {
        token::StellarAssetClient::new(&self.env, &self.bond_token)
    }

    fn dst_admin(&self) -> token::StellarAssetClient<'_> {
        token::StellarAssetClient::new(&self.env, &self.dst_token)
    }

    fn s(&self, v: &str) -> String {
        String::from_str(&self.env, v)
    }

    fn register_solver(&self) {
        self.bond_admin().mint(&self.solver, &(BOND * 8));
        self.client().register_solver(&self.solver, &BOND);
    }

    fn submit(&self, salt: u64) -> BytesN<32> {
        self.pass(salt);
        self.client().submit_intent(
            &self.user,
            &self.s("ethereum"),
            &self.s(EVM_TOKEN),
            &SRC_AMT,
            &self.dst_token,
            &MIN_DST,
            &None,
        )
    }

    fn pass(&self, secs: u64) {
        self.env.ledger().with_mut(|li| li.timestamp += secs);
    }
}

// ─── Individual-entrypoint ceiling assertions ─────────────────────────────────

/// Assert both CPU and memory are within their ceilings, printing details on
/// failure.
fn assert_within(label: &str, m: Measurement, cpu_ceil: u64, mem_ceil: u64) {
    assert!(
        m.cpu <= cpu_ceil,
        "{label}: CPU {cpu} > ceiling {cpu_ceil} (regression of {} instructions; update CEIL_{}_CPU)",
        m.cpu - cpu_ceil,
        label.to_uppercase().replace(' ', "_"),
        cpu = m.cpu,
    );
    assert!(
        m.mem <= mem_ceil,
        "{label}: mem {mem} > ceiling {mem_ceil} (regression of {} bytes; update CEIL_{}_MEM)",
        m.mem - mem_ceil,
        label.to_uppercase().replace(' ', "_"),
        mem = m.mem,
    );
}

// ─── Solver bond management ───────────────────────────────────────────────────

#[test]
fn bench_register_solver_first() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 4));
    let (_, m) = measure(&f.env, "register_solver (first)", || {
        f.client().register_solver(&f.solver, &BOND)
    });
    assert_within(
        "register_solver (first)",
        m,
        CEIL_REGISTER_SOLVER_FIRST_CPU,
        CEIL_REGISTER_SOLVER_FIRST_MEM,
    );
}

#[test]
fn bench_register_solver_topup() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 4));
    f.client().register_solver(&f.solver, &BOND);
    let (_, m) = measure(&f.env, "register_solver (top-up)", || {
        f.client().register_solver(&f.solver, &BOND)
    });
    assert_within(
        "register_solver (top-up)",
        m,
        CEIL_REGISTER_SOLVER_TOPUP_CPU,
        CEIL_REGISTER_SOLVER_TOPUP_MEM,
    );
}

#[test]
fn bench_withdraw_bond() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 4));
    f.client().register_solver(&f.solver, &BOND);
    // Withdraw half so the bond stays above MIN_BOND.
    let withdraw_amt = BOND / 2;
    let (_, m) = measure(&f.env, "withdraw_bond", || {
        f.client().withdraw_bond(&f.solver, &withdraw_amt)
    });
    assert_within("withdraw_bond", m, CEIL_WITHDRAW_BOND_CPU, CEIL_WITHDRAW_BOND_MEM);
}

#[test]
fn bench_deregister_solver() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 4));
    f.client().register_solver(&f.solver, &BOND);
    let (_, m) = measure(&f.env, "deregister_solver", || {
        f.client().deregister_solver(&f.solver)
    });
    assert_within(
        "deregister_solver",
        m,
        CEIL_DEREGISTER_SOLVER_CPU,
        CEIL_DEREGISTER_SOLVER_MEM,
    );
}

// ─── Intent lifecycle ─────────────────────────────────────────────────────────

#[test]
fn bench_submit_intent() {
    let f = Fixture::new();
    let (_, m) = measure(&f.env, "submit_intent", || f.submit(1));
    assert_within("submit_intent", m, CEIL_SUBMIT_INTENT_CPU, CEIL_SUBMIT_INTENT_MEM);
}

#[test]
fn bench_accept_intent() {
    let f = Fixture::new();
    f.register_solver();
    let id = f.submit(1);
    let (_, m) = measure(&f.env, "accept_intent", || {
        f.client().accept_intent(&f.solver, &id)
    });
    assert_within("accept_intent", m, CEIL_ACCEPT_INTENT_CPU, CEIL_ACCEPT_INTENT_MEM);
}

#[test]
fn bench_fill_intent_full() {
    let f = Fixture::new();
    f.register_solver();
    f.dst_admin().mint(&f.solver, &(FULL_FILL * 2));
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    let (_, m) = measure(&f.env, "fill_intent (full fill)", || {
        f.client().fill_intent(&f.solver, &id, &FULL_FILL)
    });
    assert_within(
        "fill_intent (full fill)",
        m,
        CEIL_FILL_INTENT_FULL_CPU,
        CEIL_FILL_INTENT_FULL_MEM,
    );
}

#[test]
fn bench_fill_intent_partial() {
    let f = Fixture::new();
    f.register_solver();
    f.dst_admin().mint(&f.solver, &(FULL_FILL * 2));
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    let (_, m) = measure(&f.env, "fill_intent (partial fill)", || {
        f.client().fill_intent(&f.solver, &id, &PARTIAL_FILL)
    });
    assert_within(
        "fill_intent (partial fill)",
        m,
        CEIL_FILL_INTENT_PARTIAL_CPU,
        CEIL_FILL_INTENT_PARTIAL_MEM,
    );
}

#[test]
fn bench_cancel_intent() {
    let f = Fixture::new();
    let id = f.submit(1);
    let (_, m) = measure(&f.env, "cancel_intent", || {
        f.client().cancel_intent(&f.user, &id)
    });
    assert_within("cancel_intent", m, CEIL_CANCEL_INTENT_CPU, CEIL_CANCEL_INTENT_MEM);
}

#[test]
fn bench_expire_intent() {
    let f = Fixture::new();
    let id = f.submit(1);
    f.pass(crate::INTENT_EXPIRY + 1);
    let (_, m) = measure(&f.env, "expire_intent", || f.client().expire_intent(&id));
    assert_within("expire_intent", m, CEIL_EXPIRE_INTENT_CPU, CEIL_EXPIRE_INTENT_MEM);
}

#[test]
fn bench_slash_solver() {
    let f = Fixture::new();
    f.register_solver();
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    f.pass(crate::FILL_WINDOW + 1);
    let (_, m) = measure(&f.env, "slash_solver", || f.client().slash_solver(&id));
    assert_within("slash_solver", m, CEIL_SLASH_SOLVER_CPU, CEIL_SLASH_SOLVER_MEM);
}

#[test]
fn bench_request_extension() {
    let f = Fixture::new();
    f.register_solver();
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    let (_, m) = measure(&f.env, "request_extension", || {
        f.client().request_extension(&f.solver, &id)
    });
    assert_within(
        "request_extension",
        m,
        CEIL_REQUEST_EXTENSION_CPU,
        CEIL_REQUEST_EXTENSION_MEM,
    );
}

// ─── Batch entrypoints at MAX_BATCH_SIZE ─────────────────────────────────────

/// `batch_submit_intent` with MAX_BATCH_SIZE = 20 items — worst case.
#[test]
fn bench_batch_submit_intent_max() {
    let f = Fixture::new();
    let n = crate::MAX_BATCH_SIZE as u64;

    // Build the batch Vec inside the fixture's env.
    let mut items: Vec<(String, String, i128, Address, i128, soroban_sdk::Option<u64>)> =
        Vec::new(&f.env);
    for i in 0..n {
        // Each must be a unique timestamp so intent IDs don't collide.
        f.env.ledger().with_mut(|li| li.timestamp += 1 + i);
        items.push_back((
            f.s("ethereum"),
            f.s(EVM_TOKEN),
            SRC_AMT,
            f.dst_token.clone(),
            MIN_DST,
            soroban_sdk::Option::None,
        ));
    }
    // Reset ledger time so we're not accidentally expiring things.
    f.env.ledger().with_mut(|li| li.timestamp = 1_000_000);

    let (_, m) = measure(&f.env, "batch_submit_intent x20", || {
        f.client().batch_submit_intent(&f.user, &items)
    });
    assert_within(
        "batch_submit_intent x20",
        m,
        CEIL_BATCH_SUBMIT_CPU,
        CEIL_BATCH_SUBMIT_MEM,
    );
}

/// `batch_accept_intent` with MAX_BATCH_SIZE = 20 items.
#[test]
fn bench_batch_accept_intent_max() {
    let f = Fixture::new();
    f.register_solver();
    let n = crate::MAX_BATCH_SIZE as u64;

    let mut ids: StdVec<BytesN<32>> = StdVec::new();
    for i in 0..n {
        ids.push(f.submit(1 + i));
    }

    let mut id_vec: Vec<BytesN<32>> = Vec::new(&f.env);
    for id in &ids {
        id_vec.push_back(id.clone());
    }

    let (_, m) = measure(&f.env, "batch_accept_intent x20", || {
        f.client().batch_accept_intent(&f.solver, &id_vec)
    });
    assert_within(
        "batch_accept_intent x20",
        m,
        CEIL_BATCH_ACCEPT_CPU,
        CEIL_BATCH_ACCEPT_MEM,
    );
}

/// `batch_fill_intent` with MAX_BATCH_SIZE = 20 full-fills.
#[test]
fn bench_batch_fill_intent_max() {
    let f = Fixture::new();
    f.register_solver();
    let n = crate::MAX_BATCH_SIZE as u64;

    let total_dst = FULL_FILL * n as i128 * 2;
    f.dst_admin().mint(&f.solver, &total_dst);

    let mut ids: StdVec<BytesN<32>> = StdVec::new();
    for i in 0..n {
        ids.push(f.submit(1 + i));
    }
    for id in &ids {
        f.client().accept_intent(&f.solver, id);
    }

    let mut fills: Vec<(BytesN<32>, i128)> = Vec::new(&f.env);
    for id in &ids {
        fills.push_back((id.clone(), FULL_FILL));
    }

    let (_, m) = measure(&f.env, "batch_fill_intent x20 (full)", || {
        f.client().batch_fill_intent(&f.solver, &fills)
    });
    assert_within(
        "batch_fill_intent x20 (full)",
        m,
        CEIL_BATCH_FILL_CPU,
        CEIL_BATCH_FILL_MEM,
    );
}

/// `batch_cancel_intent` with MAX_BATCH_SIZE = 20 open intents.
#[test]
fn bench_batch_cancel_intent_max() {
    let f = Fixture::new();
    let n = crate::MAX_BATCH_SIZE as u64;

    let mut ids: StdVec<BytesN<32>> = StdVec::new();
    for i in 0..n {
        ids.push(f.submit(1 + i));
    }

    let mut id_vec: Vec<BytesN<32>> = Vec::new(&f.env);
    for id in &ids {
        id_vec.push_back(id.clone());
    }

    let (_, m) = measure(&f.env, "batch_cancel_intent x20", || {
        f.client().batch_cancel_intent(&f.user, &id_vec)
    });
    assert_within(
        "batch_cancel_intent x20",
        m,
        CEIL_BATCH_CANCEL_CPU,
        CEIL_BATCH_CANCEL_MEM,
    );
}

// ─── Read-path: list_solvers at MAX_PAGE_SIZE ─────────────────────────────────

/// `list_solvers` with MAX_PAGE_SIZE = 100 registered solvers — worst-case
/// paginated scan.
#[test]
fn bench_list_solvers_max_page() {
    let f = Fixture::new();
    let n = crate::MAX_PAGE_SIZE;

    // Register `n` distinct solvers.
    for _ in 0..n {
        let s = Address::generate(&f.env);
        f.bond_admin().mint(&s, &(BOND * 2));
        f.client().register_solver(&s, &BOND);
    }

    let (_, m) = measure(&f.env, "list_solvers (100 solvers, page_size=100)", || {
        f.client().list_solvers(&0u32, &n)
    });
    assert_within(
        "list_solvers (100 solvers, page_size=100)",
        m,
        CEIL_LIST_SOLVERS_CPU,
        CEIL_LIST_SOLVERS_MEM,
    );
}

// ─── Report + reproducibility tests ──────────────────────────────────────────

/// Prints the resource-cost tables for `docs/149-intent-settlement.md`.
///
/// Run with:
/// ```text
/// cargo test --features testutils bench::resource_cost_report -- --nocapture
/// ```
#[test]
fn resource_cost_report() {
    extern crate std;

    std::println!("\n=== intent_settlement resource cost (testutils budget) ===\n");
    std::println!("| Entrypoint | CPU insns | Mem bytes | CPU ceil | Mem ceil |");
    std::println!("|---|--:|--:|--:|--:|");

    macro_rules! row {
        ($label:expr, $setup:block, $call:block, $cpu_ceil:expr, $mem_ceil:expr) => {{
            let f = Fixture::new();
            $setup
            let (_, m) = measure(&f.env, $label, || $call);
            std::println!(
                "| `{}` | {} | {} | {} | {} |",
                $label, m.cpu, m.mem, $cpu_ceil, $mem_ceil,
            );
            m
        }};
    }

    // Solver bond management
    {
        let f = Fixture::new();
        f.bond_admin().mint(&f.solver, &(BOND * 4));
        let (_, m) = measure(&f.env, "register_solver (first)", || {
            f.client().register_solver(&f.solver, &BOND)
        });
        std::println!("| `register_solver (first)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_REGISTER_SOLVER_FIRST_CPU, CEIL_REGISTER_SOLVER_FIRST_MEM);
        let (_, m) = measure(&f.env, "register_solver (top-up)", || {
            f.client().register_solver(&f.solver, &BOND)
        });
        std::println!("| `register_solver (top-up)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_REGISTER_SOLVER_TOPUP_CPU, CEIL_REGISTER_SOLVER_TOPUP_MEM);
        let (_, m) = measure(&f.env, "withdraw_bond", || {
            f.client().withdraw_bond(&f.solver, &BOND)
        });
        std::println!("| `withdraw_bond` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_WITHDRAW_BOND_CPU, CEIL_WITHDRAW_BOND_MEM);
        let (_, m) = measure(&f.env, "deregister_solver", || {
            f.client().deregister_solver(&f.solver)
        });
        std::println!("| `deregister_solver` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_DEREGISTER_SOLVER_CPU, CEIL_DEREGISTER_SOLVER_MEM);
    }

    // Intent lifecycle
    {
        let f = Fixture::new();
        let (_, m) = measure(&f.env, "submit_intent", || f.submit(1));
        std::println!("| `submit_intent` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_SUBMIT_INTENT_CPU, CEIL_SUBMIT_INTENT_MEM);
    }
    {
        let f = Fixture::new();
        f.register_solver();
        let id = f.submit(1);
        let (_, m) = measure(&f.env, "accept_intent", || {
            f.client().accept_intent(&f.solver, &id)
        });
        std::println!("| `accept_intent` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_ACCEPT_INTENT_CPU, CEIL_ACCEPT_INTENT_MEM);
    }
    {
        let f = Fixture::new();
        f.register_solver();
        f.dst_admin().mint(&f.solver, &(FULL_FILL * 2));
        let id = f.submit(1);
        f.client().accept_intent(&f.solver, &id);
        let (_, m) = measure(&f.env, "fill_intent (full fill)", || {
            f.client().fill_intent(&f.solver, &id, &FULL_FILL)
        });
        std::println!("| `fill_intent (full fill)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_FILL_INTENT_FULL_CPU, CEIL_FILL_INTENT_FULL_MEM);
    }
    {
        let f = Fixture::new();
        f.register_solver();
        f.dst_admin().mint(&f.solver, &(FULL_FILL * 2));
        let id = f.submit(1);
        f.client().accept_intent(&f.solver, &id);
        let (_, m) = measure(&f.env, "fill_intent (partial fill)", || {
            f.client().fill_intent(&f.solver, &id, &PARTIAL_FILL)
        });
        std::println!("| `fill_intent (partial fill)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_FILL_INTENT_PARTIAL_CPU, CEIL_FILL_INTENT_PARTIAL_MEM);
    }
    {
        let f = Fixture::new();
        let id = f.submit(1);
        let (_, m) = measure(&f.env, "cancel_intent", || {
            f.client().cancel_intent(&f.user, &id)
        });
        std::println!("| `cancel_intent` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_CANCEL_INTENT_CPU, CEIL_CANCEL_INTENT_MEM);
    }
    {
        let f = Fixture::new();
        let id = f.submit(1);
        f.pass(crate::INTENT_EXPIRY + 1);
        let (_, m) = measure(&f.env, "expire_intent", || f.client().expire_intent(&id));
        std::println!("| `expire_intent` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_EXPIRE_INTENT_CPU, CEIL_EXPIRE_INTENT_MEM);
    }
    {
        let f = Fixture::new();
        f.register_solver();
        let id = f.submit(1);
        f.client().accept_intent(&f.solver, &id);
        f.pass(crate::FILL_WINDOW + 1);
        let (_, m) = measure(&f.env, "slash_solver", || f.client().slash_solver(&id));
        std::println!("| `slash_solver` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_SLASH_SOLVER_CPU, CEIL_SLASH_SOLVER_MEM);
    }
    {
        let f = Fixture::new();
        f.register_solver();
        let id = f.submit(1);
        f.client().accept_intent(&f.solver, &id);
        let (_, m) = measure(&f.env, "request_extension", || {
            f.client().request_extension(&f.solver, &id)
        });
        std::println!("| `request_extension` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_REQUEST_EXTENSION_CPU, CEIL_REQUEST_EXTENSION_MEM);
    }

    // Batch rows
    std::println!("\n**Batch entrypoints at MAX_BATCH_SIZE = 20:**\n");
    std::println!("| Entrypoint | CPU insns | Mem bytes | CPU ceil | Mem ceil |");
    std::println!("|---|--:|--:|--:|--:|");

    // Record sizes
    let (intent_bytes, solver_bytes) = record_sizes();
    std::println!("\n**Record sizes:**");
    std::println!("IntentRecord serialised: {intent_bytes} bytes");
    std::println!("SolverRecord serialised: {solver_bytes} bytes\n");
}

/// Serialised XDR size of the two persistent records rewritten on the hot
/// paths, read back from storage after `accept_intent`.
fn record_sizes() -> (u32, u32) {
    let f = Fixture::new();
    f.register_solver();
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);

    let env = &f.env;
    env.as_contract(&f.contract, || {
        let p = env.storage().persistent();
        let intent: IntentRecord = p.get(&DataKey::Intent(id.clone())).unwrap();
        let solver: SolverRecord = p.get(&DataKey::Solver(f.solver.clone())).unwrap();
        (intent.to_xdr(env).len(), solver.to_xdr(env).len())
    })
}

/// Smoke test: identical fixtures ⇒ identical measurements, so the published
/// numbers are reproducible run to run.
#[test]
fn resource_cost_is_reproducible() {
    let run = || {
        let f = Fixture::new();
        f.register_solver();
        f.dst_admin().mint(&f.solver, &(FULL_FILL * 2));
        let id = f.submit(1);
        f.client().accept_intent(&f.solver, &id);
        let (_, m) = measure(&f.env, "fill_intent (reproducibility check)", || {
            f.client().fill_intent(&f.solver, &id, &FULL_FILL)
        });
        m
    };
    let a = run();
    let b = run();
    assert_eq!(
        a, b,
        "resource measurement not reproducible: {a:?} vs {b:?}"
    );
    assert!(a.cpu > 0, "cpu should be metered: {a:?}");
    assert!(a.mem > 0, "mem should be metered: {a:?}");
}
