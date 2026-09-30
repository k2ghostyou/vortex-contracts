# Resource Budget Ceilings — Satellite Contracts

**Issue:** [#149](https://github.com/vortex-protocol/vortex-contracts/issues/149)
**Contracts:** `solver_registry` · `proof_registry` · `reputation_badge`
**Status:** Ceilings defined; regenerate numbers after first test run.

See [`docs/149-intent-settlement.md`](./149-intent-settlement.md) for the
`intent_settlement` contract and the full ceiling methodology.

---

## How to regenerate

Run each satellite's bench suite and read the `CEILING_HINT` output:

```bash
# solver_registry
cd solver_registry
cargo test --features testutils bench -- --nocapture 2>&1 | grep CEILING_HINT

# proof_registry
cd ../proof_registry
cargo test --features testutils bench -- --nocapture 2>&1 | grep CEILING_HINT

# reputation_badge
cd ../reputation_badge
cargo test --features testutils bench -- --nocapture 2>&1 | grep CEILING_HINT
```

Copy the `cpu=` and `mem=` values from each line into the matching `CEIL_*`
constant at the top of the relevant `src/bench.rs` **and** into the tables
below, then commit.

---

## 1. `solver_registry`

**Harness:** `solver_registry/src/bench.rs`

### Worst-case fixtures

- `register_solver` bonds at the Platinum-tier minimum (50 000 USDC) so the
  tier-table walk visits all 5 rows.
- `slash` operates on a Platinum solver so the slash amount is non-trivial.
- `get_tier_table` always iterates all 5 rows.

### 1.1 Per-entrypoint ceilings

> `_regenerate_` = populate from the first `CEILING_HINT` run.

| Entrypoint | CPU (measured) | CPU ceiling | Mem (measured) | Mem ceiling |
|---|--:|--:|--:|--:|
| `set_writer` | _regenerate_ | 150,000 | _regenerate_ | 25,000 |
| `set_tier_threshold` | _regenerate_ | 200,000 | _regenerate_ | 30,000 |
| `register_solver` (Platinum bond) | _regenerate_ | 420,000 | _regenerate_ | 65,000 |
| `stake` | _regenerate_ | 380,000 | _regenerate_ | 58,000 |
| `unstake` | _regenerate_ | 380,000 | _regenerate_ | 58,000 |
| `deregister_solver` | _regenerate_ | 400,000 | _regenerate_ | 62,000 |
| `record_fill` | _regenerate_ | 280,000 | _regenerate_ | 42,000 |
| `record_failure` | _regenerate_ | 260,000 | _regenerate_ | 40,000 |
| `slash` | _regenerate_ | 420,000 | _regenerate_ | 62,000 |
| `get_tier` | _regenerate_ | 180,000 | _regenerate_ | 28,000 |
| `tier_for` (Platinum) | _regenerate_ | 130,000 | _regenerate_ | 22,000 |
| `get_reputation_score` | _regenerate_ | 150,000 | _regenerate_ | 25,000 |
| `get_solver` | _regenerate_ | 130,000 | _regenerate_ | 22,000 |
| `get_solver_count` | _regenerate_ | 100,000 | _regenerate_ | 18,000 |
| `get_tier_table` (5 rows) | _regenerate_ | 160,000 | _regenerate_ | 28,000 |

---

## 2. `proof_registry`

**Harness:** `proof_registry/src/bench.rs`

### Worst-case fixtures

- `receive_message` exercises the full Wormhole VAA verification path: mock
  Guardian-signature check, emitter-allowlist lookup, 102-byte payload decode,
  and two replay-guard writes.
- `get_fresh_proof` is called at `received_at + PROOF_VALIDITY_WINDOW - 1`
  (just inside the freshness window) to exercise the timestamp arithmetic.

### 2.1 Per-entrypoint ceilings

| Entrypoint | CPU (measured) | CPU ceiling | Mem (measured) | Mem ceiling |
|---|--:|--:|--:|--:|
| `set_authorized_emitter` | _regenerate_ | 150,000 | _regenerate_ | 25,000 |
| `remove_authorized_emitter` | _regenerate_ | 130,000 | _regenerate_ | 22,000 |
| `get_authorized_emitter` | _regenerate_ | 100,000 | _regenerate_ | 18,000 |
| `get_wormhole_core` | _regenerate_ | 100,000 | _regenerate_ | 18,000 |
| `receive_message` (Wormhole VAA) | _regenerate_ | 600,000 | _regenerate_ | 90,000 |
| `get_proof` | _regenerate_ | 120,000 | _regenerate_ | 22,000 |
| `has_proof` | _regenerate_ | 100,000 | _regenerate_ | 18,000 |
| `get_fresh_proof` (near boundary) | _regenerate_ | 130,000 | _regenerate_ | 24,000 |

---

## 3. `reputation_badge`

**Harness:** `reputation_badge/src/bench.rs`

### Worst-case fixtures

- `mint_badge` (overwrite) — solver already has a Bronze badge and is upgraded
  to Platinum.  The write always occurs; the overwrite path is the worst case.
- `burn_badge` — solver has an existing badge (passes the `has` check and
  deletes the entry).
- `get_badge` (present) — forces a persistent storage read rather than a
  storage-miss early return.

### 3.1 Per-entrypoint ceilings

| Entrypoint | CPU (measured) | CPU ceiling | Mem (measured) | Mem ceiling |
|---|--:|--:|--:|--:|
| `mint_badge` (initial) | _regenerate_ | 180,000 | _regenerate_ | 28,000 |
| `mint_badge` (overwrite) | _regenerate_ | 180,000 | _regenerate_ | 28,000 |
| `burn_badge` | _regenerate_ | 160,000 | _regenerate_ | 25,000 |
| `get_badge` (present) | _regenerate_ | 120,000 | _regenerate_ | 20,000 |
| `get_badge` (absent) | _regenerate_ | 120,000 | _regenerate_ | 20,000 |

---

## 4. Ceiling failure playbook

If CI fails with, e.g.:

```
slash: CPU 435000 > ceiling 420000 — update CEIL_SLASH_CPU ...
```

1. Pull the branch and run:
   ```bash
   cd solver_registry
   cargo test --features testutils bench -- --nocapture 2>&1 | grep CEILING_HINT
   ```
2. Is the regression expected (new feature path) or unexpected (accidental)?
3. **Expected:** update `CEIL_SLASH_CPU` in `solver_registry/src/bench.rs` to
   the new `CEILING_HINT` value, update the table above, commit.
4. **Unexpected:** fix the regression before merging.

---

## 5. Toolchain / SDK version

Numbers were captured with **`soroban-sdk 21.7.7`** on stable Rust.
Regenerate after any SDK or toolchain bump.

---

*Maintained by the Vortex Protocol contributors. See also
[`docs/149-intent-settlement.md`](./149-intent-settlement.md).*
