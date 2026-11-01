@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read_write> r: array<f32>;
@group(0) @binding(3) var<storage, read_write> g: array<f32>;
@group(0) @binding(4) var<storage, read_write> b: array<f32>;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let i = y * P.w + x;
    r[i] = s[i * 3u];
    g[i] = s[i * 3u + 1u];
    b[i] = s[i * 3u + 2u];
}
