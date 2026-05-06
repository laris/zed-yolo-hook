# Remote Zed YOLO — SSH, `remote_server`, and Headless Topologies

> 2026-05-06 status: the topology analysis still applies. The active 1.1.5
> fork now patches remote-server asset selection directly; see
> `27_v1_1_5_enhanced_source_strategy.md`.

> Date: 2026-05-05
> Scope: Where the patches in `23_source_patch_strategy.md` need to be
> deployed when Zed runs against a remote workspace.
> Companions: `23_source_patch_strategy.md`, `25_cross_compile_macos_strategy.md`.

---

## 0. Why this needs its own document

The current `zed-yolo-hook` documentation talks almost exclusively about
"local Zed on macOS arm64." Real workflows often run Zed against a remote
machine — a Linux dev box, a sandbox VM, a colleague's workstation — for
reasons that get *stronger* once we are auto-approving tool calls (a remote
sandbox is a much safer place to YOLO than your own laptop).

This doc maps each layer of the patch stack onto each remote topology so
implementors know **which binary on which host** needs which patch.

---

## 1. The Three Topologies

Zed supports three deployment shapes that touch ACP differently. They are
all visible in the source tree as distinct crates:

```
crates/
├── client/              ← collab/cloud login, gRPC to Zed cloud
├── collab/              ← Zed.dev hosted collab service
├── remote/              ← local-side SSH client (drives the connection)
├── remote_connection/   ← shared transport
└── remote_server/       ← headless Zed running on the remote box
```

### Topology A — Pure local

Everything runs on the user's laptop.

```
┌─ macOS user laptop ─────────────────────────────────────────┐
│  Zed.app                                                    │
│  ├─ agent_servers spawns claude-agent-acp / codex-acp       │
│  │   as a child process (stdio JSON-RPC)                    │
│  └─ agent_panel UI shows the dialog                         │
└─────────────────────────────────────────────────────────────┘
```

This is the topology the existing dylib hook targets. Source-level patches
work the same here as documented in `23_source_patch_strategy.md`:

- Zed binary: needs Layer 2A (handle_request_permission patch).
- Agent binary: needs Layer 1 (adapter env var).

### Topology B — SSH remote development (the common "remote Zed" flow)

The user opens a remote project via the SSH workflow
(`zed.dev/docs/remote-development`). The Zed UI runs locally; a headless
`remote_server` runs on the remote box; agents are spawned on the
**remote** side because that's where the project tree lives.

```
┌─ macOS user laptop ─────────────┐    ssh    ┌─ Linux dev box ────────────┐
│  Zed.app (UI process)           │  ───────► │  remote_server (headless)   │
│  - shows the agent panel        │           │  - HeadlessProject          │
│  - receives ACP RequestPermission│ ◄───────── │  - AgentServerStore::local │
│  - displays approval dialog     │  multiplex │    spawns claude-agent-acp │
│                                  │  channel  │    & codex-acp here        │
└─────────────────────────────────┘           └────────────────────────────┘
```

Important details from the source (verified):

- `crates/remote_server/src/headless_project.rs:236` calls
`AgentServerStore::local(...)`, i.e. **agents run on the remote**, not the
laptop. The store is `shared(REMOTE_SERVER_PROJECT_ID, session, cx)` so
the local Zed can list/configure them.
- `crates/agent_servers/src/acp.rs:670-694` builds the spawn command via
`project.remote_client().build_command_with_options(...)` when the
project is remote. `Interactive::No` means env vars are forwarded but
the shell isn't interactive. Crucially:
  - `**agent_servers.<id>.env` from `settings.json` IS forwarded across the
  SSH boundary** — that's how `ZED_YOLO=1` reaches the remote agent.
  - `settings.json` lives on the **local** laptop's `~/.config/zed/`.
- `crates/agent_servers/src/acp.rs:3189` `handle_request_permission`
runs in the **local** Zed UI process (because that's where the
`AcpThread` lives — the UI follows the user, the agent follows the
project).

So in topology B:


| Layer / Patch                                           | Where it must be deployed | Notes                                                                                                                                                           |
| ------------------------------------------------------- | ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Layer 1 (claude-agent-acp adapter)                      | **Remote** Linux box      | The npm package is installed by Zed under `~/.local/share/zed-remote-server/...` on the remote. Patched binary or env var alone works.                          |
| Layer 1 (codex-acp adapter)                             | **Remote** Linux box      | Same.                                                                                                                                                           |
| Layer 2A (Zed `handle_request_permission`)              | **Local** macOS Zed.app   | The ACP RPC is parsed by the local UI process.                                                                                                                  |
| Layer 2A (`tool_permissions.default = "allow"` setting) | **Local** `settings.json` | Settings flow from local to remote via `SettingsObserver` — same source of truth.                                                                               |
| Existing dylib (Layer 3)                                | **Local** macOS Zed.app   | The remote `remote_server` binary *cannot* be hooked the same way; it's a different binary on a different OS. We do not need to: the UI prompt happens locally. |


**Take-away**: in topology B you can choose your patch layer based on what
you control:

- If you control the remote box but not the laptop → only Layer 1 helps;
Zed will still pop the dialog because Layer 2 isn't installed.
- If you control the laptop but not the remote box → only Layer 2 helps;
but it actually fully solves the user-facing problem (no more dialogs).
- If you control both → Layer 1 + Layer 2 give you full coverage and the
cleanest UX (zero ACP `RequestPermission` round-trips).

### Topology C — Pure headless (collab / web / SDK)

`crates/remote_server/src/main.rs` shows the binary supports being launched
without any UI client. Combined with `crates/collab/`, this enables Zed
Collab and the Zed cloud sessions where multiple users attach to a single
project. Today there's no first-class "agents in collab" path
(`agent_panel.md` doesn't enumerate collab as supported), but the building
blocks exist and external integrators (e.g. via `@cursor/sdk`-style
embedders) increasingly want to programmatically drive an ACP agent
without showing any UI dialog at all.

For this topology there is no UI to pop a dialog — the
`handle_request_permission` path either short-circuits (Layer 2A) or hangs
forever waiting for a user click. So **Layer 2A is mandatory** for any
headless-Zed embedding that uses external agents. Concretely the patch
becomes a guard against deadlock.

```
┌─ Linux server ────────────────────────────────────────────────┐
│  remote_server (or future agent-only daemon)                  │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │ AcpConnection                                           │  │
│  │  - has the ACP transport                                │  │
│  │  - has handle_request_permission                        │  │
│  │  - but **no UI to drive authorize_tool_call**           │  │
│  └─────────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────┘
```

Right now the upstream `remote_server` does *not* spawn external agents on
its own — they're spawned via the UI session's `AcpConnection`. But if you
want to evolve this (e.g. a CI bot that drives Codex headlessly), Layer 2A
is the only sensible default.

---

## 2. Cross-Boundary Considerations

### 2.1 Settings propagation

Zed's settings system is one-way from local → remote:

- `crates/remote_server/src/headless_project.rs:200-210` sets up a local
`SettingsObserver` and shares it with the local-side session.
- `crates/settings/...` reads `~/.config/zed/settings.json`. On the remote
box, settings come from the local user's settings file.

Implication for our YOLO settings:

- Adding a new field like `agent.tool_permissions.external_agents.default`
is automatically delivered to the remote-side AgentServerStore. The
remote `claude-agent-acp` / `codex-acp` won't read this field directly,
but the env-var forwarding hook (`agent_servers.<id>.env`) means we can
derive `ZED_YOLO=1` from the user's setting and inject it on each agent
spawn. Patch site for that derivation:

```rust
// crates/agent_servers/src/acp.rs (around line 671-694)
fn yolo_env(cx: &App) -> HashMap<String, String> {
    let mut env = HashMap::default();
    let yolo = cx
        .read_global(|store: &SettingsStore, _| {
            store.get::<AgentSettings>(None).yolo_external_agents
        });
    if yolo {
        env.insert("ZED_YOLO".to_string(), "1".to_string());
    }
    env
}

// merged into the spawn-time env via:
//   command.env.unwrap_or_default().extend(yolo_env(cx))
```

This is a 10-line addition to the agent_servers crate that means
**flipping one local settings field auto-deploys YOLO to all agents
spawned anywhere** — local or remote.

### 2.2 Sandbox semantics

This is a subtle but important deviation between topologies:

- Codex on macOS uses **Seatbelt** (`sandbox-exec`) profiles by default.
- Codex on Linux uses **Landlock** by default.
- The `:danger-no-sandbox` profile disables the platform sandbox.

When you run YOLO on a **remote Linux box**, you're often happy to disable
the local sandbox because the box itself is the sandbox (a VM, a container,
a dev cloud instance you can nuke). When you run YOLO on the **local
laptop**, the sandbox is doing real work — it stops the agent from `cd $HOME && rm -rf .ssh`.

**Recommendation**: gate sandbox-disabling separately from
approval-disabling. Two env vars:


| Env var                | Effect                                                        | Default in patches                                                |
| ---------------------- | ------------------------------------------------------------- | ----------------------------------------------------------------- |
| `ZED_YOLO_APPROVALS=1` | Skip approval dialogs (Layer 1 + 2A semantics)                | On when topology is "remote" or `ZED_YOLO=1`                      |
| `ZED_YOLO_SANDBOX=1`   | Disable agent's platform sandbox (Codex `:danger-no-sandbox`) | On only when explicitly set, **not** auto-enabled by `ZED_YOLO=1` |


Then in the codex-acp patch:

```rust
let yolo_approvals = std::env::var("ZED_YOLO_APPROVALS").as_deref() == Ok("1")
    || std::env::var("ZED_YOLO").as_deref() == Ok("1");
let yolo_sandbox  = std::env::var("ZED_YOLO_SANDBOX").as_deref() == Ok("1");

if yolo_approvals {
    config.permissions.approval_policy
        .set(AskForApproval::Never)?;
}
if yolo_sandbox {
    config.permissions.permission_profile
        .set(PermissionProfile::Disabled)?;
}
```

This gives users the option to keep approvals off but the sandbox *on*,
which mirrors Codex's `--ask-for-approval=never` (without the `--dangerously-bypass-...` flag).

### 2.3 Helper / sub-process explosion

Per `crates/zed-yolo-hook/src/process_role.rs`, recent Zed releases (v1.1.x)
spawn ~9-10 helper processes per launch. The current dylib detects and
short-circuits in helpers to avoid 53s startup hangs (docs
`21_triage_v1.1.2_hang_2026-04-30.md`).

Source-level patches don't have this problem at all — we only patch the
permission code path that runs in the UI process and the agent process.
Helpers don't load `acp_thread` and never go through
`handle_request_permission`. This is one of the larger maintenance wins.

### 2.4 ACP Logs

`crates/agent_servers/src/acp.rs:147-200` (the `AcpDebugLog`) records every
JSON-RPC line in both directions. With Layer 2A in place, the
`session/request_permission` request appears in the log only as
`Incoming` — the response is synthesized inline and goes back as
`Outgoing` immediately. This is good for debugging: a user opening
`dev: open acp logs` sees the request and the auto-response paired up,
making it obvious why no dialog appeared.

We should add a one-line `log::info!` in the YOLO branch so the ACP debug
view reflects "auto-approved by YOLO":

```rust
log::info!(
    "ACP yolo: auto-approving permission request for tool_call_id={} \
     (option={})",
    args.tool_call.tool_call_id,
    opt.option_id);
```

---

## 3. Decision Tree for Choosing Layers

Given a topology, here's the recommended layer choice:

```
Are you running pure local macOS Zed (Topology A)?
├── Yes ──► Layer 2A only is sufficient for external agents.
│           Add Layer 1 if you want to skip the ACP request entirely
│           (slight perf win, removes a JSON-RPC roundtrip).
│
└── No, remote (Topology B):
    │
    ├── Do you control the remote box?
    │   ├── Yes, and want zero ACP overhead ──► Layer 1 on remote +
    │   │                                       Layer 2A on local
    │   ├── Yes, but want unified Zed-side policy ──► Layer 2A on local only;
    │   │                                            Layer 1 optional
    │   └── No (you're the user, IT manages the box) ──► Layer 2A on local
    │
    └── Headless / collab embedding (Topology C):
        Layer 2A is MANDATORY (otherwise hangs).
        Layer 1 strongly recommended (matches "no UI" mental model).
```

For most users we expect Topology A or B with "I patched my Zed install
once" — i.e. Layer 2A on the local Zed binary. That's the artifact
delivered by the staged build pipeline in `25_cross_compile_macos_strategy.md`.

---

## 4. Edge Cases & Gotchas

### 4.1 Agent installation cache

When Zed first spawns `claude-agent-acp` it downloads the npm package to
`~/.local/share/zed/node_modules/...` on the local box, or to the
equivalent on the remote box. **This cache persists across Zed
upgrades**. If the user installs your patched fork once and then later
clears the cache, Zed will re-download the upstream npm package and the
patch is lost.

Two ways to handle this:

1. **Pin the install location** via `agent_servers.claude-acp.command =
  "/usr/local/bin/claude-agent-acp-yolo"` and put the patched binary
   there. Zed will use the explicit path instead of its managed install.
2. **Use the env-var-only patch** (no fork): set
  `agent_servers.claude-acp.env.ZED_YOLO = "1"` AND ensure the upstream
   adapter has the ZED_YOLO branch — which means upstreaming the patch.
   For now (until upstream merges), option 1 is what we recommend.

### 4.2 Re-authentication after model switch (Claude)

`gh-agentclientprotocol__claude-agent-acp/src/acp-agent.ts:1980-1999`
shows that switching models can invalidate the current `permissionMode`
(e.g. Haiku doesn't support `auto`). The patch in `23_source_patch_strategy.md`
preserves user intent but clamps `effectiveMode`:

```typescript
let effectiveMode: PermissionMode = permissionMode;
if (effectiveMode === "auto" && !modelInfo?.supportsAutoMode) {
    effectiveMode = "default";
}
```

If the user's intent is `bypassPermissions` (our YOLO default), this
clamp doesn't fire — `bypassPermissions` is supported on every Claude
model. So we don't need extra patching here. **Test case**: switching to
Haiku-3 should still YOLO. Add a regression test in
`src/tests/acp-agent-settings.test.ts`.

### 4.3 The "Ready to code?" / ExitPlanMode prompt

`canUseTool` has a special path for `ExitPlanMode` (line 1431 of
acp-agent.ts) that always calls `requestPermission` regardless of
`permissionMode`. The current dylib sniffs the option_id list to detect
this and applies a separate `plan_option` (default `acceptEdits`).

Our Layer 1 patch should do the same — when `bypassPermissions` is in
effect, we still need to satisfy the planner. The simplest fix is to
short-circuit the ExitPlanMode branch too:

```typescript
if (toolName === "ExitPlanMode") {
    if (process.env.ZED_YOLO === "1") {
        // Move to acceptEdits mode and approve.
        await this.client.sessionUpdate({
            sessionId,
            update: { sessionUpdate: "current_mode_update",
                      currentModeId: "acceptEdits" }});
        await this.updateConfigOption(sessionId, "mode", "acceptEdits");
        return { behavior: "allow", updatedInput: toolInput };
    }
    // ... existing branch ...
}
```

Layer 2A handles the same case automatically because it picks any
`AllowAlways`-kind option, and the ExitPlanMode `acceptEdits` option is
declared with `kind: "allow_always"` (acp-agent.ts:1437).

### 4.4 Concurrency: multiple workspaces, multiple agents

Zed allows multiple project windows, each with its own `AcpThread`. The
dylib hook handles this via per-thread session tags
(`tool_authorization.rs:692`: `session_tag = format!("{:04x}", self_ptr & 0xFFFF)`).
Source-level patches don't need this complexity — `handle_request_permission`
already takes `args.session_id`, so concurrent sessions are inherently
isolated. One less thing to test.

### 4.5 Zed updates auto-revert the dylib

The macOS auto-updater replaces the binary in `Zed.app`, so any dylib
injection is wiped on next-launch. Source patches share that property
**unless** we install over the auto-update channel (i.e. ship our own
build). See `25_cross_compile_macos_strategy.md` §3 for the discussion of
release channel selection.

---

## 5. Verification Checklist

For each topology, the following manual smoke test should pass after the
patches land:

- **A**: Open Zed locally, start a Claude thread, ask the model to
`cat /etc/passwd`. Expected: no dialog, the file content streams in.
- **A**: Same with Codex thread; ask it to `ls -la ~`. Expected: no
dialog.
- **A**: Start a Claude thread, type `make a plan to add a feature`,
then `ok do it`. Expected: ExitPlanMode auto-resolves to acceptEdits.
- **B**: SSH to a Linux dev box with patched adapters; same scenarios
as A. Expected: identical behavior; `dev: open acp logs` shows
`Outgoing requestPermission ... Incoming response (selected: allow_always)`
paired immediately, with no UI render.
- **C** (future): Embed Zed via the headless path; same scenarios.
Expected: no hang, agent completes.
- **A** with `ZED_YOLO_APPROVALS=1` but `ZED_YOLO_SANDBOX=0`: ask
Codex to `cd / && rm -rf foo`. Expected: sandbox blocks the rm; no
approval dialog.

These tests should live in `agent_servers/src/e2e_tests.rs` so CI
catches regressions.

---

## 6. References

- `crates/remote_server/src/headless_project.rs:236-245` —
`AgentServerStore::local` setup on remote
- `crates/remote_server/src/headless_project.rs:289` —
`session.subscribe_to_entity(REMOTE_SERVER_PROJECT_ID, &agent_server_store)`
- `crates/agent_servers/src/acp.rs:660-714` — agent spawn with
`remote_client().build_command_with_options` and `Interactive::No`
- `crates/agent_servers/src/acp.rs:3189-3225` — `handle_request_permission`
- `crates/agent_servers/src/acp.rs:147-200` — `AcpDebugLog` machinery
- `gh-agentclientprotocol__claude-agent-acp/src/acp-agent.ts:1431-1572` —
`canUseTool` with the special ExitPlanMode branch
- `gh-zed-industries__codex-acp/src/thread.rs:114-204` — preset constants
(`:danger-no-sandbox`)
- `gh-zed-industries__codex-acp/src/thread.rs:3313-3360` —
`handle_set_mode` body
- `zed-yolo-hook/src/process_role.rs` — the helper-detection heuristic
that source patches no longer need
