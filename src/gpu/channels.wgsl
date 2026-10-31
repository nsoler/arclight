// Channels moved about (Shuffle) and images merged by depth (Merge's
// zmerge); see ops.rs. Images are storage buffers over their data windows;
// outside a window reads as transparent (planes as 0).

struct Params {
    // Data windows: x0, y0, width, height (an absent input's is empty).
    out_win: vec4<i32>,
    win_a: vec4<i32>,
    win_b: vec4<i32>,
    // Shuffle: each output channel's source kind (0 a constant, 1 a
    // component of the color, 2 plane slot), its component or slot, and
    // the constant.
    kind: vec4<u32>,
    index: vec4<u32>,
    value: vec4<f32>,
    // zmerge: the plane's rule (0 depth, 1 coverage, 2 data), and which
    // inputs have the plane (bit 0 a, bit 1 b) and a depth plane (bit 2 a,
    // bit 3 b).
    rule: u32,
    has: u32,
    _p0: u32,
    _p1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
// Shuffle: the color and up to four planes in; color or a plane out.
@group(0) @binding(1) var<storage, read> color: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> plane0: array<f32>;
@group(0) @binding(3) var<storage, read> plane1: array<f32>;
@group(0) @binding(4) var<storage, read> plane2: array<f32>;
@group(0) @binding(5) var<storage, read> plane3: array<f32>;
@group(0) @binding(6) var<storage, read_write> out_color: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> out_plane: array<f32>;
// zmerge: A's and B's color and depth, and a plane of each.
@group(0) @binding(8) var<storage, read> color_b: array<vec4<f32>>;
@group(0) @binding(9) var<storage, read> depth_a: array<f32>;
@group(0) @binding(10) var<storage, read> depth_b: array<f32>;
@group(0) @binding(11) var<storage, read> plane_a: array<f32>;
@group(0) @binding(12) var<storage, read> plane_b: array<f32>;

fn outside(id: vec2<u32>) -> bool {
    return i32(id.x) >= p.out_win.z || i32(id.y) >= p.out_win.w;
}

fn index_in(w: vec4<i32>, q: vec2<i32>) -> i32 {
    let l = q - w.xy;
    if l.x < 0 || l.y < 0 || l.x >= w.z || l.y >= w.w {
        return -1;
    }
    return l.y * w.z + l.x;
}

// -- Shuffle (same window in and out) ----------------------------------------

fn source(c: u32, i: u32) -> f32 {
    switch p.kind[c] {
        case 1u: { return color[i][p.index[c]]; }
        case 2u: {
            switch p.index[c] {
                case 0u: { return plane0[i]; }
                case 1u: { return plane1[i]; }
                case 2u: { return plane2[i]; }
                default: { return plane3[i]; }
            }
        }
        default: { return p.value[c]; }
    }
}

// The color from four sources.
@compute @workgroup_size(16, 16)
fn gather(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let i = id.y * u32(p.out_win.z) + id.x;
    out_color[i] = vec4<f32>(source(0u, i), source(1u, i), source(2u, i), source(3u, i));
}

// A plane from one source (channel 0's).
@compute @workgroup_size(16, 16)
fn extract(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let i = id.y * u32(p.out_win.z) + id.x;
    out_plane[i] = source(0u, i);
}

// -- zmerge (`Image::zmerge`) -------------------------------------------------

fn at_a(q: vec2<i32>) -> vec4<f32> {
    let i = index_in(p.win_a, q);
    if i < 0 {
        return vec4<f32>(0.0);
    }
    return color[i];
}

fn at_b(q: vec2<i32>) -> vec4<f32> {
    let i = index_in(p.win_b, q);
    if i < 0 {
        return vec4<f32>(0.0);
    }
    return color_b[i];
}

fn depth(q: vec2<i32>, of_b: bool) -> f32 {
    if of_b {
        let i = index_in(p.win_b, q);
        if i < 0 || (p.has & 8u) == 0u {
            return 0.0;
        }
        return depth_b[i];
    }
    let i = index_in(p.win_a, q);
    if i < 0 || (p.has & 4u) == 0u {
        return 0.0;
    }
    return depth_a[i];
}

// A in front: nearer, or the only one with depth, or (neither has) as given.
fn a_front(q: vec2<i32>) -> bool {
    let da = depth(q, false);
    let db = depth(q, true);
    if da > 0.0 && db > 0.0 {
        return da <= db;
    }
    return db <= 0.0;
}

fn here(id: vec2<u32>) -> vec2<i32> {
    return p.out_win.xy + vec2<i32>(id);
}

@compute @workgroup_size(16, 16)
fn zmerge(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let q = here(id.xy);
    var f = at_a(q);
    var k = at_b(q);
    if !a_front(q) {
        f = at_b(q);
        k = at_a(q);
    }
    out_color[id.y * u32(p.out_win.z) + id.x] = f + k * (1.0 - f.a);
}

fn plane_value(q: vec2<i32>, of_b: bool) -> f32 {
    if of_b {
        let i = index_in(p.win_b, q);
        if i < 0 || (p.has & 2u) == 0u {
            return 0.0;
        }
        return plane_b[i];
    }
    let i = index_in(p.win_a, q);
    if i < 0 || (p.has & 1u) == 0u {
        return 0.0;
    }
    return plane_a[i];
}

@compute @workgroup_size(16, 16)
fn zmerge_plane(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let q = here(id.xy);
    let va = plane_value(q, false);
    let vb = plane_value(q, true);
    let front_a = a_front(q);
    var v: f32;
    switch p.rule {
        // The nearer positive depth.
        case 0u: {
            v = 0.0;
            if va > 0.0 {
                v = va;
            }
            if vb > 0.0 {
                if v == 0.0 {
                    v = vb;
                } else {
                    v = min(v, vb);
                }
            }
        }
        // Coverage: over, as the color.
        case 1u: {
            if front_a {
                v = va + vb * (1.0 - at_a(q).a);
            } else {
                v = vb + va * (1.0 - at_b(q).a);
            }
        }
        // Data: the front's where it has the plane and covers the pixel.
        default: {
            if front_a {
                v = vb;
                if (p.has & 1u) != 0u && at_a(q).a > 0.0 {
                    v = va;
                }
            } else {
                v = va;
                if (p.has & 2u) != 0u && at_b(q).a > 0.0 {
                    v = vb;
                }
            }
        }
    }
    out_plane[id.y * u32(p.out_win.z) + id.x] = v;
}
