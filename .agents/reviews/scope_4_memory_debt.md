---
scope: "memory-debt"
score: 6.9
status: "CRITICAL"
critical_findings: 1
invariant_breaches:
  - "Silent data loss: `debt sync` deletes ledger debt lines lacking the `accepted` prefix (src/memory/debt.rs:364-371)"
---
# Memory & Debt (src/memory) — sequential pass 2026-10-12

## M1 (Critical, silent data loss) `debt sync` purges hand-written debt items
- src/memory/debt.rs:364-371 keeps only `item.is_accepted` lines; all others are dropped when `replace_debt_section` rewrites the section (:392).
- Repro (/tmp repo): added `- manual item <- why open -> fix trigger` under KNOWN DEBT, ran `mimori debt sync`
  -> output `manual: 0`, exit 0, line gone from .agents/memory.md.
- Conflicts with the documented DEBT_SCHEMA (`- <what> <- <why> -> <trigger>` is the open-item format) and with `memory lint` accepting such lines.
- Remediation: preserve every parsed ledger item not generated from a marker; or exit non-zero listing lines that would be dropped.

## M2 (Moderate) `debt check` does not detect unsynced markers
- src/memory/debt.rs:262-310: `passed = !ceiling_breached && !has_invalid_triggers`; never compares valid markers to ledger content.
  A valid `# mimodept:` marker absent from memory.md passes -> Untracked_Debt != 0 is not enforced.

## M3 (Minor) Ceiling truncation silent in exit code
- debt.rs:385-389 truncates to 30 and exits 0 (only a WARN string); excess markers vanish from the ledger.

## Falsification attempts that found nothing
- Unicode in marker text (`héllo ✓`, `é`): no panic at debt.rs:110-113 slicing; round-trips.
- Marker inside a string literal (`let s = "// mimodept: fake <- a -> b"`): not counted.
