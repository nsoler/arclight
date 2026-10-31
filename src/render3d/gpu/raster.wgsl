// Scanline Render on the GPU: one thread per pixel, the same steps as the
// CPU renderer's `draw_passes` (see mod.rs). Each dispatch takes one row
// of every pixel's samples and adds to what the earlier rows left. A
// second entry point, `shadow_map`, draws a light's shadow map as
// shadow.rs does (the nearest caster per texel); the shading looks them up
// as shadow.rs's `Shadows::visible` does.
//
// Tables are flat u32 words (f32 values as their bits), laid out by
// mod.rs: TRI_WORDS a triangle, LEVEL_WORDS a texture level (in `bins`);
// in `tables`, SURF_WORDS a surface, then from P.shadow: LIGHT_WORDS a
// light, SHL_WORDS a light's shadows, SRC_WORDS a shadow source (one map
// or a cube of six), MAP_WORDS a map.

// The surfaces a sample keeps (mod.rs builds a second pipeline with more,
// for frames where these overflow).
const MAX_HITS: u32 = 16u;
const MAX_OBJECTS: u32 = 32u;
const MAX_LIGHT_NODES: u32 = 16u;
const TILE: u32 = 16u;
const NONE: u32 = 0xffffffffu;
const OVERFLOW: u32 = 0x80000000u;
// How much light may still come through a sample's 16 nearest surfaces
// with more behind them before the pixel goes to the CPU: below it, what
// the rest would add is within the GPU's tolerance.
const OVERFLOW_LEFT: f32 = 1e-4;

const TRI_WORDS: u32 = 42u;
const SURF_WORDS: u32 = 16u;
const LIGHT_WORDS: u32 = 36u;
const LEVEL_WORDS: u32 = 8u;
const SHL_WORDS: u32 = 4u;
const SRC_WORDS: u32 = 8u;
const MAP_WORDS: u32 = 28u;
// Nothing in a shadow map's texel.
const FAR_AWAY: f32 = 3.0e38;

// Flags.
const DIFFUSE: u32 = 1u;
const SPECULAR: u32 = 2u;
const EMISSION: u32 = 4u;
const AMBIENT: u32 = 8u;
const LIGHTS: u32 = 16u;
const MATTES: u32 = 32u;
const ANY_PART: u32 = 64u;
const SHADOW: u32 = 128u;

// Surface flags.
const S_MATERIAL: u32 = 1u;
const S_HOLDOUT: u32 = 8u;
const S_HEADLIGHT: u32 = 16u;
const S_TWO_SIDED: u32 = 32u;
const S_WRAP: u32 = 64u;
const S_CASTS: u32 = 128u;
const S_RECEIVES: u32 = 256u;

// Light kinds.
const K_AMBIENT: u32 = 0u;
const K_DIRECTIONAL: u32 = 1u;
const K_POINT: u32 = 2u;
const K_SPOT: u32 = 3u;
const K_AREA: u32 = 4u;

struct Params {
    // width, height, samples a side, tiles across
    size: vec4<u32>,
    // jitter cell side (0: every pixel), its x and y, the sample row this
    // dispatch takes
    cell: vec4<u32>,
    // extra channels a pixel, flags, mattes at, lights at
    slots: vec4<u32>,
    // diffuse, specular, emission, ambient at
    parts: vec4<u32>,
    // lights, light nodes, objects, shadow at
    counts: vec4<u32>,
    // where the tile lists start and the texture levels start, in `bins`
    tables: vec4<u32>,
    // near, far, orthographic (1) or not (0)
    cam: vec4<f32>,
    // the camera's position
    eye: vec4<f32>,
    // its +Z in the scene (back toward an orthographic camera)
    back: vec4<f32>,
    // where the lights, their shadows, the shadow sources and the maps
    // start in `tables`
    shadow: vec4<u32>,
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> tris: array<u32>;
@group(0) @binding(2) var<storage, read> bins: array<u32>;
@group(0) @binding(3) var<storage, read> tables: array<u32>;
@group(0) @binding(4) var<storage, read> texels: array<vec4<f32>>;
// Every shadow map's texels: the nearest caster's distance (FAR_AWAY:
// none).
@group(0) @binding(5) var<storage, read> maps: array<f32>;
@group(0) @binding(6) var<storage, read_write> out_color: array<vec4<f32>>;
@group(0) @binding(7) var<storage, read_write> out_extra: array<f32>;
// Each pixel's nearest surfaces, in four parts of `pixels` entries each
// (the last a quarter as long): the nearest surface seen at least half
// opaque and the nearest fainter one, kept from row to row as (triangle +
// 1, 0 none; overflow in the first's top bit, barycentric b0, b1, depth
// bits); after the last row, the one the passes take; and every pixel's
// depth, four to an entry (0 nothing; all bits set: overflowed). The
// buffers start cleared.
@group(0) @binding(8) var<storage, read_write> out_hits: array<vec4<u32>>;

fn tu(t: u32, k: u32) -> u32 { return tris[t * TRI_WORDS + k]; }
fn tf(t: u32, k: u32) -> f32 { return bitcast<f32>(tris[t * TRI_WORDS + k]); }
fn tv2(t: u32, k: u32) -> vec2<f32> { return vec2<f32>(tf(t, k), tf(t, k + 1u)); }
fn tv3(t: u32, k: u32) -> vec3<f32> { return vec3<f32>(tf(t, k), tf(t, k + 1u), tf(t, k + 2u)); }
fn ti(t: u32, k: u32) -> i32 { return bitcast<i32>(tris[t * TRI_WORDS + k]); }

fn su(s: u32, k: u32) -> u32 { return tables[s * SURF_WORDS + k]; }
fn sf(s: u32, k: u32) -> f32 { return bitcast<f32>(tables[s * SURF_WORDS + k]); }
fn sv3(s: u32, k: u32) -> vec3<f32> { return vec3<f32>(sf(s, k), sf(s, k + 1u), sf(s, k + 2u)); }

fn lu(l: u32, k: u32) -> u32 { return tables[P.shadow.x + l * LIGHT_WORDS + k]; }
fn lf(l: u32, k: u32) -> f32 { return bitcast<f32>(tables[P.shadow.x + l * LIGHT_WORDS + k]); }
fn lv3(l: u32, k: u32) -> vec3<f32> { return vec3<f32>(lf(l, k), lf(l, k + 1u), lf(l, k + 2u)); }
fn lv4(l: u32, k: u32) -> vec4<f32> { return vec4<f32>(lf(l, k), lf(l, k + 1u), lf(l, k + 2u), lf(l, k + 3u)); }

fn edge(a: vec2<f32>, b: vec2<f32>, c: vec2<f32>) -> f32 {
    return (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
}

// `v` made unit, or `or` when it has no length.
fn unit_or(v: vec3<f32>, or: vec3<f32>) -> vec3<f32> {
    let n = length(v);
    if (n > 0.0) { return v / n; }
    return or;
}

// Edge a → b's function at q, worked out from its ends in one order
// whichever way round it's asked for: the two triangles sharing an edge
// get the same value, signs opposite, so no sample on it falls between
// them (f32 would otherwise leave cracks along shared edges).
fn edge_shared(a: vec2<f32>, b: vec2<f32>, q: vec2<f32>) -> f32 {
    if (a.x < b.x || (a.x == b.x && a.y < b.y)) { return edge(a, b, q); }
    return -edge(b, a, q);
}

// Triangle t's plain barycentric coordinates at frame position q (less
// the triangle's origin), each edge's share of their sum.
fn plain_bary(t: u32, q: vec2<f32>) -> vec3<f32> {
    let p0 = tv2(t, 0u);
    let p1 = tv2(t, 2u);
    let p2 = tv2(t, 4u);
    let e = vec3<f32>(edge_shared(p1, p2, q), edge_shared(p2, p0, q), edge_shared(p0, p1, q));
    return e / (e.x + e.y + e.z);
}

// Perspective-correct from plain (orthographic: the same).
fn correct(t: u32, b: vec3<f32>) -> vec3<f32> {
    if (tu(t, 11u) != 0u) { return b; }
    let w = b * tv3(t, 7u);
    return w / (w.x + w.y + w.z);
}

// Depth where t covers q, with the corrected barycentric coordinates in
// .yzw; depth < 0: it doesn't (or it's outside near / far).
fn cover(t: u32, q: vec2<f32>) -> vec4<f32> {
    let b = plain_bary(t, q);
    if (!(b.x >= 0.0 && b.y >= 0.0 && b.z >= 0.0)) { return vec4<f32>(-1.0); }
    let inv = tv3(t, 7u);
    var d: f32;
    var bc: vec3<f32>;
    if (tu(t, 11u) != 0u) {
        d = b.x / inv.x + b.y / inv.y + b.z / inv.z;
        bc = b;
    } else {
        let s = dot(b, inv);
        d = 1.0 / s;
        bc = b * inv / s;
    }
    if (!(d >= P.cam.x && d <= P.cam.y)) { return vec4<f32>(-1.0); }
    return vec4<f32>(d, bc);
}

fn tri_uv(t: u32, b: vec3<f32>) -> vec2<f32> {
    return tv2(t, 36u) * b.x + tv2(t, 38u) * b.y + tv2(t, 40u) * b.z;
}

fn tri_world(t: u32, b: vec3<f32>) -> vec3<f32> {
    return tv3(t, 18u) * b.x + tv3(t, 21u) * b.y + tv3(t, 24u) * b.z;
}

fn tri_normal(t: u32, b: vec3<f32>) -> vec3<f32> {
    return tv3(t, 27u) * b.x + tv3(t, 30u) * b.y + tv3(t, 33u) * b.z;
}

// ---- textures ---------------------------------------------------------

fn lw(l: u32, k: u32) -> u32 { return bins[P.tables.y + l * LEVEL_WORDS + k]; }

// Level l's texel at frame position (x, y): transparent outside its data
// window.
fn texel(l: u32, x: i32, y: i32) -> vec4<f32> {
    let lx = x - bitcast<i32>(lw(l, 3u));
    let ly = y - bitcast<i32>(lw(l, 4u));
    let ww = i32(lw(l, 5u));
    let wh = i32(lw(l, 6u));
    if (lx < 0 || ly < 0 || lx >= ww || ly >= wh) { return vec4<f32>(0.0); }
    return texels[lw(l, 0u) + u32(ly * ww + lx)];
}

fn wrap_i(v: i32, m: i32) -> i32 {
    let r = v % m;
    return select(r, r + m, r < 0);
}

// Level l at texture coordinates uv, bilinear: repeating, or clamped to
// its frame.
fn sample_level(l: u32, wrap: bool, uv: vec2<f32>) -> vec4<f32> {
    let fw = f32(lw(l, 1u));
    let fh = f32(lw(l, 2u));
    var x = uv.x * fw - 0.5;
    var y = uv.y * fh - 0.5;
    if (!wrap) {
        x = clamp(x, 0.0, max(fw - 1.0, 0.0));
        y = clamp(y, 0.0, max(fh - 1.0, 0.0));
    }
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    var ix = i32(x0);
    var iy = i32(y0);
    var ix1 = ix + 1;
    var iy1 = iy + 1;
    if (wrap) {
        let w = max(i32(lw(l, 1u)), 1);
        let h = max(i32(lw(l, 2u)), 1);
        ix = wrap_i(ix, w);
        iy = wrap_i(iy, h);
        ix1 = wrap_i(ix1, w);
        iy1 = wrap_i(iy1, h);
    }
    let top = texel(l, ix, iy) * (1.0 - fx) + texel(l, ix1, iy) * fx;
    let bot = texel(l, ix, iy1) * (1.0 - fx) + texel(l, ix1, iy1) * fx;
    return top * (1.0 - fy) + bot * fy;
}

fn tint(s: u32, px: vec4<f32>) -> vec4<f32> {
    let c = vec4<f32>(sf(s, 0u), sf(s, 1u), sf(s, 2u), sf(s, 3u));
    if (all(c == vec4<f32>(1.0))) { return px; }
    return px * c;
}

// Surface s's color at uv for a sample covering `covered` texels of its
// texture: mipmapped where that's more than one.
fn surface_color(s: u32, uv: vec2<f32>, covered: f32) -> vec4<f32> {
    let base = su(s, 13u);
    if (base == NONE) { return vec4<f32>(sf(s, 0u), sf(s, 1u), sf(s, 2u), sf(s, 3u)); }
    let wrap = (su(s, 12u) & S_WRAP) != 0u;
    let finite = covered == covered && covered < 3.0e38;
    if (covered <= 1.0 + 1e-6 || !finite) { return tint(s, sample_level(base, wrap, uv)); }
    let n = su(s, 14u);
    let lod = min(log2(covered), f32(n));
    let k = u32(floor(lod));
    let w = lod - floor(lod);
    let a = sample_level(base + k, wrap, uv);
    if (w == 0.0 || k >= n) { return tint(s, a); }
    let b = sample_level(base + k + 1u, wrap, uv);
    return tint(s, a + (b - a) * w);
}

// How many texels of surface s's texture a sample at q (uv there) covers,
// the next sample `step` pixels on.
fn footprint(t: u32, s: u32, q: vec2<f32>, uv: vec2<f32>, step: f32) -> f32 {
    let base = su(s, 13u);
    if (base == NONE) { return 0.0; }
    let size = vec2<f32>(f32(lw(base, 1u)), f32(lw(base, 2u)));
    let ax = (tri_uv(t, correct(t, plain_bary(t, q + vec2<f32>(step, 0.0)))) - uv) * size;
    let ay = (tri_uv(t, correct(t, plain_bary(t, q + vec2<f32>(0.0, step)))) - uv) * size;
    return max(length(ax), length(ay));
}

// ---- lights -----------------------------------------------------------

struct Lit {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
    ambient: vec3<f32>,
    // What shadows took away (diffuse and specular).
    lost: vec3<f32>,
};

struct Surf {
    albedo: vec3<f32>,
    specular: vec3<f32>,
    exponent: f32,
    blinn: bool,
    n: vec3<f32>,
    view: vec3<f32>,
    p: vec3<f32>,
    // Takes shadows.
    receives: bool,
};

// Light at strength e (color aside) arriving from direction l, `seen` of
// it past the shadows.
fn arrive(acc: ptr<function, Lit>, m: Surf, color: vec3<f32>, l: vec3<f32>, e: f32, seen: f32) {
    let c = dot(m.n, l);
    if (c <= 0.0) { return; }
    let ee = color * e;
    var add = m.albedo * ee * c;
    (*acc).diffuse += add * seen;
    if (m.blinn) {
        let h = unit_or(l + m.view, m.n);
        let d = max(dot(m.n, h), 0.0);
        let k = select(0.0, pow(d, m.exponent), d > 0.0);
        let sp = m.specular * ee * k;
        (*acc).specular += sp * seen;
        add += sp;
    }
    (*acc).lost += add * (1.0 - seen);
}

// ---- shadows (shadow.rs) ----------------------------------------------

fn mu(m: u32, k: u32) -> u32 { return tables[P.shadow.w + m * MAP_WORDS + k]; }
fn mf(m: u32, k: u32) -> f32 { return bitcast<f32>(tables[P.shadow.w + m * MAP_WORDS + k]); }
fn mv3(m: u32, k: u32) -> vec3<f32> { return vec3<f32>(mf(m, k), mf(m, k + 1u), mf(m, k + 2u)); }
fn mv4(m: u32, k: u32) -> vec4<f32> { return vec4<f32>(mf(m, k), mf(m, k + 1u), mf(m, k + 2u), mf(m, k + 3u)); }

// How far p is from map m's light, as the map measures it.
fn map_key(m: u32, p: vec3<f32>) -> f32 {
    if (mu(m, 16u) != 0u) { return dot(p - mv3(m, 20u), mv3(m, 17u)); }
    return length(p - mv3(m, 20u));
}

// A texel's width in the scene at p.
fn map_texel_at(m: u32, p: vec3<f32>) -> f32 {
    if (mu(m, 16u) != 0u) { return mf(m, 23u); }
    return mf(m, 23u) * map_key(m, p);
}

// Where p falls in map m (texel units), in .xy; .z < 0: not in front.
fn map_place(m: u32, p: vec3<f32>) -> vec3<f32> {
    let p4 = vec4<f32>(p, 1.0);
    let c = vec3<f32>(dot(mv4(m, 0u), p4), dot(mv4(m, 4u), p4), dot(mv4(m, 8u), p4));
    let depth = -c.z;
    if (depth < max(mf(m, 15u), 1e-9)) { return vec3<f32>(0.0, 0.0, -1.0); }
    let half = f32(mu(m, 14u)) / 2.0;
    let f = mf(m, 13u);
    if (mu(m, 12u) != 0u) { return vec3<f32>(half + f * c.x, half - f * c.y, 1.0); }
    return vec3<f32>(half + f * c.x / depth, half - f * c.y / depth, 1.0);
}

// The nearest caster at texel (x, y) of map m.
fn map_depth(m: u32, x: i32, y: i32) -> f32 {
    let res = i32(mu(m, 14u));
    if (x < 0 || y < 0 || x >= res || y >= res) { return FAR_AWAY; }
    return maps[mu(m, 24u) + u32(y * res + x)];
}

// How lit (0 to 1) a point `key` away is at texel position q: the four
// texels round it compared, weighted bilinearly.
fn map_lit_at(m: u32, q: vec2<f32>, key: f32) -> f32 {
    let x = q - vec2<f32>(0.5);
    let x0 = floor(x);
    let f = x - x0;
    let ix = i32(x0.x);
    let iy = i32(x0.y);
    let l00 = select(1.0, 0.0, map_depth(m, ix, iy) < key);
    let l10 = select(1.0, 0.0, map_depth(m, ix + 1, iy) < key);
    let l01 = select(1.0, 0.0, map_depth(m, ix, iy + 1) < key);
    let l11 = select(1.0, 0.0, map_depth(m, ix + 1, iy + 1) < key);
    return (l00 * (1.0 - f.x) + l10 * f.x) * (1.0 - f.y) + (l01 * (1.0 - f.x) + l11 * f.x) * f.y;
}

// The mean distance of the casters nearer the light than `key` within r
// texels of q; < 0: none.
fn map_blockers(m: u32, q: vec2<f32>, key: f32, r: f32) -> f32 {
    let n = i32(clamp(ceil(r), 1.0, 4.0));
    let step = r / f32(n);
    var sum = 0.0;
    var count = 0;
    for (var j = -n; j <= n; j++) {
        for (var i = -n; i <= n; i++) {
            let d = map_depth(m, i32(floor(q.x + f32(i) * step)), i32(floor(q.y + f32(j) * step)));
            if (d < key) {
                sum += d;
                count++;
            }
        }
    }
    if (count == 0) { return -1.0; }
    return sum / f32(count);
}

// How lit p is in map m, filtered over `soft` texels each way (and, for a
// light `radius` wide, the penumbra its blockers make).
fn map_lit(m: u32, p: vec3<f32>, soft: f32, radius: f32) -> f32 {
    let place = map_place(m, p);
    if (place.z < 0.0) { return 1.0; }
    let q = place.xy;
    let key = map_key(m, p);
    let texel = map_texel_at(m, p);
    var r = soft;
    if (radius > 0.0 && mu(m, 16u) == 0u) {
        let search = clamp(radius / texel, 1.0, 16.0);
        let b = map_blockers(m, q, key, search);
        if (!(b > 0.0)) { return 1.0; }
        r += radius * (key - b) / b / texel;
    }
    r = min(r, 24.0);
    if (r < 0.5) { return map_lit_at(m, q, key); }
    let n = i32(min(ceil(r), 4.0));
    let step = r / f32(n);
    var sum = 0.0;
    for (var j = -n; j <= n; j++) {
        for (var i = -n; i <= n; i++) {
            sum += map_lit_at(m, q + vec2<f32>(f32(i), f32(j)) * step, key);
        }
    }
    return sum / f32((2 * n + 1) * (2 * n + 1));
}

fn src_u(src: u32, k: u32) -> u32 { return tables[P.shadow.z + src * SRC_WORDS + k]; }
fn src_at(src: u32) -> vec3<f32> {
    let o = P.shadow.z + src * SRC_WORDS;
    return vec3<f32>(bitcast<f32>(tables[o + 2u]), bitcast<f32>(tables[o + 3u]), bitcast<f32>(tables[o + 4u]));
}

// Source src's map for p: its one map, or a cube's face toward p.
fn src_map(src: u32, p: vec3<f32>) -> u32 {
    let first = src_u(src, 1u);
    if (src_u(src, 0u) == 0u) { return first; }
    let v = p - src_at(src);
    let a = abs(v);
    var face: u32;
    if (a.x >= a.y && a.x >= a.z) {
        face = select(1u, 0u, v.x > 0.0);
    } else if (a.y >= a.z) {
        face = select(3u, 2u, v.y > 0.0);
    } else {
        face = select(5u, 4u, v.z > 0.0);
    }
    return first + face;
}

fn src_texel_at(src: u32, p: vec3<f32>) -> f32 {
    let first = src_u(src, 1u);
    if (src_u(src, 0u) == 0u) { return map_texel_at(first, p); }
    return mf(first, 23u) * length(p - mv3(first, 20u));
}

// How much of light li's shadow sample k reaches the surface point (normal
// n either way), coming from direction l: 1 where it takes no shadows.
fn visible(li: u32, k: u32, m: Surf, l: vec3<f32>) -> f32 {
    if (!m.receives) { return 1.0; }
    let sh = P.shadow.y + li * SHL_WORDS;
    if (k >= tables[sh + 1u]) { return 1.0; }
    let src = tables[sh] + k;
    let soft = bitcast<f32>(tables[sh + 2u]);
    let radius = bitcast<f32>(tables[sh + 3u]);
    // Off the surface toward the light, more the more it slants.
    let n = select(m.n, -m.n, dot(m.n, l) < 0.0);
    let texel = src_texel_at(src, m.p);
    let c = dot(n, l);
    let cc = clamp(c, 0.0, 1.0);
    let slant = sqrt(1.0 - cc * cc) / max(c, 0.2);
    let off = m.p + n * texel * (1.5 + soft) + l * texel * (1.0 + slant);
    return clamp(map_lit(src_map(src, off), off, soft, radius), 0.0, 1.0);
}

// The light from `src` at the point: direction to it and strength;
// w < 0: none.
fn reaching(li: u32, src: vec3<f32>, p: vec3<f32>) -> vec4<f32> {
    let to = src - p;
    let d = length(to);
    if (!(d > 1e-12)) { return vec4<f32>(-1.0); }
    return vec4<f32>(to / d, lf(li, 7u) / pow(d, lf(li, 11u)));
}

// The middle of cell k of an n × n grid over −1..1.
fn grid(k: u32, n: u32) -> vec2<f32> {
    return vec2<f32>((2.0 * f32(k % n) + 1.0) / f32(n) - 1.0, (2.0 * f32(k / n) + 1.0) / f32(n) - 1.0);
}

// Square −1..1 onto the unit disk, equal areas kept.
fn disk(q: vec2<f32>) -> vec2<f32> {
    let quarter = 0.78539816339744830962;
    if (q.x == 0.0 && q.y == 0.0) { return vec2<f32>(0.0); }
    var r: f32;
    var phi: f32;
    if (abs(q.x) > abs(q.y)) {
        r = q.x;
        phi = quarter * q.y / q.x;
    } else {
        r = q.y;
        phi = 2.0 * quarter - quarter * q.x / q.y;
    }
    return vec2<f32>(r * cos(phi), r * sin(phi));
}

// A point or spot light's samples (one, or a disk `radius` wide facing
// the point), each times `scale`.
// (Every sample goes through shadow sample 0: looked up once, at the
// first that faces the surface.)
fn sampled(acc: ptr<function, Lit>, li: u32, m: Surf, color: vec3<f32>, scale: f32) {
    let at = lv3(li, 8u);
    let radius = lf(li, 15u);
    var seen = -1.0;
    if (radius <= 0.0) {
        let r = reaching(li, at, m.p);
        if (r.w >= 0.0 && dot(m.n, r.xyz) > 0.0) {
            seen = visible(li, 0u, m, r.xyz);
            arrive(acc, m, color, r.xyz, r.w * scale, seen);
        }
        return;
    }
    let wv = m.p - at;
    if (!(length(wv) > 1e-12)) { return; }
    let w = normalize(wv);
    let u = normalize(cross(w, select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(w.x) < 0.9)));
    let v = cross(w, u);
    let share = 1.0 / 16.0;
    for (var k = 0u; k < 16u; k++) {
        let d = disk(grid(k, 4u));
        let r = reaching(li, at + (u * d.x + v * d.y) * radius, m.p);
        if (r.w >= 0.0 && dot(m.n, r.xyz) > 0.0) {
            if (seen < 0.0) { seen = visible(li, 0u, m, r.xyz); }
            arrive(acc, m, color, r.xyz, r.w * share * scale, seen);
        }
    }
}

// What light li alone adds at the point (shading Lambert or Blinn).
fn light_shade(li: u32, m_in: Surf) -> Lit {
    var acc: Lit;
    acc.diffuse = vec3<f32>(0.0);
    acc.specular = vec3<f32>(0.0);
    acc.ambient = vec3<f32>(0.0);
    acc.lost = vec3<f32>(0.0);
    let kind = lu(li, 0u);
    let enabled = lu(li, 1u) != 0u;
    let color = lv3(li, 4u);
    if (kind == K_AMBIENT) {
        let e = select(0.0, lf(li, 7u), enabled);
        acc.ambient = m_in.albedo * (color * e);
        return acc;
    }
    if (!enabled) { return acc; }
    var m = m_in;
    if (dot(m.n, m.view) < 0.0) { m.n = -m.n; }
    let dir = lv3(li, 12u);
    switch kind {
        case K_DIRECTIONAL: {
            if (dot(m.n, -dir) > 0.0) { arrive(&acc, m, color, -dir, lf(li, 7u), visible(li, 0u, m, -dir)); }
        }
        case K_POINT: {
            sampled(&acc, li, m, color, 1.0);
        }
        case K_SPOT: {
            let tv = m.p - lv3(li, 8u);
            if (!(length(tv) > 1e-12)) { return acc; }
            let c = clamp(dot(normalize(tv), dir), -1.0, 1.0);
            let angle = degrees(acos(c));
            let edge_at = lf(li, 16u) / 2.0;
            let soft = clamp(lf(li, 17u), 0.0, edge_at);
            var k: f32;
            if (angle >= edge_at) {
                k = 0.0;
            } else if (angle <= edge_at - soft) {
                k = 1.0;
            } else {
                let s = clamp((edge_at - angle) / soft, 0.0, 1.0);
                k = s * s * (3.0 - 2.0 * s);
            }
            let falloff = lf(li, 18u);
            if (falloff > 0.0) {
                let cp = max(c, 0.0);
                k *= select(0.0, pow(cp, falloff), cp > 0.0);
            }
            if (k <= 0.0) { return acc; }
            sampled(&acc, li, m, color, k);
        }
        case K_AREA: {
            let n = max(lu(li, 3u), 1u);
            let flags = lu(li, 19u);
            let two_sided = (flags & 1u) != 0u;
            let round = (flags & 2u) != 0u;
            let size = vec2<f32>(lf(li, 20u), lf(li, 21u));
            let share = 1.0 / f32(n * n);
            let r0 = lv4(li, 24u);
            let r1 = lv4(li, 28u);
            let r2 = lv4(li, 32u);
            for (var k = 0u; k < n * n; k++) {
                var g = grid(k, n);
                if (round) { g = disk(g); }
                let local = vec4<f32>(g.x * size.x / 2.0, g.y * size.y / 2.0, 0.0, 1.0);
                let at = vec3<f32>(dot(r0, local), dot(r1, local), dot(r2, local));
                let r = reaching(li, at, m.p);
                if (r.w < 0.0) { continue; }
                var c = dot(dir, -r.xyz);
                if (two_sided) { c = abs(c); }
                if (c > 0.0 && dot(m.n, r.xyz) > 0.0) { arrive(&acc, m, color, r.xyz, r.w * c * share, visible(li, k, m, r.xyz)); }
            }
        }
        default: {}
    }
    return acc;
}

// ---- the pixel --------------------------------------------------------

struct Hit {
    depth: f32,
    tri: u32,
    b: vec2<f32>,
    surf: u32,
};

// What a pixel adds up over its samples.
struct Sums {
    color: vec4<f32>,
    parts: array<vec3<f32>, 4>,
    shadow: vec3<f32>,
    lights: array<vec3<f32>, 16>,
    mattes: array<f32, 32>,
};

// The nearest surface seen at least half opaque and the nearest fainter
// one, as in out_hits.
var<private> seen: array<vec4<u32>, 2>;
var<private> sums: Sums;

// A later sample takes over only when nearer by more than f32's rounding:
// where the samples are at the same depth (a card facing the camera), the
// first one stays, as on the CPU.
fn see(which: u32, h: Hit) {
    let had = seen[which];
    if (had.x == 0u || h.depth < bitcast<f32>(had.w) * (1.0 - 1e-5)) {
        seen[which] = vec4<u32>(h.tri + 1u, bitcast<u32>(h.b.x), bitcast<u32>(h.b.y), bitcast<u32>(h.depth));
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = P.size.x;
    let h = P.size.y;
    // With a jitter cell, a thread each of the cell's pixels only (every
    // `side`th each way), packed so no thread of a workgroup sits idle.
    let side = P.cell.x;
    var g = gid.xy;
    if (side > 0u) { g = gid.xy * side + P.cell.yz; }
    if (g.x >= w || g.y >= h) { return; }
    let px = g.y * w + g.x;
    let tile = (g.y / TILE) * P.size.w + g.x / TILE;
    let first = bins[P.tables.x + tile];
    let last = bins[P.tables.x + tile + 1u];
    if (first == last) { return; }

    let n = P.size.z;
    let j = P.cell.w;
    let flags = P.slots.y;
    let n_lights = P.counts.x;
    let x = i32(g.x);
    let y = i32(g.y);
    let step = 1.0 / f32(n);

    let pixels = w * h;
    seen[0] = out_hits[px];
    seen[1] = out_hits[pixels + px];
    var overflowed = (seen[0].x & OVERFLOW) != 0u;
    seen[0].x = seen[0].x & ~OVERFLOW;

    for (var i = 0u; i < n; i++) {
        // This sample's surfaces, nearest first (ties by triangle).
        var hits: array<Hit, MAX_HITS>;
        var count = 0u;
        var dropped = false;
        for (var k = first; k < last; k++) {
            let t = bins[k];
            if (x < ti(t, 14u) || x >= ti(t, 16u) || y < ti(t, 15u) || y >= ti(t, 17u)) { continue; }
            let q = vec2<f32>(f32(x - ti(t, 12u)), f32(y - ti(t, 13u))) + vec2<f32>((f32(i) + 0.5) / f32(n), (f32(j) + 0.5) / f32(n));
            let c = cover(t, q);
            if (c.x < 0.0) { continue; }
            let d = c.x;
            let surf = tu(t, 10u);
            // One hit where two triangles of a surface meet.
            var twin = false;
            for (var m = 0u; m < count; m++) {
                if (hits[m].surf == surf && abs(hits[m].depth - d) <= 1e-5 * d) { twin = true; }
            }
            if (twin) { continue; }
            var at = count;
            while (at > 0u && (d < hits[at - 1u].depth || (d == hits[at - 1u].depth && t < hits[at - 1u].tri))) { at--; }
            if (at >= MAX_HITS) { dropped = true; continue; }
            if (count == MAX_HITS) { dropped = true; count--; }
            for (var m = count; m > at; m--) { hits[m] = hits[m - 1u]; }
            hits[at] = Hit(d, t, c.yz, surf);
            count++;
        }
        if (count == 0u) { continue; }

        // Front to back, premultiplied; a holdout takes its share of
        // what's left without adding to it.
        var acc = vec4<f32>(0.0);
        var held = 0.0;
        for (var m = 0u; m < count; m++) {
            let hit = hits[m];
            let t = hit.tri;
            let s = hit.surf;
            let b = vec3<f32>(hit.b, 1.0 - hit.b.x - hit.b.y);
            let q = vec2<f32>(f32(x - ti(t, 12u)), f32(y - ti(t, 13u))) + vec2<f32>((f32(i) + 0.5) * step, (f32(j) + 0.5) * step);
            let uv = tri_uv(t, b);
            var color = surface_color(s, uv, footprint(t, s, q, uv, step));
            let sflags = su(s, 12u);
            let p = tri_world(t, b);
            var view: vec3<f32>;
            if (P.cam.z != 0.0) { view = unit_or(P.back.xyz, vec3<f32>(0.0, 0.0, 1.0)); } else { view = unit_or(P.eye.xyz - p, vec3<f32>(0.0, 0.0, 1.0)); }
            if ((sflags & S_HEADLIGHT) != 0u) {
                // Headlight: lit from the camera.
                let nn = tri_normal(t, b);
                var facing = 1.0;
                if (length(nn) > 0.0) { facing = abs(dot(normalize(nn), view)); }
                let kk = 0.25 + 0.75 * facing;
                color = vec4<f32>(color.rgb * kk, color.a);
            }
            let k = 1.0 - acc.a - held;
            let holdout = (sflags & S_HOLDOUT) != 0u;
            // The light at the hit, in parts (premultiplied).
            var parts = array<vec3<f32>, 4>(vec3<f32>(0.0), vec3<f32>(0.0), color.rgb, vec3<f32>(0.0));
            var lost = vec3<f32>(0.0);
            let has_material = (sflags & S_MATERIAL) != 0u;
            let shading = (sflags >> 1u) & 3u;
            let emission = sv3(s, 8u);
            let lit = n_lights > 0u && shading != 0u;
            if (has_material && !((!lit && all(emission == vec3<f32>(0.0))) || color.a <= 0.0)) {
                let a = color.a;
                var mm: Surf;
                mm.albedo = color.rgb / a;
                mm.specular = sv3(s, 4u);
                mm.exponent = sf(s, 7u);
                mm.blinn = shading == 2u;
                var nrm = unit_or(tri_normal(t, b), vec3<f32>(0.0, 0.0, 1.0));
                if ((sflags & S_TWO_SIDED) != 0u && dot(nrm, view) < 0.0) { nrm = -nrm; }
                mm.n = nrm;
                mm.view = view;
                mm.p = p;
                mm.receives = (sflags & S_RECEIVES) != 0u;
                var em = emission;
                var dif = vec3<f32>(0.0);
                var spe = vec3<f32>(0.0);
                var amb = vec3<f32>(0.0);
                if (!lit) {
                    em += mm.albedo;
                } else {
                    for (var li = 0u; li < n_lights; li++) {
                        let one = light_shade(li, mm);
                        dif += one.diffuse;
                        spe += one.specular;
                        amb += one.ambient;
                        lost += one.lost;
                        let node = lu(li, 2u);
                        if ((flags & LIGHTS) != 0u && !holdout && node > 0u) {
                            sums.lights[node - 1u] += (one.diffuse + one.specular + one.ambient) * a * k;
                        }
                    }
                }
                parts = array<vec3<f32>, 4>(dif * a, spe * a, em * a, amb * a);
                lost *= a;
                color = vec4<f32>(parts[0] + parts[1] + parts[2] + parts[3], a);
            }
            if (color.a > 0.0) { see(select(1u, 0u, color.a >= 0.5), hit); }
            let object = su(s, 15u);
            if ((flags & MATTES) != 0u && object > 0u) {
                sums.mattes[object - 1u] += color.a * k;
            }
            if (holdout) {
                held += color.a * k;
            } else {
                acc += color * k;
                if ((flags & ANY_PART) != 0u) {
                    for (var c = 0u; c < 4u; c++) { sums.parts[c] += parts[c] * k; }
                    sums.shadow += lost * k;
                }
            }
            if (acc.a + held >= 1.0) { break; }
        }
        // Surfaces left out behind the 16 kept matter only if light still
        // came through those: more than 1e-4 of it (see OVERFLOW_LEFT).
        if (dropped && 1.0 - acc.a - held > OVERFLOW_LEFT) { overflowed = true; }
        sums.color += acc;
    }

    // Add this row's to the rows before.
    out_color[px] += sums.color;
    let stride = P.slots.x;
    let base = px * stride;
    let part_at = array<u32, 4>(P.parts.x, P.parts.y, P.parts.z, P.parts.w);
    let part_bit = array<u32, 4>(DIFFUSE, SPECULAR, EMISSION, AMBIENT);
    for (var c = 0u; c < 4u; c++) {
        if ((flags & part_bit[c]) != 0u) {
            for (var ch = 0u; ch < 3u; ch++) {
                let o = base + part_at[c] + ch;
                out_extra[o] = out_extra[o] + sums.parts[c][ch];
            }
        }
    }
    if ((flags & LIGHTS) != 0u) {
        for (var o = 0u; o < P.counts.y; o++) {
            for (var ch = 0u; ch < 3u; ch++) {
                let at = base + P.slots.w + 3u * o + ch;
                out_extra[at] = out_extra[at] + sums.lights[o][ch];
            }
        }
    }
    if ((flags & SHADOW) != 0u) {
        for (var ch = 0u; ch < 3u; ch++) {
            let at = base + P.counts.w + ch;
            out_extra[at] = out_extra[at] + sums.shadow[ch];
        }
    }
    if ((flags & MATTES) != 0u) {
        for (var o = 0u; o < P.counts.z; o++) {
            let at = base + P.slots.z + o;
            out_extra[at] = out_extra[at] + sums.mattes[o];
        }
    }
    var nearest = seen[0];
    if (overflowed) { nearest.x = nearest.x | OVERFLOW; }
    out_hits[px] = nearest;
    out_hits[pixels + px] = seen[1];
    if (j + 1u == n) {
        // The surface the passes take, and the depth.
        var chosen = seen[1];
        if (seen[0].x != 0u) { chosen = seen[0]; }
        out_hits[2u * pixels + px] = chosen;
        var depth = select(0u, chosen.w, chosen.x != 0u);
        if (overflowed) { depth = 0xffffffffu; }
        out_hits[3u * pixels + px / 4u][px % 4u] = depth;
    }
}

// ---- shadow maps --------------------------------------------------------

// The map being drawn (P.cell.w is its index; P.size its texels across,
// P.tables.x where its tiles start in `bins`).
@group(1) @binding(0) var<storage, read_write> map_out: array<f32>;

// Map P.cell.w's texel: the nearest caster's distance from the light, as
// shadow.rs's `Map::draw` finds it (casters only, holdouts not; a texel's
// middle; any texture alpha above 0 casts).
@compute @workgroup_size(16, 16)
fn shadow_map(@builtin(global_invocation_id) gid: vec3<u32>) {
    let res = P.size.x;
    if (gid.x >= res || gid.y >= res) { return; }
    let m = P.cell.w;
    let tile = (gid.y / TILE) * P.size.w + gid.x / TILE;
    let first = bins[P.tables.x + tile];
    let last = bins[P.tables.x + tile + 1u];
    let x = i32(gid.x);
    let y = i32(gid.y);
    let q = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    var d = FAR_AWAY;
    for (var k = first; k < last; k++) {
        let t = bins[k];
        if (x < ti(t, 14u) || x >= ti(t, 16u) || y < ti(t, 15u) || y >= ti(t, 17u)) { continue; }
        let s = tu(t, 10u);
        let f = su(s, 12u);
        if ((f & S_CASTS) == 0u || (f & S_HOLDOUT) != 0u) { continue; }
        let c = cover(t, q);
        if (c.x < 0.0) { continue; }
        if (surface_color(s, tri_uv(t, c.yzw), 0.0).a <= 0.0) { continue; }
        d = min(d, map_key(m, tri_world(t, c.yzw)));
    }
    map_out[mu(m, 24u) + gid.y * res + gid.x] = d;
}
