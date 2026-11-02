# Arclight: notes for Claude sessions

Arclight is a node-based VFX compositor written in Rust: a 2D compositing
graph, a 3D scene and renderer (CPU and wgpu GPU), procedural geometry,
particles, Gaussian splats, an MCP server and Python bindings. It's alpha,
and development happens on Linux (Omarchy / Hyprland).

## Read first

1. `HANDOFF.md`: current status, the progress log and the architecture.
   Its first paragraph and "Start here" say what's in flight. It's long,
   so read the top, then search it.
2. `TODO.md`: what's still wanted, the user's hand-check lists and what
   isn't planned.
3. `ROADMAP.md`, plus the `docs/*-plan.md` file for the current phase.
4. `README.md` covers user-facing features and the file format.

## Team setup: one Supervisor, two Devs (herdr)

The user runs three Claude sessions in herdr, all on this one checkout:
a **Supervisor** and two workers, **Dev1** and **Dev2**. The user talks to
the Supervisor; the Supervisor talks to the Devs. Sessions find each
other with `ListAgents` and message each other with `SendMessage` (the
older route is `herdr agent prompt supervisor "<msg>"`). Herdr window
names are like `nsoler-xx`; Dev1 and Dev2 are assigned by start order
unless the user says otherwise. A new session should ask which role it
has if that isn't clear.

### Supervisor

- **Owns all of git:** the only session that commits, pushes,
  fast-forwards `main`, bumps the version, writes CHANGELOG version
  headers, tags releases and keeps `HANDOFF.md` / `TODO.md` current.
- **Plans and assigns:** splits the user's request into one task per
  Dev, each with its core in new files that Dev owns, so the two rarely
  share a file.
- **Approves plans:** each Dev sends a plan (approach, storage / format
  choices, every file they'll touch) before writing code. The
  Supervisor checks it, answers its questions and points out where the
  two Devs' work meets.
- **Runs the file locks:** see below. One writer per shared file.
- **Verifies before committing:** never takes a Dev's word for "tests
  pass". It stages only that Dev's paths and hunks, exports the staged
  snapshot (`git checkout-index -a --prefix=../.verify/sup-<ver>/`) and
  runs `cargo test --release`, `cargo clippy --release --all-targets`
  and, when GPU code changed, `arclight gpu --check`, with
  `CARGO_TARGET_DIR=../.verify/target-sup`.
- **Commits per feature:** each Dev milestone is its own version (bump +
  CHANGELOG entry + TODO hand-check line + HANDOFF note). When both
  Devs' edits sit in the same shared files, it stages each Dev's hunks
  separately (hashed staged copies, `git update-index --cacheinfo`) so
  every commit builds alone. Never `git add -A`: a sweeping add once
  pulled another session's unfinished files into a commit.
- **Releases** when the user says so (see HANDOFF.md, "Releases" and the
  release rule): fast-forward `main`, annotated tag `v<ver>`, build the
  Linux tarball, Windows zip and `arclt` wheel + sdist in a clean
  worktree at the tag, test the wheel in a fresh venv, `gh release
  create … --latest`, then check every asset arrived. The user builds and
  uploads the macOS DMG.
- **Keeps the user posted:** short updates when plans are approved,
  when something is committed or pushed, when anything fails, and when a
  release is live. Decisions that are the user's (phases, licensing,
  what to build next) go to the user, not to the Devs.

### Dev1 / Dev2 (workers)

- **No git:** never commit, push or tag, never bump the version, and
  never `git add`, stash, reset or checkout anything.
- **Plan first:** before coding, send the Supervisor a plan with the
  approach and every file you expect to touch, marking which ones are
  shared. Wait for approval.
- **Own your files:** put the core in new files you own (e.g.
  `src/key_color.rs`). Keep edits to shared files (`lib.rs`, `ops.rs`,
  `gpu/mod.rs`, `gpu/ops.rs`, `gpu/check.rs`, `gui/*`, `README.md`,
  `docs/help.md`) small and registration-only.
- **Locks:** send `LOCK <file>` and wait for "granted" before editing a
  shared file; make the edit in one short burst; send `UNLOCK <file>` as
  soon as it builds. Never hold a lock while debugging your own code.
  The tree is shared, so a half-done shared edit breaks the other Dev's
  build.
- **Don't touch the other Dev's lines,** even to fix them. Tell the
  Supervisor, who decides who fixes it.
- **Done means:** `cargo test --release` and `cargo clippy --release
  --all-targets` clean, CPU and GPU agreeing in `gpu/check.rs` if there
  is a GPU path, and old projects loading and looking the same.
- **Final report** to the Supervisor:
  - a one-line title
  - the exact paths, and, in shared files, which hunks are yours
  - a CHANGELOG bullet
  - a HANDOFF note
  - a TODO hand-check line
  - anything not verified, e.g. the GUI by hand
- **Then stand by:** no new work until the Supervisor assigns it.

## Build and test

```sh
cargo run --release --bin arclight-gui            # GUI
cargo test --release                              # must pass before a report
cargo clippy --release --all-targets              # must be clean before a report
cargo run --release -- render graph.json          # CLI render
cargo run --release -- mcp                        # MCP server
cargo build --release -p arclight-plugin-example && python/build.sh
```

The workspace members are `.`, `plugins/example` and `python`.

## Versioning

- The scheme is `0.<phase>.<increment>`. Every shipped change bumps the
  last number.
- A bump updates `Cargo.toml`, `python/Cargo.toml` and
  `plugins/example/Cargo.toml` together, with a `CHANGELOG.md` entry.
- Each phase has its own `wip-ns-*` branch, and earlier phases' branches
  are frozen. See HANDOFF.md "Versioning" for the details.

## Rules

- **Originality rule:** the parent folder `arclight-compound/` holds a Nuke
  disassembly. Never read it, use it as a reference or copy from it. Build
  nodes from public, standard algorithms, and use descriptive node names
  rather than another product's names or panel layouts.
- **Third-party code with restrictive licences stays out of the repo.**
  For example, CorridorKey (`../CorridorKey`, CC BY-NC-SA 4.0 plus
  no-resale terms) may only be integrated as a separate, user-installed
  component that Arclight calls out to. Its code and weights are never
  vendored.
- Use GNU `sed -i`, not `sed -i ''`. ffmpeg and ffprobe are in `/usr/bin`.
- The GPU is an RTX 5070 with 12 GB.
