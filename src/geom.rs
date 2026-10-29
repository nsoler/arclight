//! 3D geometry for the scene system and the camera tracker: placing
//! things in 3D (translate, rotate, scale), the pinhole camera that turns
//! scene points into frame pixels and back, and Brown radial lens
//! distortion.
//!
//! Conventions: right-handed, Y up, a camera looks down its −Z axis.
//! Rotations are in degrees, applied about X, then Y, then Z. Frame
//! positions are in pixels with y down and pixel i spanning i..i+1, as
//! shapes and tracks count them, so the frame's center is (w/2, h/2).

use crate::image::Size;
use nalgebra::{Matrix4, Rotation3, Vector3};

pub type Vec3 = Vector3<f64>;
pub type Mat4 = Matrix4<f64>;

/// A rotation of `r` degrees about X, then Y, then Z.
pub fn rotation(r: [f64; 3]) -> Rotation3<f64> {
    let [x, y, z] = r.map(f64::to_radians);
    Rotation3::from_euler_angles(x, y, z)
}

/// The angles (degrees, X then Y then Z) of a rotation, as [`rotation`]
/// takes them.
pub fn angles(r: &Rotation3<f64>) -> [f64; 3] {
    let (x, y, z) = r.euler_angles();
    [x, y, z].map(f64::to_degrees)
}

/// Scale, then rotate, then translate: an object's (or a camera's) place in
/// the scene, mapping its own coordinates to the scene's.
pub fn trs(translate: [f64; 3], rotate: [f64; 3], scale: [f64; 3]) -> Mat4 {
    Mat4::new_translation(&Vec3::from(translate)) * rotation(rotate).to_homogeneous() * Mat4::new_nonuniform_scaling(&Vec3::from(scale))
}

/// `m` applied to point `p`.
pub fn transform_point(m: &Mat4, p: &Vec3) -> Vec3 {
    m.transform_point(&nalgebra::Point3::from(*p)).coords
}

/// The horizontal field of view (degrees) of a lens of `focal` mm over a
/// film back `aperture` mm wide.
pub fn fov(focal: f64, aperture: f64) -> f64 {
    2.0 * (aperture / (2.0 * focal)).atan().to_degrees()
}

/// The focal length (mm) giving a field of view of `fov` degrees over a
/// film back `aperture` mm wide.
pub fn focal_for_fov(fov: f64, aperture: f64) -> f64 {
    aperture / (2.0 * (fov.to_radians() / 2.0).tan())
}

/// A pinhole camera. The film back's width spans the frame's width; its
/// height follows the frame's aspect (pixels are square), so `film_back[1]`
/// only matters to tools that report a vertical field of view.
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    /// Focal length, mm.
    pub focal: f64,
    /// Film back width and height, mm.
    pub film_back: [f64; 2],
    /// Camera to scene: where the camera is and which way it faces.
    pub world: Mat4,
    /// Nothing nearer than this (scene units) is seen.
    pub near: f64,
    pub far: f64,
}

impl Default for Camera {
    fn default() -> Camera {
        Camera { focal: 35.0, film_back: [36.0, 24.0], world: Mat4::identity(), near: 0.1, far: 10000.0 }
    }
}

impl Camera {
    /// Scene to camera: the inverse of `world`.
    pub fn view(&self) -> Mat4 {
        self.world.try_inverse().unwrap_or_else(Mat4::identity)
    }

    /// The focal length in frame pixels for `format`.
    pub fn focal_pixels(&self, format: Size) -> f64 {
        self.focal / self.film_back[0].max(1e-9) * format.width as f64
    }

    /// The horizontal field of view, degrees.
    pub fn fov(&self) -> f64 {
        fov(self.focal, self.film_back[0])
    }

    /// Where camera-space point `p` lands in a frame of `format`, and how
    /// far in front of the camera it is; None nearer than `near` (or
    /// behind the camera).
    pub fn project_camera(&self, p: &Vec3, format: Size) -> Option<([f64; 2], f64)> {
        let depth = -p.z;
        if depth < self.near.max(1e-9) {
            return None;
        }
        let f = self.focal_pixels(format);
        let (cx, cy) = (format.width as f64 / 2.0, format.height as f64 / 2.0);
        Some(([cx + f * p.x / depth, cy - f * p.y / depth], depth))
    }

    /// Where scene point `p` lands in a frame of `format` (see
    /// [`project_camera`](Self::project_camera)).
    pub fn project(&self, p: &Vec3, format: Size) -> Option<([f64; 2], f64)> {
        self.project_camera(&transform_point(&self.view(), p), format)
    }

    /// The scene point seen at frame position `px`, `depth` units in front
    /// of the camera.
    pub fn unproject(&self, px: [f64; 2], depth: f64, format: Size) -> Vec3 {
        let f = self.focal_pixels(format);
        let (cx, cy) = (format.width as f64 / 2.0, format.height as f64 / 2.0);
        let p = Vec3::new((px[0] - cx) / f * depth, -(px[1] - cy) / f * depth, -depth);
        transform_point(&self.world, &p)
    }
}

/// Brown radial lens distortion: a point at distance r from `center`
/// (measured in units of half the frame's width) moves to
/// r × (1 + k1 r² + k2 r⁴). Negative k1 bends straight lines outward in
/// the middle (barrel, wide lenses), positive inward (pincushion).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Distortion {
    pub k1: f64,
    pub k2: f64,
    /// Frame pixels.
    pub center: [f64; 2],
    /// Frame pixels per unit of r.
    pub scale: f64,
}

impl Distortion {
    /// For a frame of `format`, about `center` (frame pixels).
    pub fn new(k1: f64, k2: f64, center: [f64; 2], format: Size) -> Distortion {
        Distortion { k1, k2, center, scale: (format.width as f64 / 2.0).max(1e-9) }
    }

    fn factor(&self, r2: f64) -> f64 {
        1.0 + self.k1 * r2 + self.k2 * r2 * r2
    }

    /// Where the lens puts a point that a perfect lens would put at `p`.
    pub fn distort(&self, p: [f64; 2]) -> [f64; 2] {
        let (x, y) = ((p[0] - self.center[0]) / self.scale, (p[1] - self.center[1]) / self.scale);
        let k = self.factor(x * x + y * y);
        [self.center[0] + x * k * self.scale, self.center[1] + y * k * self.scale]
    }

    /// Where a perfect lens would put a point the lens put at `p`: the
    /// inverse of [`distort`](Self::distort), by Newton's method on the
    /// radius. None past the radius where the distortion folds back on
    /// itself (strong barrel distortion far out), which no lens image
    /// reaches.
    pub fn undistort(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let (x, y) = ((p[0] - self.center[0]) / self.scale, (p[1] - self.center[1]) / self.scale);
        let rd = x.hypot(y);
        if rd < 1e-12 || (self.k1 == 0.0 && self.k2 == 0.0) {
            return Some(p);
        }
        let mut r = rd;
        for _ in 0..30 {
            let r2 = r * r;
            let g = r * self.factor(r2) - rd;
            let slope = 1.0 + 3.0 * self.k1 * r2 + 5.0 * self.k2 * r2 * r2;
            if slope <= 1e-6 {
                return None;
            }
            let step = g / slope;
            r -= step;
            if step.abs() < 1e-12 {
                break;
            }
        }
        let r2 = r * r;
        if r < 0.0 || (r * self.factor(r2) - rd).abs() > 1e-6 || 1.0 + 3.0 * self.k1 * r2 + 5.0 * self.k2 * r2 * r2 <= 0.0 {
            return None;
        }
        let k = r / rd;
        Some([self.center[0] + x * k * self.scale, self.center[1] + y * k * self.scale])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: Size = Size { width: 1920, height: 1080 };

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn rotations_and_placements() {
        // About X, then Y, then Z: angles read back as given.
        let r = [10.0, -35.0, 120.0];
        let back = angles(&rotation(r));
        assert!(r.iter().zip(back).all(|(a, b)| close(*a, b, 1e-9)), "{back:?}");
        // 90° about Y turns −Z (straight ahead) to −X (left).
        let p = transform_point(&trs([0.0; 3], [0.0, 90.0, 0.0], [1.0; 3]), &Vec3::new(0.0, 0.0, -1.0));
        assert!((p - Vec3::new(-1.0, 0.0, 0.0)).norm() < 1e-12, "{p}");
        // Scaled, then turned, then moved.
        let m = trs([5.0, 0.0, 0.0], [0.0, 0.0, 90.0], [2.0, 1.0, 1.0]);
        let p = transform_point(&m, &Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(5.0, 2.0, 0.0)).norm() < 1e-12, "{p}");
    }

    #[test]
    fn cameras_project_and_unproject() {
        assert!(close(fov(18.0, 36.0), 90.0, 1e-9));
        assert!(close(focal_for_fov(90.0, 36.0), 18.0, 1e-9));
        // A camera at (1, 2, 10) looking down −Z, 36 mm film, 18 mm lens
        // (90° across): straight ahead is the frame's center, and a point
        // as far to the side as it is ahead is the frame's right edge.
        let cam = Camera { focal: 18.0, world: trs([1.0, 2.0, 10.0], [0.0; 3], [1.0; 3]), ..Camera::default() };
        let (c, depth) = cam.project(&Vec3::new(1.0, 2.0, 0.0), HD).unwrap();
        assert_eq!((c, depth), ([960.0, 540.0], 10.0));
        let (edge, _) = cam.project(&Vec3::new(11.0, 2.0, 0.0), HD).unwrap();
        assert!(close(edge[0], 1920.0, 1e-9) && close(edge[1], 540.0, 1e-9), "{edge:?}");
        // Up in the scene is up in the frame (smaller y).
        assert!(cam.project(&Vec3::new(1.0, 3.0, 0.0), HD).unwrap().0[1] < 540.0);
        // Behind the camera: nothing.
        assert!(cam.project(&Vec3::new(1.0, 2.0, 20.0), HD).is_none());
        // Turned and moved, unprojecting then projecting comes back.
        let cam = Camera { focal: 50.0, world: trs([3.0, -1.0, 4.0], [12.0, 40.0, -5.0], [1.0; 3]), ..Camera::default() };
        for px in [[0.0, 0.0], [1920.0, 1080.0], [700.5, 123.25]] {
            let p = cam.unproject(px, 7.5, HD);
            let (back, depth) = cam.project(&p, HD).unwrap();
            assert!(close(back[0], px[0], 1e-6) && close(back[1], px[1], 1e-6) && close(depth, 7.5, 1e-9), "{px:?} → {back:?}");
        }
    }

    #[test]
    fn distortion_round_trips() {
        let center = [960.0, 540.0];
        // At r = 1 (half the width out), k1 = 0.1 moves a point 10% further.
        let d = Distortion::new(0.1, 0.0, center, HD);
        let p = d.distort([1920.0, 540.0]);
        assert!(close(p[0], 2016.0, 1e-9) && close(p[1], 540.0, 1e-9), "{p:?}");
        assert_eq!(d.distort(center), center);
        // Undistort undoes distort over the whole frame, barrel and
        // pincushion, off-center too.
        for (k1, k2, c) in [(0.1, 0.02, center), (-0.15, 0.01, center), (-0.05, -0.01, [900.0, 600.0])] {
            let d = Distortion::new(k1, k2, c, HD);
            for x in (0..=1920).step_by(160) {
                for y in (0..=1080).step_by(120) {
                    let p = [x as f64, y as f64];
                    let back = d.undistort(d.distort(p)).unwrap();
                    assert!(close(back[0], p[0], 1e-6) && close(back[1], p[1], 1e-6), "{k1} {k2}: {p:?} → {back:?}");
                }
            }
        }
        // Strong barrel distortion folds back far out: no inverse there.
        let d = Distortion::new(-0.5, 0.0, center, HD);
        assert!(d.undistort([960.0 + 960.0 * 3.0, 540.0]).is_none());
    }
}
