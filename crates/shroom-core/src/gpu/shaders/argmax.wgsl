// Depth map pass 1: running maximum and sum of the focus measure. i0 = frame index.
@group(0) @binding(1) var<storage, read_write> best: array<f32>;
@group(0) @binding(2) var<storage, read_write> total: array<f32>;
@group(0) @binding(3) var<storage, read_write> depth: array<f32>;
@group(0) @binding(4) var<storage, read> focus: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let i = y * P.w + x;
    let f = focus[i];
    total[i] = total[i] + f;
    if (f > best[i]) {
        best[i] = f;
        depth[i] = f32(P.i0);
    }
}
