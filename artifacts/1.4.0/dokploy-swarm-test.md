# mimori 1.4.0 Empirical Evaluation: Dokploy Swarm Stack Investigation

**Benchmark Task:** Identify how Docker Swarm stacks are deployed, how their domains are routed via Traefik, how runtime logs are collected, and how container terminal sessions are established in Dokploy (1,562 files · 211.2K lines of code).

---

## 1. Executive Summary

This empirical test evaluated `mimori` against an end-to-end multi-subsystem query on a live production full-stack monorepo (Dokploy). 

While `mimori` successfully bootstrapped repo orientation via `dump` and extracted symbol lineages with 1-hop upstream/downstream context via `slice`, the investigation encountered critical friction points that triggered 37 fallback tool calls (`view_file`, `grep_search`, `find_by_name`).

### Key Performance Indicators (KPIs)
- **Model Iterations**: 47 turns
- **Total Tool Invocations**: 46 calls (9 `mimori` commands, 37 fallback calls)
- **Net Delta Ingested**: ~164,020 chars (~41,000 tokens)
- **Total Generated Output**: ~23,000 chars (~5,750 tokens)
- **Cumulative Context Ingestion**: 4,671,620 chars (~1,168,000 prompt tokens across the 47 iterative round-trips)

---

## 2. Core Architectural Findings (What Was Discovered)

1. **Stack Deployment Pipeline** ([`packages/server/src/utils/builders/compose.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/utils/builders/compose.ts)):
   - Dokploy unifies compose and swarm stacks under a single `compose` domain entity using `composeType: "docker-compose" | "stack"`.
   - When `composeType === "stack"`, Dokploy executes:
     ```bash
     docker stack deploy -c <path> <appName> --prune --with-registry-auth
     ```
   - Because `docker stack deploy` rejects `--env-file`, Dokploy resolves all project and environment secrets, strips shell control characters, and exports them directly into an isolated subshell:
     ```bash
     env -i PATH="$PATH" HOME="$HOME" ${exportEnvCommand} docker stack deploy ...
     ```
   - For isolated stacks, Dokploy provisions an attachable overlay network (`docker network create --driver overlay --attachable <appName>`) and joins the proxy container.

2. **Domain Routing & Traefik Label Rewriter** ([`packages/server/src/utils/docker/domain.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/utils/docker/domain.ts)):
   - Unlike standard Compose which places Traefik labels at the service root, Swarm requires labels under `deploy.labels`.
   - Dokploy dynamically parses the Compose YAML AST (`addDomainToCompose`), strips stale labels, and injects routing directives under `deploy.labels`:
     - `traefik.enable=true`
     - `traefik.swarm.network=dokploy-network` (swapped from `traefik.docker.network`)
     - Router and service definitions (`traefik.http.routers.<app>-<id>...`)
   - Traefik runs as a manager-constrained Swarm service listening on `/var/run/docker.sock` with `providers.swarm` enabled (`exposedByDefault: false`, `watch: true`).

3. **Log Collection & Dual-Mode Streaming** ([`apps/dokploy/server/wss/docker-container-logs.ts`](file:///home/devhax/projects/clones/dokploy/apps/dokploy/server/wss/docker-container-logs.ts) & [`packages/server/src/services/docker.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/services/docker.ts)):
   - **Service-Level Stream**: Over the `/docker-container-logs` WebSocket, when `runType === "swarm"`, Dokploy executes:
     ```bash
     docker service logs --timestamps --raw --tail <N> --follow <serviceId/containerId>
     ```
     Local execution spawns a `node-pty` pseudo-terminal; remote nodes stream via SSH (`ssh2` Client with `{ pty: true }`).
   - **Replica/Task-Level Aggregation**: In the container view, Dokploy parses `docker stack ps <appName>` and pipes task IDs into `docker inspect` to map swarm tasks (`TASK : <id>`) to physical container IDs (`Status.ContainerStatus.ContainerID`).

4. **Interactive Web Terminal** ([`apps/dokploy/server/wss/docker-container-terminal.ts`](file:///home/devhax/projects/clones/dokploy/apps/dokploy/server/wss/docker-container-terminal.ts)):
   - Docker Swarm does not implement `docker service exec`. Dokploy overcomes this by resolving the task's physical container ID (`getStackTaskContainers`).
   - The UI links each task to `/docker-container-terminal?containerId=<id>&activeWay=<sh|bash>`.
   - The server authorizes tenant permissions and attaches a bidirectional PTY (`docker exec -it -w / <containerId> <shell>`), dynamically handling window resizing (`parseResizeMessage`) over WebSocket frames.

---

## 3. Quantitative Tool & Token Breakdown

### Tool Invocations

| Tool | Invocations | % of Total | Purpose |
| :--- | :---: | :---: | :--- |
| `view_file` | **22** | 47.8% | Required to read source code past line 100 where `mimori slice` was clipped. |
| `grep_search` | **11** | 23.9% | Locating CLI strings (`docker stack`, `stack deploy`, `docker service logs`, `docker-container-terminal`). |
| `mimori` (`run_command`) | **9** | 19.6% | 2 `mimori map` calls + 7 `mimori slice` calls. |
| `find_by_name` | **4** | 8.7% | Discovering backend router files and WebSocket handler trees. |
| **Total** | **46** | **100%** | |

### Token Consumption Profile

| Phase | Characters | Est. Tokens (~4 chars/token) |
| :--- | :---: | :---: |
| Pre-Turn Context (Warmup) | 17,987 chars | ~4,496 tokens |
| Turn Net Delta (Ingested + Produced) | 164,020 chars | ~41,005 tokens |
| Model Generation (Output Across 47 Steps) | 23,000 chars | ~5,750 tokens |
| **Cumulative Prompt Ingestion (Summed History)** | **4,671,620 chars** | **~1,167,905 tokens** |

*(Excluding static system prompt & tool schema definitions; factoring in ~12k tokens of base instructions across 47 turns raises total LLM prompt consumption to ~1.73M tokens).*

---

## 4. Friction Analysis & Root Causes

```
User Query: "How are swarm stacks deployed, routed, logged, and accessed?"
  │
  ├─► mimori map --focus "swarm,stack"
  │     └─► FAILURE: Surfaced only UI components & routers/swarm.ts.
  │         Root Cause: Swarm logic is a subtype inside compose.ts; AST map lacked semantic coupling.
  │
  ├─► Fallback: grep_search "stack deploy" (Found builders/compose.ts)
  │
  ├─► mimori slice builders/compose.ts
  │     └─► FAILURE: Clipped at line 100. Key logic (createCommand, env) is at lines 125–204.
  │
  ├─► Fallback: view_file lines 101–205 (Step 2)
  │
  ├─► mimori slice domain.ts:writeDomainsToCompose
  │     └─► FAILURE: Extracted lines 113–143 (outer wrapper). Real rewriter is addDomainToCompose (line 199).
  │
  ├─► Fallback: view_file lines 140–350 (Step 2 & 3)
  │
  ├─► mimori slice traefik-setup.ts / docker-container-logs.ts / docker-container-terminal.ts
        └─► FAILURE: Every slice clipped at line 100. Exec lines lived at 104+, 120+, 250+.
        └─► Result: 6 additional view_file calls.
```

### The Four Failure Modes:
1. **Hardcoded 100-Line Slice Truncation**:
   - Single largest source of bloat. 22 `view_file` calls (48% of all tool actions) occurred because vital implementation routines sat between lines 101 and 300.
2. **Lexical Keyword Blindness in `map`**:
   - `mimori map --focus "swarm"` only indexed files containing the literal token "swarm" in filename or symbols. It missed the primary execution engine (`builders/compose.ts`) where Swarm is a polymorphic branch (`composeType === "stack"`).
3. **PageRank Test & UI Inflation**:
   - `mimori map --focus "compose,terminal,traefik"` hit the 400-line ceiling immediately. Over 70% of results were React components (`apps/dokploy/components/...`) and test suites (`apps/dokploy/__test__/...`).
4. **Symbol Indirection (Wrappers vs Implementation)**:
   - Slicing an exported symbol often returns only the boundary wrapper, omitting the unexported helper functions in the same file that execute the actual domain logic.

---

## 5. Optimization Recommendations for mimori 1.4.0

### Feature 1: Adaptive Slicing (`--lines` and Complete Symbol Body)
- **Problem**: Fixed 100-line limit forces agents to issue `view_file` immediately after `mimori slice`.
- **Solution**:
  - If a file is under 300 lines, emit the entire file.
  - If slicing a symbol, emit the complete AST node body regardless of line count.
  - Support arbitrary ranges: `mimori slice <file> --lines 250` or `mimori slice <file>#L100-L220`.

### Feature 2: AST Local Callee Expansion (`--follow-local` / `-f`)
- **Problem**: Outer wrappers hide internal mechanics (e.g. `writeDomainsToCompose` delegating to `addDomainToCompose`).
- **Solution**: When `--follow-local` is passed, `mimori slice` should inspect the symbol's call graph and automatically inline private functions defined in the same file.

### Feature 3: Grep-Seeded Map (`--seed "<pattern>"`)
- **Problem**: Pure PageRank and filename matching miss polymorphic subtypes and runtime command strings (`stack deploy`).
- **Solution**: Add `mimori map --seed "stack deploy"`. Files matching the grep pattern have their PageRank boosted to the top of the structural hierarchy.

### Feature 4: Subtree & Noise Purging (`--no-tests`, `--kind backend`)
- **Problem**: Large webapps flood `mimori map` with UI dialogs and vitest/jest files.
- **Solution**:
  - `mimori map --no-tests` (excludes `__test__`, `tests/`, `*.spec.ts`, `*.test.ts`).
  - `mimori map --kind backend` (skips JSX/TSX and CSS, focusing on controllers, models, and utility engines).

---

## 6. Projected ROI with mimori 1.4.0 Features

| Metric | Baseline (Current Test) | Projected with 1.4.0 Optimizations | Savings |
| :--- | :---: | :---: | :---: |
| **Tool Invocations** | 46 calls | **11 – 14 calls** | **~72% reduction** |
| **Fallback `view_file` calls** | 22 calls | **0 – 2 calls** | **~91% reduction** |
| **Fallback `grep_search` calls** | 11 calls | **2 – 3 calls** | **~75% reduction** |
| **Model Iterations (Roundtrips)** | 47 turns | **12 – 15 turns** | **~70% reduction** |
| **Cumulative Prompt Tokens** | ~1.17M tokens | **~280K – 350K tokens** | **~70% reduction** |
| **Wall-Clock Latency** | ~2.5 minutes | **~35 – 45 seconds** | **~70% faster** |

---

*Artifact logged to `artifacts/1.4.0/dokploy-swarm-test.md` for mimori version 1.4.0 performance benchmarking.*
