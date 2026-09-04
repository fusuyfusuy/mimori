# mimori

> **Zero-config AST code-intelligence, symbol-graph, and context-slicing engine in Rust.**

[![Rust](https://img.shields.io/badge/rust-2021%2B-orange.svg)]()
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

---

## What is mimori?

`mimori` gives AI coding agents and developers sub-millisecond, token-dense structural code intelligence and spatial awareness without background daemons:

- **Polyglot Embedded Tree-sitter AST**: Statically embeds Tree-sitter parsers for Rust, TypeScript/JavaScript, Python, and Go. Indexes functions, methods, classes, traits, interfaces, exported constants, builder patterns, and object literal members (e.g. tRPC routers, Drizzle tables, Hono routes).
- **In-Degree PageRank Centrality**: Ranks architectural entry points, hubs, and core data abstractions using power iteration ($d=0.85$, 25 iterations). Pass `--seed <token>` to prioritize symbols, or `--focus <symbol>` for Personalized PageRank (PPR) around a specific component.
- **Context-Aware Slicing**: Extract isolated code slices containing exact source coordinates, 1-hop callers/callees, inlined private local callees (`-f`), and top-of-file imports (`-i`). Consumes ~120 tokens vs 3,000+ for raw whole-file reads.
- **Transitive Blast Radius**: Evaluate ripple impact up to depth $N$ before editing (`blast`), reporting affected callers, entry points (`main`, API handlers), and test suites.
- **Hybrid Search Fallback**: Fast symbol search across the codebase (`find`). When zero AST symbols match, automatically falls back to in-index literal token search.
- **Zero-Daemon SQLite Cache**: Persistent incremental index in `.mimori/index.db` (WAL mode). Tracks file changes via nanosecond timestamps and FNV-1a content hashing. Point lookups execute in `< 1ms`.
- **Action Journaling**: High-signal action logging (`log`) and single-shot context snapshots (`dump`) for agent turn-to-turn memory.

---

## Installation

### From Source

```bash
cargo install --git https://github.com/fusuyfusuy/mimori.git
```

Or clone and build:

```bash
git clone https://github.com/fusuyfusuy/mimori.git
cd mimori
cargo build --release
cp target/release/mimori ~/.local/bin/
```

---

## Usage

### Overview

```shell
mimori init
mimori map    [--scope <dir>] [--focus <target>] [--seed <term>] [--limit <N>] [--json]
mimori slice  <coordinate> [-f|--follow-local] [-i|--with-imports] [--json]
mimori find   <pattern> [-s|--symbols-only] [-f|--files-only] [--json]
mimori up     <target> [--json]
mimori down   <target> [--json]
mimori blast  <target> [-d|--depth <N>] [--json]
mimori dump   [--file] [--scope <dir>] [--seed <term>] [--limit <N>] [--json]
mimori log    -a|--action <slug> -s|--summary <text> [-f|--files <f1,f2>] [--json]
mimori clean  [--all]
```

### Examples

#### 1. Warm up workspace and snapshot context
```shell
mimori init
mimori dump --file
```

#### 2. Search for high-centrality symbols or literals
```shell
mimori find "authenticate" -s
mimori find "create-backup"
```

#### 3. Extract 1-hop AST context slice with imports & private helpers
```shell
mimori slice src/auth/service.rs:authenticate -f -i
```

#### 4. Evaluate blast radius before refactoring
```shell
mimori blast src/db/connection.rs:query -d 3
```

#### 5. Record discrete milestone in action journal
```shell
mimori log -a "oauth-refresh" -s "Migrated token refresh to Redis with 5m TTL" -f "src/auth.ts,src/redis.ts"
```

---

## License

MIT © [Yusuf Akcakaya](LICENSE)
