// Pointwise ops (see ops.rs): every output pixel from the inputs' pixels
// at the same place, one op chosen by `p.op`. Each image is a storage
// buffer over its data window (row-major, like `Image.pixels`); reading
// outside the window gives transparent, as `Image::at` does. Formulas
// follow src/ops.rs in the same order of operations.

const GRADE: u32 = 0u;
const EXPOSURE: u32 = 1u;
const SATURATION: u32 = 2u;
const INVERT: u32 = 3u;
const CLAMP: u32 = 4u;
const PREMULT: u32 = 5u;
const UNPREMULT: u32 = 6u;
const CHANNEL_SELECT: u32 = 7u;
const MERGE: u32 = 8u;
const DISSOLVE: u32 = 9u;
const CONSTANT: u32 = 10u;
const CHECKERBOARD: u32 = 11u;
const MASK_MIX: u32 = 12u;
const ACCUMULATE: u32 = 13u;

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

struct Params {
    // Data windows: x0, y0, width, height.
    out_win: vec4<i32>,
    win_a: vec4<i32>,
    win_b: vec4<i32>,
    win_m: vec4<i32>,
    // The op; its mode (merge operation, mask channel); flags (bits per
    // op); an integer (checkerboard size).
    op: u32,
    mode: u32,
    flags: u32,
    n: u32,
    v0: vec4<f32>,
    v1: vec4<f32>,
    v2: vec4<f32>,
    // Channel Select: each output channel's source (0-3 A's, 4-7 B's,
    // 8 zero, 9 one).
    sel: vec4<u32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> img_a: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> img_b: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> img_m: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> img_out: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read> plane_a: array<f32>;
@group(0) @binding(6) var<storage, read> plane_b: array<f32>;
@group(0) @binding(7) var<storage, read_write> plane_out: array<f32>;

// Index of frame pixel `q` in a window, or -1 outside it.
fn index_in(w: vec4<i32>, q: vec2<i32>) -> i32 {
    let l = q - w.xy;
    if l.x < 0 || l.y < 0 || l.x >= w.z || l.y >= w.w {
        return -1;
    }
    return l.y * w.z + l.x;
}

fn at_a(q: vec2<i32>) -> vec4<f32> {
    let i = index_in(p.win_a, q);
    if i < 0 {
        return vec4<f32>(0.0);
    }
    return img_a[i];
}

fn at_b(q: vec2<i32>) -> vec4<f32> {
    let i = index_in(p.win_b, q);
    if i < 0 {
        return vec4<f32>(0.0);
    }
    return img_b[i];
}

fn at_m(q: vec2<i32>) -> vec4<f32> {
    let i = index_in(p.win_m, q);
    if i < 0 {
        return vec4<f32>(0.0);
    }
    return img_m[i];
}

fn premultiply(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

fn unpremultiply(c: vec4<f32>) -> vec4<f32> {
    if c.a > 0.0 {
        return vec4<f32>(c.rgb / c.a, c.a);
    }
    return vec4<f32>(0.0);
}

fn grade(px: vec4<f32>) -> vec4<f32> {
    let s = unpremultiply(px);
    var rgb = vec3<f32>(0.0);
    for (var c = 0; c < 3; c++) {
        var v = s[c] * p.v0[c] + p.v1[c];
        if p.v2[c] != 1.0 {
            v = pow(max(v, 0.0), 1.0 / max(p.v2[c], 1e-6));
        }
        rgb[c] = v;
    }
    return premultiply(vec4<f32>(rgb, s.a));
}

fn invert(px: vec4<f32>) -> vec4<f32> {
    var s = unpremultiply(px);
    if (p.flags & 1u) != 0u {
        s = vec4<f32>(1.0 - s.rgb, s.a);
    }
    if (p.flags & 2u) != 0u {
        s.a = 1.0 - clamp(px.a, 0.0, 1.0);
    } else {
        s.a = px.a;
    }
    return premultiply(s);
}

fn clamp_op(px: vec4<f32>) -> vec4<f32> {
    let lo = p.v0.x;
    let hi = p.v0.y;
    var a = px.a;
    if (p.flags & 1u) != 0u {
        a = clamp(px.a, max(lo, 0.0), min(hi, 1.0));
    }
    return vec4<f32>(clamp(px.rgb, vec3<f32>(lo), vec3<f32>(hi)), a);
}

fn pick(src: u32, a: vec4<f32>, b: vec4<f32>) -> f32 {
    if src < 4u {
        return a[src];
    }
    if src < 8u {
        return b[src - 4u];
    }
    if src == 8u {
        return 0.0;
    }
    return 1.0;
}

// Merge's blend modes, A (`f`) with B (`b`), as `ops::blend`.
fn blend(f: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    switch p.mode {
        // over
        case 0u: {
            let ia = 1.0 - f.a;
            return vec4<f32>(f.rgb + b.rgb * ia, f.a + b.a * ia);
        }
        // under
        case 1u: {
            let ia = 1.0 - b.a;
            return vec4<f32>(b.rgb + f.rgb * ia, b.a + f.a * ia);
        }
        // add
        case 2u: {
            return vec4<f32>(f.rgb + b.rgb, min(f.a + b.a, 1.0));
        }
        // multiply
        case 3u: {
            return f * b;
        }
        // screen
        case 4u: {
            return f + b - f * b;
        }
        // max
        case 5u: {
            return max(f, b);
        }
        // min
        default: {
            return min(f, b);
        }
    }
}

// How much the mask lets through at `q`, 0-1 (`ops::mask_value`).
fn mask_value(q: vec2<i32>) -> f32 {
    let px = at_m(q);
    var v = px.a;
    switch p.mode {
        case 1u: { v = px.r; }
        case 2u: { v = px.g; }
        case 3u: { v = px.b; }
        case 4u: { v = px.r * LUMA.x + px.g * LUMA.y + px.b * LUMA.z; }
        default: {}
    }
    v = clamp(v, 0.0, 1.0);
    if (p.flags & 1u) != 0u {
        return 1.0 - v;
    }
    return v;
}

fn pixel(q: vec2<i32>) -> vec4<f32> {
    switch p.op {
        case GRADE: { return grade(at_a(q)); }
        case EXPOSURE: {
            let px = at_a(q);
            return vec4<f32>(px.rgb * p.v0.x, px.a);
        }
        case SATURATION: {
            let px = at_a(q);
            let y = px.r * LUMA.x + px.g * LUMA.y + px.b * LUMA.z;
            return vec4<f32>(y + (px.rgb - y) * p.v0.x, px.a);
        }
        case INVERT: { return invert(at_a(q)); }
        case CLAMP: { return clamp_op(at_a(q)); }
        case PREMULT: { return premultiply(at_a(q)); }
        case UNPREMULT: { return unpremultiply(at_a(q)); }
        case CHANNEL_SELECT: {
            let a = at_a(q);
            let b = at_b(q);
            return vec4<f32>(pick(p.sel.x, a, b), pick(p.sel.y, a, b), pick(p.sel.z, a, b), pick(p.sel.w, a, b));
        }
        case MERGE: { return blend(at_a(q), at_b(q)); }
        case DISSOLVE: {
            let a = at_a(q);
            return a + (at_b(q) - a) * p.v0.x;
        }
        case CONSTANT: { return p.v0; }
        case CHECKERBOARD: {
            let cell = (u32(q.x) / p.n + u32(q.y) / p.n) % 2u;
            if cell == 0u {
                return p.v0;
            }
            return p.v1;
        }
        // A is the base, B the op's result.
        case MASK_MIX: {
            var t = p.v0.x;
            if (p.flags & 2u) != 0u {
                t *= mask_value(q);
            }
            let b = at_a(q);
            return b + (at_b(q) - b) * t;
        }
        // A running sum (A) plus B weighted (`ops::average`).
        case ACCUMULATE: { return at_a(q) + at_b(q) * p.v0.x; }
        default: { return vec4<f32>(0.0); }
    }
}

@compute @workgroup_size(16, 16)
fn pointwise(@builtin(global_invocation_id) id: vec3<u32>) {
    if i32(id.x) >= p.out_win.z || i32(id.y) >= p.out_win.w {
        return;
    }
    let q = p.out_win.xy + vec2<i32>(id.xy);
    img_out[id.y * u32(p.out_win.z) + id.x] = pixel(q);
}

// A Merge's plane (`ops::merge_planes`): the front input's (A, window
// win_a, its plane over win_m) where it is more than half opaque, the
// back's (its plane over win_b) elsewhere; a plane an input lacks is
// passed with an empty window, so it reads 0.
@compute @workgroup_size(16, 16)
fn merge_plane(@builtin(global_invocation_id) id: vec3<u32>) {
    if i32(id.x) >= p.out_win.z || i32(id.y) >= p.out_win.w {
        return;
    }
    let q = p.out_win.xy + vec2<i32>(id.xy);
    var v = 0.0;
    if at_a(q).a > 0.5 {
        let i = index_in(p.win_m, q);
        if i >= 0 {
            v = plane_a[i];
        }
    } else {
        let i = index_in(p.win_b, q);
        if i >= 0 {
            v = plane_b[i];
        }
    }
    plane_out[id.y * u32(p.out_win.z) + id.x] = v;
}
