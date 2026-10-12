---
scope: "cross-boundary-seams"
score: 7.6
status: "MODERATE"
contract_divergences: 3
---
# Cross-Boundary Seams — sequential pass 2026-10-12 (shallow; swarm 429)

## S1 (Moderate) Ledger schema vs sync semantics
- Doc seam: AGENTS.md DEBT_SCHEMA / memory.md header ("Deliberate gaps get ledger lines: - accepted ...") vs src/memory/debt.rs:364-371.
  Header implies only `accepted` lines are durable; `memory lint` accepts plain lines; `sync` deletes them. See M1.

## S2 (Moderate) Parser -> graph resolution seam
- Parser emits call sites as `self.m()` / `recv.m()`; graph resolves them to nothing (see G1; 3651 unresolved).
  Downstream `doctor` dead-code and `blast` consume that as truth.

## S3 (Minor) Skill-copy drift (known gotcha in memory.md)
- `diff SKILL.md skills/mimori/SKILL.md`: identical. `diff SKILL.md ~/configs/agents-config/skills/mimori/SKILL.md`: DIFFERS.

## Clean cross-checks (no divergence found)
- Version: Cargo.toml 2.4.8 == `--version` == MCP serverInfo (env!(CARGO_PKG_VERSION), mod.rs:154) == SKILL.md:15.
- Workspace confinement enforced identically on CLI and MCP.

## Not audited
Storage DDL vs model round-trip, schema-version bump policy, line-number base consistency across parsers.
