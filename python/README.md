# arclt

Python scripting for [ArclightVFX Compositor](https://github.com/nsoler/arclight)
(beta): build, edit and render node graphs headless, or drive a running
ArclightVFX window (`arclt.attach()`).

```python
import arclt

bg = arclt.create_node("checkerboard", width=640, height=360)
dot = arclt.create_node("ellipse", width=640, height=360, center=[320, 180],
                        size=[200, 200], color=[1, 0.4, 0.1, 1])
comp = arclt.create_node("merge", dot, bg)   # inputs in port order: A, B
arclt.render(comp, frame=1, path="/tmp/comp.png")
```

One abi3 wheel per platform covers CPython 3.10 and later. Video files need
`ffmpeg`/`ffprobe` on `PATH` (or in /opt/homebrew/bin, /usr/local/bin).

## Building

```sh
pip install maturin            # in a venv, or: pipx install maturin
maturin build --release        # wheel in ../target/wheels/
pip install ./python           # or build and install in one go
```

For quick work in the source tree, `python/build.sh` builds the extension in
place so `PYTHONPATH=python python3 -c "import arclt"` works without maturin.
Tests: `python3 -m unittest discover -s python/tests -v` (with `arclt`
installed or on `PYTHONPATH`); set `ARCLIGHT_PLUGIN_PATH` to a folder with
the example plugin to include the plugin test.
