// The Keyer (see ops.rs `keyer` and `morph`): color-difference keying
// against a screen color (or a clean plate's), the matte shrunk / grown
// and softened (with blur.wgsl's box passes), then shaded.

struct Key {
    // Data windows: x0, y0, width, height.
    out_win: vec4<i32>,
    src_win: vec4<i32>,
    plate_win: vec4<i32>,
    matte_win: vec4<i32>,
    // 0 keyed, 1 matte, 2 despill only.
    output: u32,
    has_plate: u32,
    // Morph: 1 grows (max), 0 shrinks (min); 1 for an 8-neighbor step.
    grow: u32,
    square: u32,
    // Straight screen color.
    screen: vec4<f32>,
    // balance, clip black, clip span, despill.
    k: vec4<f32>,
    // Morph's last fraction.
    frac: f32,
    _p0: f32,
    _p1: f32,
    _p2: f32,
}

@group(0) @binding(0) var<uniform> key_: Key;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> plate: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> matte: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> dst: array<vec4<f32>>;

fn index_in(win: vec4<i32>, q: vec2<i32>) -> i32 {
    let l = q - win.xy;
    if l.x < 0 || l.y < 0 || l.x >= win.z || l.y >= win.w {
        return -1;
    }
    return l.y * win.z + l.x;
}

fn unpremultiply(c: vec4<f32>) -> vec4<f32> {
    if c.a > 0.0 {
        return vec4<f32>(c.rgb / c.a, c.a);
    }
    return vec4<f32>(0.0);
}

// The other two channels, weighted: balance 1 takes the larger.
fn others(c: vec4<f32>, q: u32, r: u32) -> f32 {
    let hi = max(c[q], c[r]);
    let lo = min(c[q], c[r]);
    return key_.k.x * hi + (1.0 - key_.k.x) * lo;
}

struct Keyed {
    alpha: f32,
    fg: vec4<f32>,
}

fn key(q: vec2<i32>) -> Keyed {
    let i = index_in(key_.src_win, q);
    var px = vec4<f32>(0.0);
    if i >= 0 {
        px = src[i];
    }
    let c = unpremultiply(px);
    var s = key_.screen;
    if key_.has_plate == 1u {
        let j = index_in(key_.plate_win, q);
        if j >= 0 {
            let ps = unpremultiply(plate[j]);
            if ps.a > 0.0 {
                s = ps;
            }
        }
    }
    // The screen's dominant channel and the other two.
    var pc = 1u;
    var oq = 0u;
    var or_ = 2u;
    if s.b > s.g && s.b > s.r {
        pc = 2u;
        oq = 0u;
        or_ = 1u;
    } else if s.r > s.g && s.r > s.b {
        pc = 0u;
        oq = 1u;
        or_ = 2u;
    }
    let ds = s[pc] - others(s, oq, or_);
    let dc = c[pc] - others(c, oq, or_);
    var raw = 1.0;
    if ds > 1e-6 {
        raw = clamp(1.0 - dc / ds, 0.0, 1.0);
    }
    let alpha = clamp((raw - key_.k.y) / key_.k.z, 0.0, 1.0);
    var fg = c;
    fg[pc] -= key_.k.w * max(c[pc] - others(c, oq, or_), 0.0);
    return Keyed(alpha * px.a, fg);
}

fn shade(m: f32, fg: vec4<f32>) -> vec4<f32> {
    switch key_.output {
        case 0u: { return vec4<f32>(fg.rgb * m, m); }
        case 1u: { return vec4<f32>(m, m, m, 1.0); }
        default: { return vec4<f32>(fg.rgb * fg.a, fg.a); }
    }
}

fn outside(id: vec2<u32>) -> bool {
    return i32(id.x) >= key_.out_win.z || i32(id.y) >= key_.out_win.w;
}

fn here(id: vec2<u32>) -> vec2<i32> {
    return key_.out_win.xy + vec2<i32>(id);
}

fn out_index(id: vec2<u32>) -> u32 {
    return id.y * u32(key_.out_win.z) + id.x;
}

// Pixel by pixel (no shrink / grow or softness).
@compute @workgroup_size(16, 16)
fn key_shade(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let k = key(here(id.xy));
    dst[out_index(id.xy)] = shade(k.alpha, k.fg);
}

// The matte alone, in every channel.
@compute @workgroup_size(16, 16)
fn key_matte(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    dst[out_index(id.xy)] = vec4<f32>(key(here(id.xy)).alpha);
}

// One morph step over the matte (out_win is the matte's window): the
// max (or min) over the 4 or 8 neighbors, edges repeated.
@compute @workgroup_size(16, 16)
fn morph_step(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let size = key_.out_win.zw;
    let p = vec2<i32>(id.xy);
    var v = matte[p.y * size.x + p.x].a;
    var n = 4;
    if key_.square == 1u {
        n = 8;
    }
    let steps = array<vec2<i32>, 8>(vec2(-1, 0), vec2(1, 0), vec2(0, -1), vec2(0, 1), vec2(-1, -1), vec2(1, -1), vec2(-1, 1), vec2(1, 1));
    for (var i = 0; i < n; i++) {
        let s = clamp(p + steps[i], vec2<i32>(0), size - 1);
        let o = matte[s.y * size.x + s.x].a;
        if key_.grow == 1u {
            v = max(v, o);
        } else {
            v = min(v, o);
        }
    }
    dst[out_index(id.xy)] = vec4<f32>(v);
}

// The last step's fraction: before (src) toward after (matte).
@compute @workgroup_size(16, 16)
fn morph_blend(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let i = out_index(id.xy);
    let b = src[i].a;
    dst[i] = vec4<f32>(b + (matte[i].a - b) * key_.frac);
}

// Shaded with the finished matte (over matte_win).
@compute @workgroup_size(16, 16)
fn key_final(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let q = here(id.xy);
    let i = index_in(key_.matte_win, q);
    var m = 0.0;
    if i >= 0 {
        m = matte[i].a;
    }
    dst[out_index(id.xy)] = shade(m, key(q).fg);
}
