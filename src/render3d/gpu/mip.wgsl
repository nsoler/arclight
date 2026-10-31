// A texture's next mipmap level from the one above, as render3d/mip.rs
// makes it on the CPU: each texel the mean of the four under it, a
// past-the-edge one wrapping round or repeating the edge; the first level
// reads the texture through its data window (transparent outside).

struct Params {
    // the level above: where its texels start, its frame width and
    // height, whether the texture wraps
    src: vec4<u32>,
    // its data window: x0, y0 (i32 bits), width, height
    window: vec4<u32>,
    // this level: where its texels start, width, height
    dst: vec4<u32>,
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read_write> texels: array<vec4<f32>>;

fn tap(x_in: u32, y_in: u32) -> vec4<f32> {
    let pw = P.src.y;
    let ph = P.src.z;
    var x = min(x_in, pw - 1u);
    var y = min(y_in, ph - 1u);
    if (P.src.w != 0u) {
        x = x_in % pw;
        y = y_in % ph;
    }
    let lx = i32(x) - bitcast<i32>(P.window.x);
    let ly = i32(y) - bitcast<i32>(P.window.y);
    if (lx < 0 || ly < 0 || lx >= i32(P.window.z) || ly >= i32(P.window.w)) { return vec4<f32>(0.0); }
    return texels[P.src.x + u32(ly) * P.window.z + u32(lx)];
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= P.dst.y || gid.y >= P.dst.z) { return; }
    let x = 2u * gid.x;
    let y = 2u * gid.y;
    let sum = tap(x, y) + tap(x + 1u, y) + tap(x, y + 1u) + tap(x + 1u, y + 1u);
    texels[P.dst.x + gid.y * P.dst.y + gid.x] = sum * 0.25;
}
