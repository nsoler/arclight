# Arclight: notes for Claude sessions

A node-based VFX compositor in Rust (ArclightVFX Compositor, beta): 2D
graph, 3D scene and renderer (CPU + wgpu GPU), procedural geometry,
particles, splats, MCP server, Python (`arclt`). Developed on Linux
(Omarchy / Hyprland); the GPU here is an RTX 5070, 12 GB.

**Read first:** `HANDOFF.md` (status, architecture, release procedure),
`TODO.md` (open items, hand checks), `ROADMAP.md` and the current
`docs/*-plan.md`; `README.md` for features and the file format.

## Team: one Supervisor, two Devs (herdr)

Three Claude sessions in herdr share this checkout. The user talks to the
Supervisor; the Supervisor talks to Dev1 and Dev2 (`ListAgents` /
`SendMessage`; herdr names like `nsoler-xx`, Devs by start order). Ask
which role you have if it isn't clear.

**Supervisor**
- Owns all of git: commits, pushes, fast-forwards `main`, version bumps,
  CHANGELOG headers, tags, releases, and keeps HANDOFF / TODO current.
- Splits each request into one task per Dev, with the core in new files
  that Dev owns; approves each Dev's plan before code; runs the locks.
- Verifies before committing: stages only that Dev's paths / hunks,
  exports the index (`git checkout-index -a --prefix=../.verify/sup-<ver>/`)
  and runs tests + clippy there (`CARGO_TARGET_DIR=../.verify/target-sup`;
  `gpu --check` if GPU code changed). Never `git add -A`.
- One version per Dev milestone (bump, CHANGELOG, TODO hand check, HANDOFF
  note). Where both Devs edited one file, stage each Dev's hunks
  separately (`git update-index --cacheinfo`) so every commit builds.
- Releases only when the user says so (procedure in HANDOFF.md). Keeps
  the user posted; the user's decisions (phases, licensing, what next)
  go to the user, not the Devs.

**Dev1 / Dev2**
- No git writes at all: no commit, push, tag, add, stash, reset,
  checkout; no version bumps.
- Send a plan first (approach, storage choices, every file, shared ones
  marked) and wait for approval.
- Shared files (`lib.rs`, `ops.rs`, `gpu/*`, `gui/*`, `README.md`,
  `docs/help.md`, …): `LOCK <file>`, wait for "granted", edit in one
  burst, `UNLOCK` as soon as it builds. Never edit the other Dev's lines;
  tell the Supervisor.
- Done = `cargo test --release` and `cargo clippy --release
  --all-targets` clean, CPU / GPU agreeing in `gpu/check.rs`, old projects
  unchanged.
- Final report: title, exact paths (your hunks in shared files),
  CHANGELOG bullet, HANDOFF note, TODO hand-check line, what wasn't
  verified. Then stand by.

## Build and test

```sh
cargo run --release --bin arclight-gui     # GUI
cargo test --release
cargo clippy --release --all-targets       # also --no-default-features --features gui
cargo run --release -- render graph.json   # CLI render
cargo run --release -- mcp                 # MCP server
cargo run --release -- gpu --check         # every GPU path vs the CPU
```

Workspace: `.`, `plugins/example`, `python`. Version `0.<phase>.<n>`, bumped
in `Cargo.toml`, `python/Cargo.toml` and `plugins/example/Cargo.toml`
together; each phase on its own `wip-ns-*` branch.

## Rules

- **Originality:** `../` (`arclight-compound/`) holds a Nuke disassembly:
  never read, reference or copy it. Use public, standard algorithms and
  descriptive node names, not another product's names or layouts.
- **Restrictive licences stay out of the repo:** e.g. CorridorKey
  (`../CorridorKey`, CC BY-NC-SA + no-resale) only as a separate,
  user-installed tool Arclight calls; never vendored.
- GNU `sed -i` (not `sed -i ''`); ffmpeg / ffprobe in `/usr/bin`.
