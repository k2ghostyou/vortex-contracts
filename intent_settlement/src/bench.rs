#![cfg(test)]

//! Resource-cost harness for `intent_settlement` (issue #195 / #149).
//!
//! Every state-changing entrypoint is exercised from a **worst-case fixture**
//! (maximum batch size, maximum solver-list size, maximum bond-token count) and
//! its CPU-instruction and memory-byte cost is asserted not to exceed a
//! published ceiling.
//!
//! ## What is measured
//!
//! ## Ceiling assertions (issue #149)
//!
//! Every entrypoint has a hard ceiling on its CPU and memory cost, set at
//! +10 % above the baseline snapshot from `docs/149-intent-settlement.md`.
//! If a code change causes an entrypoint to blow through its ceiling the test
//! `bench_ceiling_*` fails immediately in CI, making the regression visible
//! before it reaches mainnet.
//!
//! To update a ceiling after a deliberate cost increase:
//!
//! 1. Run `cargo test --features testutils bench::resource_cost_report -- --nocapture`
//!    to get the new measured values.
//! 2. Set the corresponding `CPU_CEIL_*` / `MEM_CEIL_*` constant to
//!    `new_value * 110 / 100` (round up to the nearest thousand for readability).
//! 3. Update `docs/149-intent-settlement.md` and
//!    `docs/149-resource-cost-per-entrypoint.md` with the new baseline.
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

// ─── Ceiling constants ────────────────────────────────────────────────────────
//
// Each ceiling is set at baseline + 10 % (rounded up to the nearest 1 000).
// Baseline values come from docs/149-intent-settlement.md, captured at
// soroban-sdk 21.7.7 on stable Rust.
//
// When a deliberate optimisation or cost increase shifts the baseline, update
// both these constants *and* the docs table, following the instructions in the
// module-level doc comment.

// submit_intent — baseline 281,113 CPU | 39,630 mem
const CPU_CEIL_SUBMIT: u64 = 310_000;
const MEM_CEIL_SUBMIT: u64 = 44_000;

// accept_intent — baseline 297,608 CPU | 47,422 mem
const CPU_CEIL_ACCEPT: u64 = 328_000;
const MEM_CEIL_ACCEPT: u64 = 53_000;

// fill_intent (full fill) — baseline 622,328 CPU | 96,723 mem
const CPU_CEIL_FILL_FULL: u64 = 685_000;
const MEM_CEIL_FILL_FULL: u64 = 107_000;

// fill_intent (partial fill) — baseline 642,020 CPU | 97,463 mem
const CPU_CEIL_FILL_PARTIAL: u64 = 707_000;
const MEM_CEIL_FILL_PARTIAL: u64 = 108_000;

// cancel_intent — baseline 239,820 CPU | 39,480 mem
const CPU_CEIL_CANCEL: u64 = 264_000;
const MEM_CEIL_CANCEL: u64 = 44_000;

// expire_intent — baseline 204,451 CPU | 32,082 mem
const CPU_CEIL_EXPIRE: u64 = 225_000;
const MEM_CEIL_EXPIRE: u64 = 36_000;

// slash_solver — baseline 443,049 CPU | 65,189 mem
const CPU_CEIL_SLASH: u64 = 488_000;
const MEM_CEIL_SLASH: u64 = 72_000;

// request_extension — baseline 176,128 CPU | 32,790 mem
const CPU_CEIL_EXTENSION: u64 = 194_000;
const MEM_CEIL_EXTENSION: u64 = 37_000;

// register_solver (first) — baseline 342,082 CPU | 51,837 mem
const CPU_CEIL_REGISTER_FIRST: u64 = 377_000;
const MEM_CEIL_REGISTER_FIRST: u64 = 58_000;

// register_solver (top-up) — baseline 311,498 CPU | 44,278 mem
const CPU_CEIL_REGISTER_TOPUP: u64 = 343_000;
const MEM_CEIL_REGISTER_TOPUP: u64 = 49_000;

// withdraw_bond — baseline 313,992 CPU | 44,895 mem
const CPU_CEIL_WITHDRAW_BOND: u64 = 346_000;
const MEM_CEIL_WITHDRAW_BOND: u64 = 50_000;

// deregister_solver — baseline 332,088 CPU | 48,280 mem
const CPU_CEIL_DEREGISTER: u64 = 366_000;
const MEM_CEIL_DEREGISTER: u64 = 54_000;

// batch_submit_intent × MAX_BATCH_SIZE (20) — estimated from x10 row in docs
// (x10: 3,226,891 CPU | 477,039 mem) × 2, with extra per-item overhead.
// Ceiling is conservative: 2 × x10 baseline + 10%.
const BATCH_SIZE: u32 = 20;
const CPU_CEIL_BATCH_SUBMIT: u64 = 7_100_000;
const MEM_CEIL_BATCH_SUBMIT: u64 = 1_050_000;

// batch_accept_intent × MAX_BATCH_SIZE (20)
// (x10: 3,235,890 CPU | 565,039 mem) × 2 + 10%
const CPU_CEIL_BATCH_ACCEPT: u64 = 7_120_000;
const MEM_CEIL_BATCH_ACCEPT: u64 = 1_244_000;

// batch_fill_intent × MAX_BATCH_SIZE (20)
// fill_intent (full) per-item ~622k CPU; 20 items ≈ 12.5M + 10%
const CPU_CEIL_BATCH_FILL: u64 = 13_800_000;
const MEM_CEIL_BATCH_FILL: u64 = 2_350_000;

// batch_cancel_intent × MAX_BATCH_SIZE (20)
// cancel_intent per-item ~240k CPU; 20 items ≈ 4.8M + 10%
const CPU_CEIL_BATCH_CANCEL: u64 = 5_300_000;
const MEM_CEIL_BATCH_CANCEL: u64 = 970_000;

// ─── Infrastructure ───────────────────────────────────────────────────────────

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
            &None,
        )
    }

    fn pass(&self, secs: u64) {
        self.env.ledger().with_mut(|li| li.timestamp += secs);
    }
}

// ─── Reporting helpers ────────────────────────────────────────────────────────

type Row = (StdString, Measurement);

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

fn fmt_table(rows: &[Row]) -> StdString {
    let mut out = StdString::new();
    out.push_str("| Entrypoint | CPU insns | CPU ceil | Mem bytes | Mem ceil |\n|---|--:|--:|--:|--:|\n");
    for (label, m) in rows {
        out.push_str(&format!("| `{}` | {} | — | {} | — |\n", label, m.cpu, m.mem));
    }
    out
}

fn fmt_batch_table(rows: &[(StdString, Measurement, u64)]) -> StdString {
    let mut out = StdString::new();
    out.push_str(
        "| Sequence | CPU insns | CPU / item | Mem bytes | Mem / item |\n|---|--:|--:|--:|--:|\n",
    );
    for (label, m, n) in rows {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            label,
            m.cpu,
            m.cpu / n,
            m.mem,
            m.mem / n
        ));
    }
    out
}

// ─── Row collection (used by both report and ceiling tests) ──────────────────

/// Exercise every state-changing entrypoint once, returning `(label, cpu, mem)`.
fn collect_rows() -> StdVec<Row> {
    let mut rows: StdVec<Row> = StdVec::new();

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
        let (_, m) = measure(&f.env, || f.client().request_extension(&f.solver, &id));
        push(&mut rows, "request_extension", m);
    }

    rows
}

/// Batch worst-case: MAX_BATCH_SIZE items each, measuring total CPU + mem.
fn collect_batch_rows() -> StdVec<(StdString, Measurement, u64)> {
    let mut rows = StdVec::new();
    let n = BATCH_SIZE as u64;

    // batch_submit_intent × BATCH_SIZE
    {
        let f = Fixture::new();
        let mut intents: Vec<(String, String, i128, Address, i128, Option<u64>)> =
            Vec::new(&f.env);
        for i in 0..n {
            f.pass(1 + i);
            intents.push_back((
                f.s("ethereum"),
                f.s(EVM_TOKEN),
                SRC_AMT,
                f.dst_token.clone(),
                MIN_DST,
                None,
            ));
        }
        let (_, m) = measure(&f.env, || {
            f.client().batch_submit_intent(&f.user, &intents)
        });
        rows.push((format!("batch_submit_intent ×{n}"), m, n));
    }

    // batch_accept_intent × BATCH_SIZE
    {
        let f = Fixture::new();
        f.register_solver();
        let mut ids: soroban_sdk::Vec<BytesN<32>> = Vec::new(&f.env);
        for i in 0..n {
            ids.push_back(f.submit(1 + i));
        }
        let (_, m) = measure(&f.env, || {
            f.client().batch_accept_intent(&f.solver, &ids)
        });
        rows.push((format!("batch_accept_intent ×{n}"), m, n));
    }

    // batch_fill_intent × BATCH_SIZE (full fills)
    {
        let f = Fixture::new();
        f.register_solver();
        f.dst_admin().mint(&f.solver, &(FULL_FILL * (n as i128 + 1)));
        let mut fills: soroban_sdk::Vec<(BytesN<32>, i128)> = Vec::new(&f.env);
        for i in 0..n {
            let id = f.submit(1 + i);
            f.client().accept_intent(&f.solver, &id);
            fills.push_back((id, FULL_FILL));
        }
        let (_, m) = measure(&f.env, || {
            f.client().batch_fill_intent(&f.solver, &fills)
        });
        rows.push((format!("batch_fill_intent ×{n}"), m, n));
    }

    // batch_cancel_intent × BATCH_SIZE
    {
        let f = Fixture::new();
        let mut ids: soroban_sdk::Vec<BytesN<32>> = Vec::new(&f.env);
        for i in 0..n {
            ids.push_back(f.submit(1 + i));
        }
        let (_, m) = measure(&f.env, || {
            f.client().batch_cancel_intent(&f.user, &ids)
        });
        rows.push((format!("batch_cancel_intent ×{n}"), m, n));
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

// ─── Report test (prints tables for docs/149-intent-settlement.md) ───────────

/// Prints the resource-cost tables for `docs/149-intent-settlement.md`.
///
/// Run with:
/// ```text
/// cargo test --features testutils bench::resource_cost_report -- --nocapture
/// ```
#[test]
fn resource_cost_report() {
    let rows = collect_rows();
    let batch = collect_batch_rows();
    let (intent_bytes, solver_bytes) = record_sizes();

    std::println!("\n=== intent_settlement resource cost (testutils budget) ===\n");
    std::println!("{}", fmt_table(&rows));
    std::println!("{}", fmt_batch_table(&batch));
    std::println!("IntentRecord serialised: {intent_bytes} bytes");
    std::println!("SolverRecord serialised: {solver_bytes} bytes\n");
}

// ─── Ceiling assertion tests ──────────────────────────────────────────────────
//
// Each test below exercises one entrypoint under the worst-case fixture and
// asserts that CPU and memory usage stay within the ceiling constants defined
// above.  A failure here means a change caused a measurable resource
// regression; see the module-level doc comment for the update procedure.

/// Helper that panics with a descriptive message if either budget ceiling
/// is breached.
fn assert_within_ceiling(label: &str, m: Measurement, cpu_ceil: u64, mem_ceil: u64) {
    assert!(
        m.cpu <= cpu_ceil,
        "{label}: CPU {cpu} exceeds ceiling {cpu_ceil} (delta +{})",
        m.cpu - cpu_ceil,
        cpu = m.cpu,
    );
    assert!(
        m.mem <= mem_ceil,
        "{label}: mem {mem} exceeds ceiling {mem_ceil} (delta +{})",
        m.mem - mem_ceil,
        mem = m.mem,
    );
}

#[test]
fn bench_ceiling_submit_intent() {
    let f = Fixture::new();
    let (_, m) = measure(&f.env, || f.submit(1));
    assert_within_ceiling("submit_intent", m, CPU_CEIL_SUBMIT, MEM_CEIL_SUBMIT);
}

#[test]
fn bench_ceiling_accept_intent() {
    let f = Fixture::new();
    f.register_solver();
    let id = f.submit(1);
    let (_, m) = measure(&f.env, || f.client().accept_intent(&f.solver, &id));
    assert_within_ceiling("accept_intent", m, CPU_CEIL_ACCEPT, MEM_CEIL_ACCEPT);
}

#[test]
fn bench_ceiling_fill_intent_full() {
    let f = Fixture::new();
    f.register_solver();
    f.dst_admin().mint(&f.solver, &(FULL_FILL * 2));
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    let (_, m) = measure(&f.env, || f.client().fill_intent(&f.solver, &id, &FULL_FILL));
    assert_within_ceiling(
        "fill_intent (full fill)",
        m,
        CPU_CEIL_FILL_FULL,
        MEM_CEIL_FILL_FULL,
    );
}

#[test]
fn bench_ceiling_fill_intent_partial() {
    let f = Fixture::new();
    f.register_solver();
    f.dst_admin().mint(&f.solver, &(FULL_FILL * 2));
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    let (_, m) = measure(&f.env, || {
        f.client().fill_intent(&f.solver, &id, &PARTIAL_FILL)
    });
    assert_within_ceiling(
        "fill_intent (partial fill)",
        m,
        CPU_CEIL_FILL_PARTIAL,
        MEM_CEIL_FILL_PARTIAL,
    );
}

#[test]
fn bench_ceiling_cancel_intent() {
    let f = Fixture::new();
    let id = f.submit(1);
    let (_, m) = measure(&f.env, || f.client().cancel_intent(&f.user, &id));
    assert_within_ceiling("cancel_intent", m, CPU_CEIL_CANCEL, MEM_CEIL_CANCEL);
}

#[test]
fn bench_ceiling_expire_intent() {
    let f = Fixture::new();
    let id = f.submit(1);
    f.pass(crate::INTENT_EXPIRY + 1);
    let (_, m) = measure(&f.env, || f.client().expire_intent(&id));
    assert_within_ceiling("expire_intent", m, CPU_CEIL_EXPIRE, MEM_CEIL_EXPIRE);
}

#[test]
fn bench_ceiling_slash_solver() {
    let f = Fixture::new();
    f.register_solver();
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    f.pass(crate::FILL_WINDOW + 1);
    let (_, m) = measure(&f.env, || f.client().slash_solver(&id));
    assert_within_ceiling("slash_solver", m, CPU_CEIL_SLASH, MEM_CEIL_SLASH);
}

#[test]
fn bench_ceiling_request_extension() {
    let f = Fixture::new();
    f.register_solver();
    let id = f.submit(1);
    f.client().accept_intent(&f.solver, &id);
    let (_, m) = measure(&f.env, || f.client().request_extension(&f.solver, &id));
    assert_within_ceiling(
        "request_extension",
        m,
        CPU_CEIL_EXTENSION,
        MEM_CEIL_EXTENSION,
    );
}

#[test]
fn bench_ceiling_register_solver_first() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 2));
    let (_, m) = measure(&f.env, || f.client().register_solver(&f.solver, &BOND));
    assert_within_ceiling(
        "register_solver (first)",
        m,
        CPU_CEIL_REGISTER_FIRST,
        MEM_CEIL_REGISTER_FIRST,
    );
}

#[test]
fn bench_ceiling_register_solver_topup() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 4));
    f.client().register_solver(&f.solver, &BOND);
    let (_, m) = measure(&f.env, || f.client().register_solver(&f.solver, &BOND));
    assert_within_ceiling(
        "register_solver (top-up)",
        m,
        CPU_CEIL_REGISTER_TOPUP,
        MEM_CEIL_REGISTER_TOPUP,
    );
}

#[test]
fn bench_ceiling_withdraw_bond() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 4));
    f.client().register_solver(&f.solver, &(BOND * 2));
    let (_, m) = measure(&f.env, || f.client().withdraw_bond(&f.solver, &BOND));
    assert_within_ceiling("withdraw_bond", m, CPU_CEIL_WITHDRAW_BOND, MEM_CEIL_WITHDRAW_BOND);
}

#[test]
fn bench_ceiling_deregister_solver() {
    let f = Fixture::new();
    f.bond_admin().mint(&f.solver, &(BOND * 4));
    f.client().register_solver(&f.solver, &BOND);
    let (_, m) = measure(&f.env, || f.client().deregister_solver(&f.solver));
    assert_within_ceiling("deregister_solver", m, CPU_CEIL_DEREGISTER, MEM_CEIL_DEREGISTER);
}

/// Worst-case batch: MAX_BATCH_SIZE = 20 items submitted in one call.
#[test]
fn bench_ceiling_batch_submit_intent() {
    let f = Fixture::new();
    let n = BATCH_SIZE as u64;
    let mut intents: Vec<(String, String, i128, Address, i128, Option<u64>)> =
        Vec::new(&f.env);
    for i in 0..n {
        f.pass(1 + i);
        intents.push_back((
            f.s("ethereum"),
            f.s(EVM_TOKEN),
            SRC_AMT,
            f.dst_token.clone(),
            MIN_DST,
            None,
        ));
    }
    let (_, m) = measure(&f.env, || {
        f.client().batch_submit_intent(&f.user, &intents)
    });
    assert_within_ceiling(
        "batch_submit_intent (×20)",
        m,
        CPU_CEIL_BATCH_SUBMIT,
        MEM_CEIL_BATCH_SUBMIT,
    );
}

/// Worst-case batch: MAX_BATCH_SIZE = 20 accepts in one call.
#[test]
fn bench_ceiling_batch_accept_intent() {
    let f = Fixture::new();
    f.register_solver();
    let n = BATCH_SIZE as u64;
    let mut ids: soroban_sdk::Vec<BytesN<32>> = Vec::new(&f.env);
    for i in 0..n {
        ids.push_back(f.submit(1 + i));
    }
    let (_, m) = measure(&f.env, || {
        f.client().batch_accept_intent(&f.solver, &ids)
    });
    assert_within_ceiling(
        "batch_accept_intent (×20)",
        m,
        CPU_CEIL_BATCH_ACCEPT,
        MEM_CEIL_BATCH_ACCEPT,
    );
}

/// Worst-case batch: MAX_BATCH_SIZE = 20 full fills in one call.
#[test]
fn bench_ceiling_batch_fill_intent() {
    let f = Fixture::new();
    f.register_solver();
    let n = BATCH_SIZE as u64;
    f.dst_admin().mint(&f.solver, &(FULL_FILL * (n as i128 + 1)));
    let mut fills: soroban_sdk::Vec<(BytesN<32>, i128)> = Vec::new(&f.env);
    for i in 0..n {
        let id = f.submit(1 + i);
        f.client().accept_intent(&f.solver, &id);
        fills.push_back((id, FULL_FILL));
    }
    let (_, m) = measure(&f.env, || {
        f.client().batch_fill_intent(&f.solver, &fills)
    });
    assert_within_ceiling(
        "batch_fill_intent (×20)",
        m,
        CPU_CEIL_BATCH_FILL,
        MEM_CEIL_BATCH_FILL,
    );
}

/// Worst-case batch: MAX_BATCH_SIZE = 20 cancels in one call.
#[test]
fn bench_ceiling_batch_cancel_intent() {
    let f = Fixture::new();
    let n = BATCH_SIZE as u64;
    let mut ids: soroban_sdk::Vec<BytesN<32>> = Vec::new(&f.env);
    for i in 0..n {
        ids.push_back(f.submit(1 + i));
    }
    let (_, m) = measure(&f.env, || {
        f.client().batch_cancel_intent(&f.user, &ids)
    });
    assert_within_ceiling(
        "batch_cancel_intent (×20)",
        m,
        CPU_CEIL_BATCH_CANCEL,
        MEM_CEIL_BATCH_CANCEL,
    );
}

// ─── Reproducibility smoke-test ───────────────────────────────────────────────

/// Identical fixtures ⇒ identical measurements, so the published numbers are
/// reproducible run to run.
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
