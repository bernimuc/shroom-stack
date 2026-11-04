// a += b (i0 = 0) or a = max(a, b) (i0 = 1) element-wise over a w×h grid.
@group(0) @binding(1) var<storage, read_write> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let i = y * P.w + x;
    if (P.i0 == 1u) { a[i] = max(a[i], b[i]); } else { a[i] = a[i] + b[i]; }
}
