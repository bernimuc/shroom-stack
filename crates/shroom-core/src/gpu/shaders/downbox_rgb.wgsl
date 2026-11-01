// Mean over i0 × i0 blocks of an RGB source (sw wide).
@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let f = P.i0;
    var acc = vec3(0.0);
    for (var j = 0u; j < f; j++) {
        for (var i = 0u; i < f; i++) {
            let k = ((y * f + j) * P.sw + x * f + i) * 3u;
            acc += vec3(s[k], s[k + 1u], s[k + 2u]);
        }
    }
    let n = f32(f * f);
    let o_i = (y * P.w + x) * 3u;
    o[o_i] = acc.x / n;
    o[o_i + 1u] = acc.y / n;
    o[o_i + 2u] = acc.z / n;
}
