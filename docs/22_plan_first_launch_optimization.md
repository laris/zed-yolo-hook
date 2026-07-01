# Plan: First-launch optimisation for zed-yolo-hook on Zed v1.x

> Companion to `docs/21_triage_v1.1.2_hang_2026-04-30.md`. The triage report
> describes what we know; this plan describes what to do about it.
>
> Status: PROPOSAL — needs user sign-off before any of the implementation
> blocks land. The "small" branch (Option A) is low-risk and lands first.
> The "large" branch (Option B) is opt-in.

---

## 1. Problem we are still trying to reduce

After fixing the helper-storm in `src/process_role.rs`, the residual cost
on the **first launch after each `cargo patch`** is the combination of:

1. **dyld launch-closure rebuild** (macOS: ~5–10 s for a 354 MB Mach-O).
2. **Frida-gum static initialisers** running inside our dylib's
  constructor section, on dyld's critical path before `main()` enters.
3. **Cold session-restore inside Zed itself** — 1660+ AgentThreadEntry
  replays through `AcpThread::push_entry`, each going through our
   Frida interceptor trampoline.

(1) is inherent to binary modification on macOS and resolves itself once
the closure is cached. (2) and (3) we can attack.

Acceptance criteria (subjective, since we cannot eliminate (1)):

- First launch after `cargo patch` should reach Zed's `Rendered first frame` ≤ 5 s on a recent Apple-silicon Mac.
- No `zed::reliability hang detected` in the first 30 s of a launch.
- Tool-call auto-approval still works (verified with one ACP tool call
in the user's normal session).
- All 6 `process_role` unit tests + the 5 existing hook tests still
pass.

---

## 2. Constraints

- **No regressions on Zed v0.233.x.** Some users / branches may still
be on the old marketing line. Anything we add must be a no-op on
binaries where the offsets/symbols/abi haven't moved.
- **No new dependencies for the hook crate.** Frida, tracing, libc,
ctor — that's already a heavy load profile.
- **Listeners must remain `Send`-safe.** Frida's interceptor calls
listeners from the thread that hit the hooked function; mutation of
shared state must stay lock-free or use atomics. (Existing pattern in
`entry_scanner.rs:24-29`.)
- `**#[ctor]` must complete fast.** Anything blocking the ctor blocks
every helper (whether or not we skip them) and Zed's main thread
before `main()` starts. Target: ctor returns in < 5 ms.

---

## 3. Options analysed

### Option A — detach push_entry_hook after registration (small, safe)

**Idea.** Once we've recorded an `AcpThread`* via `register_thread`,
we don't need to be called again for that pointer (or any other —
`upsert_hook` and `session_update_hook` already cover new-thread
discovery). Detach the `push_entry_hook` Frida interceptor entirely
once we've seen ≥ 1 distinct AcpThread.

**Wins.** Eliminates Frida trampoline overhead on every entry insertion
during session restore (1660+ calls). Estimated saving: a few hundred
ms during the cold-restore window — small absolute, but every ms during
the watchdog's 3-s threshold matters.

**Risks.**

- Frida-gum's `Interceptor::detach` re-locks code pages and does
another `mach_vm_protect` round-trip. If we detach too early (before
Zed has finished its restore burst) we save nothing; too late and the
burst is already over.
- We rely on `upsert_tool_call_inner` and `handle_session_update` to
catch the *new* AcpThread that Zed creates after restore. If those
paths can in v1.1.2 *not* fire for a brand-new thread (e.g. some new
code path bypasses both), we lose registration coverage.

**Mitigation.** Trigger detach only after a stable post-restore window
(see § 4.1). Keep `upsert_hook` + `session_update_hook` permanently
attached.

### Option B — defer Frida init out of `#[ctor]` (larger, more invasive)

**Idea.** Spawn a thread from `#[ctor]` whose body runs the current
`init_inner` body (Gum::obtain, 5 symbol searches, 5
`interceptor.attach` calls, scanner spawn, registry register). The
ctor itself does only:

1. `process_role::detect()` (already cheap, ~1-2 ms).
2. `tracing` + config bootstrap (file-system access, ~5 ms).
3. Spawn the worker thread.
4. Return.

**Wins.** Dyld returns to Zed's `main()` immediately after our ctor.
Zed gets to its event loop sooner. `Gum::obtain()` and the symbol
walks happen on a sibling thread in parallel with Zed's startup,
absorbing roughly 50-80 ms of work into a parallel window.

**Risks.**

- **Race on first hooked call.** If Zed calls `push_entry` in the few
ms between ctor return and worker-thread completion, the hook isn't
attached yet, the call passes through unintercepted. For YOLO this
is acceptable (worst case: one tool prompt the user has to click).
For the auto-approve guarantee we'd be relaxing it from "intercept
everything from the very first call" to "intercept everything after
~50 ms of process startup".
- **Frida re-entrancy concerns.** `Gum::obtain` is documented as
process-wide singleton-init; calling it from a non-ctor thread is
supported, but I want to confirm against frida-rust's tests before
shipping.
- **Logging order.** The `=== zed-yolo-hook v0.1.0 ===` line currently
appears in the log before any Zed activity. Under Option B it'll
interleave with Zed's logs and look out of order. Cosmetic.

**Mitigation.**

- Add a short `JoinHandle::join` with timeout at the end of ctor for
cases where the user explicitly opts in to the synchronous path
(env var `ZED_YOLO_SYNC_INIT=1`).
- Stress-test by manually triggering a tool call within 100 ms of
launch.

### Option C — replace push_entry_hook with a pointer-table scrape (big)

**Idea.** Instead of intercepting `push_entry`, periodically scan
Zed's heap for `AcpThread`* instances by signature (struct layout
fingerprint). Removes Frida overhead entirely from the registration
path.

**Wins.** Eliminates two of three Frida hooks.

**Risks.** Heap-scraping is fragile and ABI-coupled in a much harder-
to-reason-about way than offset reads. Hard pass for now.

### Option D — ship Frida-gum-lite

**Idea.** Vendor a stripped frida-gum that drops the Stalker, Cap'n
Proto, and other unused subsystems we never call. Smaller dyld-init
cost.

**Wins.** Could halve the constructor work.

**Risks.** Maintenance burden (we'd be tracking upstream frida-gum
patches against our fork). Not worth it for the marginal saving.

### Selected sequence

Land **Option A** first (small, contained, easy to roll back). Measure.
Then evaluate whether **Option B** is needed.

Skip Options C and D unless A+B together still aren't enough.

---

## 4. Implementation plan — Option A (detach push_entry after first registration)

### 4.1 Trigger condition

Detach when **all** of the following are true:

1. ≥ 1 `AcpThread`* registered in `THREAD_PTRS`.
2. ≥ 2 s elapsed since the *last* new registration (so we don't detach
  mid-restore-burst).
3. Hook has been attached for ≥ 5 s total (avoid detaching immediately
  if Zed restores very fast).

The 5 s + 2 s thresholds should comfortably bracket the 1662-entry
session restore observed in the field (§3.5 of triage doc).

### 4.2 Where to put the detach call

`stale_scanner` thread is the natural home — it already runs every
2 s, has read access to `THREAD_PTRS`, and is unconstrained from
calling Frida APIs (it is *not* in interceptor context).

### 4.3 Code touch points


| File                           | Change                                                                                                                                                  |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/hooks/entry_scanner.rs`   | Add `LAST_REGISTRATION_AT_MICROS: AtomicU64` updated by `register_thread`; expose getter                                                                |
| `src/hooks/push_entry_hook.rs` | Store the `Listener` and the obtained `interceptor` reference somewhere reachable by the scanner thread (likely a `OnceLock<DetachHandle>` in `lib.rs`) |
| `src/hooks/stale_scanner.rs`   | After each scan, check trigger condition; on hit, detach push_entry_hook and log `push_entry: detached after N registrations, T entries seen`           |
| `src/lib.rs`                   | Wire the detach handle                                                                                                                                  |


### 4.4 Concretely

Pseudo-Rust (final code TBD):

```rust
// src/hooks/entry_scanner.rs
pub static LAST_REGISTRATION_MICROS: AtomicU64 = AtomicU64::new(0);

pub fn register_thread(self_ptr: u64) {
    // ... existing body ...
    LAST_REGISTRATION_MICROS.store(now_micros(), Ordering::Relaxed);
}
```

```rust
// src/lib.rs
pub(crate) static PUSH_ENTRY_HANDLE: OnceLock<frida_gum::interceptor::ListenerHandle>
    = OnceLock::new();

// inside init_inner, after attaching push_entry_hook successfully:
let _ = PUSH_ENTRY_HANDLE.set(handle);
```

```rust
// src/hooks/stale_scanner.rs
fn maybe_detach_push_entry(scanner_started_at: Instant) {
    if PUSH_ENTRY_DETACHED.load(Ordering::Relaxed) { return; }
    if THREAD_COUNT.load(Ordering::Relaxed) == 0 { return; }
    if scanner_started_at.elapsed() < Duration::from_secs(5) { return; }
    let last_reg = LAST_REGISTRATION_MICROS.load(Ordering::Relaxed);
    if (now_micros() - last_reg) < 2_000_000 { return; }

    if let Some(handle) = PUSH_ENTRY_HANDLE.get() {
        let mut interceptor = Interceptor::obtain(GUM.get().unwrap());
        interceptor.detach(handle.clone());
        PUSH_ENTRY_DETACHED.store(true, Ordering::Relaxed);
        tracing::info!("push_entry_hook: detached after {} registrations",
                       THREAD_COUNT.load(Ordering::Relaxed));
    }
}
```

(Frida-rust may require slightly different types; will check at
implementation time.)

### 4.5 Verification

- New unit test: simulate `register_thread` calls, advance a fake
clock, assert `maybe_detach` returns true after the right window.
- Live: launch Zed, wait for the new `push_entry_hook: detached`
log line. Quit. Re-launch (no re-patch). Confirm hook still
approves a tool call (via `upsert_hook` / `session_update_hook`).
- `cargo patch --verify` end-to-end.

### 4.6 Rollback

`PUSH_ENTRY_DETACHED` defaults to false; if the detach call panics or
Frida returns an error, we log and bail without setting the flag, so
the hook stays attached. Worst case: same behaviour as today.

---

## 5. Implementation plan — Option B (defer Frida out of ctor)

> Only undertake this if Option A is insufficient.

### 5.1 New `init_inner` shape

```rust
fn init_inner() {
    let role = process_role::detect();
    if role.is_helper() { /* unchanged stderr path */ return; }

    let app_id = config::detect_app_id();
    let cfg = YoloConfig::load(&app_id);
    logging::init(&cfg.log_level);
    tracing::info!("=== zed-yolo-hook v{} ===", env!("CARGO_PKG_VERSION"));
    tracing::info!("DIAGNOSTIC: role={:?} ppid={} pid={} mode={:?} ...",
                   role, ..., cfg.mode);

    if !cfg.is_enabled() { let _ = CONFIG.set(cfg); return; }
    let _ = CONFIG.set(cfg);

    // Spawn the heavy work onto a dedicated thread.
    std::thread::Builder::new()
        .name("yolo-init".into())
        .spawn(move || install_hooks())
        .expect("spawn yolo-init");
}

fn install_hooks() {
    let gum = GUM.get_or_init(|| Gum::obtain());
    let process = Process::obtain(gum);
    let main_module = process.main_module();
    let mut interceptor = Interceptor::obtain(gum);

    /* the existing 5 hook installs and registry register move here verbatim */
}
```

### 5.2 Risk: races against early Zed activity

If Zed calls one of the hooked functions in the window between ctor
return and `install_hooks()` completion, the call goes unintercepted.
Mitigations:

- **Lock-step with Zed's startup.** Empirically, `push_entry` first
fires ~22 s into a launch (the prettier-init line at 23:22:48 in
the log; first AcpThread registered at the same moment). 22 s is
comfortably more than the ~50-80 ms we need to install hooks.
- **Sync override.** Honour `ZED_YOLO_SYNC_INIT=1` to fall back to the
current behaviour for users who prefer the strong guarantee.

### 5.3 Verification

- `cargo build --release` — clean.
- `cargo test --lib` — all green.
- Live: confirm `tracing::info!("Hooks attached")` from the worker
thread appears in the log within 100 ms of the ctor's primary log
line.
- Live: trigger a tool call shortly after launch, verify it is
auto-approved.

### 5.4 Rollback

Single env var (`ZED_YOLO_SYNC_INIT=1`) restores the old behaviour.
Code path stays present but inactive.

---

## 6. Documentation updates required

In addition to landing code, we should:

1. `**docs/23_compatibility_v1.x.md`** — table-style summary of v1.0.0
  Stable + v1.1.2 Preview verification, modeled after
   `docs/20_compatibility_v0.233.3.md`.
2. `**README.md**` — add a "first launch is slow" note next to the
  `cargo patch` instructions, reading approximately:
  > After `cargo patch`, the first launch of Zed will be ~10-30 s
  > slower than usual while macOS rebuilds its dyld launch closure
  > for the modified binary. Subsequent launches use the rebuilt
  > closure and are fast. This is inherent to any binary
  > modification on macOS and is not specific to this hook.
3. `**docs/06_yolo_upgrade_guide.md**` — append a section "Upgrading
  to a v1.x marketing version" listing the empirical findings from
   doc 21.

---

## 7. Sequenced milestones

```
M0  triage doc + plan doc                        ← THIS COMMIT
M1  Option A: detach push_entry after settle     [small commit]
M2  measure first-launch hang on M1; if < 10 s, stop here
M3  Option B (only if M2 insufficient)            [medium commit]
M4  docs/23 + README + upgrade-guide notes        [small commit]
```

Each Mn is a standalone commit suitable for review in isolation.

---

## 8. What is explicitly out of scope

- Modifying the upstream Zed binary or upstream Frida-gum.
- Replacing the injection mechanism (insert-dylib via LC_LOAD_WEAK_DYLIB
remains).
- Investigating the 11-second silent window in Zed's startup
(§3.3 in triage). That belongs in `zed-industries/zed`'s issue
tracker, not this repo.
- Rolling forward to a Frida version newer than 17.9.1 (separate
upgrade exercise).

---

## 9. Decision needed before starting

The user picks a starting point:

- **A.** "Land Option A and stop unless evidence demands more."
- **B.** "Skip A, go straight to Option B for maximum effect."
- **A+B.** "Land both eventually; A first."
- **None.** "Document but don't change code; treat first-launch slowness
as expected behaviour."

My recommendation: **A**. Cheapest commit, easiest rollback, addresses
the only on-CPU work our hook does in the first second of Zed v1.x's
critical startup window. Defer B until evidence shows A wasn't enough.