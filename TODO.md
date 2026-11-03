# Arclight — TODO

What's still open, as of 0.14.24 (2026-11-02). Done items are removed
(`CHANGELOG.md` has what shipped; the long list before this cleanup is in
git: `git show 106707a:TODO.md`). Add items as they come up and delete
them when they ship. `ROADMAP.md` is the public summary.

## To check by hand (user)

On Linux and on the Macs (the DMG), from the release builds.

**Phase 14 (0.14.4–0.14.13)**
- [ ] Cube UV layout atlas: each face its own cell; Bake Projection on a
      projected atlas cube renders like the live projection

**Phase 13 (0.13.x)**
- [ ] Scatter with a painted / keyed mask: raising density or painting
      the mask doesn't make existing points jump; Min distance; tracker
      points as the source
- [ ] Attribute Set / Noise / Falloff / from Image / Transfer, checked in
      the viewer's Sheet (Geo Info)
- [ ] Instance on Points: rocks and cards facing the camera, variant /
      random pick, pscale and Cd, shadows and motion blur; 100k cards stay
      quick with the GPU on
- [ ] Delete Geo's box / sphere moved with G, sized with S; an expression
      like `random(@id) < 0.3`
- [ ] Curves in the 3D view: pick (⇧), drag, G / R / S, Bezier handles
      (Alt breaks), Ctrl/⌘-click adds or inserts, Delete; a Curve from a
      solved camera's path
- [ ] Curve to Mesh (tube, ribbon, profile, pscale taper); Points on Curve
      with Instance on Points
- [ ] Particles to Points driving Instance on Points; an emitter on a
      Scatter's point set
- [ ] Project 3D: the plate comes back exactly from the shot camera;
      occlusion; Crop / Edge; Frame hold with a moving camera; a patch
      stacked Over; GPU on and off match. Help ▸ Examples ▸ Projection
      (multi-projection): Blend softens the seam, Fade start / end,
      Frames fills what one frame misses
- [ ] Points from Geo / Grid / Line Points; Normals by angle; Subdivide
      (Loop on a cube); Extrude (individual with Inset, region from
      Attribute Set, walls by `side`)
- [ ] Write Geo: render a scene to .obj / .gltf / .glb (textured card,
      instances), open in Blender; Read Geo reads the glTF back
- [ ] Area-light shadows at Quality 16: ~7 s a 4K frame with the GPU on

**GPU and the Macs**
- [ ] Transform / Corner Pin / Lens Distort / Keyer match with the GPU on
      and off; quitting never leaves a crash report behind
- [ ] Scanline Render with shadows and motion blur: about a second at 4K
      with the GPU on, matching the CPU
- [ ] A 2D chain with big blurs feels quicker with the GPU on

**Older**
- [ ] Several Viewer nodes (the active one shows, eye icon); 3D view
      motion-blur preview (particle streaks, a moving camera); Scanline
      Render Jitter shutter on / off

## Known gaps and follow-ups

- [ ] Scanline Render's Width / Height are ignored when a background is
      connected (the plate's size wins): say so in help, or let them win
- [ ] Geo Info: per-point selection in the sheet's filter; world-space
      values
- [ ] Project 3D targets are node names fixed at the node: fine today
      (nothing renames or merges meshes under a new node); if such an op
      is added, carry projections on Card / Model as Instances do
- [ ] Particles to Points ids pack emitter (8 bits) + birth count (24
      bits): past ~16.7 M births ids wrap (only Instance on Points'
      random pick cares)
- [ ] GPU playback: skip the target read-back, read back lazily for the
      readout / picker (~5–10 ms a 1080p frame)
- [ ] Splats: a GPU path; SOG / compressed PLY if asked
- [ ] Alembic: curves, points, cameras, face sets as materials
- [ ] A new logo: `assets/arclight.png` and the full logo still say
      ARCLIGHT
- [ ] Windows: the zip ships (since 0.13.34) but is untested on a real
      PC; MCP / GUI bridge without Unix sockets (named pipe or
      localhost), `%APPDATA%` folders, `.dll` plugins; unsigned
      (SmartScreen warning)
- [ ] RLE / 1-bit DPX (needs a sample file first)

## Next: phase 15, production readiness (proposed)

Planned in `docs/production-plan.md`, waiting for the user's answers (Q1–Q5
there); the user said stay on 0.14.x for now. P1 color management (OCIO /
ACES), P2 node groups and gizmos, P3 regions / disk cache / proxies, P4 the
farm, P5 EXR; alongside STMap, Defocus / ZDefocus, vector tools, Denoise.
On hold in `ROADMAP.md` "Later": Paint, a 3D pivot.

## Before going public

- [ ] Licence (MIT / Apache or GPL), a LICENSE and a NOTICES file
- [ ] Audit the repo for anything private (paths, emails in notes,
      `examples/mad-martian.png`, where `musicmen.MP4` came from)
- [ ] A public-facing README, issue templates, CONTRIBUTING
- [ ] Actions is free for public repos: build the macOS DMG and wheels
      there again
- [ ] Distro packages (deb / rpm / AUR); signed, notarized macOS builds

## Not planned (on purpose)

- OpenFX hosting (a large project of its own)
- Copying another product's licensing scheme
- Nuke script import (originality rule)
