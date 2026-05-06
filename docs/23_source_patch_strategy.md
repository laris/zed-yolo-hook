# Source-Level YOLO: From Dylib Hook to Upstream Patch Strategy

> 2026-05-06 status: this document remains the approval-layer background.
> The active 1.1.5 enhanced implementation is consolidated in
> `27_v1_1_5_enhanced_source_strategy.md`.

> Date: 2026-05-05
> Scope: Zed (Stable / Preview / nightly), claude-agent-acp, codex-acp
> Audience: Maintainers of `zed-yolo-hook` planning the next architecture
> Companions:
>
> - `24_remote_zed_yolo_strategy.md` — remote / headless / SSH coverage
> - `25_cross_compile_macos_strategy.md` — staged build pipeline (Linux→macOS)
> - `26_implementation_roadmap.md` — concrete plan and milestone schedule

---

## 0. TL;DR

`zed-yolo-hook` today is a **runtime instrumentation patch** (Frida-Gum) of the
shipped Zed binary. Every Zed Preview release shifts compiler-driven offsets,
which we manually re-calibrate (see docs `11_…20_…`). The goal of this document
is to design a **source-level patch strategy** that:

1. Ships a small set of well-targeted Rust patches against three upstream
  repos:
  - `zed-industries/zed` — the Zed app
  - `agentclientprotocol/claude-agent-acp` — the Claude Agent ACP adapter
  (TypeScript)
  - `zed-industries/codex-acp` — the Codex ACP adapter (Rust)
2. Survives upstream churn far better than the offset-driven dylib hook (we
  patch *names*, not *addresses*).
3. Covers **local Zed Preview**, **remote projects over SSH**, and **the
  headless `remote_server` flow** with the same policy knobs: adapter env
  injection for the remote-side agent process, and a local Zed policy for
  ACP prompts that still reach the UI boundary.
4. Plugs into a staged build pipeline (see `25_cross_compile_macos_strategy.md`)
  so the heavy Rust compilation can run on a beefy Linux server, while only
   the Apple-specific finalization stays on macOS.

Most of the "hard" problem is **understanding the layered approval handshake**;
once the layering is clear, the patches themselves are small and surgical.

---

## 1. The Approval Handshake — Three Independent Decision Points

External-agent tool calls in Zed cross **three distinct decision points**, each
with its own state machine. A correct YOLO design must address all three or it
will appear to work in dev tests and break in real workflows (e.g. a Bash tool
call gets approved but Codex still pauses on a `apply_patch` event).

### 1.1 Decision Point A — The agent's *internal* policy

The first gate is **inside** the agent process — Claude Agent SDK or Codex
core. Even before an ACP message is sent, the agent applies its own approval
policy:


| Agent  | Code path                                                                                                               | Setting                                                                                                      | Effect                                                                                                                                                                                                                                                                                                                                                |
| ------ | ----------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Claude | `gh-agentclientprotocol__claude-agent-acp/src/acp-agent.ts:1419 canUseTool()`                                           | `permissionMode`, `allowDangerouslySkipPermissions`                                                          | Decides whether to call `client.requestPermission` at all. `bypassPermissions` mode short-circuits in line 1509.                                                                                                                                                                                                                                      |
| Codex  | `gh-zed-industries__codex-acp/src/codex_agent.rs:312 build_session_config()` and `src/thread.rs:3313 handle_set_mode()` | `Config.permissions.approval_policy`, `Config.permissions.permission_profile`, and `Op::OverrideTurnContext` | Codex core only emits `EventMsg::ExecApprovalRequest` / `ApplyPatchApprovalRequest` when the loaded config asks for approval. Mode `"full-access"` maps to permission profile `:danger-no-sandbox`, but Zed applies `default_mode` after `new_session`, so a real Layer 1 patch should set the initial config before `ThreadManager::start_thread()`. |
| Gemini | gemini-cli-acp (out of scope here)                                                                                      | similar `--yolo` flag                                                                                        | —                                                                                                                                                                                                                                                                                                                                                     |


**Key observation:** if you flip Decision Point A to "always allow," Decision
Points B and C never fire — no ACP `RequestPermission` is sent and Zed never
shows a dialog. This is the cleanest, lowest-overhead intercept. It can also
be upstream-friendly if it remains an explicit opt-in, because both adapters
already expose the underlying dangerous/full-access modes; the patch only adds
a controlled way for Zed-managed launches to select them.

### 1.2 Decision Point B — The agent adapter's ACP boundary

Even when the agent *does* decide it needs approval, the request still goes
through the adapter shim:

- **Claude**: `acp-agent.ts:1458` calls `this.client.requestPermission(...)`
which serializes a JSON-RPC `session/request_permission` to Zed's stdin.
- **Codex**: `thread.rs:914` calls
`client.request_permission(tool_call, options)` — same JSON-RPC method.

If you *don't* disable the agent's internal policy (Decision Point A) but you
**do** want a single uniform policy across agents, this is the right place to
intercept: short-circuit the `requestPermission` call and return a synthetic
`{outcome: { outcome: "selected", optionId: "allow_always" }}` immediately.

Pros of intercepting here vs. Decision Point A:

- Uniform regardless of agent's internal policy phrasing (Claude calls it
`bypassPermissions`, Codex calls it `:danger-no-sandbox`, Gemini `--yolo`).
- We can express **per-tool** policy (e.g. "auto-allow read_file but always
prompt for `write_text_file` outside the workspace") with a single rule
engine living in the adapter.

Cons:

- We pay the cost of constructing the `requestPermission` argument list (it's
tiny but non-zero).
- For Codex, we don't get sandbox relaxation for free — only the
`:danger-no-sandbox` mode disables seatbelt/landlock; intercepting at B
alone leaves the sandbox active, which is *safer* but breaks workflows that
assume the agent has full disk access.

### 1.3 Decision Point C — Zed's UI / `AcpThread`

This is the gate that current `zed-yolo-hook` targets. The flow:

1. Agent → ACP `session/request_permission` → Zed's transport.
2. `gh-zed-industries__zed/crates/agent_servers/src/acp.rs:3189
  handle_request_permission` is invoked by the JSON-RPC dispatcher.
3. It calls `AcpThread::request_tool_call_authorization`
  (`crates/acp_thread/src/acp_thread.rs:2073`) which:
   a. Creates a `oneshot::Sender<SelectedPermissionOutcome>`.
   b. Sets `ToolCallStatus::WaitingForConfirmation { options, respond_tx }`.
   c. Emits `AcpThreadEvent::ToolAuthorizationRequested`.
   d. Returns a `Task<RequestPermissionOutcome>` that awaits the oneshot.
4. The `agent_ui` (`crates/agent_ui/src/conversation_view/thread_view.rs:7212`)
  renders the dialog. On click it calls
   `AcpThread::authorize_tool_call(...)` → `respond_tx.send(outcome)`.
5. The awaited outcome is wrapped in `RequestPermissionResponse` and sent
  back to the agent over ACP.

The current dylib hook hijacks step 3 (it walks `AcpThread.entries`, finds
the `WaitingForConfirmation` entry, transmutes `respond_tx` from raw
pointers, and fires its own outcome). That works but the per-version offsets
are fragile.

---

## 2. Recommended Architecture — A Three-Layer Patch Stack

We recommend implementing **all three** layers and exposing them as toggles
so users can choose the smallest viable footprint. Layers stack on top of
each other and degrade gracefully:

```
Layer 1: Adapter-side default mode (claude-agent-acp, codex-acp)
   ↓ (works for 90% of cases, zero Zed change)
Layer 2: Zed "yolo passthrough" patch (handle_request_permission)
   ↓ (catches anything Layer 1 missed; works for Zed built-in tools too)
Layer 3: Existing dylib hook (kept as a fallback for unpatched Zed binaries)
```

Layers 1 + 2 together replace the dylib in the "I built my own Zed" path.
Layer 3 remains for users who can't or won't rebuild Zed.

### 2.1 Layer 1 — Patch the Agent Adapters

Both adapters already implement an "always allow" semantic; they just don't
default to it and they gate it behind safety checks (root user, etc.). The
patches are small.

#### 2.1.1 `claude-agent-acp`

Two surgical changes in `src/acp-agent.ts`:

```typescript
// 1. Lift the IS_ROOT gate when ZED_YOLO=1 is in env.
//    Original (line 322-324):
//        const IS_ROOT = (process.geteuid?.() ?? process.getuid?.()) === 0;
//        const ALLOW_BYPASS = !IS_ROOT || !!process.env.IS_SANDBOX;
//    Patched:
const IS_ROOT = (process.geteuid?.() ?? process.getuid?.()) === 0;
const ALLOW_BYPASS =
  !IS_ROOT || !!process.env.IS_SANDBOX || !!process.env.ZED_YOLO;

// 2. Default permissionMode to "bypassPermissions" when ZED_YOLO=1 and the
//    user did not pass an explicit mode in `permissions.defaultMode`.
//    Original (line 1818-1820):
//        const permissionMode = resolvePermissionMode(
//          settingsManager.getSettings().permissions?.defaultMode,
//          this.logger,
//        );
//    Patched:
const explicitMode = settingsManager.getSettings().permissions?.defaultMode;
const yoloDefault = (explicitMode === undefined && process.env.ZED_YOLO === "1")
  ? "bypassPermissions" : explicitMode;
const permissionMode = resolvePermissionMode(yoloDefault, this.logger);
```

**Effect**: when `ZED_YOLO=1` is in the agent's environment, the Claude
adapter starts in `bypassPermissions`; normal tool calls return from
`canUseTool` without calling `client.requestPermission`. Zed shows zero
dialogs for the Claude ACP path.

**Plumbing**: in Zed, set the env var per-agent in
`agent_servers.claude-acp.env`, or globally in `agent_servers.<agent>.env`:

```json [settings.json]
{
  "agent_servers": {
    "claude-acp": { "type": "registry", "env": { "ZED_YOLO": "1" } },
    "codex-acp":  { "type": "registry", "env": { "ZED_YOLO": "1" } }
  }
}
```

Distribution: a fork of the npm package
(`@your-org/claude-agent-acp-yolo`) is the simplest path. Less elegant but
more reproducible: a one-liner `script/postinstall.sh` that patches the
installed `dist/acp-agent.js` after `npm install`.

#### 2.1.2 `codex-acp`

The Codex adapter is clean because Codex core natively supports an
approval-disabled, no-sandbox profile. The important correction is **where**
to patch it: the initial turn config is assembled in
`src/codex_agent.rs::build_session_config()` before
`ThreadManager::start_thread()`. `src/thread.rs::handle_set_mode()` is
still relevant for later user-driven mode changes, but patching it alone
is too late for the first session configuration.

```rust
// src/codex_agent.rs, inside build_session_config(), after cwd/MCP setup
// and before returning the Config used by ThreadManager::start_thread().
use codex_protocol::config_types::{ActivePermissionProfile, PermissionProfile};
use codex_protocol::protocol::AskForApproval;

const CODEX_DANGER_NO_SANDBOX_PROFILE_ID: &str = ":danger-no-sandbox";

fn zed_yolo_enabled() -> bool {
    std::env::var("ZED_YOLO").as_deref() == Ok("1")
        || std::env::var("ZED_YOLO_APPROVALS").as_deref() == Ok("never")
}

if zed_yolo_enabled() {
    config
        .permissions
        .approval_policy
        .set(AskForApproval::Never)
        .map_err(|e| Error::internal_error().data(e.to_string()))?;

    config
        .permissions
        .set_permission_profile_with_active_profile(
            PermissionProfile::Disabled,
            Some(ActivePermissionProfile::new(CODEX_DANGER_NO_SANDBOX_PROFILE_ID)),
        )
        .map_err(|e| Error::internal_error().data(e.to_string()))?;
}
```

**Effect**: Codex core never emits `ExecApprovalRequest` /
`ApplyPatchApprovalRequest` events. The `:danger-no-sandbox` profile also
disables Seatbelt (macOS) and Landlock (Linux) sandboxing, matching the
`codex --dangerously-bypass-approvals-and-sandbox` behavior.

**Existing Zed setting shortcut**: Zed already supports
`agent_servers.codex-acp.default_mode = "full-access"` and will call
`set_session_mode` after `new_session`. That is useful for smoke tests and
may be enough in simple flows, but it is asynchronous and post-session;
the source patch above is deterministic because the first Codex config is
already full-access before the thread starts.

**Distribution**: codex-acp is a Rust binary distributed via GitHub
releases *and* npm (`npm/` directory in the repo). Fork → set
`Cargo.toml`'s name, GH releases, npm package; `agent_servers.codex-acp` in
Zed settings can be pointed at the fork. Or, simpler still: build the
patched binary in your own CI and install via the registry-bypass path:

```json [settings.json]
{
  "agent_servers": {
    "codex-acp": {
      "type": "custom",
      "command": "/usr/local/bin/codex-acp-yolo",
      "args": [],
      "env": { "ZED_YOLO": "1" }
    }
  }
}
```

#### 2.1.3 Why this is good

- **Trivial to maintain**: maybe 30 lines of patch across two repos. The
only churn risk is renaming of the surrounding context lines, which `git apply --3way` can usually heal automatically.
- **Cross-platform**: TypeScript and Rust patches both work on macOS,
Linux, Windows.
- **Remote-Zed friendly**: when a project is remote, Zed asks the remote
side for the agent command (`project/src/agent_server_store.rs:754-823`).
The configured `agent_servers.<agent>.env` is merged into that command
before it is wrapped by `RemoteClient::build_command_with_options()`
(`agent_servers/src/acp.rs:671-685`), so the YOLO env var reaches the
remote-side process without installing a hook there.
- **No metal/UI risk**: we are not touching Zed's binary at all.

#### 2.1.4 Why this is **not enough on its own**

Some tool calls are dispatched by Zed's *native* agent (the first-party
agent), not via ACP. They go through `ToolPermissionDecision::from_input`
in `crates/agent/src/tool_permissions.rs`. Adapter patches don't affect
those. That's why we still need Layer 2.

### 2.2 Layer 2 — Patch Zed Itself

Two small patches in `gh-zed-industries__zed/crates/`. They are independent
and either can be omitted if you only care about external agents.

#### 2.2.1 Patch A — Auto-respond to ACP `RequestPermission`

This is the source-level analogue of the current dylib hook on
`request_tool_call_authorization`. It plugs in **before** the
`AcpThread` ever transitions to `WaitingForConfirmation`, so the UI never
even tries to render a dialog.

In `crates/agent_servers/src/acp.rs` (around line 3189):

```rust
fn handle_request_permission(
    args: acp::RequestPermissionRequest,
    responder: Responder<acp::RequestPermissionResponse>,
    cx: &mut AsyncApp,
    ctx: &ClientContext,
) {
    // YOLO short-circuit — return an allow option without consulting UI.
    //
    // Gated by `ZED_YOLO=1` for dev builds and by the new
    // `agent.tool_permissions.external_agents.default = "always_allow"`
    // setting for normal use.
    if yolo_external_agents_enabled(cx) {
        let preferred = args
            .options
            .iter()
            .find(|o| {
                ["bypassPermissions", "allow_always", "approved"]
                    .iter()
                    .any(|id| *id == o.option_id.as_ref())
            })
            .or_else(|| args.options.iter().find(|o| {
                matches!(o.kind, acp::PermissionOptionKind::AllowAlways)
            }))
            .or_else(|| args.options.iter().find(|o| {
                matches!(o.kind, acp::PermissionOptionKind::AllowOnce)
            }))
            .or_else(|| args.options.first());

        if let Some(opt) = preferred {
            log::info!(
                "ZED_YOLO auto-approving ACP permission request with option `{}`",
                opt.option_id
            );
            let _ = responder.respond(
                acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Selected(
                        acp::SelectedPermissionOutcome::new(opt.option_id.clone())
                    )
                ));
            return;
        }
    }

    // ... existing body unchanged ...
}
```

`yolo_external_agents_enabled` reads either:

- `std::env::var("ZED_YOLO") == Ok("1".to_string())`, or
- a new compiled settings field under `agent_settings::AgentSettings`,
derived from `agent.tool_permissions.external_agents.default`.

**Why this is better than the dylib hook**:

- We replace the message at the protocol boundary, so we never have to
reach into `AcpThread.entries` or worry about offsets, niche-encoded
enums, or `Arc` refcount manipulation.
- It's a pure functional change — easy to unit-test. The fake ACP
server lives in `agent_servers/src/acp.rs`, and the e2e harness in
`agent_servers/src/e2e_tests.rs` already drives the surrounding path.
- It works for *any* ACP agent, not just Claude / Codex / Gemini.
- It naturally handles ExitPlanMode and other non-standard option lists
as long as the agent offers at least one allow option. The selector should
prefer explicit YOLO/full-access IDs such as `bypassPermissions`,
`allow_always`, and Codex's patch approval `approved`, then fall back to
the first `AllowAlways` / `AllowOnce`.

**Where to gate the toggle**:

- For dev: env var `ZED_YOLO=1` honored by the binary (consistent with
Layer 1).
- For users: extend the existing `agent.tool_permissions` schema to remove
the awkward "external agents ignore tool_permissions" caveat documented at
`docs/src/ai/external-agents.md:275`. Keep the first version global rather
than per-agent; per-agent policy can be added later by carrying `AgentId`
through `ClientContext`.

```jsonc
{
  "agent": {
    "tool_permissions": {
      "default": "allow",
      "external_agents": {
        // NEW: unifies the long-standing inconsistency. Values:
        // "prompt" = current behavior; "always_allow" = Zed auto-selects
        // an allow option when an ACP agent asks for permission.
        "default": "always_allow"
      }
    }
  }
}
```

This both gives users a single knob and finally aligns external-agent
behavior with the existing `tool_permissions` mental model.

Concrete schema plumbing:

- `crates/settings_content/src/agent.rs`: add
`ToolPermissionsContent.external_agents: Option<ExternalAgentToolPermissionsContent>`.
- `crates/agent_settings/src/agent_settings.rs`: add the compiled field to
`ToolPermissions` or `AgentSettings`; default remains `"prompt"`.
- `crates/agent_servers/src/acp.rs`: read the compiled setting in
`handle_request_permission` before calling
`AcpThread::request_tool_call_authorization`.

#### 2.2.2 Patch B — Auto-allow Zed's native tools

In `crates/agent/src/tool_permissions.rs:253` `ToolPermissionDecision::from_input`,
the existing `tool_permissions.default = "allow"` setting **already does
this** — that is the supported, documented path. The dylib's
`permission_decision` hook exists today only because the user can't ship a
patched Zed and wants a runtime override.

Once Zed is built from the patched source, **delete the Layer 3 native
hook entirely** and configure:

```json
{ "agent": { "tool_permissions": { "default": "allow" } } }
```

Built-in security rules
(`HARDCODED_SECURITY_RULES.terminal_deny`, `rm -rf /`, etc.) still apply,
which is what we want anyway — those are in `tool_permissions.rs:25-66`
and are intentionally non-bypassable. The dylib's
`permission_decision` listener bypasses these (it zeros 32 bytes at the
return slot, producing `Allow` regardless), which is mildly unsafe.

**Net result**: with Layer 2A + the existing settings, no Zed code outside
`agent_servers/src/acp.rs` needs patching. We benefit from Zed's
already-tested machinery rather than parallel-building our own.

### 2.3 Layer 3 — The Existing Dylib (Fallback Only)

Keep the current `zed-yolo-hook` as a **last-resort** path for users who
run a stock Zed binary. The maintenance burden is the per-version offset
recalibration (docs `11_…20_…`). We can reduce that burden:

1. **Switch the symbol search to be more lenient** so we don't depend on
  exported-name fragments. We already do this for the function
   symbol — we should also use *runtime memory pattern scanning* for the
   `AcpThread` field offsets so we don't need a per-version table.
2. **Auto-install a release-channel-aware update notifier**: read the
  `RELEASE_CHANNEL` build-info string from the bundle and post a warning
   if the offsets-table doesn't include it. Today this is implicit (the
   hook silently misses).

Neither blocks the source-patch work — we layer the source patches in
front so most users never need the dylib at all.

---

## 3. The "Patch Train" Workflow

To keep upstream churn manageable we propose a **patch-train repo**
modeled on Linux distro patch queues (`quilt`, `git-format-patch`):

```
zed-yolo-patches/
├── claude-agent-acp/
│   ├── 0001-allow-bypass-when-zed-yolo-env.patch
│   └── 0002-default-permissionmode-bypass-when-yolo.patch
├── codex-acp/
│   ├── 0001-default-approval-never-when-zed-yolo-env.patch
│   └── 0002-default-permission-profile-disabled-when-yolo.patch
├── zed/
│   ├── 0001-yolo-shortcircuit-handle-request-permission.patch
│   └── 0002-add-agent-tool-permissions-external-agents-field.patch
├── README.md
└── apply.sh           # `git -C $repo am --3way < $patch`
```

Why this beats forks:

- `git am --3way` auto-resolves most context-line drift. When it
fails, it leaves a conflict marker that's a 5-line manual fix.
- We can publish a one-line CLI:
`cargo zed-yolo apply ~/dev/codes-repos/gh-zed-industries__zed`
that pulls the train and applies it.
- The same workflow handles Zed *Stable*, *Preview*, and *Nightly* — we
publish branches `train/preview-0.234`, `train/preview-0.235`,
`train/stable-1.0`.
- A GitHub Action runs the train against each upstream tag nightly and
fails loudly when a hunk drifts so we know the moment a patch needs
rebasing — *before* we hit the offset-recalibration emergency.

---

## 4. Why This Beats the Current Approach


| Dimension           | Dylib (today)                                                                                                                                       | Source patch (proposed)                                                                                                         |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Maintenance cadence | ~once per Preview release (offset hunting, see docs `11_…20_…`)                                                                                     | Only when upstream renames a function or restructures the permission flow — rare                                                |
| Failure mode        | Silent miss → user sees the dialog they thought they bypassed                                                                                       | Compile error / failed `git am` → noisy, easy to fix                                                                            |
| Cross-platform      | macOS arm64 today; porting to x86_64, Linux, Windows requires per-platform offset hunting                                                           | Patches are platform-agnostic Rust/TS source                                                                                    |
| Remote-Zed coverage | Helper processes are detected and skipped; permission UI runs locally so the local hook *does* catch most cases, but Codex sandboxing is unaffected | Layer 1 patches deploy to the remote (since the agent runs there); Layer 2 patches deploy to the local Zed; both work uniformly |
| Built-in tool YOLO  | `permission_decision` hook bypasses **even hardcoded rules** like `rm -rf /`                                                                        | Use `tool_permissions.default = "allow"` — keeps hardcoded rules, less footgun                                                  |
| Distribution        | Single dylib, zero dependencies (good); but binary patching needs Apple notarization or codesign workarounds                                        | A patched Zed bundle requires its own codesign chain; that's the topic of `25_cross_compile_macos_strategy.md`                  |
| Test coverage       | E2E only (the test must launch real Zed)                                                                                                            | Unit-testable via Zed's existing `FakeAcpAgentServer` and adapter test suites                                                   |


---

## 5. Concrete Code Touchpoints (Quick Reference Tables)

### 5.1 Patch Anchor Points by Repo


| Repo               | File                                              | Symbol/Line                      | What we change                                                                                   |
| ------------------ | ------------------------------------------------- | -------------------------------- | ------------------------------------------------------------------------------------------------ |
| `claude-agent-acp` | `src/acp-agent.ts:322`                            | `ALLOW_BYPASS` const             | Add `                                                                                            |
| `claude-agent-acp` | `src/acp-agent.ts:1818`                           | `permissionMode` resolution      | Default to `bypassPermissions` if env=1 and no explicit setting                                  |
| `codex-acp`        | `src/codex_agent.rs:312`                          | `build_session_config`           | Set `approval_policy = Never` and `permission_profile = Disabled` before `start_thread` if env=1 |
| `codex-acp`        | `src/thread.rs:3313`                              | `handle_set_mode`                | Keep later mode updates consistent; reuse existing `"full-access"` preset behavior               |
| `zed`              | `crates/agent_servers/src/acp.rs:3189`            | `handle_request_permission`      | Short-circuit with a selected allow option if YOLO setting is on                                 |
| `zed`              | `crates/settings_content/src/agent.rs:607`        | `ToolPermissionsContent`         | Add new `external_agents` sub-field                                                              |
| `zed`              | `crates/agent_settings/src/agent_settings.rs:169` | `AgentSettings.tool_permissions` | Plumb through compiled external-agent policy                                                     |


### 5.2 Layered Behavior Matrix


| Decision Point    | Triggered when…                     | Patched by                                           | If unpatched, what user sees     |
| ----------------- | ----------------------------------- | ---------------------------------------------------- | -------------------------------- |
| A: Agent internal | Always (every tool the model wants) | Layer 1 (adapter env var)                            | Dialog (Layer 2/3 still catches) |
| B: ACP transport  | Layer A let it through              | (no patch — a deliberate choice; we patch C instead) | n/a                              |
| C: Zed UI         | Layer B reached Zed                 | Layer 2A (source) or Layer 3 (dylib)                 | Dialog blocks the agent          |
| D: Native tool    | Zed's first-party agent             | `tool_permissions.default = "allow"` setting         | Dialog blocks first-party agent  |


### 5.3 Compatibility Notes

- **ACP version drift**: the checked-out Zed and codex-acp repos both
pin `agent-client-protocol = =0.11.1`; `zed-yolo-hook` currently pins
`=0.10.4`. The source patch path should align the hook/fallback crate to
Zed's ACP version before sharing public ACP structs across the boundary.
The type that matters for Layer 2A — `RequestPermissionRequest` /
`RequestPermissionResponse` — is stable in shape here, but the patch train
should still record the ACP version in `train.json`.
- **SelectedPermissionOutcome shim**: the dylib transmutes a struct that
matches Zed's *internal* `SelectedPermissionOutcome` (with `params`).
In Layer 2A we send `acp::RequestPermissionResponse` over the wire,
which uses the *public* `acp::SelectedPermissionOutcome` (just
`option_id`). No shim needed.

---

## 6. Risk Register & Mitigations


| Risk                                                                                             | Likelihood | Impact                                                                                                      | Mitigation                                                                                                               |
| ------------------------------------------------------------------------------------------------ | ---------- | ----------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Upstream renames `handle_request_permission`                                                     | Low        | Patch fails to apply                                                                                        | `git am --3way`; CI runs the train daily                                                                                 |
| ACP protocol breaking change to `RequestPermissionResponse`                                      | Low        | Field shape mismatch; runtime error                                                                         | Bump pinned ACP version & retest                                                                                         |
| Zed sets `tool_permissions.default` to allow but adds a second prompt for a "destructive" subset | Medium     | Some tool calls still prompt                                                                                | Layer 2A patch picks `AllowAlways` from any options list, so this still works as long as such an option exists           |
| User runs a stock Zed via `zed-yolo-hook` AND a patched Zed and the two disagree                 | Low        | Confusion                                                                                                   | Layer 3 detects Layer 2 (env var or settings field) and no-ops                                                           |
| Notarization rejects a patched bundle (entitlements drift)                                       | Medium     | DMG won't install on locked-down macOS                                                                      | We strip the same entitlements `bundle-mac` does; see `25_cross_compile_macos_strategy.md` §6                            |
| Adapter env var leaks into shell scripts run *by* the agent                                      | Low        | A script run inside the agent inherits ZED_YOLO and now skips its own confirmations (uncommon but possible) | Use a more specific env var name like `ZED_YOLO_AGENT_BYPASS=1`; or strip it from `process.env` after reading at startup |


---

## 7. References (verified against checked-out source on 2026-05-05)

- **Zed**:
  - `gh-zed-industries__zed/crates/acp_thread/src/acp_thread.rs:526-617` —
  `SelectedPermissionOutcome`, `RequestPermissionOutcome`, `ToolCallStatus`
  - `gh-zed-industries__zed/crates/acp_thread/src/acp_thread.rs:2073-2103` —
  `request_tool_call_authorization`
  - `gh-zed-industries__zed/crates/agent_servers/src/acp.rs:3189-3225` —
  `handle_request_permission`
  - `gh-zed-industries__zed/crates/agent/src/tool_permissions.rs:253-365` —
  `ToolPermissionDecision::from_input`
  - `gh-zed-industries__zed/crates/settings_content/src/agent.rs:607-620` —
  `ToolPermissionsContent`
  - `gh-zed-industries__zed/crates/agent_settings/src/agent_settings.rs:137-170` —
  `AgentSettings { tool_permissions, … }`
  - `gh-zed-industries__zed/docs/src/ai/external-agents.md:275` —
  upstream documentation that external agents "ignore" tool_permissions
- **Claude adapter**:
  - `gh-agentclientprotocol__claude-agent-acp/src/acp-agent.ts:322-324` —
  `ALLOW_BYPASS` gate
  - `gh-agentclientprotocol__claude-agent-acp/src/acp-agent.ts:1419-1573` —
  `canUseTool()`
  - `gh-agentclientprotocol__claude-agent-acp/src/acp-agent.ts:1815-1880` —
  `permissionMode` plumbing into the SDK
- **Codex adapter**:
  - `gh-zed-industries__codex-acp/src/codex_agent.rs:312-410` —
  `build_session_config` and the deterministic initial-config patch point
  - `gh-zed-industries__codex-acp/src/thread.rs:112-204` — preset constants
  and current mode resolution
  - `gh-zed-industries__codex-acp/src/thread.rs:3313-3360` — `handle_set_mode`
  body that issues `Op::OverrideTurnContext`
  - `gh-zed-industries__codex-acp/src/thread.rs:1842-1961` — `exec_approval`
  (the `EventMsg::ExecApprovalRequest` handler we want to never fire)
  - `gh-zed-industries__codex-acp/src/thread.rs:3313-3359` — later
  `handle_set_mode` updates for `"full-access"`
- **External research**:
  - cargo-zigbuild docs and pinned-image smoke validation — see
  `25_cross_compile_macos_strategy.md`
  - osxcross README, tpoechtrager/osxcross — see
  `25_cross_compile_macos_strategy.md`
  - `rcodesign` (indygreg/apple-platform-rs) — Linux-native Apple code
  signing
  - Apple `notarytool` REST API — works from Linux via curl

---

Continue to:

- `24_remote_zed_yolo_strategy.md` for the SSH / `remote_server` flow.
- `25_cross_compile_macos_strategy.md` for build-pipeline details.
- `26_implementation_roadmap.md` for the milestone-by-milestone plan.
