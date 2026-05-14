# Fix: Make Linux-SoundBoard route app streams reliably (EasyEffects-grade)

## Context

The user reports: "It fails to imitate same workflow as EasyEffects, and sets mic as sys. default, but still games and apps doesn't see it." Goal: user's voice keeps EasyEffects processing while soundboard clips bypass it, and **every** app — Discord, OBS, Firefox WebRTC, TF2 (`tf_linux64`), browsers — sees soundboard mic input without manual reconfiguration.

### What the code already does right (don't touch)

The previous refactor (documented in `plan to update.md`) already landed correctly:

- **Single null-sink virtual mic** — `packaging/pipewire/99-linuxsoundboard.conf` is correct (one `Audio/Source/Virtual` node, `priority.session = 0`, EasyEffects pattern).
- **Explicit graph links** — `src/app/audio/player/explicit_links.rs:149-214` creates `link-factory` FL+FR links from `linuxsoundboard.virtual_mic_feeder` to `linuxsoundboard.virtual_mic`. Confirmed working in `LOGS.playback.20260508-003028.log`:
  ```
  [PWLINK] linuxsoundboard.virtual_mic:input_FL
  [PWLINK]   |<- linuxsoundboard.virtual_mic_feeder:output_FL
  ```
- **EasyEffects-style stream router exists** — `src/app/audio/player/source_autoroute.rs` (665 lines) already:
  - binds to the `default` PipeWire metadata object (`bind_default_metadata_from_global`, line 52)
  - tracks every `Stream/Input/Audio` node and its app props (lines 228-249)
  - writes `target.node` + `target.object` metadata onto qualifying streams (lines 315-327) — exactly mirroring EasyEffects `set_metadata_target_node()`
  - filters out own streams, EasyEffects/NoiseTorch internals, `node.dont-move`, `stream.capture.sink`, monitor sources (lines 399-494)
  - has unit tests covering all skip cases (lines 521-664)
- **Routing-mode enum exists** — `src/app/config/types.rs:186` `DefaultSourceMode` with default `AutoRouteWhileRunning` (types.rs:302-303). Good default.
- **Mix-graph topology is correct** for the user's goal:
  - `easyeffects_source` (user voice w/ effects) → soundboard mic-capture → `virtual_mic_feeder` → `virtual_mic` ✓
  - soundboard playback → `virtual_mic_feeder` directly, **bypassing** `easyeffects_sink` for the mic path ✓
  - `linuxsoundboard.local_playback` → `easyeffects_sink` (for what the user hears on their own speakers — fine; this is output effects, not mic effects)

So the **architecture is right**. The remaining issues are configuration, sequencing, and a few legacy paths.

### Actual gaps causing the symptom

1. **Default-source claiming is still in the install path** — `packaging/linux/install-user.sh:439, 446` calls `wpctl set-default` / `pactl set-default-source` during install. This is exactly what the research doc says **not** to do; it produces the symptom "sets mic as sys. default."
2. **Runtime default claim still wired up** — `src/app/audio/player/source_routing.rs:185-239` `spawn_default_source_claim()` calls both `wpctl set-default` and `pactl set-default-source` when modes other than `AutoRouteWhileRunning` are picked. Acceptable when intentional, but currently triggered in too many code paths.
3. **`wpctl inspect @DEFAULT_SOURCE@` is timing out** — logs show repeated `wpctl inspect @DEFAULT_SOURCE@ timed out after 900 ms` (`source_routing.rs`). Each timeout kicks fallback logic and burns a worker thread. Should read default from the already-bound `default` metadata object directly, not shell out to `wpctl`.
4. **No `linking.allow-moving-streams` check** — WirePlumber must have this setting enabled for metadata routing to work. If a user/distro has it off, autoroute silently does nothing and there's no diagnostic. Grep `linking.allow-moving-streams` finds nothing in the codebase.
5. **No startup scan of pre-existing input streams** — `source_autoroute.rs` reacts to registry `global_added` events. PipeWire's registry replays existing globals on connection, but if the **virtual mic** is bound after some input streams (timing-dependent), those streams' `target.object` is never (re-)evaluated. There's no `maybe_autoroute_input_streams(state)` call on the moment the virtual mic node first appears.
6. **No diagnostic surface** — when autoroute "doesn't work" for an app like TF2, there's no way for the user (or us) to see why: was the stream filtered? not seen as `Stream/Input/Audio`? blocked by `dont-move`? blocked by `linking.allow-moving-streams=false`? The app needs a `--diagnose` (or in-UI panel) that prints the routing decision per current stream.
7. **TF2 / Source-engine reality check** — Source engine voice may register as `Stream/Input/Audio` *with `node.dont-move=true`* or may use Steamworks voice that doesn't appear in PipeWire at all. The diagnostic above tells us which.
8. **Filter for `easyeffects.` is anchored on prefix** — `is_processor_internal_stream` (line 466) skips `easyeffects.` and `ee_`. EasyEffects' *own* in-process streams are named `easyeffects_input` / `easyeffects_output`. The prefix check `starts_with("easyeffects.")` (note dot) won't match `easyeffects_input` (underscore). Need to verify behavior — may already be caught by app_name fuzzy check at line 482, but worth tightening.

## What changes — focused, surgical

### A. Strip default-source claiming from the install path

| File | Change |
|------|--------|
| `packaging/linux/install-user.sh:432-447` | Remove or gate behind `--legacy-default-mode` flag. The default install must **not** call `wpctl set-default` / `pactl set-default-source`. |
| `packaging/linux/install-user.sh:449` (`restore_preinstall_default_source`) | Keep — needed for clean uninstall if a previous install pinned the default. |
| `src/app/pipewire/persistent_mic.rs:579` (`pactl set-default-source`) | Remove from auto path; allow only when called by `set_default_source_mode(AlwaysDefault)` explicitly. |

### B. Drop the `wpctl inspect` shell-out — read from metadata proxy

| File | Change |
|------|--------|
| `src/app/audio/player/source_routing.rs:307+` `current_default_source_name()` | Replace shell-out with a read from `state.default_metadata` (the proxy already bound in `source_autoroute.rs:52`). Default source is metadata key `default.audio.source` on subject `0` of the `default` metadata object. Listen on the property callback (the listener at `source_autoroute.rs:68-83` already runs — extend it). |
| `src/app/audio/player/source_routing.rs:180-183` | Source the "previous default" from metadata instead of `wpctl`. |

### C. Startup scan on virtual-mic-appearance

| File | Change |
|------|--------|
| `src/app/audio/player/source_autoroute.rs:174` `maybe_autoroute_input_streams` | Already exists. Need a **caller** in the path where the virtual mic node global first arrives. Find that path in `src/app/audio/player/mod.rs` registry callback (around lines 1549-1677, where `sources.insert` happens for the virtual mic). After the insert, call `maybe_autoroute_input_streams(state)` so any input streams that arrived first get re-evaluated. |

### D. WirePlumber settings preflight + auto-enable

| File | Change |
|------|--------|
| `src/app/init/audio.rs` (or extend `src/app/pipewire/detection.rs`) | At startup, run `wpctl settings linking.allow-moving-streams` to read current value. If `false` or unset, log a clear warning and attempt `wpctl settings --save linking.allow-moving-streams true`. WirePlumber 0.5+ supports this. If write fails (older WirePlumber or perms), surface a diagnostic with the manual fix command. |
| `docs/` | Add a short troubleshooting note pointing at this preflight. |

### E. Diagnostic command — `linux-soundboard --diagnose`

| File | Change |
|------|--------|
| `src/app/main.rs` (or `lib.rs`) | Add a `--diagnose` CLI flag. Runs a one-shot probe and prints to stdout. |
| New: `src/app/diagnostics/routing.rs` | Implements the probe: detects audio server, finds `linuxsoundboard.virtual_mic` id+serial, reads `linking.allow-moving-streams`, enumerates every `Stream/Input/Audio` via PipeWire registry, prints for each: id, node.name, app.name, app.process.binary, current `target.object`, `dont_move`, `stream.capture.sink`, and the **decision** the autoroute filter (`input_stream_should_autoroute`) makes for it — with the **reason** if blocked. |

This is the one piece that closes the "I don't know why app X doesn't work" loop forever. Reuses `input_stream_should_autoroute` from `source_autoroute.rs:399` so logic stays in one place.

### F. Tighten the EasyEffects-internal filter

| File | Change |
|------|--------|
| `src/app/audio/player/source_autoroute.rs:467-468` | Change `starts_with("easyeffects.")` to also catch `starts_with("easyeffects_")` (EasyEffects' real node names use underscore). Add a test fixture covering `easyeffects_input` / `easyeffects_output` node names. |

### G. Better autoroute observability in the existing log path

| File | Change |
|------|--------|
| `src/app/audio/player/source_autoroute.rs:330` "Routed input stream" | Already exists. Add a symmetric line at **filter-out** time inside `input_stream_should_autoroute` (or in `maybe_autoroute_input_stream` after the filter call) at `debug!` level: `Skipped routing '{app}' (reason: {dont_move|capture_sink|own|processor_internal|external_target=…})`. Lets users grep the log to find why TF2 (or whoever) was skipped. |

### H. PulseAudio-only path: equivalent stream moving

| File | Change |
|------|--------|
| `src/app/audio/player/pulse_backend.rs` | Already exists (449 lines). On pure PulseAudio (no PipeWire), there is no `default` metadata. Use `pactl subscribe` to watch `source-output` events and `pactl move-source-output <id> linuxsoundboard.virtual_mic` for qualifying inputs. Hide the difference behind the same trait the autoroute logic already calls. |

Out-of-scope-but-worth-noting: if the user is on PipeWire (visible by `pactl info` reporting `PulseAudio (on PipeWire ...)`), pipewire-pulse handles compat. Pure PulseAudio is the corner case — Debian 11 / Ubuntu 22.04 minus pipewire.

## Critical files

| File | Lines | Role |
|------|-------|------|
| `src/app/audio/player/source_autoroute.rs` | 1-665 | Already implements EE-style routing. Extend filter (F), add filter-out debug log (G), add startup-scan caller (C) |
| `src/app/audio/player/source_routing.rs` | 180-310 | Strip `wpctl inspect` shell-out (B); keep `claim_default_source` only for explicit-opt-in modes |
| `src/app/audio/player/mod.rs` | 1549-1677 (registry) | Call `maybe_autoroute_input_streams(state)` when virtual mic source first inserted (C) |
| `src/app/pipewire/persistent_mic.rs` | 579 | Drop unconditional `pactl set-default-source` |
| `src/app/config/types.rs` | 186-217, 302-303 | Verify `AutoRouteWhileRunning` is the on-disk default for fresh installs; add a config-migration to demote any user who has `AlwaysDefault` set from prior installer bug (D) |
| `packaging/linux/install-user.sh` | 432-447 | Remove unconditional default-source claim |
| `src/app/init/audio.rs` | (new code) | Preflight `linking.allow-moving-streams` (D) |
| `src/app/diagnostics/routing.rs` | (NEW) | `--diagnose` printer (E) |
| `src/app/main.rs` | CLI parse | Wire up `--diagnose` flag (E) |

## Reuse — don't reinvent

- `source_autoroute.rs:399` `input_stream_should_autoroute` — single source of truth for the filter. `--diagnose` MUST call this, not a re-implementation.
- `source_autoroute.rs:316-327` metadata write — already does the right Spa:Id formatting; don't duplicate.
- `explicit_links.rs:149-214` — link-factory pattern. Reuse the same `Core` handle when binding metadata (already done).
- `pulse_backend.rs` already exists; extend with stream-move logic rather than starting fresh.

## Verification

End-to-end (manual), in this exact order:

1. **Build & install** (no system writes from app yet):
   ```
   cd /home/flinux/opencode/LinuxSoundBoardv1
   cargo build --release
   ```
2. **Clean slate**:
   ```
   pactl get-default-source                          # save the value, must NOT be linuxsoundboard.* after our fix
   wpctl settings linking.allow-moving-streams       # should print true after preflight (D)
   ```
3. **Launch soundboard**, then in a second terminal:
   ```
   pw-metadata -n default -m                          # live-watch metadata writes
   ```
4. **Open apps one at a time** and confirm a `target.object` write fires for each:
   - Firefox WebRTC mic test page
   - Discord (start a call to "Echo / Sound Test")
   - OBS (add an Audio Input Capture source)
   - TF2 (`tf_linux64`) — voice chat enabled, then watch metadata stream
5. **Speak into mic** — voice should reach the app **with** EasyEffects processing (since soundboard pulls from `easyeffects_source`).
6. **Trigger a soundboard clip** — clip should reach the app **without** EasyEffects processing (feeder → virtual_mic direct path).
7. **Disable autoroute** in soundboard UI — expect `pw-metadata -n default -m` to show `target.node` / `target.object` clears on each tracked stream.
8. **Default source unaffected**:
   ```
   pactl get-default-source                          # still the user's pre-install value (e.g. easyeffects_source)
   ```
9. **Diagnostic command**:
   ```
   ./target/release/linux-soundboard --diagnose
   ```
   For each `Stream/Input/Audio` it should print a one-line decision and reason. This is the key tool for ongoing TF2/Discord/etc. debugging.
10. **WirePlumber restart**:
    ```
    systemctl --user restart pipewire pipewire-pulse wireplumber
    ```
    Within ~1 s soundboard should re-bind metadata and rebuild links. Discord call audio should not drop for >1 s.
11. **Uninstall** (`install-user.sh --uninstall`):
    ```
    pactl get-default-source                          # restored to step-2 pre-install value
    pactl list short sources | grep linuxsoundboard   # empty
    ```

Unit tests:
- Extend `source_autoroute.rs` test suite with a case for `easyeffects_input` node name (gap F).
- Add tests for the new `--diagnose` decision printer with synthetic fixtures.
- Keep all 277 existing tests green.

## Recovery commands the user should run before testing the fix

The user's system likely has stale state from earlier iterations. Run once before validating:

```
rm -f ~/.config/pipewire/pipewire.conf.d/99-linuxsoundboard.conf
sudo rm -f /usr/share/pipewire/pipewire.conf.d/99-linuxsoundboard.conf 2>/dev/null
rm -f ~/.local/state/wireplumber/default-nodes
rm -f ~/.local/state/wireplumber/restore-stream
pactl list short modules | awk '/null-sink/ && /linuxsoundboard/ {print $1}' | xargs -r -n1 pactl unload-module
pkill -f linux-soundboard 2>/dev/null
# Restore default source explicitly if pinned to soundboard:
wpctl set-default "$(pactl list short sources | awk '/alsa_input/ {print $1; exit}')"
systemctl --user restart wireplumber pipewire-pulse pipewire
```

## Out of scope

- Replacing `explicit_links.rs` — current approach is correct and matches EasyEffects' `pw_link_manager.cpp`.
- Output-side processing (Stream/Output/Audio → some sink). Soundboard's job is mic-side only.
- Flatpak/sandbox permissions for Discord — separate issue documented in `freeze-bt.txt`.
- Replacing the in-process feeder fallback for unsupported audio servers.
- Touching the persistent PipeWire conf — it's already correct.
