// Warps and resampling (see ops.rs `sample_warped`, `WarpMap`,
// `carry_planes` and `resample`). Images are storage buffers over their
// data windows; outside a window reads as transparent (planes as 0).

const TURN: u32 = 0u;
const AFFINE: u32 = 1u;
const PERSPECTIVE: u32 = 2u;
const LENS: u32 = 3u;
// The most taps a side `sample_warped` averages.
const MAX_TAPS: f32 = 16.0;

struct Warp {
    // Data windows: x0, y0, width, height.
    out_win: vec4<i32>,
    src_win: vec4<i32>,
    // The map (TURN, ...); LENS: 1 to the plate (distorting), 0 back.
    kind: u32,
    to_plate: u32,
    // Resample: 1 to keep taps on the data's edge pixels (hard edges).
    hard_edges: u32,
    _pad: u32,
    // TURN: translate, center; scale, sin, cos.
    // AFFINE: m0..m3; m4, m5.
    // PERSPECTIVE: h0..h3; h4..h7; h8.
    // LENS: k1, k2, center; scale.
    // Resample: ox, oy, fx, fy; nx, ny; data px0, px1, py0, py1.
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
}

@group(0) @binding(0) var<uniform> w: Warp;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> src_plane: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst_plane: array<f32>;

fn at(q: vec2<i32>) -> vec4<f32> {
    let l = q - w.src_win.xy;
    if l.x < 0 || l.y < 0 || l.x >= w.src_win.z || l.y >= w.src_win.w {
        return vec4<f32>(0.0);
    }
    return src[l.y * w.src_win.z + l.x];
}

fn plane_at(q: vec2<i32>) -> f32 {
    let l = q - w.src_win.xy;
    if l.x < 0 || l.y < 0 || l.x >= w.src_win.z || l.y >= w.src_win.w {
        return 0.0;
    }
    return src_plane[l.y * w.src_win.z + l.x];
}

// `Image::sample_bilinear`: pixel centers on integers.
fn bilinear(p: vec2<f32>) -> vec4<f32> {
    let p0 = floor(p);
    let f = p - p0;
    let i = vec2<i32>(p0);
    let top = at(i) * (1.0 - f.x) + at(i + vec2<i32>(1, 0)) * f.x;
    let bot = at(i + vec2<i32>(0, 1)) * (1.0 - f.x) + at(i + vec2<i32>(1, 1)) * f.x;
    return top * (1.0 - f.y) + bot * f.y;
}

fn lens_factor(r2: f32) -> f32 {
    return 1.0 + w.a.x * r2 + w.a.y * r2 * r2;
}

// `geom::Distortion::undistort`'s Newton iteration on the radius; z = 0
// where the lens folds back.
fn undistort(p: vec2<f32>) -> vec3<f32> {
    let center = w.a.zw;
    let v = (p - center) / w.b.x;
    let rd = length(v);
    if rd < 1e-12 || (w.a.x == 0.0 && w.a.y == 0.0) {
        return vec3<f32>(p, 1.0);
    }
    var r = rd;
    for (var i = 0; i < 30; i++) {
        let r2 = r * r;
        let g = r * lens_factor(r2) - rd;
        let slope = 1.0 + 3.0 * w.a.x * r2 + 5.0 * w.a.y * r2 * r2;
        if slope <= 1e-6 {
            return vec3<f32>(0.0);
        }
        let step = g / slope;
        r -= step;
        if abs(step) < 1e-7 {
            break;
        }
    }
    let r2 = r * r;
    // (f32: the residual allowed is wider than the CPU's 1e-6.)
    if r < 0.0 || abs(r * lens_factor(r2) - rd) > 1e-5 || 1.0 + 3.0 * w.a.x * r2 + 5.0 * w.a.y * r2 * r2 <= 0.0 {
        return vec3<f32>(0.0);
    }
    return vec3<f32>(center + v * (r / rd) * w.b.x, 1.0);
}

// Where output position `p` reads the source (pixel centers on
// integers); z = 0 for nowhere (`WarpMap::inverse`).
fn inverse(p: vec2<f32>) -> vec3<f32> {
    switch w.kind {
        case TURN: {
            let d = p - w.a.xy - w.a.zw;
            let s = w.b.y;
            let c = w.b.z;
            return vec3<f32>((d.x * c - d.y * s) / w.b.x + w.a.z, (d.x * s + d.y * c) / w.b.w + w.a.w, 1.0);
        }
        case AFFINE: {
            let q = p + 0.5;
            return vec3<f32>(w.a.x * q.x + w.a.y * q.y + w.a.z - 0.5, w.a.w * q.x + w.b.x * q.y + w.b.y - 0.5, 1.0);
        }
        case PERSPECTIVE: {
            let q = p + 0.5;
            let d = w.b.z * q.x + w.b.w * q.y + w.c.x;
            if d <= 1e-9 {
                return vec3<f32>(0.0);
            }
            return vec3<f32>((w.a.x * q.x + w.a.y * q.y + w.a.z) / d - 0.5, (w.a.w * q.x + w.b.x * q.y + w.b.y) / d - 0.5, 1.0);
        }
        default: {
            let q = p + 0.5;
            if w.to_plate == 1u {
                let v = (q - w.a.zw) / w.b.x;
                return vec3<f32>(w.a.zw + v * lens_factor(dot(v, v)) * w.b.x - 0.5, 1.0);
            }
            let u = undistort(q);
            return vec3<f32>(u.xy - 0.5, u.z);
        }
    }
}

fn taps(d: vec2<f32>) -> f32 {
    return min(max(ceil(length(d) - 1e-3), 1.0), MAX_TAPS);
}

// `sample_warped`: bilinear where the warp keeps or enlarges, the mean of
// a grid of taps over the pixel's footprint where it shrinks.
fn sample_warped(o: vec2<i32>) -> vec4<f32> {
    let x = vec2<f32>(o);
    let a = inverse(x);
    if a.z == 0.0 {
        return vec4<f32>(0.0);
    }
    let ax = inverse(x + vec2<f32>(1.0, 0.0));
    let ay = inverse(x + vec2<f32>(0.0, 1.0));
    if ax.z == 0.0 || ay.z == 0.0 {
        return bilinear(a.xy);
    }
    let du = ax.xy - a.xy;
    let dv = ay.xy - a.xy;
    let nu = taps(du);
    let nv = taps(dv);
    if nu == 1.0 && nv == 1.0 {
        return bilinear(a.xy);
    }
    var acc = vec4<f32>(0.0);
    for (var j = 0.0; j < nv; j += 1.0) {
        let t = (j + 0.5) / nv - 0.5;
        for (var k = 0.0; k < nu; k += 1.0) {
            let s = (k + 0.5) / nu - 0.5;
            acc += bilinear(a.xy + du * s + dv * t);
        }
    }
    return acc / (nu * nv);
}

fn outside(id: vec2<u32>) -> bool {
    return i32(id.x) >= w.out_win.z || i32(id.y) >= w.out_win.w;
}

@compute @workgroup_size(16, 16)
fn warp(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    dst[id.y * u32(w.out_win.z) + id.x] = sample_warped(w.out_win.xy + vec2<i32>(id.xy));
}

// Rust's `f32::round`: halves away from zero (WGSL's `round` goes to even).
fn round_away(v: f32) -> f32 {
    return sign(v) * floor(abs(v) + 0.5);
}

// A plane's nearest value from where each pixel came from
// (`Image::carry_planes`).
@compute @workgroup_size(16, 16)
fn warp_plane(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let s = inverse(vec2<f32>(w.out_win.xy + vec2<i32>(id.xy)));
    var v = 0.0;
    if s.z != 0.0 {
        v = plane_at(vec2<i32>(i32(round_away(s.x)), i32(round_away(s.y))));
    }
    dst_plane[id.y * u32(w.out_win.z) + id.x] = v;
}

// Resample (`ops::resample`): a.xy the scaled image's offset, a.zw source
// pixels per output pixel, b.xy taps per side, b.zw / c.xy where the
// scaled data lands (x0, x1, y0, y1).
fn resample_tap(p: vec2<f32>) -> vec4<f32> {
    if w.hard_edges == 1u && w.src_win.z > 0 && w.src_win.w > 0 {
        let lo = vec2<f32>(w.src_win.xy);
        let hi = vec2<f32>(w.src_win.xy + w.src_win.zw - vec2<i32>(1));
        return bilinear(clamp(p, lo, hi));
    }
    return bilinear(p);
}

fn span(a: f32, lo: f32, hi: f32) -> f32 {
    return clamp(min(hi, a + 1.0) - max(lo, a), 0.0, 1.0);
}

@compute @workgroup_size(16, 16)
fn resample(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let o = w.out_win.xy + vec2<i32>(id.xy);
    let x = vec2<f32>(o);
    let fxy = w.a.zw;
    let cxy = (x + 0.5 - w.a.xy) * fxy - 0.5;
    var k = 1.0;
    if w.hard_edges == 1u {
        k = span(x.x, w.b.z, w.b.w) * span(x.y, w.c.x, w.c.y);
    }
    let n = w.b.xy;
    var v: vec4<f32>;
    if n.x == 1.0 && n.y == 1.0 {
        v = resample_tap(cxy) * k;
    } else {
        var acc = vec4<f32>(0.0);
        for (var j = 0.0; j < n.y; j += 1.0) {
            for (var i = 0.0; i < n.x; i += 1.0) {
                let t = cxy + fxy * ((vec2<f32>(i, j) + 0.5) / n - 0.5);
                acc += resample_tap(t);
            }
        }
        v = acc * (k / (n.x * n.y));
    }
    dst[id.y * u32(w.out_win.z) + id.x] = v;
}

@compute @workgroup_size(16, 16)
fn resample_plane(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let x = vec2<f32>(w.out_win.xy + vec2<i32>(id.xy));
    let s = (x + 0.5 - w.a.xy) * w.a.zw - 0.5;
    dst_plane[id.y * u32(w.out_win.z) + id.x] = plane_at(vec2<i32>(i32(round_away(s.x)), i32(round_away(s.y))));
}
