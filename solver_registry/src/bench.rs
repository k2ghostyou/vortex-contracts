#![cfg(test)]

//! Resource-budget ceiling assertions for `solver_registry` (issue #149).
//!
//! Every state-changing and read entrypoint is exercised from a worst-case
//! fixture and its CPU-instruction and memory-byte cost is asserted not to
//! exceed a published ceiling.
//!
//! ## Ceiling methodology
//!
//! Ceilings = **measured value × 1.10** (10% headroom) rounded up to the
//! nearest 1 000.  After any SDK bump or significant change to this contract,
//! regenerate with:
//!
//! ```text
//! cargo test --features testutils bench -- --nocapture
//! ```
//!
//! The `CEILING_HINT` lines printed by the report test contain the new values.
//!
//! ## Worst-case fixtures
//!
//! * `register_solver` — bonds at the Platinum-tier minimum (50 000 USDC) so
//!   the tier-table walk always visits all 5 rows.
//! * `slash` — solver bond is large enough that the slash is non-trivial.
//! * `get_tier_table` — always returns all 5 rows.
//! * All other entrypoints operate on a single solver record.
//!
//! See `docs/149-satellite-contracts.md` for the full reference table.

extern crate std;

use soroban_sdk::{
    testutils::Address as _,
    token,
    Address, Env,
};

use crate::{SolverRegistry, SolverRegistryClient, USDC};

// ─── Fixture constants ────────────────────────────────────────────────────────

/// Platinum-tier bond (50 000 USDC × 10^7) — forces the full tier-table walk.
const PLAT_BOND: i128 = 50_000 * USDC;
/// Small increment for top-up / stake operations.
const STAKE_AMT: i128 = 100 * USDC;
/// Unstake amount that leaves bond above the floor.
const UNSTAKE_AMT: i128 = 100 * USDC;

// ─── Resource budget ceilings ─────────────────────────────────────────────────
//
// Set to floor(measured × 1.10 / 1_000) × 1_000.
// Regenerate with: cargo test --features testutils bench -- --nocapture
//
// Initial values are estimates from first-principles analysis of the code
// paths.  Pin to real measurements on first run by reading CEILING_HINT lines.
const CEIL_REGISTER_SOLVER_CPU: u64 = 420_000;
const CEIL_REGISTER_SOLVER_MEM: u64 = 65_000;

const CEIL_STAKE_CPU: u64 = 380_000;
const CEIL_STAKE_MEM: u64 = 58_000;

const CEIL_UNSTAKE_CPU: u64 = 380_000;
const CEIL_UNSTAKE_MEM: u64 = 58_000;

const CEIL_DEREGISTER_SOLVER_CPU: u64 = 400_000;
const CEIL_DEREGISTER_SOLVER_MEM: u64 = 62_000;

const CEIL_SET_WRITER_CPU: u64 = 150_000;
const CEIL_SET_WRITER_MEM: u64 = 25_000;

const CEIL_SET_TIER_THRESHOLD_CPU: u64 = 200_000;
const CEIL_SET_TIER_THRESHOLD_MEM: u64 = 30_000;

const CEIL_RECORD_FILL_CPU: u64 = 280_000;
const CEIL_RECORD_FILL_MEM: u64 = 42_000;

const CEIL_RECORD_FAILURE_CPU: u64 = 260_000;
const CEIL_RECORD_FAILURE_MEM: u64 = 40_000;

const CEIL_SLASH_CPU: u64 = 420_000;
const CEIL_SLASH_MEM: u64 = 62_000;

const CEIL_GET_TIER_CPU: u64 = 180_000;
const CEIL_GET_TIER_MEM: u64 = 28_000;

const CEIL_TIER_FOR_CPU: u64 = 130_000;
const CEIL_TIER_FOR_MEM: u64 = 22_000;

const CEIL_GET_REPUTATION_SCORE_CPU: u64 = 150_000;
const CEIL_GET_REPUTATION_SCORE_MEM: u64 = 25_000;

const CEIL_GET_SOLVER_CPU: u64 = 130_000;
const CEIL_GET_SOLVER_MEM: u64 = 22_000;

const CEIL_GET_SOLVER_COUNT_CPU: u64 = 100_000;
const CEIL_GET_SOLVER_COUNT_MEM: u64 = 18_000;

const CEIL_GET_TIER_TABLE_CPU: u64 = 160_000;
const CEIL_GET_TIER_TABLE_MEM: u64 = 28_000;

// ─── Measurement helper ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Measurement {
    cpu: u64,
    mem: u64,
}

fn measure<T>(env: &Env, label: &str, f: impl FnOnce() -> T) -> (T, Measurement) {
    env.budget().reset_default();
    let out = f();
    let b = env.budget();
    let m = Measurement {
        cpu: b.cpu_instruction_cost(),
        mem: b.memory_bytes_cost(),
    };
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

fn assert_within(label: &str, m: Measurement, cpu_ceil: u64, mem_ceil: u64) {
    assert!(
        m.cpu <= cpu_ceil,
        "{label}: CPU {cpu} > ceiling {cpu_ceil} — update CEIL constant and docs/149-satellite-contracts.md",
        cpu = m.cpu,
    );
    assert!(
        m.mem <= mem_ceil,
        "{label}: mem {mem} > ceiling {mem_ceil} — update CEIL constant and docs/149-satellite-contracts.md",
        mem = m.mem,
    );
}

// ─── Fixture ─────────────────────────────────────────────────────────────────

struct Ctx {
    env: Env,
    admin: Address,
    fee_recipient: Address,
    solver: Address,
    bond_token: Address,
    contract_id: Address,
}

impl Ctx {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let fee_recipient = Address::generate(&env);
        let solver = Address::generate(&env);
        let bond_token = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let contract_id = env.register_contract(None, SolverRegistry);

        let ctx = Ctx { env, admin, fee_recipient, solver, bond_token, contract_id };
        ctx.client().initialize(&ctx.admin, &ctx.bond_token, &ctx.fee_recipient);
        ctx
    }

    fn client(&self) -> SolverRegistryClient<'_> {
        SolverRegistryClient::new(&self.env, &self.contract_id)
    }

    fn bond_sac(&self) -> token::StellarAssetClient<'_> {
        token::StellarAssetClient::new(&self.env, &self.bond_token)
    }

    fn mint(&self, to: &Address, amount: i128) {
        self.bond_sac().mint(to, &amount);
    }

    fn register(&self) {
        self.mint(&self.solver, PLAT_BOND * 2);
        self.client().register_solver(&self.solver, &PLAT_BOND);
    }
}

// ─── Admin / configuration ───────────────────────────────────────────────────

#[test]
fn bench_set_writer() {
    let ctx = Ctx::new();
    let writer = Address::generate(&ctx.env);
    let (_, m) = measure(&ctx.env, "set_writer", || {
        ctx.client().set_writer(&writer)
    });
    assert_within("set_writer", m, CEIL_SET_WRITER_CPU, CEIL_SET_WRITER_MEM);
}

#[test]
fn bench_set_tier_threshold() {
    let ctx = Ctx::new();
    // Tune tier 1 (Bronze) to a valid value that stays monotonically between
    // tier 0 (50 USDC, score 0) and tier 2 (2 000 USDC, score 3 500).
    let new_bond = 600 * USDC;
    let new_score = 1_100u32;
    let (_, m) = measure(&ctx.env, "set_tier_threshold (tier 1)", || {
        ctx.client().set_tier_threshold(&1u32, &new_bond, &new_score)
    });
    assert_within(
        "set_tier_threshold",
        m,
        CEIL_SET_TIER_THRESHOLD_CPU,
        CEIL_SET_TIER_THRESHOLD_MEM,
    );
}

// ─── Solver self-service ──────────────────────────────────────────────────────

#[test]
fn bench_register_solver() {
    let ctx = Ctx::new();
    ctx.mint(&ctx.solver, PLAT_BOND * 2);
    let (_, m) = measure(&ctx.env, "register_solver (Platinum bond)", || {
        ctx.client().register_solver(&ctx.solver, &PLAT_BOND)
    });
    assert_within("register_solver", m, CEIL_REGISTER_SOLVER_CPU, CEIL_REGISTER_SOLVER_MEM);
}

#[test]
fn bench_stake() {
    let ctx = Ctx::new();
    ctx.register();
    ctx.mint(&ctx.solver, STAKE_AMT * 2);
    let (_, m) = measure(&ctx.env, "stake", || {
        ctx.client().stake(&ctx.solver, &STAKE_AMT)
    });
    assert_within("stake", m, CEIL_STAKE_CPU, CEIL_STAKE_MEM);
}

#[test]
fn bench_unstake() {
    let ctx = Ctx::new();
    ctx.register();
    // Leave at least the Platinum minimum after unstake.
    let (_, m) = measure(&ctx.env, "unstake", || {
        ctx.client().unstake(&ctx.solver, &UNSTAKE_AMT)
    });
    assert_within("unstake", m, CEIL_UNSTAKE_CPU, CEIL_UNSTAKE_MEM);
}

#[test]
fn bench_deregister_solver() {
    let ctx = Ctx::new();
    ctx.register();
    let (_, m) = measure(&ctx.env, "deregister_solver", || {
        ctx.client().deregister_solver(&ctx.solver)
    });
    assert_within("deregister_solver", m, CEIL_DEREGISTER_SOLVER_CPU, CEIL_DEREGISTER_SOLVER_MEM);
}

// ─── Settlement write path ────────────────────────────────────────────────────

#[test]
fn bench_record_fill() {
    let ctx = Ctx::new();
    ctx.register();
    // Use admin as caller (acts as writer when no writer is configured).
    let fill_amount = 1_000 * USDC;
    let (_, m) = measure(&ctx.env, "record_fill", || {
        ctx.client().record_fill(&ctx.admin, &ctx.solver, &fill_amount)
    });
    assert_within("record_fill", m, CEIL_RECORD_FILL_CPU, CEIL_RECORD_FILL_MEM);
}

#[test]
fn bench_record_failure() {
    let ctx = Ctx::new();
    ctx.register();
    let (_, m) = measure(&ctx.env, "record_failure", || {
        ctx.client().record_failure(&ctx.admin, &ctx.solver)
    });
    assert_within("record_failure", m, CEIL_RECORD_FAILURE_CPU, CEIL_RECORD_FAILURE_MEM);
}

#[test]
fn bench_slash() {
    let ctx = Ctx::new();
    ctx.register();
    let (_, m) = measure(&ctx.env, "slash", || {
        ctx.client().slash(&ctx.admin, &ctx.solver)
    });
    assert_within("slash", m, CEIL_SLASH_CPU, CEIL_SLASH_MEM);
}

// ─── Read-only views ──────────────────────────────────────────────────────────

#[test]
fn bench_get_tier() {
    let ctx = Ctx::new();
    ctx.register();
    let (_, m) = measure(&ctx.env, "get_tier", || {
        ctx.client().get_tier(&ctx.solver)
    });
    assert_within("get_tier", m, CEIL_GET_TIER_CPU, CEIL_GET_TIER_MEM);
}

#[test]
fn bench_tier_for() {
    let ctx = Ctx::new();
    // Platinum: high score, large bond.
    let (_, m) = measure(&ctx.env, "tier_for (Platinum)", || {
        ctx.client().tier_for(&9_000u32, &PLAT_BOND)
    });
    assert_within("tier_for", m, CEIL_TIER_FOR_CPU, CEIL_TIER_FOR_MEM);
}

#[test]
fn bench_get_reputation_score() {
    let ctx = Ctx::new();
    ctx.register();
    // Give the solver some fill history so the score formula is non-trivial.
    ctx.client().record_fill(&ctx.admin, &ctx.solver, &(500 * USDC));
    ctx.client().record_failure(&ctx.admin, &ctx.solver);
    let (_, m) = measure(&ctx.env, "get_reputation_score", || {
        ctx.client().get_reputation_score(&ctx.solver)
    });
    assert_within(
        "get_reputation_score",
        m,
        CEIL_GET_REPUTATION_SCORE_CPU,
        CEIL_GET_REPUTATION_SCORE_MEM,
    );
}

#[test]
fn bench_get_solver() {
    let ctx = Ctx::new();
    ctx.register();
    let (_, m) = measure(&ctx.env, "get_solver", || {
        ctx.client().get_solver(&ctx.solver)
    });
    assert_within("get_solver", m, CEIL_GET_SOLVER_CPU, CEIL_GET_SOLVER_MEM);
}

#[test]
fn bench_get_solver_count() {
    let ctx = Ctx::new();
    ctx.register();
    let (_, m) = measure(&ctx.env, "get_solver_count", || {
        ctx.client().get_solver_count()
    });
    assert_within("get_solver_count", m, CEIL_GET_SOLVER_COUNT_CPU, CEIL_GET_SOLVER_COUNT_MEM);
}

#[test]
fn bench_get_tier_table() {
    let ctx = Ctx::new();
    let (_, m) = measure(&ctx.env, "get_tier_table (5 rows)", || {
        ctx.client().get_tier_table()
    });
    assert_within("get_tier_table", m, CEIL_GET_TIER_TABLE_CPU, CEIL_GET_TIER_TABLE_MEM);
}

// ─── Report test ──────────────────────────────────────────────────────────────

/// Prints the resource-cost table for `docs/149-satellite-contracts.md`
/// (solver_registry section).
///
/// Run with:
/// ```text
/// cargo test --features testutils bench::resource_cost_report -- --nocapture
/// ```
#[test]
fn resource_cost_report() {
    extern crate std;

    std::println!("\n=== solver_registry resource cost (testutils budget) ===\n");
    std::println!("| Entrypoint | CPU insns | Mem bytes | CPU ceil | Mem ceil |");
    std::println!("|---|--:|--:|--:|--:|");

    macro_rules! report_row {
        ($env:expr, $label:expr, $call:expr, $cpu_c:expr, $mem_c:expr) => {{
            let (_, m) = measure($env, $label, || $call);
            std::println!(
                "| `{}` | {} | {} | {} | {} |",
                $label, m.cpu, m.mem, $cpu_c, $mem_c,
            );
        }};
    }

    {
        let ctx = Ctx::new();
        let writer = Address::generate(&ctx.env);
        report_row!(&ctx.env, "set_writer", ctx.client().set_writer(&writer),
            CEIL_SET_WRITER_CPU, CEIL_SET_WRITER_MEM);
    }
    {
        let ctx = Ctx::new();
        report_row!(&ctx.env, "set_tier_threshold",
            ctx.client().set_tier_threshold(&1u32, &(600 * USDC), &1_100u32),
            CEIL_SET_TIER_THRESHOLD_CPU, CEIL_SET_TIER_THRESHOLD_MEM);
    }
    {
        let ctx = Ctx::new();
        ctx.mint(&ctx.solver, PLAT_BOND * 2);
        report_row!(&ctx.env, "register_solver (Platinum bond)",
            ctx.client().register_solver(&ctx.solver, &PLAT_BOND),
            CEIL_REGISTER_SOLVER_CPU, CEIL_REGISTER_SOLVER_MEM);
        ctx.mint(&ctx.solver, STAKE_AMT * 2);
        report_row!(&ctx.env, "stake",
            ctx.client().stake(&ctx.solver, &STAKE_AMT),
            CEIL_STAKE_CPU, CEIL_STAKE_MEM);
        report_row!(&ctx.env, "unstake",
            ctx.client().unstake(&ctx.solver, &UNSTAKE_AMT),
            CEIL_UNSTAKE_CPU, CEIL_UNSTAKE_MEM);
        report_row!(&ctx.env, "record_fill",
            ctx.client().record_fill(&ctx.admin, &ctx.solver, &(1_000 * USDC)),
            CEIL_RECORD_FILL_CPU, CEIL_RECORD_FILL_MEM);
        report_row!(&ctx.env, "record_failure",
            ctx.client().record_failure(&ctx.admin, &ctx.solver),
            CEIL_RECORD_FAILURE_CPU, CEIL_RECORD_FAILURE_MEM);
        report_row!(&ctx.env, "slash",
            ctx.client().slash(&ctx.admin, &ctx.solver),
            CEIL_SLASH_CPU, CEIL_SLASH_MEM);
        report_row!(&ctx.env, "get_tier",
            ctx.client().get_tier(&ctx.solver),
            CEIL_GET_TIER_CPU, CEIL_GET_TIER_MEM);
        report_row!(&ctx.env, "get_reputation_score",
            ctx.client().get_reputation_score(&ctx.solver),
            CEIL_GET_REPUTATION_SCORE_CPU, CEIL_GET_REPUTATION_SCORE_MEM);
        report_row!(&ctx.env, "get_solver",
            ctx.client().get_solver(&ctx.solver),
            CEIL_GET_SOLVER_CPU, CEIL_GET_SOLVER_MEM);
        report_row!(&ctx.env, "deregister_solver",
            ctx.client().deregister_solver(&ctx.solver),
            CEIL_DEREGISTER_SOLVER_CPU, CEIL_DEREGISTER_SOLVER_MEM);
    }
    {
        let ctx = Ctx::new();
        report_row!(&ctx.env, "tier_for (Platinum)",
            ctx.client().tier_for(&9_000u32, &PLAT_BOND),
            CEIL_TIER_FOR_CPU, CEIL_TIER_FOR_MEM);
        report_row!(&ctx.env, "get_solver_count",
            ctx.client().get_solver_count(),
            CEIL_GET_SOLVER_COUNT_CPU, CEIL_GET_SOLVER_COUNT_MEM);
        report_row!(&ctx.env, "get_tier_table",
            ctx.client().get_tier_table(),
            CEIL_GET_TIER_TABLE_CPU, CEIL_GET_TIER_TABLE_MEM);
    }

    std::println!();
}

/// Smoke test: measurements are deterministic run to run.
#[test]
fn resource_cost_is_reproducible() {
    let run = || {
        let ctx = Ctx::new();
        ctx.register();
        let fill_amount = 1_000 * USDC;
        ctx.client().record_fill(&ctx.admin, &ctx.solver, &fill_amount);
        measure(&ctx.env, "slash (reproducibility)", || {
            ctx.client().slash(&ctx.admin, &ctx.solver)
        })
        .1
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "resource measurement not reproducible: {a:?} vs {b:?}");
    assert!(a.cpu > 0, "cpu should be metered: {a:?}");
    assert!(a.mem > 0, "mem should be metered: {a:?}");
}
