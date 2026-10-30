"""Scripting API for the Arclight compositor.

Build, edit and render node graphs from Python::

    import arclt

    bg = arclt.create_node("checkerboard", width=640, height=360)
    dot = arclt.create_node("ellipse", width=640, height=360, center=[320, 180], size=[200, 200],
                            color=[1, 0.4, 0.1, 1])
    comp = arclt.create_node("merge", dot, bg)          # inputs in port order: A, B
    arclt.create_node("blur", comp, name="soft")["size"] = 4
    arclt.render("soft", frame=1, path="/tmp/soft.png")
    arclt.save("/tmp/comp.json")

By default calls act on a headless graph owned by this module. ``attach()``
sends them to a running Arclight GUI instead, where the edits appear live
and can be undone there.
"""

from __future__ import annotations

import json as _json
from typing import Any, Iterable

from . import _arclt

__version__ = _arclt.__version__

__all__ = [
    "ArcltError", "Node", "create_node", "node", "nodes", "delete", "connect", "disconnect",
    "save", "load", "new", "render", "pixels", "to_numpy", "ops", "op_info", "project",
    "undo", "redo", "metadata", "graph", "plugins", "instances", "attach", "detach",
    "select", "view", "view_state", "Backdrop", "backdrops", "backdrop", "create_backdrop",
    "set_positions", "layout",
]


class ArcltError(RuntimeError):
    """Raised when the compositor rejects a call (unknown node, bad param, ...)."""


def _call(tool: str, **args: Any) -> Any:
    args = {k: v for k, v in args.items() if v is not None}
    try:
        return _json.loads(_arclt.call(tool, _json.dumps(args)))
    except RuntimeError as e:
        raise ArcltError(str(e)) from None


def _name(n: "Node | str") -> str:
    return n.name if isinstance(n, Node) else str(n)


class Node:
    """A handle to a node by name. Params read and write through ``node[param]``."""

    __slots__ = ("_name",)

    def __init__(self, name: str):
        self._name = name

    @property
    def name(self) -> str:
        return self._name

    def info(self) -> dict:
        """Op, inputs, outputs and params (defaults filled in)."""
        return _call("describe_node", node=self._name)

    @property
    def op(self) -> str:
        return self.info()["op"]

    def params(self) -> dict:
        return self.info()["params"]

    knobs = params

    def __getitem__(self, param: str) -> Any:
        params = self.params()
        if param not in params:
            raise KeyError(f"{self._name} has no param '{param}'")
        return params[param]

    def __setitem__(self, param: str, value: Any) -> None:
        self.set(**{param: value})

    @property
    def pos(self) -> list[float] | None:
        """Its place in the node editor, [x, y] (top-left corner), or None
        while unplaced (a GUI then puts it beside what feeds it)."""
        return self.info()["pos"]

    @pos.setter
    def pos(self, value: list[float]) -> None:
        _call("set_node_position", node=self._name, pos=value)

    def set(self, **params: Any) -> "Node":
        """Set several params as one undo step."""
        _call("set_params", node=self._name, params=params)
        return self

    def set_key(self, param: str, frame: int, value: Any) -> "Node":
        _call("set_key", node=self._name, param=param, frame=frame, value=value)
        return self

    def to_transform(self, mode: str = "match-move", name: str | None = None, input: "Node | str | None" = None, linked: bool = True) -> "Node":
        """For a Tracker: a new Transform node carrying its solved motion
        ("match-move" or "stabilize"), fed by ``input`` if given. Linked, it
        follows the Tracker live; else it's keyed with a copy."""
        return self._export("transform", mode, name, input, linked)

    def to_corner_pin(self, mode: str = "match-move", name: str | None = None, input: "Node | str | None" = None, linked: bool = True) -> "Node":
        """For a Tracker with all four tracks on: a new Corner Pin that
        follows them in perspective ("match-move" or "stabilize"), live
        (``linked``) or as a copy."""
        return self._export("corner_pin", mode, name, input, linked)

    def _export(self, op: str, mode: str, name: str | None, input: "Node | str | None", linked: bool) -> "Node":
        src = None if input is None else _name(input)
        return Node(_call("export_tracker", node=self._name, op=op, mode=mode, name=name, input=src, linked=linked)["node"])

    def track_camera(self, mode: str | None = None, distance: float | None = None) -> dict:
        """For a Camera Tracker: follow features over the project's frame
        range, solve the camera and 3D points, and keep the solve on the
        node. ``mode`` is "free move" (or "free") for a camera that
        travels, "tripod" for one that only turns about its center, or
        "auto" (picks tripod when the camera only turns); ``distance`` is
        how far from the camera a tripod solve puts its points. Both
        default to the node's settings and are kept on it. Returns frames
        solved, points, error (px), focal length, the ``mode`` solved in
        and the ``pnp_frames``."""
        return _call("track_camera", node=self._name, mode=mode, distance=distance)

    def scene_objects(self, frame: int | None = None) -> dict:
        """What this 3D node's scene holds at ``frame``: ``cameras``,
        ``cards``, ``meshes`` and ``lights`` (each with its ``node`` and
        world ``position``; lights also ``type``, ``direction``, ``color``,
        ``intensity``, ``enabled``, ``shadows`` and their type's settings),
        ``splats`` (each capture's ``node``, ``count``, ``bounds``,
        ``sh_degree`` and ``position``) and the number of ``points``."""
        return _call("scene_objects", node=self._name, frame=frame)

    def particles(self, frame: int | None = None, offset: int = 0, limit: int | None = None) -> dict:
        """The particles this node passes on at ``frame`` (simulated as
        needed), as columns: ``ids``, ``positions``, ``velocities``,
        ``ages``, ``lives``, ``sizes``, ``colors`` and ``stream`` (an index
        into ``streams``), with ``total``. All of them by default, or
        ``limit`` from ``offset``."""
        columns = ("ids", "positions", "velocities", "ages", "lives", "sizes", "colors", "stream")
        if limit is not None:
            return _call("particles", node=self._name, frame=frame, offset=offset, limit=limit)
        out = _call("particles", node=self._name, frame=frame, offset=offset, limit=100_000)
        while out.get("next_offset") is not None:
            page = _call("particles", node=self._name, frame=frame, offset=out["next_offset"], limit=100_000)
            for c in columns:
                out[c].extend(page[c])
            out["count"] += page["count"]
            out["next_offset"] = page.get("next_offset")
        return out

    def splats(self, frame: int | None = None, offset: int = 0, limit: int | None = None) -> dict:
        """The Gaussian splats this node's scene holds at ``frame`` (a Read
        Splat's capture, or those a Scene gathers), as columns:
        ``positions`` (in the scene), ``scales`` and ``rotations`` (w, x,
        y, z; in the capture's own space), ``opacities``, ``colors``
        (linear RGB, the view-independent part) and ``set`` (an index into
        ``sets``: ``node``, ``count``, ``sh_degree``, ``world``), with
        ``total``. All of them by default, or ``limit`` from ``offset``."""
        columns = ("positions", "scales", "rotations", "opacities", "colors", "set")
        if limit is not None:
            return _call("splats", node=self._name, frame=frame, offset=offset, limit=limit)
        out = _call("splats", node=self._name, frame=frame, offset=offset, limit=100_000)
        while out.get("next_offset") is not None:
            page = _call("splats", node=self._name, frame=frame, offset=out["next_offset"], limit=100_000)
            for c in columns:
                out[c].extend(page[c])
            out["count"] += page["count"]
            out["next_offset"] = page.get("next_offset")
        return out

    def camera_points(self, frame: int | None = None) -> list[dict]:
        """For a solved Camera Tracker: its points (``index``,
        ``position`` in 3D, ``frame_position`` in ``frame`` or None)."""
        return _call("camera_points", node=self._name, frame=frame)["points"]

    def orient_camera(self, ground: list[int] | None = None, origin: int | None = None,
                      scale: tuple[int, int, float] | None = None) -> "Node":
        """Move a Camera Tracker's solve: the plane through ``ground``
        points level at 0, point ``origin`` at 0,0,0, and ``scale`` = (a, b,
        distance) between two points."""
        sc = None if scale is None else {"points": [scale[0], scale[1]], "distance": scale[2]}
        _call("orient_camera", node=self._name, ground=ground, origin=origin, scale=sc)
        return self

    def export_camera(self, linked: bool = True) -> dict[str, "Node"]:
        """From a solved Camera Tracker: a Camera following the solve live
        (``linked=False``: keyed with a copy), a Point Cloud, a Scene and a
        Scanline Render over the plate, by role; with a solved lens, also
        ``"lens"``, a Lens Distort undistorting the plate under the render."""
        return {k: Node(v) for k, v in _call("export_camera", node=self._name, linked=linked).items()}

    def place_on_points(self, points: list[int], what: str = "card", frame: int | None = None,
                        input: "Node | str | None" = None, name: str | None = None) -> "Node":
        """Place a ``"card"`` (on the points' plane, sized to them, showing
        ``input``) or a ``"locator"`` (at their middle) on a solved Camera
        Tracker's points."""
        src = None if input is None else _name(input)
        return Node(_call("place_on_points", node=self._name, points=points, what=what, frame=frame, input=src, name=name)["node"])

    def input(self, port: int | str = 0) -> "Node | None":
        for p in self.info()["inputs"]:
            if p["port"] == port or (isinstance(port, str) and p["label"].lower() == port.lower()):
                return Node(p["source"]) if p["source"] else None
        raise ArcltError(f"{self._name} has no input {port!r}")

    def inputs(self) -> list["Node | None"]:
        return [Node(p["source"]) if p["source"] else None for p in self.info()["inputs"]]

    def set_input(self, port: int | str, source: "Node | str | None") -> "Node":
        if source is None:
            disconnect(self, port)
        else:
            connect(source, self, port)
        return self

    def outputs(self) -> list["Node"]:
        return [Node(o["node"]) for o in self.info()["outputs"]]

    def rename(self, new_name: str) -> "Node":
        _call("rename_node", node=self._name, new_name=new_name)
        self._name = new_name
        return self

    def delete(self) -> None:
        delete(self)

    def render(self, frame: int | None = None, path: str | None = None) -> dict:
        return render(self, frame=frame, path=path)

    def select(self) -> dict:
        return select(self)

    def view(self, frame: int | None = None) -> dict:
        return view(self, frame)

    def __repr__(self) -> str:
        return f"<arclt.Node {self._name}>"

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Node) and other._name == self._name

    def __hash__(self) -> int:
        return hash(self._name)


def create_node(op: str, *inputs: "Node | str | None", name: str | None = None, pos: list[float] | None = None,
                **params: Any) -> Node:
    """Add a node of type ``op``. Positional ``inputs`` connect ports in order
    (``None`` skips a port); ``pos`` places it in the node editor; other
    keyword arguments set params."""
    ins = ["" if i is None else _name(i) for i in inputs]
    return Node(_call("create_node", op=op, name=name, params=params, inputs=ins, pos=pos)["node"])


def set_positions(positions: "dict[Node | str, list[float]]") -> None:
    """Move several nodes in the node editor as one undo step:
    ``{node: [x, y]}`` (top-left corners; nodes are 150 x 46)."""
    _call("set_node_position", positions={_name(n): p for n, p in positions.items()})


def layout(all: bool = False) -> dict[str, list[float]]:
    """Place the nodes with no position (``all``: every node) as the node
    editor does: a column per depth from the sources, left to right.
    Returns every node's position."""
    return _call("layout_nodes", all=all)["positions"]


class Backdrop:
    """A handle to a backdrop (a group box behind nodes in the node editor)
    by id. Colors are a palette name (slate, teal, green, olive, amber,
    rust, plum, blue), an index 0-7 into it, or ``[r, g, b]`` 0-1."""

    __slots__ = ("_id",)

    def __init__(self, id: str):
        self._id = id

    @property
    def id(self) -> str:
        return self._id

    def info(self) -> dict:
        """id, title, pos, size, color (and color_name for a palette color),
        font_size and nodes (those fully inside it)."""
        for b in _call("list_backdrops")["backdrops"]:
            if b["id"] == self._id:
                return b
        raise ArcltError(f"no backdrop named '{self._id}'")

    @property
    def title(self) -> str:
        return self.info()["title"]

    @title.setter
    def title(self, value: str) -> None:
        self.set(title=value)

    @property
    def color(self) -> list[float]:
        return self.info()["color"]

    @color.setter
    def color(self, value: "str | int | list[float]") -> None:
        self.set(color=value)

    @property
    def font_size(self) -> float:
        return self.info()["font_size"]

    @font_size.setter
    def font_size(self, value: float) -> None:
        self.set(font_size=value)

    @property
    def pos(self) -> list[float]:
        return self.info()["pos"]

    @pos.setter
    def pos(self, value: list[float]) -> None:
        self.set(pos=value)

    @property
    def size(self) -> list[float]:
        return self.info()["size"]

    @size.setter
    def size(self, value: list[float]) -> None:
        self.set(size=value)

    def nodes(self) -> list[Node]:
        """The nodes fully inside it (they move with it in the node editor)."""
        return [Node(n) for n in self.info()["nodes"]]

    def set(self, title: str | None = None, color: "str | int | list[float] | None" = None,
            font_size: float | None = None, pos: list[float] | None = None,
            size: list[float] | None = None) -> "Backdrop":
        """Change several settings as one undo step."""
        _call("update_backdrop", backdrop=self._id, title=title, color=color, font_size=font_size, pos=pos, size=size)
        return self

    def fit(self, *nodes: "Node | str") -> "Backdrop":
        """Refit it around ``nodes``, with a margin, as Group does."""
        _call("update_backdrop", backdrop=self._id, around=[_name(n) for n in nodes])
        return self

    def delete(self) -> None:
        """Remove it; the nodes in it stay."""
        _call("delete_backdrop", backdrop=self._id)

    def __repr__(self) -> str:
        return f"<arclt.Backdrop {self._id}>"

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Backdrop) and other._id == self._id

    def __hash__(self) -> int:
        return hash(self._id)


def backdrops() -> list[Backdrop]:
    """Every backdrop, back to front."""
    return [Backdrop(b["id"]) for b in _call("list_backdrops")["backdrops"]]


def backdrop(id: str) -> Backdrop | None:
    """The backdrop ``id``, or None."""
    return Backdrop(id) if any(b["id"] == id for b in _call("list_backdrops")["backdrops"]) else None


def create_backdrop(*nodes: "Node | str", title: str | None = None, color: "str | int | list[float] | None" = None,
                    font_size: float | None = None, pos: list[float] | None = None,
                    size: list[float] | None = None, id: str | None = None) -> Backdrop:
    """Add a backdrop in front of the others: fitted around ``nodes`` with a
    margin (as Group does), or at ``pos`` with ``size``. The title defaults
    to "Group", the id to backdrop1, backdrop2, ..."""
    around = [_name(n) for n in nodes] or None
    return Backdrop(_call("create_backdrop", around=around, title=title, color=color, font_size=font_size,
                          pos=pos, size=size, id=id)["backdrop"])


def node(name: str) -> Node | None:
    """The node called ``name``, or None."""
    return Node(name) if any(n["id"] == name for n in graph()["nodes"]) else None


def nodes(op: str | None = None) -> list[Node]:
    """Every node, optionally only those of one op."""
    return [Node(n["id"]) for n in graph()["nodes"] if op is None or n["op"] == op]


def delete(n: "Node | str") -> None:
    _call("delete_node", node=_name(n))


def connect(source: "Node | str", target: "Node | str", port: int | str = 0) -> None:
    """Feed ``source`` into input ``port`` (index or label such as "B" or "mask") of ``target``."""
    _call("connect", **{"from": _name(source), "to": _name(target), "port": port})


def disconnect(target: "Node | str", port: int | str = 0) -> None:
    _call("disconnect", node=_name(target), port=port)


def save(path: str | None = None) -> str:
    """Save the graph (to ``path``, or where it was loaded from). Returns the path."""
    return _call("save_graph", path=path)["path"]


def load(path: str) -> None:
    _call("load_graph", path=path)


def new() -> None:
    """Start an empty graph."""
    _call("new_graph")


def render(n: "Node | str", frame: int | None = None, path: str | None = None) -> dict:
    """Render a node.

    For a ``media_out`` node with no ``frame`` this renders its whole frame
    range to its file(s). Otherwise it renders one frame (the project's first
    by default), writes it to ``path`` if given, and returns its size and
    mean/min/max color.
    """
    name = _name(n)
    if frame is None and path is None and Node(name).op == "media_out":
        return _call("render_output", node=name)
    return _call("render_frame", node=name, frame=frame, path=path)


def pixels(n: "Node | str", frame: int | None = None) -> tuple[int, int, bytes]:
    """(width, height, data): float32 RGBA, linear, premultiplied, top row first."""
    if frame is None:
        frame = project()["first_frame"]
    return _arclt.pixels(_name(n), frame)


def to_numpy(n: "Node | str", frame: int | None = None):
    """The rendered frame as a (height, width, 4) float32 numpy array."""
    import numpy as np

    w, h, data = pixels(n, frame)
    return np.frombuffer(data, dtype=np.float32).reshape(h, w, 4)


def ops(category: str | None = None) -> list[dict]:
    """Available node types (name, label, category), including plugin ops."""
    return _call("list_ops", category=category)


def op_info(op: str) -> dict:
    """Ports, params (kind, range, default) and help for an op."""
    return _call("describe_op", op=op)


def project(first: int | None = None, last: int | None = None, fps: float | None = None,
            width: int | None = None, height: int | None = None) -> dict:
    """Read, or with arguments set, the timeline (first_frame, last_frame, fps)
    and format (the frame size of sources whose own width/height is 0)."""
    if first is None and last is None and fps is None and width is None and height is None:
        return graph()["project"]
    return _call("set_project", first_frame=first, last_frame=last, fps=fps, width=width, height=height)


def undo() -> bool:
    return _call("undo")["undone"]


def redo() -> bool:
    return _call("redo")["redone"]


def metadata(source: "Node | str") -> dict:
    """Size, frame range and fps of a media file, or of a media_in/read node's file."""
    if isinstance(source, Node) or node(str(source)) is not None:
        path = Node(_name(source)).params().get("path", "")
        if not path:
            raise ArcltError(f"{_name(source)} has no file")
        source = path
    return _call("probe_media", path=str(source))


def graph() -> dict:
    """The whole graph as a dict (nodes, project)."""
    return _call("get_graph")["graph"]


def plugins() -> list[dict]:
    return _call("list_plugins")


def instances() -> list[dict]:
    """Running Arclight GUIs that ``attach`` can connect to."""
    return _call("list_instances")


def attach(pid: int | None = None) -> dict:
    """Send further calls to a running GUI (the newest, or the one with ``pid``)."""
    return _call("attach_instance", pid=pid)["attached"]


def detach() -> None:
    """Go back to this module's own headless graph."""
    _call("detach_instance")


def select(n: "Node | str | None") -> dict:
    """Select a node in the attached GUI (opens its properties); None clears.
    Returns the view state, like ``view_state()``."""
    # A missing node (None is dropped by _call) clears the selection.
    return _call("select_node", node=None if n is None else _name(n))


def view(n: "Node | str", frame: int | None = None) -> dict:
    """Show a node in the attached GUI's viewer and select it; ``frame`` also
    moves the playhead. Returns the view state."""
    return _call("view_node", node=_name(n), frame=frame)


def view_state() -> dict:
    """What the attached GUI's user is looking at: selected, viewer, viewing,
    frame, playing."""
    return _call("get_view")
