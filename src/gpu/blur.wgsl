// Blur (see ops.rs `blur`): one box pass of radius `r` along rows or
// columns of an image `w` × `h` (its data window), edge pixels repeated.
// Each thread sums its 2r + 1 taps.

struct Pass {
    w: i32,
    h: i32,
    r: i32,
    // 1 along rows, 0 down columns.
    horizontal: u32,
}

@group(0) @binding(0) var<uniform> pass_: Pass;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> dst: array<vec4<f32>>;

@compute @workgroup_size(16, 16)
fn box_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = i32(id.x);
    let y = i32(id.y);
    if x >= pass_.w || y >= pass_.h {
        return;
    }
    var acc = vec4<f32>(0.0);
    for (var k = -pass_.r; k <= pass_.r; k++) {
        var i: i32;
        if pass_.horizontal == 1u {
            i = y * pass_.w + clamp(x + k, 0, pass_.w - 1);
        } else {
            i = clamp(y + k, 0, pass_.h - 1) * pass_.w + x;
        }
        acc += src[i];
    }
    dst[y * pass_.w + x] = acc / f32(2 * pass_.r + 1);
}
