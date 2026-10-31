// The viewer's display of an image on the GPU (see display.rs): as the
// viewer's `to_color_image` does on the CPU, linear premultiplied float
// to 8-bit sRGB with exposure and channel isolation, overscan dimmed, each
// texel the mean of a `step` x `step` block when the image is larger than a
// texture can be.

struct Display {
    // The image's data window: x0, y0, width, height.
    src_win: vec4<i32>,
    // What the texture covers, in frame pixels: x0, y0, x1, y1.
    region: vec4<i32>,
    // The frame's width and height; frame pixels per texel; the texture's size.
    frame: vec2<i32>,
    step: i32,
    // 0 RGB, 1 red, 2 green, 3 blue, 4 alpha.
    channel: u32,
    size: vec2<u32>,
    // 2^exposure.
    gain: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> d: Display;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var out: texture_storage_2d<rgba8unorm, write>;

fn at(q: vec2<i32>) -> vec4<f32> {
    let l = q - d.src_win.xy;
    if l.x < 0 || l.y < 0 || l.x >= d.src_win.z || l.y >= d.src_win.w {
        return vec4<f32>(0.0);
    }
    return src[l.y * d.src_win.z + l.x];
}

// `color::linear_to_srgb`.
fn linear_to_srgb(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

// A byte as Rust's `(x * 255.0 + 0.5) as u8` makes it (truncated, at
// most 255).
fn byte(v: f32) -> f32 {
    return min(floor(v * 255.0 + 0.5), 255.0);
}

@compute @workgroup_size(16, 16)
fn display(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= d.size.x || id.y >= d.size.y {
        return;
    }
    let q = d.region.xy + vec2<i32>(id.xy) * d.step;
    var px: vec4<f32>;
    if d.step == 1 {
        px = at(q);
    } else {
        var sum = vec4<f32>(0.0);
        let end = min(q + vec2<i32>(d.step), d.region.zw);
        for (var sy = q.y; sy < end.y; sy++) {
            for (var sx = q.x; sx < end.x; sx++) {
                sum += at(vec2<i32>(sx, sy));
            }
        }
        let n = f32(max((end.x - q.x) * (end.y - q.y), 1));
        px = sum / n;
    }
    var gain = d.gain;
    if q.x < 0 || q.y < 0 || q.x >= d.frame.x || q.y >= d.frame.y {
        gain *= 0.35;
    }
    var rgb: vec3<f32>;
    switch d.channel {
        case 0u: { rgb = vec3<f32>(byte(linear_to_srgb(px.r * gain)), byte(linear_to_srgb(px.g * gain)), byte(linear_to_srgb(px.b * gain))); }
        case 1u: { rgb = vec3<f32>(byte(linear_to_srgb(px.r * gain))); }
        case 2u: { rgb = vec3<f32>(byte(linear_to_srgb(px.g * gain))); }
        case 3u: { rgb = vec3<f32>(byte(linear_to_srgb(px.b * gain))); }
        // Alpha is coverage, not light: without the sRGB curve.
        default: { rgb = vec3<f32>(byte(clamp(px.a, 0.0, 1.0))); }
    }
    // (n / 255 stores the byte n exactly.)
    textureStore(out, vec2<i32>(id.xy), vec4<f32>(rgb / 255.0, 1.0));
}
