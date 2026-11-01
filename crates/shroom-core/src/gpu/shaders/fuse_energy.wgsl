// Pyramid fusion: mask = new energy wins; keeps the max energy and, if i1 == 1, records
// frame i0 in the source map.
@group(0) @binding(1) var<storage, read_write> fused: array<f32>;
@group(0) @binding(2) var<storage, read> energy: array<f32>;
@group(0) @binding(3) var<storage, read_write> mask: array<f32>;
@group(0) @binding(4) var<storage, read_write> src_map: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let i = y * P.w + x;
    if (energy[i] > fused[i]) {
        fused[i] = energy[i];
        mask[i] = 1.0;
        if (P.i1 == 1u) { src_map[i] = f32(P.i0); }
    } else {
        mask[i] = 0.0;
    }
}
