// Malvar–He–Cutler demosaicing; mirrors raw::demosaic. i0..i3: CFA colours of the 2×2 cell.
@group(0) @binding(1) var<storage, read> m: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;

fn refl(i: i32, n: i32) -> i32 {
    var j = abs(i);
    if (j >= n) { j = 2 * (n - 1) - j; }
    return j;
}
fn at(x: i32, y: i32) -> f32 {
    return m[u32(refl(y, i32(P.h))) * P.w + u32(refl(x, i32(P.w)))];
}
fn colour(x: u32, y: u32) -> u32 {
    switch ((y & 1u) * 2u + (x & 1u)) {
        case 0u: { return P.i0; }
        case 1u: { return P.i1; }
        case 2u: { return P.i2; }
        default: { return P.i3; }
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= P.w || y >= P.h) { return; }
    let xi = i32(x);
    let yi = i32(y);
    let c = at(xi, yi);
    let cross1 = at(xi - 1, yi) + at(xi + 1, yi) + at(xi, yi - 1) + at(xi, yi + 1);
    let cross2 = at(xi - 2, yi) + at(xi + 2, yi) + at(xi, yi - 2) + at(xi, yi + 2);
    let diag = at(xi - 1, yi - 1) + at(xi + 1, yi - 1) + at(xi - 1, yi + 1) + at(xi + 1, yi + 1);
    let horiz1 = at(xi - 1, yi) + at(xi + 1, yi);
    let vert1 = at(xi, yi - 1) + at(xi, yi + 1);
    let horiz2 = at(xi - 2, yi) + at(xi + 2, yi);
    let vert2 = at(xi, yi - 2) + at(xi, yi + 2);
    var rgb: vec3<f32>;
    let col = colour(x, y);
    if (col == 1u) {
        let row_val = (5.0 * c - diag - horiz2 + 0.5 * vert2 + 4.0 * horiz1) / 8.0;
        let col_val = (5.0 * c - diag - vert2 + 0.5 * horiz2 + 4.0 * vert1) / 8.0;
        if (colour(x + 1u, y) == 0u) { rgb = vec3(row_val, c, col_val); } else { rgb = vec3(col_val, c, row_val); }
    } else {
        let g = (4.0 * c - cross2 + 2.0 * cross1) / 8.0;
        let opposite = (6.0 * c - 1.5 * cross2 + 2.0 * diag) / 8.0;
        if (col == 0u) { rgb = vec3(c, g, opposite); } else { rgb = vec3(opposite, g, c); }
    }
    let i = (y * P.w + x) * 3u;
    o[i] = max(rgb.x, 0.0);
    o[i + 1u] = max(rgb.y, 0.0);
    o[i + 2u] = max(rgb.z, 0.0);
}
