// Motion blur with particle sprites, on the GPU: each time renders into a
// scratch frame (raster.wgsl's buffers), `resolve` turns it into a picture
// and a depth the sprites go over (sprites.wgsl draws them there), and
// `add` puts the jitter cell's pixels of that into the frame summing every
// time. The sums keep raster.wgsl's scale (a render's samples added up),
// so the mean comes out as without sprites.

struct Params {
    // width, height, extra channels a pixel, samples a pixel (n × n)
    size: vec4<u32>,
    // jitter cell side (0: every pixel), its x and y
    cell: vec4<u32>,
};

// All set: a pixel whose samples met more surfaces than kept (see
// raster.wgsl's `out_hits`).
const OVERFLOWED: u32 = 0xffffffffu;

@group(0) @binding(0) var<uniform> P: Params;
// The scratch frame: one time's render (raster.wgsl's out_color, out_extra
// and out_hits).
@group(0) @binding(1) var<storage, read> time_color: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> time_extra: array<f32>;
@group(0) @binding(3) var<storage, read> time_hits: array<vec4<u32>>;
// That render as a picture and depth, which the sprites go over.
@group(0) @binding(4) var<storage, read_write> picture: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> depth: array<f32>;
// The frame summing every time (raster.wgsl's layout).
@group(0) @binding(6) var<storage, read_write> sum_color: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> sum_extra: array<f32>;
@group(0) @binding(8) var<storage, read_write> sum_hits: array<vec4<u32>>;

fn in_cell(x: u32, y: u32) -> bool {
    let side = P.cell.x;
    return side == 0u || (x % side == P.cell.y && y % side == P.cell.z);
}

// The time's picture (its samples' mean) and depth (0: nothing there).
@compute @workgroup_size(16, 16)
fn resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = P.size.x;
    let h = P.size.y;
    if (gid.x >= w || gid.y >= h) { return; }
    let px = gid.y * w + gid.x;
    let pixels = w * h;
    picture[px] = time_color[px] / f32(P.size.w);
    let bits = time_hits[3u * pixels + px / 4u][px % 4u];
    depth[px] = select(bitcast<f32>(bits), 0.0, bits == OVERFLOWED);
}

// The cell's pixels of the time (sprites and all) into the sum, at a
// render's scale; a pixel that overflowed stays marked.
@compute @workgroup_size(16, 16)
fn add(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = P.size.x;
    let h = P.size.y;
    if (gid.x >= w || gid.y >= h || !in_cell(gid.x, gid.y)) { return; }
    let px = gid.y * w + gid.x;
    let pixels = w * h;
    sum_color[px] += picture[px] * f32(P.size.w);
    let stride = P.size.z;
    for (var k = 0u; k < stride; k++) {
        sum_extra[px * stride + k] += time_extra[px * stride + k];
    }
    if (time_hits[3u * pixels + px / 4u][px % 4u] == OVERFLOWED) {
        sum_hits[3u * pixels + px / 4u][px % 4u] = OVERFLOWED;
    }
}
