// Images on the GPU (see image.rs): storage buffers laid out like
// `Image.pixels`, row-major over the data window, nothing outside it.

// Data windows: x0, y0, width, height.
struct Windows {
    src: vec4<i32>,
    dst: vec4<i32>,
}

@group(0) @binding(0) var<uniform> win: Windows;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> src_plane: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst_plane: array<f32>;

// Where destination pixel `id` reads its source: its index, or -1 outside
// the source window (transparent there).
fn source_index(id: vec2<u32>) -> i32 {
    let x = win.dst.x + i32(id.x) - win.src.x;
    let y = win.dst.y + i32(id.y) - win.src.y;
    if x < 0 || y < 0 || x >= win.src.z || y >= win.src.w {
        return -1;
    }
    return y * win.src.z + x;
}

fn outside(id: vec2<u32>) -> bool {
    return i32(id.x) >= win.dst.z || i32(id.y) >= win.dst.w;
}

// The same pixels over another data window.
@compute @workgroup_size(16, 16)
fn rewindow(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let i = source_index(id.xy);
    var v = vec4<f32>(0.0);
    if i >= 0 {
        v = src[i];
    }
    dst[id.y * u32(win.dst.z) + id.x] = v;
}

// The same for an extra plane.
@compute @workgroup_size(16, 16)
fn rewindow_plane(@builtin(global_invocation_id) id: vec3<u32>) {
    if outside(id.xy) {
        return;
    }
    let i = source_index(id.xy);
    var v = 0.0;
    if i >= 0 {
        v = src_plane[i];
    }
    dst_plane[id.y * u32(win.dst.z) + id.x] = v;
}
