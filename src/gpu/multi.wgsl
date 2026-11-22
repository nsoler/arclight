// Shuffle routes from several inputs and Matte Combine on the GPU (see
// multi.rs): chained passes, each reading one source and writing the
// output being built, so no pass binds more than four storage buffers.
// A thread per pixel of the output window; sources read 0 outside their
// own window, as on the CPU.

struct M {
    // Data windows in the frame: x0, y0, width, height.
    out_win: vec4<i32>,
    src_win: vec4<i32>,
    // Source kind (0 constant, 1 color component, 2 plane, 3 luminance),
    // its component, the destination color component, -.
    a: vec4<u32>,
    // Invert, operation (0 union, 1 subtract, 2 intersect, 3 difference),
    // soft, first step.
    b: vec4<u32>,
    // The constant (x).
    value: vec4<f32>,
}

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

@group(0) @binding(0) var<uniform> m: M;
@group(0) @binding(1) var<storage, read> src_color: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> src_plane: array<f32>;
@group(0) @binding(3) var<storage, read_write> out_color: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> out_plane: array<f32>;

// This thread's frame position, or false when it's past the output.
fn here(id: vec3<u32>, q: ptr<function, vec2<i32>>) -> bool {
    if i32(id.x) >= m.out_win.z || i32(id.y) >= m.out_win.w {
        return false;
    }
    *q = m.out_win.xy + vec2<i32>(id.xy);
    return true;
}

fn out_index(id: vec3<u32>) -> u32 {
    return id.y * u32(m.out_win.z) + id.x;
}

// The source's index at `q`, or -1 outside its window.
fn src_index(q: vec2<i32>) -> i32 {
    let l = q - m.src_win.xy;
    if l.x < 0 || l.y < 0 || l.x >= m.src_win.z || l.y >= m.src_win.w {
        return -1;
    }
    return l.y * m.src_win.z + l.x;
}

fn color_at(q: vec2<i32>) -> vec4<f32> {
    let i = src_index(q);
    if i < 0 {
        return vec4<f32>(0.0);
    }
    return src_color[i];
}

fn plane_at(q: vec2<i32>) -> f32 {
    let i = src_index(q);
    if i < 0 {
        return 0.0;
    }
    return src_plane[i];
}

// The source's value at `q`, as its kind says.
fn source(q: vec2<i32>) -> f32 {
    switch m.a.x {
        case 0u: { return m.value.x; }
        case 1u: { return color_at(q)[m.a.y]; }
        case 2u: { return plane_at(q); }
        default: {
            let px = color_at(q);
            return px.r * LUMA.x + px.g * LUMA.y + px.b * LUMA.z;
        }
    }
}

// One channel of a source into a component of the output's color.
@compute @workgroup_size(16, 16)
fn route_to_color(@builtin(global_invocation_id) id: vec3<u32>) {
    var q: vec2<i32>;
    if !here(id, &q) {
        return;
    }
    let i = out_index(id);
    var c = out_color[i];
    c[m.a.z] = source(q);
    out_color[i] = c;
}

// One channel of a source into an output plane.
@compute @workgroup_size(16, 16)
fn route_to_plane(@builtin(global_invocation_id) id: vec3<u32>) {
    var q: vec2<i32>;
    if !here(id, &q) {
        return;
    }
    out_plane[out_index(id)] = source(q);
}

// A color buffer moved onto the output window (transparent where it had nothing).
@compute @workgroup_size(16, 16)
fn rewindow_color(@builtin(global_invocation_id) id: vec3<u32>) {
    var q: vec2<i32>;
    if !here(id, &q) {
        return;
    }
    out_color[out_index(id)] = color_at(q);
}

// A plane moved onto the output window (0 where it had nothing).
@compute @workgroup_size(16, 16)
fn rewindow_plane(@builtin(global_invocation_id) id: vec3<u32>) {
    var q: vec2<i32>;
    if !here(id, &q) {
        return;
    }
    out_plane[out_index(id)] = plane_at(q);
}

// `matte_combine::combine`.
fn combine(r: f32, v: f32) -> f32 {
    let soft = m.b.z != 0u;
    switch m.b.y {
        case 1u: {
            if soft {
                return r * (1.0 - v);
            }
            return max(r - v, 0.0);
        }
        case 2u: {
            if soft {
                return r * v;
            }
            return min(r, v);
        }
        case 3u: {
            if soft {
                return r + v - 2.0 * r * v;
            }
            return abs(r - v);
        }
        default: {
            if soft {
                return r + v - r * v;
            }
            return max(r, v);
        }
    }
}

// One input of a Matte Combine folded into the result so far (the
// output plane); the first step just writes its matte.
@compute @workgroup_size(16, 16)
fn matte_step(@builtin(global_invocation_id) id: vec3<u32>) {
    var q: vec2<i32>;
    if !here(id, &q) {
        return;
    }
    var v = clamp(source(q), 0.0, 1.0);
    if m.b.x != 0u {
        v = 1.0 - v;
    }
    let i = out_index(id);
    if m.b.w != 0u {
        out_plane[i] = v;
    } else {
        out_plane[i] = combine(out_plane[i], v);
    }
}

// The output's color from the result (read from `src_plane`, over the
// output window): white matte, or with b.x set, input 1's color (the
// source color) with the result as alpha.
@compute @workgroup_size(16, 16)
fn matte_output(@builtin(global_invocation_id) id: vec3<u32>) {
    var q: vec2<i32>;
    if !here(id, &q) {
        return;
    }
    let i = out_index(id);
    let r = src_plane[i];
    if m.b.x != 0u {
        var c = color_at(q);
        c.a = r;
        out_color[i] = c;
    } else {
        out_color[i] = vec4<f32>(r);
    }
}
