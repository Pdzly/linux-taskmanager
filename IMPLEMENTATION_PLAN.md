# TaskMonitor — Implementation Plan

A lightweight, modern task manager for Linux with everything Windows Task Manager has, in a soft card-based UI (the attached reference), set in **Mona Sans**.

---

## 1. Tech stack decision

**Recommendation: Rust + Slint, reading `/proc` and `/sys` directly.**

| Option | Idle RAM (approx.) | Custom "card" UI | Verdict |
|---|---|---|---|
| **Rust + Slint** | ~20–40 MB | Excellent: declarative `.slint` markup, rounded cards, pills, gradients, custom fonts | ✅ **Pick** |
| Rust + iced | ~30–50 MB | Good, but every custom widget is hand-written; no built-in virtualized table | Runner-up |
| Rust + GTK4/libadwaita | ~50–80 MB | Fights you: libadwaita pushes the GNOME look; the reference design isn't GNOME | Native, but wrong look |
| Rust + egui | ~30 MB | Easy, but immediate mode redraws constantly; bad for idle CPU in a monitor | ❌ |
| Tauri (Rust + WebKitGTK) | ~120–200 MB | Trivial with CSS | ❌ Too heavy for a monitor |
| Electron | 250 MB+ | Trivial | ❌ |

Why **Rust**: no GC pauses, tiny static binary, near-zero-cost `/proc` parsing, and safe concurrency between the sampling thread and the UI. Nothing beats it here: C/C++ isn't meaningfully faster and is riskier; Go and C# carry a runtime and GC.

Why **Slint**: the reference UI is a custom design (white rounded cards on a light grey canvas, pill chips, coloured initial badges, huge numerals). Slint is built for this. It renders retained-mode, so it only redraws when something changes. It has a virtualized `ListView` for 500+ process rows, `Path` for live graphs, and bundled font import. Licence: Slint's royalty-free licence covers desktop apps (attribution required), or GPLv3.

### Crates

| Concern | Crate | Notes |
|---|---|---|
| UI | `slint` with the **software renderer only** | Measured in M0: the OpenGL renderer pulled in ~100 MB of Mesa/NVIDIA driver libraries (RSS 131 MB vs 31 MB) and can wake the dGPU. Same visuals. |
| `/proc` parsing | `procfs` | Full coverage: stat, statm, status, io, fd, cgroup, net |
| Signals, nice, affinity | `nix` | `kill`, `setpriority`, `sched_setaffinity` |
| systemd, logind, UPower | `zbus` (pure Rust D-Bus) | Services tab, Users tab, polkit auth |
| NVIDIA GPU | `nvml-wrapper` | Loaded at runtime with `dlopen`; works without NVIDIA |
| Intel/AMD GPU | read sysfs + DRM `fdinfo` directly | Per-process GPU % with no root needed |
| Settings | `serde` + `toml` | `~/.config/taskmonitor/config.toml` |
| App history | `rusqlite` (bundled) | Optional; only if app history is enabled |
| Errors/logging | `anyhow`, `log` + `env_logger` | |

Deliberately **not** used: `sysinfo` (convenient, but it hides `/proc` details we need, such as I/O, cgroups, fdinfo and per-thread data), `tokio` (one sampling thread plus D-Bus blocking calls is enough; `zbus` can run in blocking mode).

### Performance budget (CI-enforced later)

- **CPU:** < 1% of one core at a 1 s refresh with ~500 processes
- **RAM:** < 40 MB RSS
- **Binary:** < 15 MB stripped (LTO, `codegen-units=1`, `panic=abort`, `strip`)
- **Cold start:** < 200 ms to first frame

---

## 2. Feature scope (Windows Task Manager parity)

| Windows tab | TaskMonitor on Linux | Data source |
|---|---|---|
| **Processes** | Apps vs background processes, grouped by app; CPU %, memory, disk MB/s, network, GPU %; expandable child rows | `/proc/[pid]/{stat,statm,status,io,cgroup}`, `.desktop` matching, DRM `fdinfo` |
| **Performance** | CPU (total + per-core graphs, frequency, uptime, processes/threads, cache sizes, virtualization); Memory (used/available/cached/swap/committed, composition bar); each Disk (active time, read/write, response time); each NIC (send/recv, IPv4/IPv6); each GPU (utilization, VRAM, temperature, power) | `/proc/stat`, `/proc/meminfo`, `/proc/diskstats`, `/sys/block`, `/proc/net/dev`, `/sys/class/net`, `/sys/class/hwmon`, NVML, sysfs |
| **App history** | Cumulative CPU time, network and disk per app since tracking began | Sampled aggregates in SQLite (opt-in) |
| **Startup apps** | Enable/disable autostart entries; startup impact (measured) | `~/.config/autostart/*.desktop`, `/etc/xdg/autostart`, systemd `--user` units |
| **Users** | Logged-in users/sessions with per-user CPU/memory; log off, disconnect | logind over D-Bus + per-UID aggregation |
| **Details** | Flat table: PID, name, user, status, CPU, memory (private/shared), threads, nice, cmdline, cgroup, start time, I/O; column chooser | `/proc/[pid]/*` |
| **Services** | systemd units: status, description, PID; start/stop/restart, enable/disable | systemd over D-Bus (polkit handles auth) |
| **Actions** | End task (SIGTERM), Kill (SIGKILL), End process tree, Suspend/Resume (SIGSTOP/CONT), set priority (nice), set affinity, open file location, properties, **Efficiency mode** (nice 19 + `SCHED_IDLE`), Run new task | `nix`, `xdg-open`, `pkexec` helper for elevation |
| **Extras** | Search/filter, sortable columns, refresh speed (0.5/1/2/5 s, paused), always on top, light/dark theme, keyboard shortcuts (`Ctrl+Shift+Esc` via a desktop shortcut, `Del` = end task) | |

### Linux caveats to plan for

- **Per-process network bandwidth** is not exposed by `/proc`. It needs eBPF or packet capture with root. Ship v1 with a per-process **connection count** (`sock_diag` / `/proc/net/tcp*` mapped to inodes). Add bandwidth later through an optional privileged eBPF helper.
- **Elevated actions** on processes you don't own, or setting a negative nice value, go through a tiny `taskmonitor-helper` binary started with `pkexec` and a polkit policy. The UI never runs as root.
- **Wayland** (your session) has no global "window to PID" API. Detect apps from `.desktop` files plus cgroup scopes (`app-gnome-firefox-1234.scope`), which is how GNOME and KDE tag apps.
- **Flatpak** puts the app in its own PID namespace, so it would only see itself. Ship native packages first (`.deb`, `.rpm`, AppImage, AUR). Flatpak needs a host-side helper started with `flatpak-spawn --host`, which is how Mission Center does it.
- **Your machine** has hybrid graphics (Intel Iris Xe + RTX 3060 Mobile), which is ideal for testing both GPU paths. Don't let NVML calls wake a sleeping dGPU: check `/sys/bus/pci/devices/…/power/runtime_status` first and show "Suspended".

---

## 3. Architecture

```
taskmonitor/                      (cargo workspace)
├── crates/
│   ├── core/                     # no UI deps; unit-testable against fixture /proc trees
│   │   ├── sampler.rs            # timer loop, builds Snapshot, computes deltas
│   │   ├── cpu.rs  mem.rs  disk.rs  net.rs  gpu/{nvidia,drm}.rs
│   │   ├── process.rs            # per-PID state + delta math, app grouping
│   │   ├── services.rs users.rs startup.rs   # D-Bus / XDG
│   │   └── actions.rs            # kill, renice, affinity, efficiency mode
│   ├── app/                      # Slint UI binary "taskmonitor"
│   │   ├── ui/                   # .slint files: theme, components, one file per page
│   │   ├── fonts/MonaSans-*.ttf
│   │   └── src/{main.rs, bridge.rs}   # Snapshot → Slint models
│   └── helper/                   # tiny privileged binary + polkit policy
└── packaging/  (deb, rpm, AppImage, .desktop, icons, polkit .policy)
```

**Data flow**

1. A **sampler thread** wakes every *N* ms. It reads only what the **visible page** needs, so the Services tab never parses 500 `/proc/[pid]/io` files. It produces an immutable `Snapshot`.
2. Deltas (CPU %, MB/s) are computed in `core` from the previous sample. Ring buffers of 60–300 points feed the graphs.
3. The snapshot goes to the UI through `slint::invoke_from_event_loop`. `bridge.rs` **diffs it by PID** into `VecModel`s (update the row in place, insert, remove). Selection and scroll position survive refreshes and Slint repaints only changed rows.
4. User actions go from the UI callback to `core::actions`. If the action gets `EPERM`, it retries through the helper over `pkexec`.

**Optimisation rules**

- Reuse read buffers, and open `/proc/[pid]` files with `openat`-style relative reads.
- Cache static per-PID data (cmdline, exe, user name, matched `.desktop` file) and invalidate it when the PID's start time changes, which handles PID reuse.
- When the window is minimised or hidden, sample only what the tray/summary needs, or pause.
- Never block the UI thread on I/O or D-Bus.

---

## 4. UI design system (from the reference)

| Token | Value | Use |
|---|---|---|
| Canvas | `#EDEDED` | App background |
| Card | `#FFFFFF`, radius 24 px, padding 20 px, no border, very soft shadow | Every panel |
| Inner tile | `#F5F5F5`, radius 16 px | Nested cards (like the Team Collaboration rows) |
| Accent | `#FFD84D` yellow header band | Hero card (CPU) and the selected item |
| Status dots | green `#22C55E`, red `#EF4444` | Running, suspended or zombie; service up or down |
| Badges | Circles in green, blue, orange, pink and yellow with white initials | Process/app/user avatars when no icon is available |
| Danger | `#F25555` round button | "End task" (the red logout button in the reference) |
| Text | `#111` primary, `#9A9A9A` secondary | |
| Dark theme | Canvas `#0F0F10`, card `#1A1A1C`, tile `#232326`, same accent | |

**Typography (Mona Sans):** bundle the static TTFs (Regular, Medium, SemiBold) in `app/fonts/` and register them with Slint's font `import`. Mona Sans is SIL OFL licensed, so bundling is fine. Use display numerals at about 72–96 px Light, like the "10" and "5.7" in the reference, for headline stats (CPU %, RAM GB). Use 13–14 px for body text and 11–12 px secondary for captions. Numeric table columns must not jitter: use tabular figures if Slint exposes the `tnum` feature, otherwise give the columns a fixed width and right-align them.

**Mapping the reference to TaskMonitor:**

- **Left icon rail** → Processes, Performance, App history, Startup, Users, Details, Services. Settings sits at the bottom, with a red "End task" button for the selected process below it.
- **Top pill bar** ("On Meet …") → a live summary pill (CPU 23% · RAM 9.1/16 GB · GPU 4%) with a mini sparkline. Search sits on the right, then a refresh-rate menu, then a user pill showing your user name, initials badge and uptime.
- **Yellow "To do list" card** → **CPU hero card**: total %, a per-core mini-bar grid, frequency and the top 5 processes.
- **"Reminder / Sunday 10" card** → **Memory card**: a huge "9.1 GB" numeral with a composition bar (used, cached, free, swap).
- **"Team Collaboration"** → **Users** (badges with green online dots) on the dashboard, and the full Users page.
- **Angled tabs** ("Project Overview / Notifications Panel") → page sub-tabs (Apps | Background; or CPU | Memory | Disk | Network | GPU on Performance).
- **"Detailed Report" with Mon/Tue columns** → Disk and Network stat columns (Read 3.2 MB/s ↑, Write 0.4 MB/s ↓).
- **"AI Chat" input** → the **Run new task** command box ("Run a command…" with a send button).
- **"Performances 5.7"** → **Uptime / load average** card.

---

## 5. Milestones

Each milestone ends with something you can run.

### M0: Skeleton (½ day) ✅ done
Result: 31 MB RSS, ~0% idle CPU, 15.8 MB binary (0.8 MB over budget; the embedded fonts are about that, so revisit in M7).

- Cargo workspace; release profile (LTO, strip, `panic=abort`).
- Slint window with the theme tokens, Mona Sans loaded, left rail, top bar and empty cards in light and dark.
- **Done when:** `cargo run --release` shows the styled shell with an RSS under 30 MB.

### M1: Core sampler + Performance page (3–4 days) ✅ done
Result: numbers match `free -b`, `top` (TaskMonitor counts I/O wait as idle, like Windows), `/proc/loadavg`. Sampling costs 0.54 ms per tick. Rendering costs ~7 ms per update frame (headless benchmark: `TASKMONITOR_BENCH=1`), about 1.1–2.2% of one core at 1 s refresh on this laptop: over the 1% target.
Lessons: (1) never `animate` values that change every tick (each animation re-renders every frame for its duration; removing two halved CPU); (2) the software renderer re-rasterizes SVG images on every draw, so icons are a subset Lucide font (`tools/build_icons.py`); (3) staggering updates across frames is slower (fixed per-frame cost). Follow-up: profile the per-frame cost with `--features profiling` and get under 1%.

- CPU total and per-core, frequency, load average, uptime, process and thread counts.
- Memory from `meminfo`, including swap and the composition bar.
- Disks and NICs with rates, active time and response time.
- Ring buffers and line/area graphs drawn with Slint `Path`.
- Refresh-rate menu and pause.
- **Done when:** the numbers match `top`, `free -h`, `iostat -x 1` and `ip -s link` within rounding.

### M2: Processes + Details (4–5 days) ✅ done
Result: apps detected from systemd scopes (`app-gnome-*`, `app-flatpak-*`, `snap.*`) matched to `.desktop` names; Apps / Background / System sections with expandable groups, heat-tinted CPU/memory/disk cells, sortable columns, search; Details lists every process including kernel threads. Process sampling: 6–8 ms per tick for ~480 processes (only while those pages are open); whole app ~2.1% CPU, 34 MB on the Processes page. Follow-up: shared with M1's render-cost work.

- Per-process CPU, memory, I/O and threads, with cached static info and PID-reuse detection.
- App grouping (cgroup scope plus `.desktop`), Apps vs Background sections, expandable trees.
- Virtualized sortable table, search, column chooser, and in-place model diffing.
- **Done when:** 500+ processes scroll at 60 fps and TaskMonitor itself stays under 1% CPU.

### M3: Process actions (2 days) ✅ done (elevated path not yet tried with a real password prompt)
Result: right-click menu on Processes and Details rows (End task, End process tree, Kill, Suspend/Resume, Efficiency mode, Set priority, Set affinity…, Open file location, Properties), End task button and rail button, confirmation for Kill and End process tree, Run new task, toasts. Actions run directly; EPERM/EACCES (raising priority returns EACCES) are retried in one batch via `pkexec taskmonitor-helper`, which refuses PID 0/1 and takes numeric arguments only. Binary grew to 17.9 MB: size work moves to M7.

- End task, Kill, End tree, Suspend/Resume, priority, affinity dialog, Efficiency mode, open file location, properties sheet.
- Helper binary, polkit policy and `pkexec` path for `EPERM`; confirmation dialogs for destructive actions.
- **Done when:** you can kill your own process directly and a root process after a polkit prompt.

### M4: GPU (2–3 days) ✅ done
Result: Intel Iris Xe via i915 fdinfo (per-process and total, own processes only) + sysfs clocks; NVIDIA RTX 3060 via NVML (usage, VRAM, temperature, clock, power, per-process). AMD paths (gpu_busy_percent, VRAM, hwmon) written but untested on this machine. NVML is only opened on the GPU tab or Processes page while the card is awake, and dropped elsewhere so the dGPU can power down. GPU tick 1.6 ms without NVML, ~9 ms with. Loading NVML adds ~20 MB RSS.

- NVIDIA through NVML: utilization, VRAM, temperature, power, per-process usage. Respect dGPU runtime suspend.
- Intel/AMD through sysfs and DRM `fdinfo` engine counters, both per-GPU and per-process.
- **Done when:** both of your GPUs show up and a game or video shows per-process GPU %.

### M5: Services, Startup, Users (3 days)
- systemd units over D-Bus: list, filter, start/stop/restart, enable/disable.
- Autostart `.desktop` plus `systemd --user` toggles; startup impact from `systemd-analyze` data where available.
- logind sessions with per-UID aggregation, plus log off/terminate session.

### M6: App history + polish (3 days)
- Opt-in SQLite aggregates every 60 s, pruned after 30 days.
- Settings (theme, refresh rate, default page, always on top), keyboard shortcuts, accessibility labels, empty and error states, and a "requires root" hint for network bandwidth.

### M7: Packaging & CI (2 days)
- `.desktop` file, icons, `cargo-deb`, `cargo-generate-rpm`, AppImage, AUR `PKGBUILD`.
- CI: `clippy -D warnings`, tests against fixture `/proc` trees, a binary-size check and a release build.

**Later:** per-process network bandwidth through an eBPF helper; container/cgroup view (Docker/Podman); temperatures and fans page; Flatpak with a host helper; a tray icon with a mini graph.

---

## 6. Testing strategy

- **`core` unit tests** run against captured fixture directories (`tests/fixtures/proc-*`) so parsing and delta math are deterministic and don't depend on the live machine.
- **Cross-check tests** (ignored by default, run locally) compare TaskMonitor's numbers with `ps`, `free` and `/proc/stat` sampled at the same moment.
- **Perf guard:** a headless benchmark samples 1,000 synthetic PIDs and fails if one tick takes longer than 5 ms.
- **Manual QA matrix:** GNOME and KDE on Wayland, X11, Intel-only and NVIDIA hybrid setups, and a machine with no swap.

---

## 7. Reference projects worth reading (don't copy; learn the gotchas)

- **Mission Center** (Rust + GTK4): Windows-like Performance pages, GPU through fdinfo/NVML, and its Flatpak helper approach.
- **Resources** (Rust + GTK4): app detection from cgroups and `.desktop` files.
- **bottom / btop**: efficient `/proc` sampling patterns.

---

## 8. Open decisions

1. **Licence:** Slint royalty-free (attribution) vs GPLv3. Pick before the first public release.
2. **App history:** opt-in (recommended for privacy and footprint) or on by default?
3. **Per-process network bandwidth:** worth an eBPF root helper in v1, or defer?
