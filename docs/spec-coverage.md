# Spec coverage

How the planning documents in `../Archlight-Development-Documentationm/`
(architecture plans, technical documentation, roadmap, implementation
strategies) map onto this codebase. The plans assume C++17 and Qt6. Arclight
is written in Rust, with egui instead of Qt.

| Planned component | Status | Where |
|---|---|---|
| **Core engine** | | |
| Node graph (DAG, dependency tracking, cycle detection) | Done | `graph.rs` |
| Node registry / factory, typed params with validation | Done | `ops.rs` (`OPS`, `ParamSpec`), `session.rs::validate_param` |
| Undo/redo | Done: GUI (`gui/doc.rs`) and headless (`session.rs`) | |
| Parallel processing | Done: per-pixel rayon in ops; tracker search | `ops.rs`, `track.rs` |
| Read/Write, Merge, Grade, color ops, transforms, shapes, roto, tracker | Done | `ops.rs`, `shape.rs`, `track.rs` |
| Image data, cache with LRU eviction | Done: per-node result cache + GUI frame LRU (size in settings) | `graph.rs::Evaluator`, `gui/worker.rs` |
| Tile-based / ROI processing, memory pools, multi-resolution | **Not done** | Handoff "Phase 0.6"; needs bounding boxes first |
| FFmpeg video I/O, EXR, image sequences | Done (ffmpeg CLI) | `media.rs`, `image.rs` |
| **Automation layer** | | |
| Python module `arclt` (`create_node`, `node`, `save`, `render`, metadata) | Done | `python/` |
| MCP server, JSON-RPC 2.0 (stdio + HTTP), tool framework with validation | Done | `mcp.rs`, `arclight mcp` |
| App bridge + instance registry | Done: Unix socket per GUI, registry in `$XDG_RUNTIME_DIR` | `bridge.rs` |
| `import_nuke` | **Not done on purpose**: see the originality rule in `HANDOFF.md` | |
| **Plugin host** | | |
| Native plugins: discovery, loading, op registration | Done (own C ABI) | `plugin.rs`, `include/arclight_plugin.h`, `plugins/example` |
| OpenFX host | **Not done**: OFX is a large suite API (property sets, clips, parameters, multithread suites). Its own project. | |
| Python plugins (custom nodes in Python) | **Not done** | Could reuse the `arclt` crate by embedding Python in the host |
| Plugin process isolation | **Not done**: plugins run in-process, and a crashing plugin crashes the app | |
| **GUI** | | |
| Node graph editor, viewer, property panels, curve editor, player | Done (egui) | `gui/` |
| Material editor, 3D viewport | **Not done** (Phase 4: 3D) | |
| Linux integration: XDG paths, settings | Done | `config.rs` |
| Crash reporting, system tray | **Not done** | |
| **Advanced** | | |
| USD/Hydra, camera solver | **Not done** (Phase 4) | |
| CUDA/OpenCL GPU acceleration | **Not done**: CPU (rayon) only. `wgpu` compute would be the portable Rust route. | |
| **Licensing** (machine-id, Ed25519, clock-rollback checks) | **Not implemented.** The plan asks for verification "identical" to another product's licensing scheme, which this project doesn't copy. If Arclight needs its own licensing, design it separately. | |
| Packaging (distro packages), CI | **Not done** | |
