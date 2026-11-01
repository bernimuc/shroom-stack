// Binomial 5×5 blur of the source (sw×sh), sampled at even positions; mirrors filter::reduce.
@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    var K = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);
    var acc = 0.0;
    for (var j = 0; j < 5; j++) {
        let yy = u32(clamp(i32(2u * y) + j - 2, 0, i32(P.sh) - 1));
        for (var i = 0; i < 5; i++) {
            let xx = u32(clamp(i32(2u * x) + i - 2, 0, i32(P.sw) - 1));
            acc += K[i] * K[j] * s[yy * P.sw + xx];
        }
    }
    o[y * P.w + x] = acc;
}
