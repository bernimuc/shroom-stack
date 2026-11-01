// Upsampling counterpart of reduce; mirrors filter::expand.
@group(0) @binding(1) var<storage, read> s: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;

fn taps(x: u32, n: u32) -> array<vec2<f32>, 3> {
    // (index, weight) pairs, clamped; unused taps have weight 0.
    let j = i32(x / 2u);
    let last = i32(n) - 1;
    if (x % 2u == 0u) {
        return array(vec2(f32(clamp(j - 1, 0, last)), 0.125), vec2(f32(clamp(j, 0, last)), 0.75),
                     vec2(f32(clamp(j + 1, 0, last)), 0.125));
    }
    return array(vec2(f32(clamp(j, 0, last)), 0.5), vec2(f32(clamp(j + 1, 0, last)), 0.5), vec2(0.0, 0.0));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    var tx = taps(x, P.sw);
    var ty = taps(y, P.sh);
    var acc = 0.0;
    for (var j = 0; j < 3; j++) {
        for (var i = 0; i < 3; i++) {
            acc += tx[i].y * ty[j].y * s[u32(ty[j].x) * P.sw + u32(tx[i].x)];
        }
    }
    o[y * P.w + x] = acc;
}
