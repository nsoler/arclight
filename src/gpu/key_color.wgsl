// Color Difference Key (see key_color.rs `shade`): a weighted color-
// difference matte against a screen color or a screen plate's, the
// screen's share taken out of the color, despill. Pixel by pixel.

struct Params {
    // Data windows: x0, y0, width, height.
    out_win: vec4<i32>,
    src_win: vec4<i32>,
    plate_win: vec4<i32>,
    // output (0 keyed, 1 matte, 2 despill only), has a screen plate,
    // subtract the screen, unused.
    flags: vec4<u32>,
    // The screen channel, the red-weighted one, the second-weighted one.
    chans: vec4<u32>,
    // Straight screen color.
    screen: vec4<f32>,
    // red weight, second weight, clip black, clip span.
    k: vec4<f32>,
    // despill.
    k2: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p_: Params;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> plate: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> dst: array<vec4<f32>>;

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

fn diff(c: vec4<f32>) -> f32 {
    return c[p_.chans.x] - (p_.k.x * c[p_.chans.y] + p_.k.y * c[p_.chans.z]);
}

fn despilled(c: vec4<f32>) -> vec4<f32> {
    var o = c;
    o[p_.chans.x] -= p_.k2.x * max(diff(c), 0.0);
    return o;
}

fn shade(px: vec4<f32>, q: vec2<i32>) -> vec4<f32> {
    let pa = px.a;
    let c = unpremultiply(px);
    if p_.flags.x == 2u {
        let f = despilled(c);
        return vec4<f32>(f.rgb * pa, pa);
    }
    var s = p_.screen;
    if p_.flags.y == 1u {
        let j = index_in(p_.plate_win, q);
        if j >= 0 {
            let ps = unpremultiply(plate[j]);
            if ps.a > 0.0 {
                s = ps;
            }
        }
    }
    let ds = diff(s);
    var raw = 1.0;
    if ds > 1e-6 {
        raw = clamp(1.0 - diff(c) / ds, 0.0, 1.0);
    }
    let alpha = clamp((raw - p_.k.z) / p_.k.w, 0.0, 1.0);
    let m = alpha * pa;
    if p_.flags.x == 1u {
        return vec4<f32>(m, m, m, 1.0);
    }
    var u: vec4<f32>;
    if p_.flags.z == 1u {
        u = vec4<f32>(max(c.rgb - (1.0 - alpha) * s.rgb, vec3<f32>(0.0)), alpha);
    } else {
        u = vec4<f32>(c.rgb * alpha, alpha);
    }
    u = despilled(u);
    return vec4<f32>(u.rgb * pa, m);
}

@compute @workgroup_size(16, 16)
fn key(@builtin(global_invocation_id) id: vec3<u32>) {
    if i32(id.x) >= p_.out_win.z || i32(id.y) >= p_.out_win.w {
        return;
    }
    let q = p_.out_win.xy + vec2<i32>(id.xy);
    let i = index_in(p_.src_win, q);
    var px = vec4<f32>(0.0);
    if i >= 0 {
        px = src[i];
    }
    dst[id.y * u32(p_.out_win.z) + id.x] = shade(px, q);
}
