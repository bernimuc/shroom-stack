// 4-neighbour Laplacian, clamped at the border; squared if i0 == 1.
@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let l = select(x - 1u, 0u, x == 0u);
    let r = min(x + 1u, P.w - 1u);
    let u = select(y - 1u, 0u, y == 0u);
    let d = min(y + 1u, P.h - 1u);
    let v = s[y * P.w + l] + s[y * P.w + r] + s[u * P.w + x] + s[d * P.w + x] - 4.0 * s[y * P.w + x];
    o[y * P.w + x] = select(v, v * v, P.i0 == 1u);
}
