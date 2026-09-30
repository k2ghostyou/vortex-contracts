#![cfg(test)]

//! Resource-budget ceiling assertions for `reputation_badge` (issue #149).
//!
//! Every public entrypoint is exercised from a worst-case fixture (all tiers,
//! sequential mint/burn cycles) and its CPU-instruction and memory-byte cost is
//! asserted not to exceed a published ceiling.
//!
//! ## Ceiling methodology
//!
//! Ceilings = **measured value × 1.10** (10% headroom) rounded up to the
//! nearest 1 000.  Regenerate with:
//!
//! ```text
//! cargo test --features testutils bench -- --nocapture
//! ```
//!
//! ## Worst-case fixtures
//!
//! * `mint_badge` — called for a solver that already has a badge (tier
//!   overwrite path), since the write always occurs whether or not the badge
//!   previously existed.
//! * `burn_badge` — solver has an existing badge, so the `has` check passes.
//! * `get_badge` — solver has a badge (triggers a storage read rather than
//!   returning `None` immediately).
//!
//! See `docs/149-satellite-contracts.md` for the full reference table.

extern crate std;

use soroban_sdk::{testutils::Address as _, Address, Env};

use crate::{ReputationBadge, ReputationBadgeClient, Tier};

// ─── Resource budget ceilings ─────────────────────────────────────────────────
//
// Set to floor(measured × 1.10 / 1_000) × 1_000.
// Regenerate: cargo test --features testutils bench -- --nocapture
//
// Initial estimates from code-path analysis. Pin to real values on first run.
const CEIL_MINT_BADGE_CPU: u64 = 180_000;
const CEIL_MINT_BADGE_MEM: u64 = 28_000;

const CEIL_BURN_BADGE_CPU: u64 = 160_000;
const CEIL_BURN_BADGE_MEM: u64 = 25_000;

const CEIL_GET_BADGE_CPU: u64 = 120_000;
const CEIL_GET_BADGE_MEM: u64 = 20_000;

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
    solver: Address,
    contract_id: Address,
}

impl Ctx {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let solver = Address::generate(&env);
        let contract_id = env.register_contract(None, ReputationBadge);

        let ctx = Ctx { env, admin, solver, contract_id };
        ctx.client().initialize(&ctx.admin);
        ctx
    }

    fn client(&self) -> ReputationBadgeClient<'_> {
        ReputationBadgeClient::new(&self.env, &self.contract_id)
    }
}

// ─── Ceiling assertions ───────────────────────────────────────────────────────

/// `mint_badge` — initial mint (no prior badge).
#[test]
fn bench_mint_badge_initial() {
    let ctx = Ctx::new();
    let (_, m) = measure(&ctx.env, "mint_badge (initial, Bronze)", || {
        ctx.client().mint_badge(&ctx.solver, &Tier::Bronze)
    });
    assert_within("mint_badge (initial)", m, CEIL_MINT_BADGE_CPU, CEIL_MINT_BADGE_MEM);
}

/// `mint_badge` — overwrite (tier upgrade, worst case since the write
/// always happens whether or not the solver had a prior badge).
#[test]
fn bench_mint_badge_overwrite() {
    let ctx = Ctx::new();
    ctx.client().mint_badge(&ctx.solver, &Tier::Bronze);
    let (_, m) = measure(&ctx.env, "mint_badge (overwrite → Platinum)", || {
        ctx.client().mint_badge(&ctx.solver, &Tier::Platinum)
    });
    assert_within("mint_badge (overwrite)", m, CEIL_MINT_BADGE_CPU, CEIL_MINT_BADGE_MEM);
}

/// `burn_badge` — solver has an existing badge (happy path; the `has` check
/// passes and the entry is deleted).
#[test]
fn bench_burn_badge() {
    let ctx = Ctx::new();
    ctx.client().mint_badge(&ctx.solver, &Tier::Gold);
    let (_, m) = measure(&ctx.env, "burn_badge (Gold)", || {
        ctx.client().burn_badge(&ctx.solver)
    });
    assert_within("burn_badge", m, CEIL_BURN_BADGE_CPU, CEIL_BURN_BADGE_MEM);
}

/// `get_badge` — solver has a Platinum badge (forces a storage read).
#[test]
fn bench_get_badge_present() {
    let ctx = Ctx::new();
    ctx.client().mint_badge(&ctx.solver, &Tier::Platinum);
    let (_, m) = measure(&ctx.env, "get_badge (Platinum, present)", || {
        ctx.client().get_badge(&ctx.solver)
    });
    assert_within("get_badge (present)", m, CEIL_GET_BADGE_CPU, CEIL_GET_BADGE_MEM);
}

/// `get_badge` — solver has no badge (returns `None`; cheaper storage miss
/// path, but share the same ceiling).
#[test]
fn bench_get_badge_absent() {
    let ctx = Ctx::new();
    let (_, m) = measure(&ctx.env, "get_badge (absent / Unranked)", || {
        ctx.client().get_badge(&ctx.solver)
    });
    // Same ceiling — the absent path is cheaper, so this asserts a looser
    // bound and will always pass if the present-badge test passes.
    assert_within("get_badge (absent)", m, CEIL_GET_BADGE_CPU, CEIL_GET_BADGE_MEM);
}

// ─── Report test ──────────────────────────────────────────────────────────────

/// Prints the resource-cost table for `docs/149-satellite-contracts.md`
/// (reputation_badge section).
///
/// Run with:
/// ```text
/// cargo test --features testutils bench -- --nocapture
/// ```
#[test]
fn resource_cost_report() {
    extern crate std;

    std::println!("\n=== reputation_badge resource cost (testutils budget) ===\n");
    std::println!("| Entrypoint | CPU insns | Mem bytes | CPU ceil | Mem ceil |");
    std::println!("|---|--:|--:|--:|--:|");

    {
        let ctx = Ctx::new();
        let (_, m) = measure(&ctx.env, "mint_badge (initial, Bronze)", || {
            ctx.client().mint_badge(&ctx.solver, &Tier::Bronze)
        });
        std::println!("| `mint_badge (initial)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_MINT_BADGE_CPU, CEIL_MINT_BADGE_MEM);

        let (_, m) = measure(&ctx.env, "mint_badge (overwrite → Platinum)", || {
            ctx.client().mint_badge(&ctx.solver, &Tier::Platinum)
        });
        std::println!("| `mint_badge (overwrite)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_MINT_BADGE_CPU, CEIL_MINT_BADGE_MEM);

        let (_, m) = measure(&ctx.env, "get_badge (Platinum, present)", || {
            ctx.client().get_badge(&ctx.solver)
        });
        std::println!("| `get_badge (present)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_GET_BADGE_CPU, CEIL_GET_BADGE_MEM);

        let (_, m) = measure(&ctx.env, "burn_badge (Platinum)", || {
            ctx.client().burn_badge(&ctx.solver)
        });
        std::println!("| `burn_badge` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_BURN_BADGE_CPU, CEIL_BURN_BADGE_MEM);
    }
    {
        let ctx = Ctx::new();
        let (_, m) = measure(&ctx.env, "get_badge (absent / Unranked)", || {
            ctx.client().get_badge(&ctx.solver)
        });
        std::println!("| `get_badge (absent)` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_GET_BADGE_CPU, CEIL_GET_BADGE_MEM);
    }

    std::println!();
}

/// Smoke test: measurements are deterministic run to run.
#[test]
fn resource_cost_is_reproducible() {
    let run = || {
        let ctx = Ctx::new();
        ctx.client().mint_badge(&ctx.solver, &Tier::Silver);
        measure(&ctx.env, "burn_badge (reproducibility)", || {
            ctx.client().burn_badge(&ctx.solver)
        })
        .1
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "resource measurement not reproducible: {a:?} vs {b:?}");
    assert!(a.cpu > 0, "cpu should be metered: {a:?}");
    assert!(a.mem > 0, "mem should be metered: {a:?}");
}
