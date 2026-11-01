// Catmull–Rom resampling under an affine transform; mirrors align::warp_rgb.
// Output (w×h) pixel (x, y) samples source (sw×sh) at T(i0 + x, i1 + y), T = f0..f5.
@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;

fn weights(t: f32) -> vec4<f32> {
    let t2 = t * t;
    let t3 = t2 * t;
    return 0.5 * vec4(-t3 + 2.0 * t2 - t, 3.0 * t3 - 5.0 * t2 + 2.0, -3.0 * t3 + 4.0 * t2 + t, t3 - t2);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let ox = f32(P.i0 + x);
    let oy = f32(P.i1 + y);
    let sx = P.f0 * ox + P.f1 * oy + P.f2;
    let sy = P.f3 * ox + P.f4 * oy + P.f5;
    let fx = floor(sx);
    let fy = floor(sy);
    let wx = weights(sx - fx);
    let wy = weights(sy - fy);
    var acc = vec3(0.0);
    for (var j = 0; j < 4; j++) {
        let yy = u32(clamp(i32(fy) + j - 1, 0, i32(P.sh) - 1));
        for (var i = 0; i < 4; i++) {
            let xx = u32(clamp(i32(fx) + i - 1, 0, i32(P.sw) - 1));
            let k = (yy * P.sw + xx) * 3u;
            acc += wx[i] * wy[j] * vec3(s[k], s[k + 1u], s[k + 2u]);
        }
    }
    let o_i = (y * P.w + x) * 3u;
    o[o_i] = max(acc.x, 0.0);
    o[o_i + 1u] = max(acc.y, 0.0);
    o[o_i + 2u] = max(acc.z, 0.0);
}
