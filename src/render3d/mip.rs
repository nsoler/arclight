//! Mipmaps: a texture halved again and again (each texel the mean of the
//! four under it), so a surface seen small takes its texture's mean over
//! what a sample covers instead of one texel that happens to be under it
//! (which shimmers and moirés). Made when a render first needs them and
//! kept for the next renders of the same texture.

use crate::color::Rgba;
use crate::image::Image;
use std::sync::{Arc, Mutex, Weak};

/// A texture's smaller levels: half its frame each way, then half that,
/// down to a texel (the texture itself is level 0).
pub(super) struct Mips {
    pub levels: Vec<Image>,
}

impl Mips {
    fn bytes(&self) -> usize {
        self.levels.iter().map(|l| l.pixels.len() * 16).sum()
    }
}

/// How many textures' mipmaps are kept, and at most how many bytes.
const KEEP: usize = 16;
const KEEP_BYTES: usize = 512 << 20;

/// Kept mipmaps, by texture and whether they wrap, most recently used
/// last.
type Kept = Vec<(Weak<Image>, bool, Arc<Mips>)>;
static KEPT: Mutex<Kept> = Mutex::new(Vec::new());

/// `img`'s mipmaps, `wrap`ping round its edges (a repeating texture) or
/// not: kept from an earlier render, else made.
pub(super) fn of(img: &Arc<Image>, wrap: bool) -> Arc<Mips> {
    let found = |kept: &mut Kept| {
        kept.retain(|(w, _, _)| w.strong_count() > 0);
        let i = kept.iter().position(|(w, wraps, _)| *wraps == wrap && std::ptr::eq(w.as_ptr(), Arc::as_ptr(img)))?;
        let hit = kept.remove(i);
        let mips = hit.2.clone();
        kept.push(hit);
        Some(mips)
    };
    if let Some(m) = found(&mut KEPT.lock().unwrap_or_else(|e| e.into_inner())) {
        return m;
    }
    let mips = Arc::new(build(img, wrap));
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = found(&mut kept) {
        return m;
    }
    kept.push((Arc::downgrade(img), wrap, mips.clone()));
    while kept.len() > KEEP || (kept.len() > 1 && kept.iter().map(|k| k.2.bytes()).sum::<usize>() > KEEP_BYTES) {
        kept.remove(0);
    }
    mips
}

/// The levels below `img` (its frame: outside its data window is clear).
/// On one thread: it's made inside a render's parallel loop, which mustn't
/// wait on itself.
fn build(img: &Image, wrap: bool) -> Mips {
    let (w, h) = (img.format.width.max(1), img.format.height.max(1));
    let mut levels: Vec<Image> = Vec::new();
    let (mut pw, mut ph) = (w, h);
    while pw > 1 || ph > 1 {
        let (nw, nh) = (pw.div_ceil(2), ph.div_ceil(2));
        let prev = levels.last();
        // Texel (x, y) of the level above: a past-the-edge one wraps round
        // or repeats the edge.
        let tap = |x: usize, y: usize| -> Rgba {
            let (x, y) = if wrap { (x % pw, y % ph) } else { (x.min(pw - 1), y.min(ph - 1)) };
            match prev {
                Some(p) => p.pixels[y * pw + x],
                None => img.at(x as i32, y as i32),
            }
        };
        let mut next = Image::new(nw, nh);
        for (i, out) in next.pixels.iter_mut().enumerate() {
            let (x, y) = (i % nw, i / nw);
            let q = [tap(2 * x, 2 * y), tap(2 * x + 1, 2 * y), tap(2 * x, 2 * y + 1), tap(2 * x + 1, 2 * y + 1)];
            *out = std::array::from_fn(|c| (q[0][c] + q[1][c] + q[2][c] + q[3][c]) * 0.25);
        }
        levels.push(next);
        (pw, ph) = (nw, nh);
    }
    Mips { levels }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_halve_to_a_texel_averaging_and_are_kept() {
        let mut img = Image::new(5, 3);
        for (i, p) in img.pixels.iter_mut().enumerate() {
            *p = [i as f32, 1.0, 0.0, 1.0];
        }
        let img = Arc::new(img);
        let m = of(&img, false);
        let sizes: Vec<_> = m.levels.iter().map(|l| (l.format.width, l.format.height)).collect();
        assert_eq!(sizes, [(3, 2), (2, 1), (1, 1)]);
        // The first texel: the mean of (0, 0), (1, 0), (0, 1), (1, 1).
        assert_eq!(m.levels[0].pixels[0], [(0.0 + 1.0 + 5.0 + 6.0) / 4.0, 1.0, 0.0, 1.0]);
        // Past the right edge: the edge again, or (wrapped) the left.
        assert_eq!(m.levels[0].pixels[2][0], (4.0 + 4.0 + 9.0 + 9.0) / 4.0);
        assert_eq!(build(&img, true).levels[0].pixels[2][0], (4.0 + 0.0 + 9.0 + 5.0) / 4.0);
        assert!(Arc::ptr_eq(&m, &of(&img, false)), "kept");
    }
}
