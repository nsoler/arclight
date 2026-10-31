// Gaussian splats prepared for a view on the GPU: what render3d/splats.rs's
// `project_all` does on the CPU. `project` sees each splat through the
// camera (culled as there, its ellipse and color as there) and counts the
// tiles it reaches; `offsets` turns the counts into where each tile's list
// starts; `scatter` writes each splat into its tiles' lists; `sort_tiles`
// puts each list nearest first (ties by splat, so the order is the same
// every time).
//
// The splats of every set are numbered one after another (`g`). A set is
// SET_WORDS words in `sets`: its space to the camera's (3 rows of 4), the
// camera in its space (or, orthographic, the way it looks), its first
// splat, how many, where its data starts in `data`, the SH degree stored
// and the one used. A splat's data is 11 words (position, scales,
// rotation w x y z, opacity) and then its SH coefficients.

const TILE: f32 = 16.0;
const SET_WORDS: u32 = 24u;
const PROJ_WORDS: u32 = 12u;
const ALPHA_MIN: f32 = 1.0 / 255.0;
const ALPHA_MAX: f32 = 0.99;
const DILATE: f32 = 0.3;
// Pairs a tile sorts in workgroup memory (more: in place in `pairs`).
const LOCAL: u32 = 2048u;

const SH_C0: f32 = 0.2820948;
const SH_C1: f32 = 0.4886025;

struct Params {
    // width, height, tiles across, tiles down
    size: vec4<u32>,
    // splats, sets, orthographic (1) or not, tiles
    counts: vec4<u32>,
    // focal (pixels), or pixels a unit (orthographic); near; far
    cam: vec4<f32>,
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> sets: array<u32>;
@group(0) @binding(2) var<storage, read> data: array<f32>;
// Per splat: center x y, conic xx xy yy, depth, color r g b, opacity,
// (unused), set + 1 (0: culled).
@group(0) @binding(3) var<storage, read_write> proj: array<u32>;
// Per tile: how many splats reach it; then (after `offsets`) where its
// list starts, with the total last; and each tile's fill cursor.
@group(0) @binding(4) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> starts: array<u32>;
@group(0) @binding(6) var<storage, read_write> cursors: array<atomic<u32>>;
// (depth bits, splat) pairs, by tile.
@group(0) @binding(7) var<storage, read_write> pairs: array<vec2<u32>>;

fn set_word(k: u32, w: u32) -> u32 { return sets[k * SET_WORDS + w]; }
fn set_f(k: u32, w: u32) -> f32 { return bitcast<f32>(sets[k * SET_WORDS + w]); }
fn set_row(k: u32, r: u32) -> vec4<f32> { return vec4<f32>(set_f(k, 4u * r), set_f(k, 4u * r + 1u), set_f(k, 4u * r + 2u), set_f(k, 4u * r + 3u)); }

// The set splat g is in.
fn set_of(g: u32) -> u32 {
    var k = 0u;
    for (var s = 0u; s < P.counts.y; s++) {
        if (g >= set_word(s, 15u)) { k = s; }
    }
    return k;
}

fn pf(g: u32, w: u32) -> f32 { return bitcast<f32>(proj[g * PROJ_WORDS + w]); }

// Rotation matrix of quaternion (w, x, y, z), normalized here (columns).
fn rotation(q_in: vec4<f32>) -> mat3x3<f32> {
    let n = length(q_in);
    var q = vec4<f32>(1.0, 0.0, 0.0, 0.0);
    if (n > 0.0) { q = q_in / n; }
    let w = q.x;
    let x = q.y;
    let y = q.z;
    let z = q.w;
    return mat3x3<f32>(
        vec3<f32>(1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y)),
        vec3<f32>(2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)),
        vec3<f32>(2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)),
    );
}

// The color SH coefficients at `base` (degree `degree`) give along unit
// direction d (as splat.rs's `sh_color`).
fn sh_color(base: u32, degree: u32, d: vec3<f32>) -> vec3<f32> {
    var out: vec3<f32>;
    let x = d.x;
    let y = d.y;
    let z = d.z;
    for (var ch = 0u; ch < 3u; ch++) {
        var v = SH_C0 * data[base + ch];
        if (degree >= 1u) {
            v += SH_C1 * (-y * data[base + 3u + ch] + z * data[base + 6u + ch] - x * data[base + 9u + ch]);
        }
        if (degree >= 2u) {
            let xx = x * x;
            let yy = y * y;
            let zz = z * z;
            let xy = x * y;
            let yz = y * z;
            let xz = x * z;
            let c = array<f32, 5>(1.0925484, -1.0925484, 0.31539157, -1.0925484, 0.5462742);
            v += c[0] * xy * data[base + 12u + ch] + c[1] * yz * data[base + 15u + ch] + c[2] * (2.0 * zz - xx - yy) * data[base + 18u + ch] + c[3] * xz * data[base + 21u + ch] + c[4] * (xx - yy) * data[base + 24u + ch];
            if (degree >= 3u) {
                let e = array<f32, 7>(-0.5900436, 2.8906114, -0.4570458, 0.37317633, -0.4570458, 1.4453057, -0.5900436);
                v += e[0] * y * (3.0 * xx - yy) * data[base + 27u + ch]
                    + e[1] * xy * z * data[base + 30u + ch]
                    + e[2] * y * (4.0 * zz - xx - yy) * data[base + 33u + ch]
                    + e[3] * z * (2.0 * zz - 3.0 * xx - 3.0 * yy) * data[base + 36u + ch]
                    + e[4] * x * (4.0 * zz - xx - yy) * data[base + 39u + ch]
                    + e[5] * z * (xx - yy) * data[base + 42u + ch]
                    + e[6] * x * (xx - 3.0 * yy) * data[base + 45u + ch];
            }
        }
        out[ch] = max(v + 0.5, 0.0);
    }
    return out;
}

// How far out (squared, in σ) splat g counts: 3σ, or nearer where it
// fades below ALPHA_MIN first.
fn reach2(g: u32) -> f32 {
    return clamp(2.0 * log(pf(g, 9u) / ALPHA_MIN), 0.0, 9.0);
}

// The tiles (x0, x1, y0, y1, inclusive) the box round splat g's ellipse
// covers.
fn tile_box(g: u32) -> vec4<u32> {
    let a = pf(g, 2u);
    let b = pf(g, 3u);
    let c = pf(g, 4u);
    let det = 1.0 / (a * c - b * b);
    let r = sqrt(reach2(g));
    let rx = r * sqrt(c * det);
    let ry = r * sqrt(a * det);
    let cx = pf(g, 0u);
    let cy = pf(g, 1u);
    let tx = f32(P.size.z - 1u);
    let ty = f32(P.size.w - 1u);
    return vec4<u32>(
        u32(min(max(floor((cx - rx) / TILE), 0.0), tx)),
        u32(min(max(floor((cx + rx) / TILE), 0.0), tx)),
        u32(min(max(floor((cy - ry) / TILE), 0.0), ty)),
        u32(min(max(floor((cy + ry) / TILE), 0.0), ty)),
    );
}

// Whether splat g's ellipse reaches tile (tx, ty) (see splats.rs).
fn reaches(g: u32, tx: u32, ty: u32) -> bool {
    let x0 = f32(tx) * TILE - pf(g, 0u);
    let y0 = f32(ty) * TILE - pf(g, 1u);
    let x1 = x0 + TILE;
    let y1 = y0 + TILE;
    if (x0 <= 0.0 && 0.0 <= x1 && y0 <= 0.0 && 0.0 <= y1) { return true; }
    let a = pf(g, 2u);
    let b = pf(g, 3u);
    let c = pf(g, 4u);
    var least = 3.0e38;
    for (var e = 0u; e < 2u; e++) {
        let x = select(x0, x1, e == 1u);
        let y = clamp(-b * x / c, y0, y1);
        least = min(least, a * x * x + 2.0 * b * x * y + c * y * y);
        let yy = select(y0, y1, e == 1u);
        let xx = clamp(-b * yy / a, x0, x1);
        least = min(least, a * xx * xx + 2.0 * b * xx * yy + c * yy * yy);
    }
    return least <= reach2(g);
}

@compute @workgroup_size(256)
fn project(@builtin(global_invocation_id) gid: vec3<u32>) {
    let g = gid.x + gid.y * 65535u * 256u;
    if (g >= P.counts.x) { return; }
    for (var w = 0u; w < PROJ_WORDS; w++) { proj[g * PROJ_WORDS + w] = 0u; }
    let k = set_of(g);
    let stride = 11u + 3u * (set_word(k, 18u) + 1u) * (set_word(k, 18u) + 1u);
    let base = set_word(k, 17u) + (g - set_word(k, 15u)) * stride;
    let opacity = data[base + 10u];
    if (opacity < ALPHA_MIN) { return; }
    let pos = vec4<f32>(data[base], data[base + 1u], data[base + 2u], 1.0);
    let r0 = set_row(k, 0u);
    let r1 = set_row(k, 1u);
    let r2 = set_row(k, 2u);
    let p = vec3<f32>(dot(r0, pos), dot(r1, pos), dot(r2, pos));
    let depth = -p.z;
    if (depth < max(P.cam.y, 1e-9) || depth > P.cam.z) { return; }
    let w = f32(P.size.x);
    let h = f32(P.size.y);
    let f = P.cam.x;
    var center: vec2<f32>;
    // The projection's Jacobian at the center (2 × 3, as rows).
    var j0: vec3<f32>;
    var j1: vec3<f32>;
    if (P.counts.z != 0u) {
        center = vec2<f32>(w / 2.0 + f * p.x, h / 2.0 - f * p.y);
        j0 = vec3<f32>(f, 0.0, 0.0);
        j1 = vec3<f32>(0.0, -f, 0.0);
    } else {
        center = vec2<f32>(w / 2.0 + f * p.x / depth, h / 2.0 - f * p.y / depth);
        j0 = vec3<f32>(f / depth, 0.0, f * p.x / (depth * depth));
        j1 = vec3<f32>(0.0, -f / depth, -f * p.y / (depth * depth));
    }
    // Its covariance in camera space (M Mᵀ), then on the frame (J Σ Jᵀ).
    let lin = transpose(mat3x3<f32>(r0.xyz, r1.xyz, r2.xyz));
    let rot = rotation(vec4<f32>(data[base + 6u], data[base + 7u], data[base + 8u], data[base + 9u]));
    let sc = vec3<f32>(data[base + 3u], data[base + 4u], data[base + 5u]);
    let m = lin * rot * mat3x3<f32>(vec3<f32>(sc.x, 0.0, 0.0), vec3<f32>(0.0, sc.y, 0.0), vec3<f32>(0.0, 0.0, sc.z));
    let cov = m * transpose(m);
    let cj0 = cov * j0;
    let cj1 = cov * j1;
    let a = dot(j0, cj0) + DILATE;
    let b = dot(j0, cj1);
    let c = dot(j1, cj1) + DILATE;
    let det = a * c - b * b;
    if (det <= 0.0) { return; }
    let mid = (a + c) / 2.0;
    let reach = 3.0 * sqrt(mid + sqrt(max(mid * mid - det, 0.0)));
    if (center.x + reach < 0.0 || center.y + reach < 0.0 || center.x - reach > w || center.y - reach > h) { return; }
    let eye = vec3<f32>(set_f(k, 12u), set_f(k, 13u), set_f(k, 14u));
    var look = pos.xyz - eye;
    if (P.counts.z != 0u) { look = eye; }
    var dir = vec3<f32>(0.0, 0.0, 1.0);
    if (length(look) > 0.0) { dir = normalize(look); }
    let color = sh_color(base + 11u, set_word(k, 19u), dir);
    let o = g * PROJ_WORDS;
    proj[o] = bitcast<u32>(center.x);
    proj[o + 1u] = bitcast<u32>(center.y);
    proj[o + 2u] = bitcast<u32>(c / det);
    proj[o + 3u] = bitcast<u32>(-b / det);
    proj[o + 4u] = bitcast<u32>(a / det);
    proj[o + 5u] = bitcast<u32>(depth);
    proj[o + 6u] = bitcast<u32>(color.r);
    proj[o + 7u] = bitcast<u32>(color.g);
    proj[o + 8u] = bitcast<u32>(color.b);
    proj[o + 9u] = bitcast<u32>(min(opacity, ALPHA_MAX));
    proj[o + 11u] = k + 1u;
    // The tiles it reaches.
    let bx = tile_box(g);
    for (var ty = bx.z; ty <= bx.w; ty++) {
        for (var tx = bx.x; tx <= bx.y; tx++) {
            if (reaches(g, tx, ty)) { atomicAdd(&counts[ty * P.size.z + tx], 1u); }
        }
    }
}

// Where each tile's list starts (one workgroup, a running total over the
// tiles a chunk at a time), the total last; the cursors cleared.
var<workgroup> chunk: array<u32, 256>;

@compute @workgroup_size(256)
fn offsets(@builtin(local_invocation_id) lid: vec3<u32>) {
    let tiles = P.counts.w;
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
    let g = gid.x + gid.y * 65535u * 256u;
    if (g >= P.counts.x || proj[g * PROJ_WORDS + 11u] == 0u) { return; }
    let key = proj[g * PROJ_WORDS + 5u];
    let bx = tile_box(g);
    for (var ty = bx.z; ty <= bx.w; ty++) {
        for (var tx = bx.x; tx <= bx.y; tx++) {
            if (reaches(g, tx, ty)) {
                let t = ty * P.size.z + tx;
                let slot = atomicAdd(&cursors[t], 1u);
                pairs[starts[t] + slot] = vec2<u32>(key, g);
            }
        }
    }
}

// Whether pair a goes after pair b: farther, or as far and a later splat.
fn after(a: vec2<u32>, b: vec2<u32>) -> bool {
    return a.x > b.x || (a.x == b.x && a.y > b.y);
}

var<workgroup> local_pairs: array<vec2<u32>, LOCAL>;
var<workgroup> tile_first: u32;
var<workgroup> tile_len: u32;

// One workgroup a tile: its list nearest first. A bitonic sort whose
// every compare puts the smaller first (so the list's length needn't be a
// power of two: past its end counts as farthest of all, and never moves).
@compute @workgroup_size(256)
fn sort_tiles(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let t = wid.x + wid.y * 65535u;
    if (t >= P.counts.w) { return; }
    // (Read once, the same for the whole workgroup, so its barriers are.)
    if (lid.x == 0u) {
        tile_first = starts[t];
        tile_len = starts[t + 1u] - tile_first;
    }
    let first = workgroupUniformLoad(&tile_first);
    let n = workgroupUniformLoad(&tile_len);
    if (n < 2u) { return; }
    var size = 2u;
    while (size < n) { size <<= 1u; }
    let local = size <= LOCAL;
    if (local) {
        for (var i = lid.x; i < size; i += 256u) {
            if (i < n) { local_pairs[i] = pairs[first + i]; } else { local_pairs[i] = vec2<u32>(0xffffffffu); }
        }
        workgroupBarrier();
    }
    for (var k = 2u; k <= size; k <<= 1u) {
        for (var j = k >> 1u; j > 0u; j >>= 1u) {
            for (var i = lid.x; i < size; i += 256u) {
                // The first step of each round mirrors (i with its mirror
                // in the block of k); the rest halve.
                var partner = i ^ j;
                if (j == (k >> 1u)) { partner = i ^ (k - 1u); }
                if (partner > i && partner < size) {
                    if (local) {
                        let a = local_pairs[i];
                        let b = local_pairs[partner];
                        if (after(a, b)) {
                            local_pairs[i] = b;
                            local_pairs[partner] = a;
                        }
                    } else if (partner < n) {
                        let a = pairs[first + i];
                        let b = pairs[first + partner];
                        if (after(a, b)) {
                            pairs[first + i] = b;
                            pairs[first + partner] = a;
                        }
                    }
                }
            }
            if (local) { workgroupBarrier(); } else { storageBarrier(); workgroupBarrier(); }
        }
    }
    if (local) {
        for (var i = lid.x; i < n; i += 256u) { pairs[first + i] = local_pairs[i]; }
    }
}
