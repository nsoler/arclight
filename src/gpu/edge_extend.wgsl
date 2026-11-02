// Edge Extend (see edge_extend.rs): pull-push fill of premultiplied color
// and coverage, a square reach limit, then shading. Every entry runs a
// thread per pixel of its output level; the same passes, in the same
// order and arithmetic, as the CPU.

struct Ext {
    // Data windows in the frame: x0, y0, width, height.
    src_win: vec4<i32>,
    matte_win: vec4<i32>,
    // The region the fill covers (level 0 and the output).
    region: vec4<i32>,
    // The fine level's size, then the coarse level's.
    dims: vec4<u32>,
    // Matte channel (alpha, red, green, blue, luminance), invert,
    // has a matte, alpha mode (0 solid, 1 original).
    flags: vec4<u32>,
    // Reach radius, fill everything, -, -.
    more: vec4<u32>,
}

@group(0) @binding(0) var<uniform> e: Ext;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> matte: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> a: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> b: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> dst: array<vec4<f32>>;

fn at(win: vec4<i32>, q: vec2<i32>, which: u32) -> vec4<f32> {
    let l = q - win.xy;
    if l.x < 0 || l.y < 0 || l.x >= win.z || l.y >= win.w {
        return vec4<f32>(0.0);
    }
    let i = l.y * win.z + l.x;
    if which == 0u {
        return src[i];
    }
    return matte[i];
}

fn unpremultiply(c: vec4<f32>) -> vec4<f32> {
    if c.a > 0.0 {
        return vec4<f32>(c.r / c.a, c.g / c.a, c.b / c.a, c.a);
    }
    return vec4<f32>(0.0);
}

fn coverage(px: vec4<f32>) -> f32 {
    var v = px.a;
    switch e.flags.x {
        case 1u: { v = px.r; }
        case 2u: { v = px.g; }
        case 3u: { v = px.b; }
        case 4u: { v = px.r * 0.2126 + px.g * 0.7152 + px.b * 0.0722; }
        default: {}
    }
    v = clamp(v, 0.0, 1.0);
    if e.flags.y == 1u {
        return 1.0 - v;
    }
    return v;
}

// Level 0: straight color × coverage, and the coverage.
@compute @workgroup_size(16, 16)
fn seed(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = e.dims.x;
    if id.x >= w || id.y >= e.dims.y {
        return;
    }
    let q = e.region.xy + vec2<i32>(id.xy);
    let px = at(e.src_win, q, 0u);
    var out = vec4<f32>(0.0);
    if px.a > 0.0 {
        var m = px;
        if e.flags.z == 1u {
            m = at(e.matte_win, q, 1u);
        }
        let cov = coverage(m);
        let c = unpremultiply(px);
        out = vec4<f32>(c.r * cov, c.g * cov, c.b * cov, cov);
    }
    dst[id.y * w + id.x] = out;
}

fn fine_at(x: u32, y: u32) -> vec4<f32> {
    if x < e.dims.x && y < e.dims.y {
        return a[y * e.dims.x + x];
    }
    return vec4<f32>(0.0);
}

// The next level up: 2×2 sums, coverage capped at 1.
@compute @workgroup_size(16, 16)
fn pull(@builtin(global_invocation_id) id: vec3<u32>) {
    let cw = e.dims.z;
    if id.x >= cw || id.y >= e.dims.w {
        return;
    }
    let x = 2u * id.x;
    let y = 2u * id.y;
    let s = fine_at(x, y) + fine_at(x + 1u, y) + fine_at(x, y + 1u) + fine_at(x + 1u, y + 1u);
    var out = vec4<f32>(0.0);
    if s.a > 0.0 {
        let w = min(s.a, 1.0);
        let k = w / s.a;
        out = vec4<f32>(s.r * k, s.g * k, s.b * k, w);
    }
    dst[id.y * cw + id.x] = out;
}

fn coarse_at(x: u32, y: u32) -> vec4<f32> {
    return b[y * e.dims.z + x];
}

fn clampi(v: f32, n: u32) -> u32 {
    return min(u32(max(v, 0.0)), n - 1u);
}

// A level filled from the filled level above (b), the gaps its coverage
// leaves taking the coarse level's bilinear upsample.
@compute @workgroup_size(16, 16)
fn push(@builtin(global_invocation_id) id: vec3<u32>) {
    let fw = e.dims.x;
    if id.x >= fw || id.y >= e.dims.y {
        return;
    }
    let fx = f32(id.x) * 0.5 - 0.25;
    let fy = f32(id.y) * 0.5 - 0.25;
    let x0 = floor(fx);
    let y0 = floor(fy);
    let tx = fx - x0;
    let ty = fy - y0;
    let xa = clampi(x0, e.dims.z);
    let xb = clampi(x0 + 1.0, e.dims.z);
    let ya = clampi(y0, e.dims.w);
    let yb = clampi(y0 + 1.0, e.dims.w);
    let top = coarse_at(xa, ya) * (1.0 - tx) + coarse_at(xb, ya) * tx;
    let bottom = coarse_at(xa, yb) * (1.0 - tx) + coarse_at(xb, yb) * tx;
    let u = top * (1.0 - ty) + bottom * ty;
    let f = a[id.y * fw + id.x];
    dst[id.y * fw + id.x] = f + (1.0 - f.a) * u;
}

// Within the reach of coverage, across (a: level 0, its coverage).
@compute @workgroup_size(16, 16)
fn reach_h(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = e.dims.x;
    if id.x >= w || id.y >= e.dims.y {
        return;
    }
    let r = e.more.x;
    let lo = select(0u, id.x - r, id.x >= r);
    let hi = min(id.x + r, w - 1u);
    var hit = 0.0;
    for (var x = lo; x <= hi; x++) {
        if a[id.y * w + x].a > 0.0 {
            hit = 1.0;
            break;
        }
    }
    dst[id.y * w + id.x] = vec4<f32>(hit, 0.0, 0.0, 0.0);
}

// ... then down (a: reach_h's result).
@compute @workgroup_size(16, 16)
fn reach_v(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = e.dims.x;
    let h = e.dims.y;
    if id.x >= w || id.y >= h {
        return;
    }
    let r = e.more.x;
    let lo = select(0u, id.y - r, id.y >= r);
    let hi = min(id.y + r, h - 1u);
    var hit = 0.0;
    for (var y = lo; y <= hi; y++) {
        if a[y * w + id.x].x > 0.0 {
            hit = 1.0;
            break;
        }
    }
    dst[id.y * w + id.x] = vec4<f32>(hit, 0.0, 0.0, 0.0);
}

// The output (a: filled level 0, b: the reach unless filling everything).
@compute @workgroup_size(16, 16)
fn shade(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = e.dims.x;
    if id.x >= w || id.y >= e.dims.y {
        return;
    }
    let i = id.y * w + id.x;
    let f = a[i];
    var c = vec3<f32>(0.0);
    if f.a > 0.0 {
        c = vec3<f32>(f.r / f.a, f.g / f.a, f.b / f.a);
    }
    let reached = e.more.y == 1u || b[i].x > 0.0;
    let px = at(e.src_win, e.region.xy + vec2<i32>(id.xy), 0u);
    var out = vec4<f32>(0.0);
    if e.flags.w == 0u {
        if reached {
            out = vec4<f32>(c, 1.0);
        }
    } else if reached {
        out = vec4<f32>(c, px.a);
    } else {
        out = vec4<f32>(unpremultiply(px).rgb, px.a);
    }
    dst[i] = out;
}
