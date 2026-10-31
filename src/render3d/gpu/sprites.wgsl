// Particle sprites on the GPU: what particles_render.rs's `draw` does,
// one thread a pixel. The sprites come sorted as it sorts them (farthest
// first when laid over each other) and binned into 16 × 16 tiles in that
// order; each pixel goes through its tile's in turn, from the background
// (or nothing) the buffers start with.
//
// A sprite is SPRITE_WORDS words (f32 as bits): middle x, y (frame
// pixels), radius (at least half a pixel), faint (a sub-pixel sprite's
// share), depth, cos and sin of its turn, color r g b a, image (NONE: no
// texture). An image is IMAGE_WORDS words in `images`: where its texels
// start, frame width and height, window x0 y0 (i32 bits), width, height.

const TILE: u32 = 16u;
const NONE: u32 = 0xffffffffu;
const SPRITE_WORDS: u32 = 12u;
const IMAGE_WORDS: u32 = 8u;

// Flags.
const DEPTH: u32 = 1u;
const ADDITIVE: u32 = 2u;
const SQUARE: u32 = 4u;

struct Params {
    // width, height, tiles across, flags
    size: vec4<u32>,
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> sprites: array<u32>;
// Tile starts (one more than there are tiles, as indexes into the whole
// array), then each tile's sprites in drawing order.
@group(0) @binding(2) var<storage, read> bins: array<u32>;
@group(0) @binding(3) var<storage, read> images: array<u32>;
@group(0) @binding(4) var<storage, read> texels: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> color: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> depth: array<f32>;

fn sf(s: u32, k: u32) -> f32 { return bitcast<f32>(sprites[s * SPRITE_WORDS + k]); }

// Image k's pixel at frame position (x, y): transparent outside its window.
fn pixel(k: u32, x: i32, y: i32) -> vec4<f32> {
    let o = k * IMAGE_WORDS;
    let lx = x - bitcast<i32>(images[o + 3u]);
    let ly = y - bitcast<i32>(images[o + 4u]);
    let ww = i32(images[o + 5u]);
    let wh = i32(images[o + 6u]);
    if (lx < 0 || ly < 0 || lx >= ww || ly >= wh) { return vec4<f32>(0.0); }
    return texels[images[o] + u32(ly * ww + lx)];
}

// Image k at (u, v), 0 to 1 across its frame (v down), bilinear.
fn sample(k: u32, u: f32, v: f32) -> vec4<f32> {
    let o = k * IMAGE_WORDS;
    let x = u * f32(images[o + 1u]) - 0.5;
    let y = v * f32(images[o + 2u]) - 0.5;
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let ix = i32(x0);
    let iy = i32(y0);
    let top = pixel(k, ix, iy) * (1.0 - fx) + pixel(k, ix + 1, iy) * fx;
    let bot = pixel(k, ix, iy + 1) * (1.0 - fx) + pixel(k, ix + 1, iy + 1) * fx;
    return top * (1.0 - fy) + bot * fy;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = P.size.x;
    let h = P.size.y;
    if (gid.x >= w || gid.y >= h) { return; }
    let flags = P.size.w;
    let i = gid.y * w + gid.x;
    let tile = (gid.y / TILE) * P.size.z + gid.x / TILE;
    let first = bins[tile];
    let last = bins[tile + 1u];
    if (first == last) { return; }
    var dst = color[i];
    var d = depth[i];
    // What the background puts in front.
    let wall = d;
    let x = f32(gid.x);
    let y = f32(gid.y);
    for (var k = first; k < last; k++) {
        let s = bins[k];
        let at = vec2<f32>(sf(s, 0u), sf(s, 1u));
        let r = sf(s, 2u);
        let sd = sf(s, 4u);
        // Squares reach their corners.
        var reach = r;
        if ((flags & SQUARE) != 0u) { reach = r * 1.41421356237; }
        if (x < max(floor(at.x - reach), 0.0) || x > ceil(at.x + reach) || y < max(floor(at.y - reach), 0.0) || y > ceil(at.y + reach)) { continue; }
        if ((flags & DEPTH) != 0u && wall > 0.0 && wall < sd) { continue; }
        let dx = x + 0.5 - at.x;
        let dy = y + 0.5 - at.y;
        let c = sf(s, 5u);
        let sn = sf(s, 6u);
        // In the sprite's own axes (turned back), −1 to 1 across.
        let u = (dx * c + dy * sn) / r;
        let v = (-dx * sn + dy * c) / r;
        // Coverage with a pixel-wide soft edge.
        var cover: f32;
        if ((flags & SQUARE) != 0u) {
            cover = clamp((1.0 - abs(u)) * r + 0.5, 0.0, 1.0) * clamp((1.0 - abs(v)) * r + 0.5, 0.0, 1.0);
        } else {
            cover = clamp(r - length(vec2<f32>(dx, dy)) + 0.5, 0.0, 1.0);
        }
        cover *= sf(s, 3u);
        if (cover <= 0.0) { continue; }
        var tex = vec4<f32>(1.0);
        let image = sprites[s * SPRITE_WORDS + 11u];
        if (image != NONE) { tex = sample(image, (u + 1.0) / 2.0, (v + 1.0) / 2.0); }
        let src = vec4<f32>(sf(s, 7u), sf(s, 8u), sf(s, 9u), sf(s, 10u)) * tex * cover;
        if (src.a <= 0.0 && all(src.rgb <= vec3<f32>(0.0))) { continue; }
        if ((flags & ADDITIVE) != 0u) {
            dst = vec4<f32>(dst.rgb + src.rgb, dst.a + src.a * (1.0 - dst.a));
        } else {
            dst = src + dst * (1.0 - src.a);
        }
        if ((flags & DEPTH) != 0u && (d == 0.0 || sd < d)) { d = sd; }
    }
    color[i] = dst;
    depth[i] = d;
}
