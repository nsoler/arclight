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

    def set(self, **params: Any) -> "Node":
        """Set several params as one undo step."""
        _call("set_params", node=self._name, params=params)
        return self

    def set_key(self, param: str, frame: int, value: Any) -> "Node":
        _call("set_key", node=self._name, param=param, frame=frame, value=value)
        return self

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

    def __repr__(self) -> str:
        return f"<arclt.Node {self._name}>"

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Node) and other._name == self._name

    def __hash__(self) -> int:
        return hash(self._name)


def create_node(op: str, *inputs: "Node | str | None", name: str | None = None, **params: Any) -> Node:
    """Add a node of type ``op``. Positional ``inputs`` connect ports in order
    (``None`` skips a port); keyword arguments set params."""
    ins = ["" if i is None else _name(i) for i in inputs]
    return Node(_call("create_node", op=op, name=name, params=params, inputs=ins)["node"])


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


def project(first: int | None = None, last: int | None = None, fps: float | None = None) -> dict:
    """Read, or with arguments set, the timeline (first_frame, last_frame, fps)."""
    if first is None and last is None and fps is None:
        return graph()["project"]
    return _call("set_project", first_frame=first, last_frame=last, fps=fps)


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
