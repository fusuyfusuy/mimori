# Dokploy Swarm Stacks Architecture: Discovery, Tool Calls & Token Spend Benchmark

**Target Repository:** Dokploy (`Dokploy/dokploy` — ~1,500 files / 205K+ LOC)  
**Investigation Scope:** Docker Swarm stacks deployment pipeline, log collection & real-time streaming, interactive container terminal access, and Traefik dynamic domain routing.  
**Tooling Environment:** `mimori` v1.4.0 (Python zero-daemon AST & context slicing CLI)  
**Date:** 2026-09-02  

---

## 1. Architectural Findings

### A. Swarm Stack Deployment Pipeline
* **Orchestration Core**:
  * Managed by `deployCompose` in [`packages/server/src/services/compose.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/services/compose.ts#L228-L340).
  * Repositories (GitHub, GitLab, Bitbucket, Gitea, raw git) or raw YAML are saved to disk under `${COMPOSE_PATH}/${appName}/code`.
  * Compose files are mutated in-memory to inject Traefik Swarm routing labels via `writeDomainsToCompose` before deployment.
  * Patches are compiled and executed via `generateApplyPatchesCommand`.
* **Execution Script Generation**:
  * Built by `getBuildComposeCommand` in [`packages/server/src/utils/builders/compose.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/utils/builders/compose.ts#L20-L76):
    * Generates project `.env` files via `getCreateEnvFileCommand`.
    * For Swarm (`composeType === "stack"`), exports environment variables inline and invokes:
      ```bash
      env -i PATH="$PATH" HOME="$HOME" <EXPORTED_ENVS> docker stack deploy -c <composePath> <appName> --prune --with-registry-auth
      ```
    * If `isolatedDeployment` is configured: creates an attachable overlay network (`docker network create --driver overlay --attachable <appName>`) and connects Traefik (`docker network connect <appName> $(docker ps --filter "name=dokploy-traefik" -q)`).
  * Executed locally via `execAsync` or remotely over SSH via `execAsyncRemote(serverId, ...)`, piping full stdout/stderr to `${deployment.logPath}`.

---

### B. Dynamic Domain Routing (Traefik Ingress)
* **Traefik Dual Provider Topology**:
  * Defined in `getDefaultTraefikConfig` in [`packages/server/src/setup/traefik-setup.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/setup/traefik-setup.ts#L253-L281):
    ```ts
    providers: {
      swarm: { exposedByDefault: false, watch: true },
      docker: { exposedByDefault: false, watch: true, network: "dokploy-network" },
      file:   { directory: "/etc/dokploy/traefik/dynamic", watch: true }
    }
    ```
* **AST Label Mutation**:
  * In [`packages/server/src/utils/docker/domain.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/utils/docker/domain.ts#L235-L310), Dokploy branches based on `composeType`:
    * For standard compose, labels attach to `services.<name>.labels`.
    * For Swarm stacks (`composeType === "stack"`), labels are injected into **`services.<name>.deploy.labels`**:
      * `traefik.enable=true`
      * `traefik.swarm.network=dokploy-network` (or the isolated stack overlay)
      * `traefik.http.routers.<appName>-<uniqueKey>-<entrypoint>.rule=Host(...)`
      * `traefik.http.services.<appName>-<uniqueKey>-<entrypoint>.loadbalancer.server.port=<port>`
      * Entrypoint bindings (`web` port 80 and `websecure` port 443 with Let's Encrypt / TLS resolver).
  * Traefik's Swarm provider detects service-level deploy labels directly through Docker Swarm API events.

---

### C. Log Collection & Real-Time Streaming
* **Swarm Task vs Container Resolution**:
  * Swarm tasks are distributed across nodes. In [`packages/server/src/services/docker.ts`](file:///home/devhax/projects/clones/dokploy/packages/server/src/services/docker.ts#L173-L231):
    * `getStackContainersByAppName`: Queries `docker stack ps <appName>` for high-level tasks.
    * `getStackTaskContainers`: Maps Swarm tasks to underlying Docker container IDs by parsing:
      ```bash
      docker stack ps <appName> -q --no-trunc --filter "desired-state=running" | \
      xargs -r docker inspect --format '{{if .Status.ContainerStatus}}TASK : {{.ID}} | ContainerId: {{.Status.ContainerStatus.ContainerID}}{{end}}'
      ```
* **Dual-Mode UI Viewer**:
  * [`ShowDockerLogsStack`](file:///home/devhax/projects/clones/dokploy/apps/dokploy/components/dashboard/compose/logs/show-stack.tsx#L42-L181) provides a toggle between:
    1. **Swarm mode**: streams service-level aggregate logs.
    2. **Native mode**: streams individual task container logs.
* **WebSocket Log Server**:
  * Implemented in [`apps/dokploy/server/wss/docker-container-logs.ts`](file:///home/devhax/projects/clones/dokploy/apps/dokploy/server/wss/docker-container-logs.ts#L15-L206) at `/docker-container-logs`:
    ```bash
    docker <runType === "swarm" ? "service" : "container"> logs --timestamps <swarm ? "--raw" : ""> --tail <tail> --follow <containerId/serviceId>
    ```
  * Local host: spawned via `node-pty` (`spawn(shell, ["-c", command])`).
  * Remote nodes: streamed over SSH via `ssh2` Client with `{ pty: true }` so SIGHUP cleanly terminates the Docker log stream when the browser WebSocket disconnects.

---

### D. Interactive Container Terminal Access
* **Underlying Task Container Exec**:
  * Since Docker CLI has no native `docker stack exec`, Dokploy resolves the container ID on the host using `getStackTaskContainers`.
* **Interactive Web Terminal Modal**:
  * Exposed in [`apps/dokploy/components/dashboard/compose/containers/show-compose-containers.tsx`](file:///home/devhax/projects/clones/dokploy/apps/dokploy/components/dashboard/compose/containers/show-compose-containers.tsx#L254-L260) via `DockerTerminalModal`.
  * Connects to WebSocket endpoint `/docker-container-terminal` ([`apps/dokploy/server/wss/docker-container-terminal.ts`](file:///home/devhax/projects/clones/dokploy/apps/dokploy/server/wss/docker-container-terminal.ts#L14-L221)).
  * Launches `docker exec -it -w / <containerId> <shell>` (defaulting to `sh` or user-specified shell).
  * Handles bidirectional input, terminal resize messages (`parseResizeMessage`), and process termination over PTY streams.

---

## 2. Tool Calls Inventory & Breakdown

During the investigation, **29 tool calls** were executed across **30 model turns** (29 intermediate discovery cycles + 1 final synthesis):

| Category | Tool Calls | Purpose |
| :--- | :---: | :--- |
| **AST / Topic PageRank** (`mimori map`) | **1** | Ranked symbols seeded with `"swarm"` across backend tier. |
| **Codebase Search** (`rg`, `find`, `ls`) | **14** | Targeted symbol discovery across multi-package monorepo boundaries. |
| **Context Slicing** (`mimori slice`) | **14** | Exact coordinate ranges and 1-hop lineage across builders, routers, and UI. |
| **Total Tool Calls** | **29** | *(0 full-file reads loaded into context)* |

### Chronological Execution Log

```text
 1. [Step 31] run_command: mimori map --stdout --seed "swarm" --kind backend
 2. [Step 33] run_command: rg -l "compose" apps/dokploy/server/
 3. [Step 35] run_command: rg -l "stack deploy" .
 4. [Step 37] run_command: find packages/server/src -type f
 5. [Step 39] run_command: mimori slice packages/server/src/utils/builders/compose.ts
 6. [Step 41] run_command: rg -n "getBuildComposeCommand" .
 7. [Step 43] run_command: mimori slice packages/server/src/services/compose.ts#L290-L340
 8. [Step 45] run_command: mimori slice packages/server/src/services/compose.ts#L200-L289
 9. [Step 47] run_command: rg -n "writeDomainsToCompose" packages/server/
10. [Step 49] run_command: mimori slice packages/server/src/utils/docker/domain.ts#L100-L190
11. [Step 51] run_command: mimori slice packages/server/src/utils/docker/domain.ts#L220-L310
12. [Step 53] run_command: rg -n "swarmMode" packages/server/
13. [Step 55] run_command: rg -n "providers" packages/server/src/setup/traefik-setup.ts
14. [Step 57] run_command: mimori slice packages/server/src/setup/traefik-setup.ts#L250-L290
15. [Step 59] run_command: rg -n "docker-logs" . || rg -l "getServiceLogs" . || rg -l "readServiceLogs" .
16. [Step 61] run_command: rg -l "logs" apps/dokploy/server/wss/ packages/server/src/wss/
17. [Step 63] run_command: ls apps/dokploy/server/wss/
18. [Step 65] run_command: rg -n "getContainersByAppName|getServicesByAppName|stack" apps/dokploy/server/api/routers/docker.ts apps/dokploy/server/api/routers/compose.ts
19. [Step 67] run_command: rg -n "getContainersByAppNameMatch" packages/server/ apps/dokploy/
20. [Step 69] run_command: mimori slice packages/server/src/services/docker.ts#L106-L160
21. [Step 71] run_command: mimori slice packages/server/src/services/docker.ts#L50-L105
22. [Step 73] run_command: rg -n "getStackTaskContainers" packages/server/
23. [Step 75] run_command: mimori slice packages/server/src/services/docker.ts#L173-L245
24. [Step 77] run_command: mimori slice apps/dokploy/server/wss/docker-container-logs.ts
25. [Step 79] run_command: mimori slice apps/dokploy/server/wss/docker-container-terminal.ts
26. [Step 81] run_command: mimori slice apps/dokploy/components/dashboard/compose/logs/show-stack.tsx#L1-L100
27. [Step 83] run_command: mimori slice apps/dokploy/components/dashboard/compose/logs/show-stack.tsx#L100-L182
28. [Step 85] run_command: mimori slice apps/dokploy/components/dashboard/compose/containers/show-compose-containers.tsx#L90-L170
29. [Step 87] run_command: mimori slice apps/dokploy/components/dashboard/compose/containers/show-compose-containers.tsx#L220-L290
```

---

## 3. Token Spend & Context Volume Analysis

### A. Turn Level (Net New Content Exchanged)

| Metric | Characters | Estimated Tokens (~4 chars/token) |
| :--- | :---: | :---: |
| **Tool Inputs & Execution Output** | 84,887 | ~21,220 |
| **Model Chain-of-Thought** | 2,508 | ~627 |
| **Synthesized Agent Findings** | 6,665 | ~1,666 |
| **Net Turn Content** | **94,060** | **~23,515** |

### B. Cumulative API Invocations (Multi-Turn Re-evaluation)

In autonomous agent architectures, each tool execution loop re-sends prior conversation history. Over the **30 sequential model invocations**:

| Phase | Characters Evaluated | Estimated Tokens |
| :--- | :---: | :---: |
| **Cumulative Prompt Context** | 2,476,463 | ~619,115 |
| **Cumulative Token Generation** | 18,641 | ~4,660 |
| **Total Cumulative API Footprint** | **2,495,104** | **~623,776** |

---

## 4. Efficiency Analysis: `mimori slice` vs Whole-File Ingestion

Without `mimori`, standard agents typically load entire files into context using `read_file` or `view_file`. The targeted files in this investigation were large:
* `packages/server/src/services/docker.ts`: **928 lines** (~38,000 chars / ~9,500 tokens)
* `packages/server/src/utils/docker/domain.ts`: **590 lines** (~24,500 chars / ~6,125 tokens)
* `packages/server/src/services/compose.ts`: **586 lines** (~23,800 chars / ~5,950 tokens)
* `packages/server/src/setup/traefik-setup.ts`: **434 lines** (~18,000 chars / ~4,500 tokens)
* `apps/dokploy/components/dashboard/compose/containers/show-compose-containers.tsx`: **312 lines** (~12,500 chars / ~3,125 tokens)
* `apps/dokploy/server/wss/docker-container-terminal.ts`: **222 lines** (~8,800 chars / ~2,200 tokens)
* `apps/dokploy/server/wss/docker-container-logs.ts`: **207 lines** (~8,200 chars / ~2,050 tokens)
* `packages/server/src/utils/builders/compose.ts`: **205 lines** (~8,100 chars / ~2,025 tokens)
* `apps/dokploy/components/dashboard/compose/logs/show-stack.tsx`: **182 lines** (~7,200 chars / ~1,800 tokens)

### Comparative Impact Table

| Strategy | Total Lines Ingested | Ingested Token Load | Cumulative Re-Evaluation Impact |
| :--- | :---: | :---: | :---: |
| **Naive Whole-File Reads** (9 files) | **3,666 lines** | **~37,275 tokens** | **~1,118,000+ tokens** |
| **`mimori slice` Coordinate Extraction** (14 targeted slices) | **~980 lines** | **~10,200 tokens** | **~619,000 tokens** (~45% reduction) |

By slicing bounded coordinates (e.g., `#L106-L160` and `#L173-L245` in `docker.ts`) and relying on 1-hop AST lineage headers (`Ancestors` and `Dependencies`), context growth remained strictly constrained to functional boundaries.
