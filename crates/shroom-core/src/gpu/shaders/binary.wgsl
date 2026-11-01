// out = a ∘ b element-wise over a w×h grid. i0: 0 = subtract, 1 = add, 2 = multiply.
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> o: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let i = y * P.w + x;
    switch (P.i0) {
        case 0u: { o[i] = a[i] - b[i]; }
        case 1u: { o[i] = a[i] + b[i]; }
        default: { o[i] = a[i] * b[i]; }
    }
}
