// Scanline Render on the GPU: one thread per pixel, the same steps as the
// CPU renderer's `draw_passes` (see mod.rs). Each dispatch takes one row
// of every pixel's samples and adds to what the earlier rows left. A
// second entry point, `shadow_map`, draws a light's shadow map as
// shadow.rs does (the nearest caster per texel); the shading looks them up
// as shadow.rs's `Shadows::visible` does.
//
// Tables are flat u32 words (f32 values as their bits), laid out by
// mod.rs: TRI_WORDS a triangle, LEVEL_WORDS a texture level (in `words`);
// in the tables, SURF_WORDS a surface, then from P.shadow: LIGHT_WORDS a
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
// A kept surface that's a splat (with its number).
const SPLAT: u32 = 0x40000000u;
// A triangle or surface of an instance (see `tri_at`): the hit's `tri` is
// this and its instance triangle's number, its `surf` this and its
// instance's.
const INSTANCE: u32 = 0x20000000u;
// An instance's record (in `proj`, from P.inst.y): its placement (3 rows
// of 4), prototype, object (+ 1, 0 none), the number of its first
// triangle, (unused), and what its prototype's color is multiplied by.
const INST_WORDS: u32 = 20u;
// A prototype (in `words`, from P.inst.w): its surface, where its vertices
// and triangles start (from P.inst.w too), how many triangles, whether
// its vertices have normals.
const PROTO_WORDS: u32 = 8u;
// A prototype's vertex: position, normal, texture coordinates.
const VERT_WORDS: u32 = 8u;
// The bins instances' triangles are listed in, INST_BIN pixels across.
const INST_BIN: u32 = 8u;
const PROJ_WORDS: u32 = 12u;
const SPLAT_SET_WORDS: u32 = 2u;
// A pixel's splats stop once less than this shows through them; ones
// fainter than SPLAT_ALPHA_MIN add nothing (as splats.rs).
const T_MIN: f32 = 1e-4;
const SPLAT_ALPHA_MIN: f32 = 1.0 / 255.0;
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
// The see-through casters a shadow map's texel gathers while it's drawn,
// nearest first (it keeps the map's own number of them, fewer).
const LAYERS_GATHERED: u32 = 8u;
// A see-through caster's alpha from which it casts as an opaque one: f32
// filtering of opaque texels can come out a rounding short of 1.
const OPAQUE_ALPHA: f32 = 1.0 - 1.0 / 1048576.0;

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
// A caster that lets light through somewhere (see mod.rs's `sees_through`):
// its fainter texels go into the map's layers.
const S_SEE_THROUGH: u32 = 512u;

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
    // where (in `words`) the tile starts, the texture levels, the tables
    // and the triangles start
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
    // splats: there are some (1) or not, where the pairs start in `lists`
    // (after the tile starts), where the sets' table starts in `words`
    // (SPLAT_SET_WORDS a set: object + 1, holdout), (unused)
    splat: vec4<u32>,
    // instances (instances.wgsl): their bins + 1 (0: none), where their
    // records start in `proj`, where their bins' lists start in `lists`
    // (the bin starts, then the entries: triangle, instance, its pixels;
    // then each triangle set up), where the prototypes start in `words`
    inst: vec4<u32>,
    // the camera's view (scene to camera space), three rows
    view0: vec4<f32>,
    view1: vec4<f32>,
    view2: vec4<f32>,
    // its lens: focal length in pixels (orthographic: pixels a unit), the
    // frame's middle x and y; and how many bin entries the instances have
    // (u32 bits: each triangle set up comes after them)
    lens: vec4<f32>,
};

@group(0) @binding(0) var<uniform> P: Params;
// The frame's tables, one after another: the tiles' triangle lists (from
// 0: tile starts, then the lists), the texture levels (from P.tables.y),
// the surfaces, lights and shadows' tables (from P.tables.z) and the
// triangles (from P.tables.w).
@group(0) @binding(1) var<storage, read> words: array<u32>;
@group(0) @binding(2) var<storage, read> texels: array<vec4<f32>>;
// Every shadow map's texels: the nearest caster's distance (FAR_AWAY:
// none).
@group(0) @binding(3) var<storage, read> maps: array<f32>;
@group(0) @binding(4) var<storage, read_write> out_color: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> out_extra: array<f32>;
// Each pixel's nearest surfaces, in five parts of `pixels` entries each
// (the last a quarter as long): the nearest surface seen at least half
// opaque and the nearest fainter one, kept from row to row as (triangle +
// 1, 0 none; overflow in the first's top bit, barycentric b0, b1, depth
// bits); after the last row, the one the passes take (a splat: SPLAT and
// its number, depth last); where the samples' blends first got half
// opaque, nearest (1 if any, depth bits, the splat there or NONE), kept
// from row to row; and every pixel's depth, four to an entry (0 nothing;
// all bits set: overflowed). The buffers start cleared.
@group(0) @binding(6) var<storage, read_write> out_hits: array<vec4<u32>>;
// The view's splats (splat_view.wgsl): each splat seen, PROJ_WORDS a
// splat; and each tile's start, then the (depth, splat) pairs nearest
// first.
@group(0) @binding(7) var<storage, read> proj: array<u32>;
@group(0) @binding(8) var<storage, read> lists: array<u32>;

fn tu(t: u32, k: u32) -> u32 { return words[P.tables.w + t * TRI_WORDS + k]; }
fn tf(t: u32, k: u32) -> f32 { return bitcast<f32>(words[P.tables.w + t * TRI_WORDS + k]); }
fn tv2(t: u32, k: u32) -> vec2<f32> { return vec2<f32>(tf(t, k), tf(t, k + 1u)); }
fn tv3(t: u32, k: u32) -> vec3<f32> { return vec3<f32>(tf(t, k), tf(t, k + 1u), tf(t, k + 2u)); }
fn ti(t: u32, k: u32) -> i32 { return bitcast<i32>(words[P.tables.w + t * TRI_WORDS + k]); }

fn su(s: u32, k: u32) -> u32 { return words[P.tables.z + s * SURF_WORDS + k]; }
fn sf(s: u32, k: u32) -> f32 { return bitcast<f32>(words[P.tables.z + s * SURF_WORDS + k]); }
fn sv3(s: u32, k: u32) -> vec3<f32> { return vec3<f32>(sf(s, k), sf(s, k + 1u), sf(s, k + 2u)); }

fn lu(l: u32, k: u32) -> u32 { return words[P.tables.z + P.shadow.x + l * LIGHT_WORDS + k]; }
fn lf(l: u32, k: u32) -> f32 { return bitcast<f32>(words[P.tables.z + P.shadow.x + l * LIGHT_WORDS + k]); }
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

// A triangle as coverage reads it: its corners in frame pixels, 1 / depth
// at each, whether it's seen orthographically.
struct Tri3 {
    p0: vec2<f32>,
    p1: vec2<f32>,
    p2: vec2<f32>,
    inv: vec3<f32>,
    ortho: bool,
};

fn is_inst(s: u32) -> bool { return (s & INSTANCE) != 0u; }
fn iu(i: u32, k: u32) -> u32 { return proj[P.inst.y + i * INST_WORDS + k]; }
fn ifl(i: u32, k: u32) -> f32 { return bitcast<f32>(iu(i, k)); }
fn irow(i: u32, r: u32) -> vec4<f32> { return vec4<f32>(ifl(i, 4u * r), ifl(i, 4u * r + 1u), ifl(i, 4u * r + 2u), ifl(i, 4u * r + 3u)); }
fn pu(p: u32, k: u32) -> u32 { return words[P.inst.w + p * PROTO_WORDS + k]; }
fn vf(at: u32, k: u32) -> f32 { return bitcast<f32>(words[at + k]); }
fn vpos(at: u32) -> vec3<f32> { return vec3<f32>(vf(at, 0u), vf(at, 1u), vf(at, 2u)); }

// Instance i's triangle number g: where its three vertices are in `words`.
fn inst_corners(i: u32, g: u32) -> vec3<u32> {
    let p = iu(i, 12u);
    let at = P.inst.w + pu(p, 2u) + 3u * (g - iu(i, 14u));
    let vb = P.inst.w + pu(p, 1u);
    return vec3<u32>(vb + words[at] * VERT_WORDS, vb + words[at + 1u] * VERT_WORDS, vb + words[at + 2u] * VERT_WORDS);
}

// (The same in instances.wgsl, word for word.)
// Vertex `v` (its prototype's space) of an instance placed by rows w0..w2,
// seen by the camera (view rows c0..c2, lens as P.lens): its frame
// position and 1 / depth.
fn inst_corner(w0: vec4<f32>, w1: vec4<f32>, w2: vec4<f32>, v: vec3<f32>, c0: vec4<f32>, c1: vec4<f32>, c2: vec4<f32>, lens: vec4<f32>, ortho: bool) -> vec3<f32> {
    let p = vec4<f32>(v, 1.0);
    let world = vec4<f32>(dot(w0, p), dot(w1, p), dot(w2, p), 1.0);
    let c = vec3<f32>(dot(c0, world), dot(c1, world), dot(c2, world));
    let d = -c.z;
    if (ortho) { return vec3<f32>(lens.y + lens.x * c.x, lens.z - lens.x * c.y, 1.0 / d); }
    return vec3<f32>(lens.y + lens.x * c.x / d, lens.z - lens.x * c.y / d, 1.0 / d);
}

// Triangle t of surface s (a hit's: an instance's when s says so).
fn tri_at(t: u32, s: u32) -> Tri3 {
    if (is_inst(s)) {
        let i = s & ~INSTANCE;
        let v = inst_corners(i, t & ~INSTANCE);
        let w0 = irow(i, 0u);
        let w1 = irow(i, 1u);
        let w2 = irow(i, 2u);
        let ortho = P.cam.z != 0.0;
        let a = inst_corner(w0, w1, w2, vpos(v.x), P.view0, P.view1, P.view2, P.lens, ortho);
        let b = inst_corner(w0, w1, w2, vpos(v.y), P.view0, P.view1, P.view2, P.lens, ortho);
        let c = inst_corner(w0, w1, w2, vpos(v.z), P.view0, P.view1, P.view2, P.lens, ortho);
        return Tri3(a.xy, b.xy, c.xy, vec3<f32>(a.z, b.z, c.z), ortho);
    }
    return Tri3(tv2(t, 0u), tv2(t, 2u), tv2(t, 4u), tv3(t, 7u), tu(t, 11u) != 0u);
}

// Triangle c's plain barycentric coordinates at frame position q, each
// edge's share of their sum.
fn plain_bary(c: Tri3, q: vec2<f32>) -> vec3<f32> {
    let e = vec3<f32>(edge_shared(c.p1, c.p2, q), edge_shared(c.p2, c.p0, q), edge_shared(c.p0, c.p1, q));
    return e / (e.x + e.y + e.z);
}

// Perspective-correct from plain (orthographic: the same).
fn correct(c: Tri3, b: vec3<f32>) -> vec3<f32> {
    if (c.ortho) { return b; }
    let w = b * c.inv;
    return w / (w.x + w.y + w.z);
}

// Depth where c covers q, with the corrected barycentric coordinates in
// .yzw; depth < 0: it doesn't (or it's outside near / far).
fn cover(c: Tri3, q: vec2<f32>) -> vec4<f32> {
    let b = plain_bary(c, q);
    if (!(b.x >= 0.0 && b.y >= 0.0 && b.z >= 0.0)) { return vec4<f32>(-1.0); }
    let inv = c.inv;
    var d: f32;
    var bc: vec3<f32>;
    if (c.ortho) {
        d = b.x / inv.x + b.y / inv.y + b.z / inv.z;
        bc = b;
    } else {
        let sum = dot(b, inv);
        d = 1.0 / sum;
        bc = b * inv / sum;
    }
    if (!(d >= P.cam.x && d <= P.cam.y)) { return vec4<f32>(-1.0); }
    return vec4<f32>(d, bc);
}

fn tri_uv(t: u32, s: u32, b: vec3<f32>) -> vec2<f32> {
    if (is_inst(s)) {
        let v = inst_corners(s & ~INSTANCE, t & ~INSTANCE);
        return vec2<f32>(vf(v.x, 6u), vf(v.x, 7u)) * b.x + vec2<f32>(vf(v.y, 6u), vf(v.y, 7u)) * b.y + vec2<f32>(vf(v.z, 6u), vf(v.z, 7u)) * b.z;
    }
    return tv2(t, 36u) * b.x + tv2(t, 38u) * b.y + tv2(t, 40u) * b.z;
}

// Instance i's placement of point p (its prototype's space).
fn inst_world(i: u32, p: vec3<f32>) -> vec3<f32> {
    let p4 = vec4<f32>(p, 1.0);
    return vec3<f32>(dot(irow(i, 0u), p4), dot(irow(i, 1u), p4), dot(irow(i, 2u), p4));
}

fn tri_world(t: u32, s: u32, b: vec3<f32>) -> vec3<f32> {
    if (is_inst(s)) {
        let i = s & ~INSTANCE;
        let v = inst_corners(i, t & ~INSTANCE);
        return inst_world(i, vpos(v.x)) * b.x + inst_world(i, vpos(v.y)) * b.y + inst_world(i, vpos(v.z)) * b.z;
    }
    return tv3(t, 18u) * b.x + tv3(t, 21u) * b.y + tv3(t, 24u) * b.z;
}

fn tri_normal(t: u32, s: u32, b: vec3<f32>) -> vec3<f32> {
    if (is_inst(s)) {
        let i = s & ~INSTANCE;
        let v = inst_corners(i, t & ~INSTANCE);
        if (pu(iu(i, 12u), 4u) == 0u) {
            // No normals: the face's, as the CPU's.
            let a = inst_world(i, vpos(v.x));
            let bb = inst_world(i, vpos(v.y));
            let c = inst_world(i, vpos(v.z));
            return unit_or(cross(bb - a, c - a), vec3<f32>(0.0, 0.0, 1.0));
        }
        // By the placement's inverse transpose (its cofactors, the sign of
        // its determinant), each made unit, as the CPU's.
        let r0 = irow(i, 0u).xyz;
        let r1 = irow(i, 1u).xyz;
        let r2 = irow(i, 2u).xyz;
        let k0 = cross(r1, r2);
        let k1 = cross(r2, r0);
        let k2 = cross(r0, r1);
        let sign = select(1.0, -1.0, dot(r0, k0) < 0.0);
        let n0 = vec3<f32>(vf(v.x, 3u), vf(v.x, 4u), vf(v.x, 5u));
        let n1 = vec3<f32>(vf(v.y, 3u), vf(v.y, 4u), vf(v.y, 5u));
        let n2 = vec3<f32>(vf(v.z, 3u), vf(v.z, 4u), vf(v.z, 5u));
        let m0 = unit_or(vec3<f32>(dot(k0, n0), dot(k1, n0), dot(k2, n0)) * sign, n0);
        let m1 = unit_or(vec3<f32>(dot(k0, n1), dot(k1, n1), dot(k2, n1)) * sign, n1);
        let m2 = unit_or(vec3<f32>(dot(k0, n2), dot(k1, n2), dot(k2, n2)) * sign, n2);
        return m0 * b.x + m1 * b.y + m2 * b.z;
    }
    return tv3(t, 27u) * b.x + tv3(t, 30u) * b.y + tv3(t, 33u) * b.z;
}

// A hit's surface (an instance's prototype's) and what its color is
// multiplied by (an instance's tint), and its object (+ 1, 0 none).
fn hit_surface(s: u32) -> u32 {
    if (is_inst(s)) { return pu(iu(s & ~INSTANCE, 12u), 0u); }
    return s;
}
fn hit_tint(s: u32) -> vec4<f32> {
    if (is_inst(s)) {
        let i = s & ~INSTANCE;
        return vec4<f32>(ifl(i, 16u), ifl(i, 17u), ifl(i, 18u), ifl(i, 19u));
    }
    return vec4<f32>(1.0);
}
fn hit_object(s: u32) -> u32 {
    if (is_inst(s)) { return iu(s & ~INSTANCE, 13u); }
    return su(s, 15u);
}

// ---- textures ---------------------------------------------------------

fn lw(l: u32, k: u32) -> u32 { return words[P.tables.y + l * LEVEL_WORDS + k]; }

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

// How many texels of surface s's texture a sample of triangle t (hit
// surface hs) at q (uv there) covers, the next sample `step` pixels on.
fn footprint(t: u32, hs: u32, s: u32, q: vec2<f32>, uv: vec2<f32>, step: f32) -> f32 {
    let base = su(s, 13u);
    if (base == NONE) { return 0.0; }
    let size = vec2<f32>(f32(lw(base, 1u)), f32(lw(base, 2u)));
    let c = tri_at(t, hs);
    let ax = (tri_uv(t, hs, correct(c, plain_bary(c, q + vec2<f32>(step, 0.0)))) - uv) * size;
    let ay = (tri_uv(t, hs, correct(c, plain_bary(c, q + vec2<f32>(0.0, step)))) - uv) * size;
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

fn mu(m: u32, k: u32) -> u32 { return words[P.tables.z + P.shadow.w + m * MAP_WORDS + k]; }
fn mf(m: u32, k: u32) -> f32 { return bitcast<f32>(words[P.tables.z + P.shadow.w + m * MAP_WORDS + k]); }
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

// A shadow lookup met a texel whose see-through casters it needed more of
// than the map kept: the pixel goes to the CPU (as one that overflowed).
var<private> layers_short: bool;

// Map m's layers (see `shadow_map`): K a texel, each its distance and how
// much light it lets through, nearest first, after a word saying how far
// they're complete (FAR_AWAY: all there).
fn map_layers(m: u32) -> u32 { return mu(m, 26u); }
fn map_layer_at(m: u32, i: u32) -> u32 { return mu(m, 25u) + i * (2u * map_layers(m) + 1u); }

// Map m's words a lookup reads per texel: texels across, where its depths
// and layers start, its layers a texel, and where its blocks start (see
// `map_blocks`).
struct MapAt { res: i32, depth: u32, layers: u32, k: u32, blocks: u32 };
fn map_at(m: u32) -> MapAt { return MapAt(i32(mu(m, 14u)), mu(m, 24u), mu(m, 25u), map_layers(m), mu(m, 27u)); }

// The nearest caster at texel (x, y) of map m, opaque or not.
fn map_depth(m: u32, x: i32, y: i32) -> f32 {
    let res = i32(mu(m, 14u));
    if (x < 0 || y < 0 || x >= res || y >= res) { return FAR_AWAY; }
    let i = u32(y * res + x);
    let d = maps[mu(m, 24u) + i];
    if (map_layers(m) == 0u) { return d; }
    return min(d, maps[map_layer_at(m, i) + 1u]);
}

// How lit a point `key` away is at texel (x, y) of map m: 0 behind an
// opaque caster, else what the see-through ones nearer let through (as
// shadow.rs's `through`).
fn map_lit_texel(a: MapAt, x: i32, y: i32, key: f32) -> f32 {
    if (x < 0 || y < 0 || x >= a.res || y >= a.res) { return 1.0; }
    let i = u32(y * a.res + x);
    if (maps[a.depth + i] < key) { return 0.0; }
    if (a.k == 0u) { return 1.0; }
    let at = a.layers + i * (2u * a.k + 1u);
    if (maps[at] < key) { layers_short = true; }
    var through = 1.0;
    for (var j = 0u; j < a.k; j++) {
        let l = at + 1u + 2u * j;
        if (!(maps[l] < key)) { break; }
        through *= maps[l + 1u];
    }
    return through;
}

// How lit (0 to 1) a point `key` away is at texel position q: the four
// texels round it compared, weighted bilinearly.
fn map_lit_at(a: MapAt, q: vec2<f32>, key: f32) -> f32 {
    let x = q - vec2<f32>(0.5);
    let x0 = floor(x);
    let f = x - x0;
    let ix = i32(x0.x);
    let iy = i32(x0.y);
    let l00 = map_lit_texel(a, ix, iy, key);
    let l10 = map_lit_texel(a, ix + 1, iy, key);
    let l01 = map_lit_texel(a, ix, iy + 1, key);
    let l11 = map_lit_texel(a, ix + 1, iy + 1, key);
    return (l00 * (1.0 - f.x) + l10 * f.x) * (1.0 - f.y) + (l01 * (1.0 - f.x) + l11 * f.x) * f.y;
}

// map_lit's (2n + 1)² taps where they fall a texel apart (step 1): each
// tap as map_lit_at weighs it (its own fraction, worked out the same way)
// and summed in the same order, but each texel of their footprint read
// once (a row kept for the next), not four times; and none read where the
// blocks under the footprint say every texel is lit (the taps weighed with
// 1 for each) or behind an opaque caster (0). False (and nothing summed)
// where rounding puts a tap on a texel other than the footprint's (then
// the taps go one by one).
fn map_lit_grid(a: MapAt, q: vec2<f32>, key: f32, n: i32, out: ptr<function, f32>) -> bool {
    let x0 = floor(q - vec2<f32>(0.5));
    let ix = i32(x0.x) - n;
    let iy = i32(x0.y) - n;
    let w = 2 * n + 2;
    var fx: array<f32, 9>;
    var fy: array<f32, 9>;
    for (var i = 0; i < w - 1; i++) {
        let t = q + vec2<f32>(f32(i - n)) - vec2<f32>(0.5);
        let t0 = floor(t);
        if (i32(t0.x) != ix + i || i32(t0.y) != iy + i) { return false; }
        fx[i] = t.x - t0.x;
        fy[i] = t.y - t0.y;
    }
    let whole = 2 * n + 1;
    let nb = (a.res + 3) / 4;
    let bx0 = ix >> 2u;
    let by0 = iy >> 2u;
    let bx1 = (ix + w - 1) >> 2u;
    let by1 = (iy + w - 1) >> 2u;
    if (bx0 >= 0 && by0 >= 0 && bx1 < nb && by1 < nb) {
        var near = FAR_AWAY;
        var far = 0.0;
        for (var by = by0; by <= by1; by++) {
            for (var bx = bx0; bx <= bx1; bx++) {
                let b = a.blocks + 2u * u32(by * nb + bx);
                near = min(near, maps[b]);
                far = max(far, maps[b + 1u]);
            }
        }
        if (far < key) {
            *out = 0.0;
            return true;
        }
        if (!(near < key)) {
            var sum = 0.0;
            for (var j = 0; j < w - 1; j++) {
                let f_y = fy[j];
                for (var i = 0; i < w - 1; i++) {
                    let f_x = fx[i];
                    sum += ((1.0 - f_x) + f_x) * (1.0 - f_y) + ((1.0 - f_x) + f_x) * f_y;
                }
            }
            *out = sum / f32(whole * whole);
            return true;
        }
    }
    var lo: array<f32, 10>;
    var hi: array<f32, 10>;
    for (var i = 0; i < w; i++) { lo[i] = map_lit_texel(a, ix + i, iy, key); }
    var sum = 0.0;
    for (var j = 0; j < w - 1; j++) {
        for (var i = 0; i < w; i++) { hi[i] = map_lit_texel(a, ix + i, iy + j + 1, key); }
        let f_y = fy[j];
        for (var i = 0; i < w - 1; i++) {
            let f_x = fx[i];
            sum += (lo[i] * (1.0 - f_x) + lo[i + 1] * f_x) * (1.0 - f_y) + (hi[i] * (1.0 - f_x) + hi[i + 1] * f_x) * f_y;
        }
        lo = hi;
    }
    *out = sum / f32(whole * whole);
    return true;
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
    let a = map_at(m);
    if (r < 0.5) { return map_lit_at(a, q, key); }
    let n = i32(min(ceil(r), 4.0));
    let step = r / f32(n);
    var lit: f32;
    if (step == 1.0 && map_lit_grid(a, q, key, n, &lit)) { return lit; }
    var sum = 0.0;
    for (var j = -n; j <= n; j++) {
        for (var i = -n; i <= n; i++) {
            sum += map_lit_at(a, q + vec2<f32>(f32(i), f32(j)) * step, key);
        }
    }
    return sum / f32((2 * n + 1) * (2 * n + 1));
}

fn src_u(src: u32, k: u32) -> u32 { return words[P.tables.z + P.shadow.z + src * SRC_WORDS + k]; }
fn src_at(src: u32) -> vec3<f32> {
    let o = P.shadow.z + src * SRC_WORDS;
    return vec3<f32>(bitcast<f32>(words[P.tables.z + o + 2u]), bitcast<f32>(words[P.tables.z + o + 3u]), bitcast<f32>(words[P.tables.z + o + 4u]));
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
    if (k >= words[P.tables.z + sh + 1u]) { return 1.0; }
    let src = words[P.tables.z + sh] + k;
    let soft = bitcast<f32>(words[P.tables.z + sh + 2u]);
    let radius = bitcast<f32>(words[P.tables.z + sh + 3u]);
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

// ---- splats (splats.rs's fragments) ----------------------------------

struct Frag {
    depth: f32,
    color: vec4<f32>,
    g: u32,
    capture: u32,
    valid: bool,
};

// Where a pixel is in its tile's splats, and how much shows through those
// so far.
struct Stream {
    k: u32,
    end: u32,
    through: f32,
};

fn pjf(g: u32, w: u32) -> f32 { return bitcast<f32>(proj[g * PROJ_WORDS + w]); }

fn stream_at(tile: u32) -> Stream {
    if (P.splat.x == 0u) { return Stream(0u, 0u, 1.0); }
    return Stream(lists[tile], lists[tile + 1u], 1.0);
}

// The next splat at pixel middle q, nearest first: its share there
// (opacity times the Gaussian), until less than T_MIN shows through.
fn next_frag(st: ptr<function, Stream>, q: vec2<f32>) -> Frag {
    var f: Frag;
    f.valid = false;
    if ((*st).through < T_MIN) { return f; }
    while ((*st).k < (*st).end) {
        let g = lists[P.splat.y + 2u * (*st).k + 1u];
        (*st).k += 1u;
        let d = q - vec2<f32>(pjf(g, 0u), pjf(g, 1u));
        let power = -0.5 * (pjf(g, 2u) * d.x * d.x + pjf(g, 4u) * d.y * d.y) - pjf(g, 3u) * d.x * d.y;
        // (Past 3σ it counts for nothing, whichever tile it's in.)
        if (!(power >= -4.5 && power <= 0.0)) { continue; }
        let alpha = pjf(g, 9u) * exp(power);
        if (alpha < SPLAT_ALPHA_MIN) { continue; }
        f = Frag(pjf(g, 5u), vec4<f32>(vec3<f32>(pjf(g, 6u), pjf(g, 7u), pjf(g, 8u)) * alpha, alpha), g, proj[g * PROJ_WORDS + 11u] - 1u, true);
        (*st).through *= 1.0 - alpha;
        return f;
    }
    return f;
}

// The splats alone at a pixel (where a sample meets no surface): blended
// once, as splats.rs's `blend_alone` does.
struct Alone {
    acc: vec4<f32>,
    held: f32,
    emission: vec3<f32>,
    mattes: array<f32, 32>,
    half: vec4<u32>,
    dominant: u32,
    share: f32,
    any: bool,
    through: f32,
};

fn alone_add(a: ptr<function, Alone>, f: Frag, mattes: bool) {
    (*a).any = true;
    let k = 1.0 - (*a).acc.a - (*a).held;
    let share = f.color.a * k;
    if ((*a).dominant == NONE || share > (*a).share) {
        (*a).dominant = f.g;
        (*a).share = share;
    }
    let o = splat_object(f.capture);
    if (mattes && o > 0u) { (*a).mattes[o - 1u] += f.color.a * k; }
    if (splat_holdout(f.capture)) {
        (*a).held += f.color.a * k;
    } else {
        (*a).acc += f.color * k;
        (*a).emission += f.color.rgb * k;
    }
    if ((*a).half.x == 0u && (*a).acc.a >= 0.5) { (*a).half = vec4<u32>(1u, bitcast<u32>(f.depth), f.g, 0u); }
}

// A batch of a tile's splats in workgroup memory, loaded by the whole
// workgroup together: center and conic xx xy; conic yy, depth, opacity,
// capture (bits); color; which splat.
const BATCH: u32 = 256u;
var<workgroup> batch_a: array<vec4<f32>, 256>;
var<workgroup> batch_b: array<vec4<f32>, 256>;
var<workgroup> batch_c: array<vec4<f32>, 256>;
var<workgroup> batch_g: array<u32, 256>;
var<workgroup> wg_start: u32;
var<workgroup> wg_end: u32;
var<workgroup> wg_live: atomic<u32>;
var<workgroup> wg_live_n: u32;

// The splats alone at each pixel of workgroup `tile` (a 16 × 16 tile),
// taken a batch at a time for the whole workgroup (much quicker than each
// pixel reading the list on its own); every thread must call it (`in_frame`:
// a pixel of the frame, at `pq`).
fn alone_batched(a: ptr<function, Alone>, tile: u32, li: u32, in_frame: bool, pq: vec2<f32>, mattes: bool) {
    if (li == 0u) {
        wg_start = lists[tile];
        wg_end = lists[tile + 1u];
    }
    let start = workgroupUniformLoad(&wg_start);
    let end = workgroupUniformLoad(&wg_end);
    var done = !in_frame;
    for (var b = start; b < end; b += BATCH) {
        let k = b + li;
        if (k < end) {
            let g = lists[P.splat.y + 2u * k + 1u];
            batch_a[li] = vec4<f32>(pjf(g, 0u), pjf(g, 1u), pjf(g, 2u), pjf(g, 3u));
            batch_b[li] = vec4<f32>(pjf(g, 4u), pjf(g, 5u), pjf(g, 9u), bitcast<f32>(proj[g * PROJ_WORDS + 11u] - 1u));
            batch_c[li] = vec4<f32>(pjf(g, 6u), pjf(g, 7u), pjf(g, 8u), 0.0);
            batch_g[li] = g;
        }
        workgroupBarrier();
        if (!done) {
            let n = min(BATCH, end - b);
            for (var i = 0u; i < n; i++) {
                let ba = batch_a[i];
                let bb = batch_b[i];
                let d = pq - ba.xy;
                let power = -0.5 * (ba.z * d.x * d.x + bb.x * d.y * d.y) - ba.w * d.x * d.y;
                if (!(power >= -4.5 && power <= 0.0)) { continue; }
                let alpha = bb.z * exp(power);
                if (alpha < SPLAT_ALPHA_MIN) { continue; }
                alone_add(a, Frag(bb.y, vec4<f32>(batch_c[i].rgb * alpha, alpha), batch_g[i], bitcast<u32>(bb.w), true), mattes);
                (*a).through *= 1.0 - alpha;
                if ((*a).through < T_MIN) {
                    done = true;
                    break;
                }
            }
        }
        // On while any pixel still sees through.
        if (!done) { atomicOr(&wg_live, 1u); }
        workgroupBarrier();
        if (li == 0u) {
            wg_live_n = atomicLoad(&wg_live);
            atomicStore(&wg_live, 0u);
        }
        if (workgroupUniformLoad(&wg_live_n) == 0u) { break; }
    }
}

fn splat_object(k: u32) -> u32 { return words[P.splat.z + k * SPLAT_SET_WORDS]; }
fn splat_holdout(k: u32) -> bool { return words[P.splat.z + k * SPLAT_SET_WORDS + 1u] != 0u; }

// ---- the pixel --------------------------------------------------------

struct Hit {
    depth: f32,
    tri: u32,
    b: vec2<f32>,
    surf: u32,
};

// Hit h into a sample's surfaces (`count` of them, nearest first; ties by
// triangle); where two triangles of a surface meet (one there already as
// near), only the first triangle of the two (as the CPU, going through
// them in order, keeps it: so the order they come in doesn't matter); one
// past MAX_HITS dropped (`dropped` set).
fn add_hit(hits: ptr<function, array<Hit, MAX_HITS>>, count: ptr<function, u32>, dropped: ptr<function, bool>, h: Hit) {
    var n = *count;
    // Full, and behind the last by more than a twin's tolerance: dropped,
    // without going through them (deep stacks of copies have many such).
    if (n == MAX_HITS && h.depth > (*hits)[n - 1u].depth * (1.0 + 1e-5)) {
        *dropped = true;
        return;
    }
    for (var m = 0u; m < n; m++) {
        if ((*hits)[m].surf == h.surf && abs((*hits)[m].depth - h.depth) <= 1e-5 * h.depth) {
            if (h.tri >= (*hits)[m].tri) { return; }
            // This one comes first: it takes the other's place.
            for (var k = m; k + 1u < n; k++) { (*hits)[k] = (*hits)[k + 1u]; }
            n -= 1u;
            *count = n;
            break;
        }
    }
    var at = n;
    while (at > 0u && (h.depth < (*hits)[at - 1u].depth || (h.depth == (*hits)[at - 1u].depth && h.tri < (*hits)[at - 1u].tri))) { at--; }
    if (at >= MAX_HITS) {
        *dropped = true;
        return;
    }
    var k = n;
    if (n == MAX_HITS) {
        *dropped = true;
        k = n - 1u;
    }
    for (var m = k; m > at; m--) { (*hits)[m] = (*hits)[m - 1u]; }
    (*hits)[at] = h;
    *count = k + 1u;
}

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
// The nearer of two "got half opaque" records (the first if as near).
fn nearer(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    if (a.x == 0u) { return b; }
    if (b.x != 0u && bitcast<f32>(b.y) < bitcast<f32>(a.y)) { return b; }
    return a;
}

fn see(which: u32, h: Hit) {
    let had = seen[which];
    if (had.x == 0u || h.depth < bitcast<f32>(had.w) * (1.0 - 1e-5)) {
        seen[which] = vec4<u32>(h.tri + 1u, bitcast<u32>(h.b.x), bitcast<u32>(h.b.y), bitcast<u32>(h.depth));
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let w = P.size.x;
    let h = P.size.y;
    // With a jitter cell, a thread each of the cell's pixels only (every
    // `side`th each way), packed so no thread of a workgroup sits idle.
    let side = P.cell.x;
    var g = gid.xy;
    if (side > 0u) { g = gid.xy * side + P.cell.yz; }
    let in_frame = g.x < w && g.y < h;
    let pq = vec2<f32>(f32(g.x) + 0.5, f32(g.y) + 0.5);
    let flags = P.slots.y;
    // The splats alone (the same for every sample of the pixel that meets
    // no surface): worked out once, in the last row's dispatch, and added
    // for each such sample of every row. First, before any thread leaves:
    // the workgroup loads them together (a tile being a workgroup without
    // a cell).
    let last_row = P.cell.w + 1u == P.size.z;
    var alone: Alone;
    alone.dominant = NONE;
    alone.share = -1.0;
    alone.through = 1.0;
    if (P.splat.x != 0u && side == 0u && last_row) {
        alone_batched(&alone, wid.y * P.size.w + wid.x, li, in_frame, pq, (flags & MATTES) != 0u);
    }
    if (!in_frame) { return; }
    let px = g.y * w + g.x;
    let tile = (g.y / TILE) * P.size.w + g.x / TILE;
    let first = words[P.tables.x + tile];
    let last = words[P.tables.x + tile + 1u];
    let splat_tile = stream_at(tile);
    // (Instances' triangles are binned finer: INST_BIN pixels across.)
    let bin = (g.y / INST_BIN) * ((P.size.x + INST_BIN - 1u) / INST_BIN) + g.x / INST_BIN;
    let no_instances = P.inst.x == 0u || lists[P.inst.z + bin] == lists[P.inst.z + bin + 1u];
    if (first == last && splat_tile.k == splat_tile.end && no_instances) { return; }

    let n = P.size.z;
    let j = P.cell.w;
    let n_lights = P.counts.x;
    let x = i32(g.x);
    let y = i32(g.y);
    let step = 1.0 / f32(n);

    let pixels = w * h;
    seen[0] = out_hits[px];
    seen[1] = out_hits[pixels + px];
    var overflowed = (seen[0].x & OVERFLOW) != 0u;
    seen[0].x = seen[0].x & ~OVERFLOW;
    // Where the samples' blends first got half opaque, nearest (kept from
    // row to row): 1 if any, depth bits, the splat there or NONE.
    var median = out_hits[3u * pixels + px];
    // How many of this render's samples so far met no surface (kept in
    // the median's last word from row to row; from 0 at a render's first
    // row: motion blur adds many renders into one frame).
    var lone = select(median.w, 0u, P.cell.w == 0u);

    // (With a jitter cell: each pixel through the list on its own.)
    if (P.splat.x != 0u && side != 0u && last_row) {
        var st = splat_tile;
        loop {
            let f = next_frag(&st, pq);
            if (!f.valid) { break; }
            alone_add(&alone, f, (flags & MATTES) != 0u);
        }
    }
    let any_splat = alone.any;
    let dominant = alone.dominant;

    for (var i = 0u; i < n; i++) {
        // This sample's surfaces, nearest first (ties by triangle).
        var hits: array<Hit, MAX_HITS>;
        var count = 0u;
        var dropped = false;
        let sq = vec2<f32>(f32(x), f32(y)) + vec2<f32>((f32(i) + 0.5) / f32(n), (f32(j) + 0.5) / f32(n));
        for (var k = first; k < last; k++) {
            let t = words[k];
            if (x < ti(t, 14u) || x >= ti(t, 16u) || y < ti(t, 15u) || y >= ti(t, 17u)) { continue; }
            let q = vec2<f32>(f32(x - ti(t, 12u)), f32(y - ti(t, 13u))) + vec2<f32>((f32(i) + 0.5) / f32(n), (f32(j) + 0.5) / f32(n));
            let surf = tu(t, 10u);
            let c = cover(tri_at(t, surf), q);
            if (c.x < 0.0) { continue; }
            add_hit(&hits, &count, &dropped, Hit(c.x, t, c.yz, surf));
        }
        // The instances' triangles over this tile (by triangle number).
        if (P.inst.x != 0u) {
            let ia = lists[P.inst.z + bin];
            let ib = lists[P.inst.z + bin + 1u];
            let entries = P.inst.z + P.inst.x;
            let setups = entries + 4u * bitcast<u32>(P.lens.w);
            // (The near plane cuts nothing here: instances crossing it are
            // drawn as surfaces of their own.)
            let ortho = P.cam.z != 0.0;
            for (var k = ia; k < ib; k++) {
                let at = entries + 4u * k;
                // Most of a bin's triangles miss this pixel: by their box;
                // or, the sample's surfaces full, are behind the last.
                let b0 = lists[at + 1u];
                let b1 = lists[at + 2u];
                if (x < i32(b0 & 0xffffu) || y < i32(b0 >> 16u) || x >= i32(b1 & 0xffffu) || y >= i32(b1 >> 16u)) { continue; }
                if (count == MAX_HITS && bitcast<f32>(lists[at + 3u]) > hits[MAX_HITS - 1u].depth * (1.0 + 1e-5)) {
                    dropped = true;
                    continue;
                }
                let g = lists[at];
                let t = INSTANCE | g;
                // (Set up once, by instances.wgsl.)
                let su_at = setups + 10u * g;
                let surf = INSTANCE | lists[su_at + 9u];
                let c = cover(Tri3(vec2<f32>(bitcast<f32>(lists[su_at]), bitcast<f32>(lists[su_at + 1u])), vec2<f32>(bitcast<f32>(lists[su_at + 2u]), bitcast<f32>(lists[su_at + 3u])), vec2<f32>(bitcast<f32>(lists[su_at + 4u]), bitcast<f32>(lists[su_at + 5u])), vec3<f32>(bitcast<f32>(lists[su_at + 6u]), bitcast<f32>(lists[su_at + 7u]), bitcast<f32>(lists[su_at + 8u])), ortho), sq);
                if (c.x < 0.0) { continue; }
                add_hit(&hits, &count, &dropped, Hit(c.x, t, c.yz, surf));
            }
        }
        if (count == 0u) {
            // No surface: the splats alone (added in the last row).
            if (P.splat.x != 0u) { lone += 1u; }
            continue;
        }

        // Front to back, premultiplied, with the splats in depth order
        // among them; a holdout takes its share of what's left without
        // adding to it.
        var acc = vec4<f32>(0.0);
        var held = 0.0;
        var half = vec4<u32>(0u);
        var st = splat_tile;
        var f = next_frag(&st, pq);
        var m = 0u;
        loop {
            let has_hit = m < count;
            if (!has_hit && !f.valid) { break; }
            if (f.valid && (!has_hit || f.depth < hits[m].depth)) {
                let k = 1.0 - acc.a - held;
                let o = splat_object(f.capture);
                if ((flags & MATTES) != 0u && o > 0u) { sums.mattes[o - 1u] += f.color.a * k; }
                if (splat_holdout(f.capture)) {
                    held += f.color.a * k;
                } else {
                    acc += f.color * k;
                    if ((flags & ANY_PART) != 0u) { sums.parts[2] += f.color.rgb * k; }
                }
                if (half.x == 0u && acc.a >= 0.5) { half = vec4<u32>(1u, bitcast<u32>(f.depth), f.g, 0u); }
                if (acc.a + held >= 1.0) { break; }
                f = next_frag(&st, pq);
                continue;
            }
            let hit = hits[m];
            m += 1u;
            let t = hit.tri;
            let hs = hit.surf;
            let s = hit_surface(hs);
            let b = vec3<f32>(hit.b, 1.0 - hit.b.x - hit.b.y);
            var q = vec2<f32>(f32(x), f32(y)) + vec2<f32>((f32(i) + 0.5) * step, (f32(j) + 0.5) * step);
            if (!is_inst(hs)) { q = vec2<f32>(f32(x - ti(t, 12u)), f32(y - ti(t, 13u))) + vec2<f32>((f32(i) + 0.5) * step, (f32(j) + 0.5) * step); }
            let uv = tri_uv(t, hs, b);
            var color = surface_color(s, uv, footprint(t, hs, s, q, uv, step)) * hit_tint(hs);
            let sflags = su(s, 12u);
            let p = tri_world(t, hs, b);
            var view: vec3<f32>;
            if (P.cam.z != 0.0) { view = unit_or(P.back.xyz, vec3<f32>(0.0, 0.0, 1.0)); } else { view = unit_or(P.eye.xyz - p, vec3<f32>(0.0, 0.0, 1.0)); }
            if ((sflags & S_HEADLIGHT) != 0u) {
                // Headlight: lit from the camera.
                let nn = tri_normal(t, hs, b);
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
                var nrm = unit_or(tri_normal(t, hs, b), vec3<f32>(0.0, 0.0, 1.0));
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
            let object = hit_object(hs);
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
            if (half.x == 0u && acc.a >= 0.5) { half = vec4<u32>(1u, bitcast<u32>(hit.depth), NONE, 0u); }
            if (acc.a + held >= 1.0) { break; }
        }
        median = nearer(median, half);
        // Surfaces left out behind the 16 kept matter only if light still
        // came through those: more than 1e-4 of it (see OVERFLOW_LEFT).
        if (dropped && 1.0 - acc.a - held > OVERFLOW_LEFT) { overflowed = true; }
        sums.color += acc;
    }
    // Every row's samples that met no surface: the splats alone, each.
    if (last_row && any_splat && lone > 0u) {
        let c = f32(lone);
        sums.color += alone.acc * c;
        median = nearer(median, alone.half);
        for (var o = 0u; o < 32u; o++) { sums.mattes[o] += alone.mattes[o] * c; }
        if ((flags & ANY_PART) != 0u) { sums.parts[2] += alone.emission * c; }
    }
    median.w = lone;

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
    if (layers_short) { overflowed = true; }
    var nearest = seen[0];
    if (overflowed) { nearest.x = nearest.x | OVERFLOW; }
    out_hits[px] = nearest;
    out_hits[pixels + px] = seen[1];
    out_hits[3u * pixels + px] = median;
    if (j + 1u == n) {
        // The surface the passes take, and the depth: with splats here,
        // where the blend got half opaque (the splat giving the most, if
        // it got there at a splat); else the surface's.
        var chosen = seen[1];
        if (seen[0].x != 0u) { chosen = seen[0]; }
        var depth = select(0u, chosen.w, chosen.x != 0u);
        if (any_splat && median.x != 0u) {
            depth = median.y;
            if (median.z != NONE) {
                var top = median.z;
                if (dominant != NONE) { top = dominant; }
                chosen = vec4<u32>(SPLAT | top, 0u, 0u, median.y);
            }
        }
        out_hits[2u * pixels + px] = chosen;
        if (overflowed) { depth = 0xffffffffu; }
        out_hits[4u * pixels + px / 4u][px % 4u] = depth;
    }
}


// A map texel's casters so far (`shadow_map`): the nearest opaque one's
// distance; the see-through ones nearer than it, nearest first (as many as
// gathered; a tie after the one before, as the CPU's sort keeps them):
// distance and alpha, and surface; where some were left out, the nearest
// of those.
var<private> map_d: f32;
var<private> map_lay: array<vec2<f32>, LAYERS_GATHERED>;
var<private> map_lay_surf: array<u32, LAYERS_GATHERED>;
var<private> map_count: u32;
var<private> map_cut: f32;

// A caster `key` from the light, `a` opaque, of surface `s` (flags `f`):
// the nearest opaque one, or a layer.
fn map_caster(key: f32, a: f32, f: u32, s: u32) {
    if ((f & S_SEE_THROUGH) == 0u || a >= OPAQUE_ALPHA) {
        map_d = min(map_d, key);
        return;
    }
    if (key >= map_d || key >= map_cut) { return; }
    var at = map_count;
    while (at > 0u && map_lay[at - 1u].x > key) { at--; }
    if (map_count == LAYERS_GATHERED) {
        if (at == LAYERS_GATHERED) {
            map_cut = key;
            return;
        }
        map_cut = min(map_cut, map_lay[LAYERS_GATHERED - 1u].x);
        map_count--;
    }
    for (var i = map_count; i > at; i--) {
        map_lay[i] = map_lay[i - 1u];
        map_lay_surf[i] = map_lay_surf[i - 1u];
    }
    map_lay[at] = vec2<f32>(key, a);
    map_lay_surf[at] = s;
    map_count++;
}

// ---- shadow maps --------------------------------------------------------

// The map being drawn (P.cell.w is its index; P.size its texels across,
// P.tables.x where its tiles start in `bins`).
@group(1) @binding(0) var<storage, read_write> map_out: array<f32>;

// Map P.cell.w's texel: the nearest opaque caster's distance from the
// light, and the see-through ones nearer (its layers, if the map has
// them), as shadow.rs's `Map::draw` finds them (casters only, holdouts
// not; a texel's middle; any texture alpha above 0 casts). Layers past
// the map's K, or past those gathered, are left out, and the texel says
// from how far: a lookup further than that sends its pixel to the CPU.
@compute @workgroup_size(16, 16)
fn shadow_map(@builtin(global_invocation_id) gid: vec3<u32>) {
    let res = P.size.x;
    if (gid.x >= res || gid.y >= res) { return; }
    let m = P.cell.w;
    let tile = (gid.y / TILE) * P.size.w + gid.x / TILE;
    let first = words[P.tables.x + tile];
    let last = words[P.tables.x + tile + 1u];
    let x = i32(gid.x);
    let y = i32(gid.y);
    let q = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    map_d = FAR_AWAY;
    map_count = 0u;
    map_cut = FAR_AWAY;
    for (var k = first; k < last; k++) {
        let t = words[k];
        if (x < ti(t, 14u) || x >= ti(t, 16u) || y < ti(t, 15u) || y >= ti(t, 17u)) { continue; }
        let s = tu(t, 10u);
        let f = su(s, 12u);
        if ((f & S_CASTS) == 0u || (f & S_HOLDOUT) != 0u) { continue; }
        let c = cover(tri_at(t, s), q);
        if (c.x < 0.0) { continue; }
        let a = surface_color(s, tri_uv(t, s, c.yzw), 0.0).a;
        if (a <= 0.0) { continue; }
        map_caster(map_key(m, tri_world(t, s, c.yzw)), a, f, s);
    }
    // The instances' triangles over this texel (as the raster's: by their
    // boxes, from their setups).
    if (P.inst.x != 0u) {
        let bin = (gid.y / INST_BIN) * ((res + INST_BIN - 1u) / INST_BIN) + gid.x / INST_BIN;
        let ia = lists[P.inst.z + bin];
        let ib = lists[P.inst.z + bin + 1u];
        let entries = P.inst.z + P.inst.x;
        let setups = entries + 4u * bitcast<u32>(P.lens.w);
        let ortho = P.cam.z != 0.0;
        for (var k = ia; k < ib; k++) {
            let at = entries + 4u * k;
            let b0 = lists[at + 1u];
            let b1 = lists[at + 2u];
            if (x < i32(b0 & 0xffffu) || y < i32(b0 >> 16u) || x >= i32(b1 & 0xffffu) || y >= i32(b1 >> 16u)) { continue; }
            let g = lists[at];
            let su_at = setups + 10u * g;
            let t = INSTANCE | g;
            let s = INSTANCE | lists[su_at + 9u];
            let ps = hit_surface(s);
            let f = su(ps, 12u);
            if ((f & S_CASTS) == 0u || (f & S_HOLDOUT) != 0u) { continue; }
            let c = cover(Tri3(vec2<f32>(bitcast<f32>(lists[su_at]), bitcast<f32>(lists[su_at + 1u])), vec2<f32>(bitcast<f32>(lists[su_at + 2u]), bitcast<f32>(lists[su_at + 3u])), vec2<f32>(bitcast<f32>(lists[su_at + 4u]), bitcast<f32>(lists[su_at + 5u])), vec3<f32>(bitcast<f32>(lists[su_at + 6u]), bitcast<f32>(lists[su_at + 7u]), bitcast<f32>(lists[su_at + 8u])), ortho), q);
            if (c.x < 0.0) { continue; }
            let a = surface_color(ps, tri_uv(t, s, c.yzw), 0.0).a * hit_tint(s).a;
            if (a <= 0.0) { continue; }
            map_caster(map_key(m, tri_world(t, s, c.yzw)), a, f, s);
        }
    }
    let d = map_d;
    let count = map_count;
    let cut = map_cut;
    let i = gid.y * res + gid.x;
    map_out[mu(m, 24u) + i] = d;
    let kmax = map_layers(m);
    if (kmax == 0u) { return; }
    // Those nearer than d, one where two triangles of a surface meet (as
    // the CPU's dedup: the same surface as the one kept before, as far
    // within 1e-6), the map's K of them.
    let base = map_layer_at(m, i);
    var kept = 0u;
    var prev = 0u;
    var limit = cut;
    for (var j = 0u; j < count; j++) {
        let key = map_lay[j].x;
        if (key >= d) { break; }
        if (kept > 0u && map_lay_surf[j] == map_lay_surf[prev] && abs(map_lay[prev].x - key) <= 1e-6 * abs(map_lay[prev].x)) { continue; }
        if (kept == kmax) {
            limit = min(limit, key);
            break;
        }
        map_out[base + 1u + 2u * kept] = key;
        map_out[base + 2u + 2u * kept] = 1.0 - map_lay[j].y;
        kept++;
        prev = j;
    }
    for (var j = kept; j < kmax; j++) {
        map_out[base + 1u + 2u * j] = FAR_AWAY;
        map_out[base + 2u + 2u * j] = 1.0;
    }
    map_out[base] = limit;
}

// Map P.cell.w's 4 × 4 texel blocks, after it's drawn (a thread a block):
// the nearest caster of any kind there, and the farthest opaque one
// (FAR_AWAY where a texel has layers or is past the map: never wholly
// behind). A point no further than the first is lit at every texel of the
// block; one beyond the second, at none.
@compute @workgroup_size(16, 16)
fn map_blocks(@builtin(global_invocation_id) gid: vec3<u32>) {
    let m = P.cell.w;
    let res = i32(mu(m, 14u));
    let nb = (res + 3) / 4;
    if (i32(gid.x) >= nb || i32(gid.y) >= nb) { return; }
    let k = map_layers(m);
    var near = FAR_AWAY;
    var far = 0.0;
    var plain = true;
    for (var y = 0; y < 4; y++) {
        for (var x = 0; x < 4; x++) {
            let tx = i32(gid.x) * 4 + x;
            let ty = i32(gid.y) * 4 + y;
            if (tx >= res || ty >= res) {
                plain = false;
                continue;
            }
            let i = u32(ty * res + tx);
            let d = map_out[mu(m, 24u) + i];
            near = min(near, d);
            far = max(far, d);
            if (k > 0u) {
                let l0 = map_out[map_layer_at(m, i) + 1u];
                near = min(near, l0);
                if (l0 < FAR_AWAY) { plain = false; }
            }
        }
    }
    if (!plain) { far = FAR_AWAY; }
    let b = mu(m, 27u) + 2u * (gid.y * u32(nb) + gid.x);
    map_out[b] = near;
    map_out[b + 1u] = far;
}
