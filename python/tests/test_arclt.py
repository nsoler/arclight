"""Tests for the arclt module. Run after python/build.sh:

    PYTHONPATH=python python3 -m unittest discover -s python/tests -v

Set ARCLIGHT_PLUGIN_PATH=target/release (after building the example plugin)
to also exercise plugin ops.
"""

import array
import os
import tempfile
import unittest

import arclt


def mean_rgba(node, frame=None):
    w, h, data = arclt.pixels(node, frame)
    px = array.array("f", data)
    return [sum(px[c::4]) / (w * h) for c in range(4)]


class ArcltTest(unittest.TestCase):
    def setUp(self):
        arclt.new()

    def test_build_render_and_edit(self):
        bg = arclt.create_node("constant", name="bg", width=32, height=16, color=[0, 0, 1, 1])
        fg = arclt.create_node("constant", width=32, height=16, color=[1, 0, 0, 0.5])
        comp = arclt.create_node("merge", fg, bg)
        self.assertEqual(comp.op, "merge")
        self.assertEqual(comp.input("A"), fg)
        self.assertEqual(comp.input(1), bg)

        stats = arclt.render(comp)
        self.assertEqual((stats["width"], stats["height"]), (32, 16))
        r, g, b, a = mean_rgba(comp)
        self.assertAlmostEqual(r, 0.5, places=5)
        self.assertAlmostEqual(b, 0.5, places=5)
        self.assertAlmostEqual(a, 1.0, places=5)

        comp["operation"] = "add"
        self.assertEqual(comp["operation"], "add")
        self.assertTrue(arclt.undo())
        self.assertEqual(comp["operation"], "over")

    def test_errors_raise_arclt_error(self):
        c = arclt.create_node("constant")
        with self.assertRaisesRegex(arclt.ArcltError, "no param"):
            c["nope"] = 1
        with self.assertRaisesRegex(arclt.ArcltError, "unknown op"):
            arclt.create_node("not_an_op")
        with self.assertRaises(KeyError):
            c["nope"]

    def test_save_load_and_files(self):
        with tempfile.TemporaryDirectory() as d:
            arclt.create_node("checkerboard", name="checks", width=16, height=16, size=4)
            arclt.render("checks", path=os.path.join(d, "checks.png"))
            self.assertTrue(os.path.exists(os.path.join(d, "checks.png")))
            meta = arclt.metadata(os.path.join(d, "checks.png"))
            self.assertEqual((meta["kind"], meta["width"]), ("still", 16))

            path = arclt.save(os.path.join(d, "g.json"))
            arclt.new()
            self.assertEqual(arclt.nodes(), [])
            arclt.load(path)
            self.assertEqual([n.name for n in arclt.nodes()], ["checks"])

    def test_project_format(self):
        arclt.project(width=320, height=200)
        self.assertEqual(arclt.project()["format"], {"width": 320, "height": 200})
        c = arclt.create_node("constant")
        w, h, _ = arclt.pixels(c)
        self.assertEqual((w, h), (320, 200))

    def test_keyframes_and_project(self):
        arclt.project(first=1, last=10, fps=25)
        self.assertEqual(arclt.project()["fps"], 25)
        c = arclt.create_node("constant", width=4, height=4)
        c.set_key("color", 1, [0, 0, 0, 1]).set_key("color", 10, [1, 1, 1, 1])
        self.assertLess(mean_rgba(c, 1)[0], 1e-6)
        self.assertGreater(mean_rgba(c, 10)[0], 0.999)

    def test_tracker_to_transform(self):
        plate = arclt.create_node("constant", width=64, height=32)
        trk = arclt.create_node("tracker", plate, name="trk")
        trk["track1"] = {"keys": [[1, [10.5, 10.5]], [4, [13.5, 9.5]]], "interp": "linear"}
        xf = trk.to_transform(input=plate, linked=False)
        self.assertEqual(xf.input(), plate)
        self.assertEqual(xf["translate"]["keys"][1], [4, [3.0, -1.0]])
        steady = trk.to_transform("stabilize", linked=False)
        self.assertEqual(steady["translate"]["keys"][1], [4, [-3.0, 1.0]])
        # By default it follows the Tracker live.
        live = trk.to_transform(input=plate)
        self.assertEqual(live["translate"]["link"]["node"], "trk")
        with self.assertRaises(arclt.ArcltError):
            plate.to_transform()
        for i in (2, 3, 4):
            trk[f"track{i}_on"] = True
        pin = trk.to_corner_pin(input=plate, linked=False)
        self.assertEqual(pin["from1"], [10.5, 10.5])
        self.assertEqual(pin["to1"]["keys"][1], [4, [13.5, 9.5]])

    def test_camera_tracker_needs_a_solve(self):
        # (The solve itself is tested in Rust; here, the wrappers reach it.)
        arclt.project(first=1, last=4)
        plate = arclt.create_node("constant", width=64, height=32)
        ct = arclt.create_node("camera_tracker", plate, name="ct")
        with self.assertRaises(arclt.ArcltError):
            ct.camera_points()
        with self.assertRaises(arclt.ArcltError):
            ct.track_camera()  # a still, featureless plate can't be solved
        with self.assertRaises(arclt.ArcltError):
            ct.place_on_points([0, 1, 2])

    def test_view_tools_need_a_gui(self):
        c = arclt.create_node("constant")
        with self.assertRaisesRegex(arclt.ArcltError, "attach_instance"):
            arclt.view(c)
        with self.assertRaisesRegex(arclt.ArcltError, "attach_instance"):
            arclt.view_state()

    def test_ops_catalog(self):
        names = {o["name"] for o in arclt.ops()}
        self.assertTrue({"merge", "grade", "blur", "media_in"} <= names)
        info = arclt.op_info("merge")
        self.assertEqual([p["label"] for p in info["inputs"]], ["A", "B", "mask"])

    @unittest.skipUnless(any(o["name"] == "vignette" for o in arclt.ops()), "example plugin not on ARCLIGHT_PLUGIN_PATH")
    def test_plugin_ops(self):
        src = arclt.create_node("constant", width=64, height=64, color=[1, 1, 1, 1])
        vig = arclt.create_node("vignette", src, amount=1.0, radius=0.0, softness=1.0)
        w, h, data = arclt.pixels(vig)
        px = array.array("f", data)
        center = px[(32 * w + 32) * 4]
        corner = px[0]
        self.assertGreater(center, 0.9)
        self.assertLess(corner, 0.1)

        grain = arclt.create_node("film_grain", src, amount=0.2)
        self.assertNotEqual(arclt.pixels(grain, 1)[2], arclt.pixels(grain, 2)[2], "grain changes per frame")


if __name__ == "__main__":
    unittest.main()
