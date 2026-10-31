// Instances' triangles binned into the frame's 8 × 8 bins on the GPU
// (instances.rs): each (instance, prototype triangle) seen through the
// camera and set up once (`count`, which also counts its bins), the bins'
// starts worked out (`offsets`), each bin's list filled (`scatter`, in no
// particular order: the raster keeps the same surfaces whatever order they
// come in). The raster (raster.wgsl) tests a pixel against its bin's
// triangles from their setups, and sets up in full only those it keeps.
//
// An instance's record and a prototype's are as raster.wgsl reads them:
// INST_WORDS and PROTO_WORDS (there).

// The bins, BIN pixels across (smaller than the raster's 16 × 16 tiles:
// far instances are small and many).
const BIN: i32 = 8;
const INST_WORDS: u32 = 20u;
const PROTO_WORDS: u32 = 8u;
const VERT_WORDS: u32 = 8u;

struct Params {
    // width, height, tiles across, tiles down
    size: vec4<u32>,
    // instances, triangles in all, tiles, orthographic (1) or not
    counts: vec4<u32>,
    // the camera's view (scene to camera space), three rows
    view0: vec4<f32>,
    view1: vec4<f32>,
    view2: vec4<f32>,
    // focal length in pixels (orthographic: pixels a unit), the frame's
    // middle x and y, (unused)
    lens: vec4<f32>,
};

@group(0) @binding(0) var<uniform> P: Params;
// The prototypes (PROTO_WORDS each, from 0), their vertices and triangles.
@group(0) @binding(1) var<storage, read> protos: array<u32>;
// The instances' records.
@group(0) @binding(2) var<storage, read> records: array<u32>;
@group(0) @binding(3) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(4) var<storage, read_write> starts: array<u32>;
@group(0) @binding(5) var<storage, read_write> cursors: array<atomic<u32>>;
// The bins' entries, each bin's together: triangle number, its pixels (x0,
// y0 in the second word's halves, x1, y1 in the third's, x1 and y1 past
// the last; a pixel more each way than its corners reach) and its nearest
// depth (f32 bits): for the raster to pass by most of a bin's without
// reading their setups.
@group(0) @binding(6) var<storage, read_write> pairs: array<vec4<u32>>;
// Each triangle as coverage reads it (raster.wgsl's Tri3, set up once
// here rather than at every pixel): its corners in frame pixels, then 1 /
// depth at each; then its instance.
@group(0) @binding(7) var<storage, read_write> setups: array<f32>;

fn iu(i: u32, k: u32) -> u32 { return records[i * INST_WORDS + k]; }
fn irow(i: u32, r: u32) -> vec4<f32> {
    let k = i * INST_WORDS + 4u * r;
    return vec4<f32>(bitcast<f32>(records[k]), bitcast<f32>(records[k + 1u]), bitcast<f32>(records[k + 2u]), bitcast<f32>(records[k + 3u]));
}
fn vpos(at: u32) -> vec3<f32> { return vec3<f32>(bitcast<f32>(protos[at]), bitcast<f32>(protos[at + 1u]), bitcast<f32>(protos[at + 2u])); }

// (The same in raster.wgsl, word for word.)
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

// Triangle number g's instance: the last whose first triangle is at most g.
fn instance_of(g: u32) -> u32 {
    var lo = 0u;
    var hi = P.counts.x;
    while (hi - lo > 1u) {
        let mid = (lo + hi) / 2u;
        if (iu(mid, 14u) <= g) { lo = mid; } else { hi = mid; }
    }
    return lo;
}

// Triangle g's corners seen by the camera (frame position and 1 / depth).
fn corners_of(g: u32) -> array<vec3<f32>, 3> {
    let i = instance_of(g);
    let p = iu(i, 12u);
    let at = protos[p * PROTO_WORDS + 2u] + 3u * (g - iu(i, 14u));
    let vb = protos[p * PROTO_WORDS + 1u];
    let w0 = irow(i, 0u);
    let w1 = irow(i, 1u);
    let w2 = irow(i, 2u);
    let ortho = P.counts.w != 0u;
    let a = inst_corner(w0, w1, w2, vpos(vb + protos[at] * VERT_WORDS), P.view0, P.view1, P.view2, P.lens, ortho);
    let b = inst_corner(w0, w1, w2, vpos(vb + protos[at + 1u] * VERT_WORDS), P.view0, P.view1, P.view2, P.lens, ortho);
    let c = inst_corner(w0, w1, w2, vpos(vb + protos[at + 2u] * VERT_WORDS), P.view0, P.view1, P.view2, P.lens, ortho);
    return array<vec3<f32>, 3>(a, b, c);
}

// Corners `k`'s pixels (x0, y0, x1, y1, x1 and y1 past the last), a pixel
// more each way than they reach, within the frame; x0 >= x1: none (off
// the frame, or no area).
fn pixels_of(k: array<vec3<f32>, 3>) -> vec4<i32> {
    let a = k[0];
    let b = k[1];
    let c = k[2];
    let none = vec4<i32>(0);
    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    if (!(abs(area) > 0.0) || !(a.z > 0.0 && b.z > 0.0 && c.z > 0.0)) { return none; }
    let lo = floor(min(min(a.xy, b.xy), c.xy)) - vec2<f32>(1.0);
    let hi = ceil(max(max(a.xy, b.xy), c.xy)) + vec2<f32>(1.0);
    let w = f32(P.size.x);
    let h = f32(P.size.y);
    if (!(hi.x > 0.0 && hi.y > 0.0 && lo.x < w && lo.y < h)) { return none; }
    let p0 = clamp(lo, vec2<f32>(0.0), vec2<f32>(w, h));
    let p1 = clamp(hi, vec2<f32>(0.0), vec2<f32>(w, h));
    return vec4<i32>(vec2<i32>(p0), vec2<i32>(p1));
}

// Pixels `p`'s bins (x0, y0, x1, y1, inclusive; x0 > x1: none).
fn tiles_of(p: vec4<i32>) -> vec4<i32> {
    if (p.x >= p.z || p.y >= p.w) { return vec4<i32>(1, 0, 0, 0); }
    return vec4<i32>(p.x / BIN, p.y / BIN, (p.z - 1) / BIN, (p.w - 1) / BIN);
}

fn index(gid: vec3<u32>) -> u32 { return gid.x + gid.y * 65535u * 256u; }

@compute @workgroup_size(256)
fn count(@builtin(global_invocation_id) gid: vec3<u32>) {
    let g = index(gid);
    if (g >= P.counts.y) { return; }
    let k = corners_of(g);
    for (var j = 0u; j < 3u; j++) {
        setups[10u * g + 2u * j] = k[j].x;
        setups[10u * g + 2u * j + 1u] = k[j].y;
        setups[10u * g + 6u + j] = k[j].z;
    }
    setups[10u * g + 9u] = bitcast<f32>(instance_of(g));
    let t = tiles_of(pixels_of(k));
    for (var y = t.y; y <= t.w; y++) {
        for (var x = t.x; x <= t.z; x++) {
            atomicAdd(&counts[u32(y) * P.size.z + u32(x)], 1u);
        }
    }
}

var<workgroup> chunk: array<u32, 256>;

// The tiles' starts (one workgroup), and their cursors back to 0.
@compute @workgroup_size(256)
fn offsets(@builtin(local_invocation_id) lid: vec3<u32>) {
    let tiles = P.counts.z;
    var total = 0u;
    for (var at = 0u; at < tiles; at += 256u) {
        let i = at + lid.x;
        var v = 0u;
        if (i < tiles) { v = atomicLoad(&counts[i]); }
        chunk[lid.x] = v;
        workgroupBarrier();
        // Inclusive scan of the chunk (Hillis–Steele).
        for (var s = 1u; s < 256u; s <<= 1u) {
            var add = 0u;
            if (lid.x >= s) { add = chunk[lid.x - s]; }
            workgroupBarrier();
            chunk[lid.x] += add;
            workgroupBarrier();
        }
        if (i < tiles) {
            starts[i] = total + chunk[lid.x] - v;
            atomicStore(&cursors[i], 0u);
        }
        total += chunk[255];
        workgroupBarrier();
    }
    if (lid.x == 0u) { starts[tiles] = total; }
}

@compute @workgroup_size(256)
fn scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    let g = index(gid);
    if (g >= P.counts.y) { return; }
    let k = array<vec3<f32>, 3>(
        vec3<f32>(setups[10u * g], setups[10u * g + 1u], setups[10u * g + 6u]),
        vec3<f32>(setups[10u * g + 2u], setups[10u * g + 3u], setups[10u * g + 7u]),
        vec3<f32>(setups[10u * g + 4u], setups[10u * g + 5u], setups[10u * g + 8u]),
    );
    let p = pixels_of(k);
    let t = tiles_of(p);
    if (t.x > t.z) { return; }
    // (Its nearest point: at the corner with the most 1 / depth; for an
    // orthographic camera too, 1 / depth there being 1 / the distance.)
    let near = 1.0 / max(max(k[0].z, k[1].z), k[2].z);
    let entry = vec4<u32>(g, u32(p.x) | (u32(p.y) << 16u), u32(p.z) | (u32(p.w) << 16u), bitcast<u32>(near));
    for (var y = t.y; y <= t.w; y++) {
        for (var x = t.x; x <= t.z; x++) {
            let k = u32(y) * P.size.z + u32(x);
            pairs[starts[k] + atomicAdd(&cursors[k], 1u)] = entry;
        }
    }
}
