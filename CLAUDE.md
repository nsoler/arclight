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

## Several sessions at once (herdr)

The user runs several Claude sessions in herdr on this one checkout.

- **Supervisor** (herdr name `supervisor`): the only session that commits,
  pushes, fast-forwards `main`, bumps the version, writes CHANGELOG
  version headers, tags releases and keeps `HANDOFF.md` current.
- **Workers** (Dev1, Dev2, …):
  - Never commit, push or tag, never bump the version, and never
    `git add`, stash, reset or checkout anything.
  - Before starting, tell the Supervisor what you'll work on and which
    files you expect to touch. Two workers must not edit the same file
    at once.
  - When a milestone is done, report it to the Supervisor. The report
    has a one-line title, the exact paths you changed or added, a
    CHANGELOG bullet, and any README or HANDOFF notes.
- The Supervisor commits only the reported paths, never `git add -A`. A
  sweeping add once pulled another session's unfinished files into a
  commit.

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
