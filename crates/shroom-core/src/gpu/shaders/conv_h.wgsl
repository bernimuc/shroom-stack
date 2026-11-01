// One direction of a separable convolution, clamped at the border. i0 = kernel radius.
@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read> taps_k: array<f32>;
@group(0) @binding(3) var<storage, read_write> o: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let r = i32(P.i0);
    var acc = 0.0;
    for (var k = 0; k <= 2 * r; k++) {
        acc += taps_k[k] * s[u32(clamp(i32(x) + k - r, 0, i32(P.w) - 1)) + y * P.w];
    }
    o[y * P.w + x] = acc;
}
