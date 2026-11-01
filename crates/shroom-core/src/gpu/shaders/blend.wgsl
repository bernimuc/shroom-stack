// Depth map pass 2: out += frame · max(0, 1 − |depth − i0|).
@group(0) @binding(1) var<storage, read_write> o: array<f32>;
@group(0) @binding(2) var<storage, read> frame: array<f32>;
@group(0) @binding(3) var<storage, read> depth: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let i = y * P.w + x;
    let w = max(0.0, 1.0 - abs(depth[i] - f32(P.i0)));
    if (w > 0.0) {
        o[i * 3u] += w * frame[i * 3u];
        o[i * 3u + 1u] += w * frame[i * 3u + 1u];
        o[i * 3u + 2u] += w * frame[i * 3u + 2u];
    }
}
