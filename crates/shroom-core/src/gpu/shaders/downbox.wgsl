// Mean over i0 × i0 blocks of the source (sw wide).
@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let f = P.i0;
    var acc = 0.0;
    for (var j = 0u; j < f; j++) {
        for (var i = 0u; i < f; i++) {
            acc += s[(y * f + j) * P.sw + x * f + i];
        }
    }
    o[y * P.w + x] = acc / f32(f * f);
}
