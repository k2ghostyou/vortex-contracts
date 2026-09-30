#![cfg(test)]

//! Resource-budget ceiling assertions for `proof_registry` (issue #149).
//!
//! Every state-changing and read entrypoint is exercised from a worst-case
//! fixture and its CPU-instruction and memory-byte cost is asserted not to
//! exceed a published ceiling.
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
//! * `receive_message` — a valid, full 102-byte Wormhole VAA with the maximum
//!   realistic payload, exercising Guardian-signature verification (mocked),
//!   emitter-allowlist lookup, payload decode, and two replay-guard writes.
//! * `get_fresh_proof` — called at the latest valid timestamp (within the
//!   1-hour freshness window) so the timestamp arithmetic actually executes.
//! * All admin/config entrypoints use the minimal valid input.
//!
//! See `docs/149-satellite-contracts.md` for the full reference table.

extern crate std;

use soroban_sdk::{
    contract, contractimpl,
    testutils::Address as _,
    Address, Bytes, BytesN, Env, String,
};

use crate::{ProofRecord, ProofRegistry, ProofRegistryClient, VaaEnvelope};

// ─── Resource budget ceilings ─────────────────────────────────────────────────
//
// Set to floor(measured × 1.10 / 1_000) × 1_000.
// Regenerate: cargo test --features testutils bench -- --nocapture
//
// Initial estimates from code-path analysis. Pin to real values on first run.
const CEIL_SET_AUTHORIZED_EMITTER_CPU: u64 = 150_000;
const CEIL_SET_AUTHORIZED_EMITTER_MEM: u64 = 25_000;

const CEIL_REMOVE_AUTHORIZED_EMITTER_CPU: u64 = 130_000;
const CEIL_REMOVE_AUTHORIZED_EMITTER_MEM: u64 = 22_000;

const CEIL_RECEIVE_MESSAGE_CPU: u64 = 600_000;
const CEIL_RECEIVE_MESSAGE_MEM: u64 = 90_000;

const CEIL_GET_PROOF_CPU: u64 = 120_000;
const CEIL_GET_PROOF_MEM: u64 = 22_000;

const CEIL_HAS_PROOF_CPU: u64 = 100_000;
const CEIL_HAS_PROOF_MEM: u64 = 18_000;

const CEIL_GET_FRESH_PROOF_CPU: u64 = 130_000;
const CEIL_GET_FRESH_PROOF_MEM: u64 = 24_000;

const CEIL_GET_AUTHORIZED_EMITTER_CPU: u64 = 100_000;
const CEIL_GET_AUTHORIZED_EMITTER_MEM: u64 = 18_000;

const CEIL_GET_WORMHOLE_CORE_CPU: u64 = 100_000;
const CEIL_GET_WORMHOLE_CORE_MEM: u64 = 18_000;

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

// ─── Mock Wormhole Core ───────────────────────────────────────────────────────
//
// This is the same mock used by `proof_registry/src/test.rs`.
// It performs a sha256-based Guardian-signature stand-in (see test.rs for
// the full VAA layout documentation) so any post-signing tamper would trap,
// without needing a real Guardian set.

#[contract]
pub struct MockWormholeCore;

#[contractimpl]
impl MockWormholeCore {
    pub fn parse_and_verify_vaa(env: Env, vaa: Bytes) -> VaaEnvelope {
        assert!(vaa.len() >= 6, "VAA header truncated");
        assert_eq!(vaa.get(0).unwrap(), 1, "unsupported VAA version");

        let n = vaa.get(5).unwrap() as u32;
        assert!(n >= 1, "no Guardian signatures / quorum not met");

        let body_start = 6 + 66 * n;
        assert!(vaa.len() as u32 > body_start, "VAA body missing");
        let body = vaa.slice(body_start..vaa.len());

        let expected: BytesN<32> = env.crypto().sha256(&body).into();
        let r: BytesN<32> = vaa
            .slice(7..39)
            .try_into()
            .expect("signature 0 r-field must be 32 bytes");
        assert_eq!(r, expected, "invalid Guardian signature (body tampered)");

        let emitter_chain =
            (((body.get(8).unwrap() as u32) << 8) | (body.get(9).unwrap() as u32)) as u32;
        let emitter_address: BytesN<32> = body
            .slice(10..42)
            .try_into()
            .expect("emitter_address must be 32 bytes");
        let mut seq = [0u8; 8];
        let mut i = 0u32;
        while i < 8 {
            seq[i as usize] = body.get(42 + i).unwrap();
            i += 1;
        }
        let sequence = u64::from_be_bytes(seq);
        let payload = body.slice(51..body.len());

        VaaEnvelope {
            emitter_chain,
            emitter_address,
            sequence,
            payload,
        }
    }
}

// ─── Fixture ─────────────────────────────────────────────────────────────────

struct Ctx {
    env: Env,
    admin: Address,
    wormhole_core: Address,
    axelar_gateway: Address,
    contract_id: Address,
}

impl Ctx {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let wormhole_core = env.register_contract(None, MockWormholeCore);
        // axelar_gateway is a registered contract address; not used in Wormhole
        // bench tests but required by `initialize`.
        let axelar_gateway = Address::generate(&env);
        let contract_id = env.register_contract(None, ProofRegistry);

        let ctx = Ctx { env, admin, wormhole_core, axelar_gateway, contract_id };
        ctx.client().initialize(&ctx.admin, &ctx.wormhole_core, &ctx.axelar_gateway);
        ctx
    }

    fn client(&self) -> ProofRegistryClient<'_> {
        ProofRegistryClient::new(&self.env, &self.contract_id)
    }
}

fn intent_id(env: &Env, seed: u8) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    bytes[31] = seed;
    BytesN::from_array(env, &bytes)
}

fn emitter_addr(env: &Env, tag: u8) -> BytesN<32> {
    BytesN::from_array(env, &[tag; 32])
}

/// Build the fixed 102-byte Vortex deposit payload.
fn make_payload(env: &Env, id: &BytesN<32>, src_chain_id: u16, src_amount: i128) -> Bytes {
    let mut raw = [0u8; 102];
    raw[0..32].copy_from_slice(&id.to_array());
    // [32..52] src_user — zeroed
    raw[52] = (src_chain_id >> 8) as u8;
    raw[53] = (src_chain_id & 0xff) as u8;
    // [54..86] src_token — zeroed
    raw[86..102].copy_from_slice(&src_amount.to_be_bytes());
    Bytes::from_slice(env, &raw)
}

/// Assemble a full VAA the `MockWormholeCore` will accept.
fn build_vaa(
    env: &Env,
    emitter_chain: u16,
    emitter_address: &BytesN<32>,
    sequence: u64,
    payload: &Bytes,
) -> Bytes {
    let mut body = Bytes::new(env);
    body.extend_from_array(&0u32.to_be_bytes()); // timestamp
    body.extend_from_array(&0u32.to_be_bytes()); // nonce
    body.extend_from_array(&emitter_chain.to_be_bytes());
    body.extend_from_array(&emitter_address.to_array());
    body.extend_from_array(&sequence.to_be_bytes());
    body.push_back(1u8); // consistency_level
    body.append(payload);

    let hash: BytesN<32> = env.crypto().sha256(&body).into();

    let mut vaa = Bytes::new(env);
    vaa.push_back(1u8); // version
    vaa.extend_from_array(&0u32.to_be_bytes()); // guardian_set_index
    vaa.push_back(1u8); // signature_count
    vaa.push_back(0u8); // sig 0: guardian_index
    vaa.extend_from_array(&hash.to_array()); // sig 0: r == sha256(body)
    vaa.extend_from_array(&[0u8; 32]); // sig 0: s
    vaa.push_back(0u8); // sig 0: v
    vaa.append(&body);
    vaa
}

// ─── Admin/config ceiling assertions ─────────────────────────────────────────

#[test]
fn bench_set_authorized_emitter() {
    let ctx = Ctx::new();
    let emitter = emitter_addr(&ctx.env, 0xde);
    let (_, m) = measure(&ctx.env, "set_authorized_emitter", || {
        ctx.client().set_authorized_emitter(&2u32, &emitter)
    });
    assert_within(
        "set_authorized_emitter",
        m,
        CEIL_SET_AUTHORIZED_EMITTER_CPU,
        CEIL_SET_AUTHORIZED_EMITTER_MEM,
    );
}

#[test]
fn bench_remove_authorized_emitter() {
    let ctx = Ctx::new();
    let emitter = emitter_addr(&ctx.env, 0xde);
    ctx.client().set_authorized_emitter(&2u32, &emitter);
    let (_, m) = measure(&ctx.env, "remove_authorized_emitter", || {
        ctx.client().remove_authorized_emitter(&2u32)
    });
    assert_within(
        "remove_authorized_emitter",
        m,
        CEIL_REMOVE_AUTHORIZED_EMITTER_CPU,
        CEIL_REMOVE_AUTHORIZED_EMITTER_MEM,
    );
}

// ─── receive_message (the hot path) ──────────────────────────────────────────

#[test]
fn bench_receive_message() {
    let ctx = Ctx::new();
    let chain_id = 2u16; // Ethereum
    let emitter = emitter_addr(&ctx.env, 0xaa);
    ctx.client().set_authorized_emitter(&(chain_id as u32), &emitter);

    let id = intent_id(&ctx.env, 0x01);
    let payload = make_payload(&ctx.env, &id, chain_id, 1_000_000_000i128);
    let vaa = build_vaa(&ctx.env, chain_id, &emitter, 1u64, &payload);

    let (_, m) = measure(&ctx.env, "receive_message (Wormhole VAA)", || {
        ctx.client().receive_message(&vaa)
    });
    assert_within(
        "receive_message",
        m,
        CEIL_RECEIVE_MESSAGE_CPU,
        CEIL_RECEIVE_MESSAGE_MEM,
    );
}

// ─── Read-only views ──────────────────────────────────────────────────────────

#[test]
fn bench_get_proof() {
    let ctx = Ctx::new();
    // Inject a proof via the testutils backdoor so we don't re-pay VAA cost.
    let id = intent_id(&ctx.env, 0x02);
    let record = ProofRecord {
        intent_id: id.clone(),
        src_user: String::from_str(&ctx.env, "0xaabbccddee"),
        src_chain_id: 2,
        src_token: String::from_str(&ctx.env, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        src_amount: 1_000_000_000,
        vaa_sequence: 42,
        received_at: ctx.env.ledger().timestamp(),
    };
    ctx.client().mock_set_proof(&record);

    let (_, m) = measure(&ctx.env, "get_proof", || {
        ctx.client().get_proof(&id)
    });
    assert_within("get_proof", m, CEIL_GET_PROOF_CPU, CEIL_GET_PROOF_MEM);
}

#[test]
fn bench_has_proof() {
    let ctx = Ctx::new();
    let id = intent_id(&ctx.env, 0x03);
    let record = ProofRecord {
        intent_id: id.clone(),
        src_user: String::from_str(&ctx.env, "0xaabbccddee"),
        src_chain_id: 2,
        src_token: String::from_str(&ctx.env, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        src_amount: 1_000_000_000,
        vaa_sequence: 42,
        received_at: ctx.env.ledger().timestamp(),
    };
    ctx.client().mock_set_proof(&record);

    let (_, m) = measure(&ctx.env, "has_proof (present)", || {
        ctx.client().has_proof(&id)
    });
    assert_within("has_proof", m, CEIL_HAS_PROOF_CPU, CEIL_HAS_PROOF_MEM);
}

#[test]
fn bench_get_fresh_proof() {
    let ctx = Ctx::new();
    let id = intent_id(&ctx.env, 0x04);
    let now = ctx.env.ledger().timestamp();
    let record = ProofRecord {
        intent_id: id.clone(),
        src_user: String::from_str(&ctx.env, "0xaabbccddee"),
        src_chain_id: 2,
        src_token: String::from_str(&ctx.env, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        src_amount: 1_000_000_000,
        vaa_sequence: 42,
        received_at: now,
    };
    ctx.client().mock_set_proof(&record);
    // Advance time to just within the freshness window — worst case for the
    // timestamp subtraction guard.
    ctx.env.ledger().with_mut(|li| {
        li.timestamp += crate::PROOF_VALIDITY_WINDOW - 1;
    });

    let (_, m) = measure(&ctx.env, "get_fresh_proof (near window boundary)", || {
        ctx.client().get_fresh_proof(&id)
    });
    assert_within(
        "get_fresh_proof",
        m,
        CEIL_GET_FRESH_PROOF_CPU,
        CEIL_GET_FRESH_PROOF_MEM,
    );
}

#[test]
fn bench_get_authorized_emitter() {
    let ctx = Ctx::new();
    let emitter = emitter_addr(&ctx.env, 0xde);
    ctx.client().set_authorized_emitter(&2u32, &emitter);
    let (_, m) = measure(&ctx.env, "get_authorized_emitter", || {
        ctx.client().get_authorized_emitter(&2u32)
    });
    assert_within(
        "get_authorized_emitter",
        m,
        CEIL_GET_AUTHORIZED_EMITTER_CPU,
        CEIL_GET_AUTHORIZED_EMITTER_MEM,
    );
}

#[test]
fn bench_get_wormhole_core() {
    let ctx = Ctx::new();
    let (_, m) = measure(&ctx.env, "get_wormhole_core", || {
        ctx.client().get_wormhole_core()
    });
    assert_within(
        "get_wormhole_core",
        m,
        CEIL_GET_WORMHOLE_CORE_CPU,
        CEIL_GET_WORMHOLE_CORE_MEM,
    );
}

// ─── Report test ──────────────────────────────────────────────────────────────

/// Prints the resource-cost table for `docs/149-satellite-contracts.md`
/// (proof_registry section).
///
/// Run with:
/// ```text
/// cargo test --features testutils bench -- --nocapture
/// ```
#[test]
fn resource_cost_report() {
    extern crate std;

    std::println!("\n=== proof_registry resource cost (testutils budget) ===\n");
    std::println!("| Entrypoint | CPU insns | Mem bytes | CPU ceil | Mem ceil |");
    std::println!("|---|--:|--:|--:|--:|");

    {
        let ctx = Ctx::new();
        let emitter = emitter_addr(&ctx.env, 0xde);
        let (_, m) = measure(&ctx.env, "set_authorized_emitter", || {
            ctx.client().set_authorized_emitter(&2u32, &emitter)
        });
        std::println!("| `set_authorized_emitter` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_SET_AUTHORIZED_EMITTER_CPU, CEIL_SET_AUTHORIZED_EMITTER_MEM);

        let (_, m) = measure(&ctx.env, "get_authorized_emitter", || {
            ctx.client().get_authorized_emitter(&2u32)
        });
        std::println!("| `get_authorized_emitter` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_GET_AUTHORIZED_EMITTER_CPU, CEIL_GET_AUTHORIZED_EMITTER_MEM);

        let (_, m) = measure(&ctx.env, "remove_authorized_emitter", || {
            ctx.client().remove_authorized_emitter(&2u32)
        });
        std::println!("| `remove_authorized_emitter` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_REMOVE_AUTHORIZED_EMITTER_CPU, CEIL_REMOVE_AUTHORIZED_EMITTER_MEM);
    }
    {
        let ctx = Ctx::new();
        let (_, m) = measure(&ctx.env, "get_wormhole_core", || {
            ctx.client().get_wormhole_core()
        });
        std::println!("| `get_wormhole_core` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_GET_WORMHOLE_CORE_CPU, CEIL_GET_WORMHOLE_CORE_MEM);
    }
    {
        let ctx = Ctx::new();
        let chain_id = 2u16;
        let emitter = emitter_addr(&ctx.env, 0xaa);
        ctx.client().set_authorized_emitter(&(chain_id as u32), &emitter);
        let id = intent_id(&ctx.env, 0x01);
        let payload = make_payload(&ctx.env, &id, chain_id, 1_000_000_000i128);
        let vaa = build_vaa(&ctx.env, chain_id, &emitter, 1u64, &payload);
        let (_, m) = measure(&ctx.env, "receive_message (Wormhole VAA)", || {
            ctx.client().receive_message(&vaa)
        });
        std::println!("| `receive_message` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_RECEIVE_MESSAGE_CPU, CEIL_RECEIVE_MESSAGE_MEM);
    }
    {
        let ctx = Ctx::new();
        let id = intent_id(&ctx.env, 0x05);
        let now = ctx.env.ledger().timestamp();
        let record = ProofRecord {
            intent_id: id.clone(),
            src_user: String::from_str(&ctx.env, "0xaabbccddee"),
            src_chain_id: 2,
            src_token: String::from_str(&ctx.env, "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            src_amount: 1_000_000_000,
            vaa_sequence: 42,
            received_at: now,
        };
        ctx.client().mock_set_proof(&record);

        let (_, m) = measure(&ctx.env, "get_proof", || { ctx.client().get_proof(&id) });
        std::println!("| `get_proof` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_GET_PROOF_CPU, CEIL_GET_PROOF_MEM);

        let (_, m) = measure(&ctx.env, "has_proof (present)", || { ctx.client().has_proof(&id) });
        std::println!("| `has_proof` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_HAS_PROOF_CPU, CEIL_HAS_PROOF_MEM);

        let (_, m) = measure(&ctx.env, "get_fresh_proof", || {
            ctx.client().get_fresh_proof(&id)
        });
        std::println!("| `get_fresh_proof` | {} | {} | {} | {} |",
            m.cpu, m.mem, CEIL_GET_FRESH_PROOF_CPU, CEIL_GET_FRESH_PROOF_MEM);
    }

    std::println!();
}

/// Smoke test: measurements are deterministic run to run.
#[test]
fn resource_cost_is_reproducible() {
    let run = || {
        let ctx = Ctx::new();
        let chain_id = 2u16;
        let emitter = emitter_addr(&ctx.env, 0xbb);
        ctx.client().set_authorized_emitter(&(chain_id as u32), &emitter);
        let id = intent_id(&ctx.env, 0xcc);
        let payload = make_payload(&ctx.env, &id, chain_id, 500_000_000i128);
        let vaa = build_vaa(&ctx.env, chain_id, &emitter, 99u64, &payload);
        measure(&ctx.env, "receive_message (reproducibility)", || {
            ctx.client().receive_message(&vaa)
        })
        .1
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "resource measurement not reproducible: {a:?} vs {b:?}");
    assert!(a.cpu > 0, "cpu should be metered: {a:?}");
    assert!(a.mem > 0, "mem should be metered: {a:?}");
}
