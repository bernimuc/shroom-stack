// In-place element-wise op over a w×h grid. i0: 0 = scale by f0, 1 = max(a, f0), 2 = square.
@group(0) @binding(1) var<storage, read_write> a: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let i = y * P.w + x;
    switch (P.i0) {
        case 0u: { a[i] = a[i] * P.f0; }
        case 1u: { a[i] = max(a[i], P.f0); }
        default: { a[i] = a[i] * a[i]; }
    }
}
