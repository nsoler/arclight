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

    def test_particles(self):
        emit = arclt.create_node("particle_emitter", name="emit", rate=0, burst=30, speed=0, life=1000, seed=1)
        fall = arclt.create_node("particle_gravity", emit)
        p = fall.particles(20)
        self.assertEqual((p["total"], p["count"], len(p["positions"])), (30, 30, 30))
        self.assertLess(p["velocities"][0][1], 0.0)
        page = fall.particles(20, offset=25, limit=10)
        self.assertEqual((page["count"], page["ids"]), (5, p["ids"][25:]))
        self.assertEqual(fall.particles(20)["positions"], p["positions"])
        with self.assertRaises(arclt.ArcltError):
            arclt.create_node("constant").particles()

    def test_splats(self):
        sample = os.path.join(os.path.dirname(__file__), "..", "..", "examples", "geo", "splats.ply")
        cap = arclt.create_node("read_splat", name="cap", file=os.path.abspath(sample), translate=[0, 0, -5])
        objs = cap.scene_objects()
        self.assertEqual([(g["node"], g["count"], g["sh_degree"]) for g in objs["splats"]], [("cap", 320, 1)])
        s = cap.splats()
        self.assertEqual((s["total"], s["count"], len(s["positions"]), s["sets"][0]["node"]), (320, 320, 320, "cap"))
        self.assertTrue(all(-7.1 < p[2] < -2.9 for p in s["positions"]), "placed in the scene")
        self.assertTrue(all(0.0 <= a <= 1.0 for a in s["opacities"]) and all(len(c) == 3 for c in s["colors"]))
        page = cap.splats(offset=300, limit=50)
        self.assertEqual((page["count"], page["next_offset"], page["positions"]), (20, None, s["positions"][300:]))
        with self.assertRaises(arclt.ArcltError):
            arclt.create_node("cube").splats()

    def test_lights_and_scene_objects(self):
        target = arclt.create_node("locator", name="target", translate=[3, 0, 0])
        key = arclt.create_node("light_spot", target, name="key", translate=[0, 4, 0], cone=30)
        soft = arclt.create_node("light_area", name="soft", translate=[1, 2, 3], size=[2, 1])
        box = arclt.create_node("cube", name="box")
        scene = arclt.create_node("scene", key, soft, box)
        objs = scene.scene_objects()
        self.assertEqual([l["type"] for l in objs["lights"]], ["spot", "area"])
        self.assertEqual(objs["lights"][0]["position"], [0.0, 4.0, 0.0])
        self.assertAlmostEqual(objs["lights"][0]["direction"][0], 0.6, places=9)
        self.assertEqual(objs["lights"][1]["size"], [2.0, 1.0])
        self.assertEqual(objs["meshes"][0]["node"], "box")
        plate = arclt.create_node("constant", width=32, height=16, color=[0.1, 0.1, 0.1, 1])
        with self.assertRaises(arclt.ArcltError):
            plate.scene_objects()
        lamp = arclt.create_node("light_2d", plate, type="point", position=[16, 8], radius=40)
        self.assertGreater(mean_rgba(lamp)[0], 0.1)

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

    def test_gpu_info_and_cpu_renders(self):
        info = arclt.gpu_info()
        self.assertIn(info["status"]["state"], ("active", "cpu"))
        self.assertTrue(info["line"].startswith("Using "))
        self.assertTrue(info["built"])
        plate = arclt.create_node("constant", width=8, height=4, color=[0.25, 0.5, 1, 1])
        self.assertEqual(arclt.render(plate, cpu=True), plate.render())
        if info["status"]["state"] == "active":
            report = arclt.gpu_check()
            self.assertTrue(report["passed"], report["summary"])
            self.assertTrue(all(r["pass"] for r in report["rows"]))

    def test_backdrops(self):
        plate = arclt.create_node("constant", name="plate")
        soft = arclt.create_node("blur", plate, name="soft")
        box = arclt.create_backdrop(plate, soft, title="Plate", color="teal")
        self.assertEqual((box.id, box.title, box.info()["color_name"]), ("backdrop1", "Plate", "teal"))
        self.assertEqual(box.nodes(), [plate, soft])
        notes = arclt.create_backdrop(pos=[600, 0], size=[300, 200], id="notes")
        self.assertEqual(arclt.backdrops(), [box, notes])
        self.assertEqual((notes.title, notes.nodes()), ("Group", []))
        notes.set(title="Notes", color=7, font_size=24)
        notes.size = [400, 250]
        self.assertEqual((notes.title, notes.font_size, notes.size), ("Notes", 24, [400, 250]))
        # (Unplaced headless nodes sit at 0,0: moved off them, it holds none.)
        box.pos = [1000, 1000]
        self.assertEqual(box.nodes(), [])
        box.fit(soft)
        self.assertEqual(box.nodes(), [plate, soft])
        self.assertTrue(arclt.undo())
        self.assertEqual((box.nodes(), box.pos), ([], [1000, 1000]))
        with self.assertRaisesRegex(arclt.ArcltError, "mauve"):
            box.color = "mauve"
        with self.assertRaisesRegex(arclt.ArcltError, "'around'"):
            arclt.create_backdrop(title="nowhere")
        box.delete()
        self.assertIsNone(arclt.backdrop("backdrop1"))
        self.assertEqual(arclt.backdrop("notes"), notes)
        self.assertEqual(len(arclt.nodes()), 2)
        with self.assertRaisesRegex(arclt.ArcltError, "no backdrop"):
            box.title

    def test_node_positions(self):
        plate = arclt.create_node("constant", name="plate", pos=[0, 0])
        soft = arclt.create_node("blur", plate)
        look = arclt.create_node("grade", soft)
        self.assertEqual((plate.pos, soft.pos), ([0, 0], None))
        laid = arclt.layout()
        self.assertGreater(laid[look.name][0], laid[soft.name][0])
        soft.pos = [300, 0]
        arclt.set_positions({plate: [-300, 0], look.name: [600, 0]})
        self.assertEqual((plate.pos, soft.pos, look.pos), ([-300, 0], [300, 0], [600, 0]))
        self.assertEqual(arclt.create_backdrop(soft, look).nodes(), [soft, look])
        arclt.undo()
        arclt.undo()
        self.assertEqual(soft.pos, [300, 0])
        self.assertNotEqual(plate.pos, [-300, 0])
        with self.assertRaisesRegex(arclt.ArcltError, "nope"):
            arclt.set_positions({"nope": [0, 0]})

    def test_disabled_nodes(self):
        plate = arclt.create_node("constant", width=4, height=4, color=[1, 0, 0, 1])
        look = arclt.create_node("grade", plate, multiply=[0.5, 0.5, 0.5])
        self.assertFalse(look.disabled)
        self.assertAlmostEqual(mean_rgba(look)[0], 0.5, places=5)
        look.disabled = True
        self.assertTrue(look.disabled)
        self.assertAlmostEqual(mean_rgba(look)[0], 1.0, places=5)
        self.assertTrue(arclt.undo())
        self.assertFalse(look.disabled)

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
        with self.assertRaisesRegex(arclt.ArcltError, "isn't one of"):
            ct.track_camera(mode="sideways")
        with self.assertRaises(arclt.ArcltError):
            ct.track_camera(mode="tripod", distance=5)
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
