// The card bloom splat/blend inner loop, as a compute pass.
//
// The CPU original (`src/ui/sidebar/image_card.rs::lay_splat`) *scatters*: it
// walks each splat's own bounding box and keeps the brightest contribution per
// pixel in a side field, then composites that field over the canvas. This
// gathers instead — every pixel looks at every splat that could reach it and
// takes the max itself — which is the same arithmetic in the same order with no
// atomics and no second pass, and is why the two agree bit for bit.
//
// Every constant this pass needs comes in through `Tile`; nothing about the
// card's look is written twice. See `measured::BLOOM_PEAK` and its siblings for
// where the numbers come from.

struct Splat {
    // x, y, w, h of the card's rounded rect, in this tile's pixels.
    rect: vec4<f32>,
    // radius, near_sigma, far_sigma, unused.
    curve: vec4<f32>,
    // The splat's own clamped bounding box: x0, y0, x1, y1, covering
    // [x0, x1) x [y0, y1).
    bounds: vec4<u32>,
    // max_step, column_offset, unused, unused. `max_step` is the last valid
    // index into the distance profile: past it the CPU's `profile.get(..)`
    // returns `None` and the pixel is skipped. `column_offset` is where this
    // splat's per-column inks start in `columns`.
    lookup: vec4<u32>,
}

// One card's image. Bound with a dynamic offset, so a whole frame's cards are
// one buffer, one pass and one readback.
struct Tile {
    // width, height, splat_offset, splat_count.
    dims: vec4<u32>,
    // pixel_offset into the shared pixel buffer, then unused.
    offsets: vec4<u32>,
    // steps_per_px, peak, near_weight, far_weight.
    curve: vec4<f32>,
    // paint_floor, then unused.
    limits: vec4<f32>,
}

@group(0) @binding(0) var<uniform> tile: Tile;
@group(0) @binding(1) var<storage, read> splats: array<Splat>;
// (r, g, b, strength) per pixel column. r/g/b are 0..255 because the CPU
// resolves them to 8-bit sRGB before the column table is built, and rounding
// them here instead would be a second, different answer.
@group(0) @binding(2) var<storage, read> columns: array<vec4<f32>>;
// RGBA8 little-endian, one u32 per pixel — the same layout `Canvas` holds.
@group(0) @binding(3) var<storage, read_write> pixels: array<u32>;

/// Signed distance to a rounded rect: negative inside, positive outside.
/// Mirrors `canvas::RoundRect::distance`.
fn round_rect_distance(s: Splat, px: f32, py: f32) -> f32 {
    let hx = s.rect.z / 2.0;
    let hy = s.rect.w / 2.0;
    let r = max(min(min(s.curve.x, hx), hy), 0.0);
    let dx = abs(px - (s.rect.x + hx)) - (hx - r);
    let dy = abs(py - (s.rect.y + hy)) - (hy - r);
    let outside = sqrt(max(dx, 0.0) * max(dx, 0.0) + max(dy, 0.0) * max(dy, 0.0));
    return outside + min(max(dx, dy), 0.0) - r;
}

/// Rust's `f32::round` is half-away-from-zero; WGSL's `round` is half-to-even.
/// Every value rounded here is non-negative, so this is the former.
fn round_half_up(v: f32) -> f32 {
    return floor(v + 0.5);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= tile.dims.x || y >= tile.dims.y) {
        return;
    }

    let px = f32(x) + 0.5;
    let py = f32(y) + 0.5;

    // The brightest bloom any card lays on this pixel, max rather than sum —
    // see `lay_splat`'s own doc for why a tree of cards four pixels apart must
    // not add its neighbours' halos together.
    var best_amount = 0.0;
    var best_color = vec3<f32>(0.0, 0.0, 0.0);

    for (var i = 0u; i < tile.dims.w; i = i + 1u) {
        let s = splats[tile.dims.z + i];
        if (x < s.bounds.x || x >= s.bounds.z || y < s.bounds.y || y >= s.bounds.w) {
            continue;
        }
        let d = round_rect_distance(s, px, py);
        if (d <= 0.0) {
            continue;
        }
        // The CPU samples a profile table built at `steps_per_px` samples per
        // pixel and indexed by truncation. Quantising the distance the same way
        // and evaluating the curve here is the same number without shipping the
        // table.
        let step = u32(d * tile.curve.x);
        if (step > s.lookup.x) {
            continue;
        }
        let dq = f32(step) / tile.curve.x;
        let near = exp(-(dq * dq) / (2.0 * s.curve.y * s.curve.y));
        let far = exp(-(dq * dq) / (2.0 * s.curve.z * s.curve.z));
        let profile = tile.curve.y * (tile.curve.z * near + tile.curve.w * far);
        let column = columns[s.lookup.y + (x - s.bounds.x)];
        let amount = profile * column.w;
        if (amount > tile.limits.x && amount > best_amount) {
            best_amount = amount;
            best_color = column.xyz;
        }
    }

    if (best_amount <= 0.0) {
        return;
    }

    // Source-over, straight alpha. `Canvas::blend`, branch for branch.
    let alpha = clamp(best_amount, 0.0, 1.0);
    let index = tile.offsets.x + y * tile.dims.x + x;
    let packed = pixels[index];
    let dst = vec4<f32>(
        f32(packed & 0xffu),
        f32((packed >> 8u) & 0xffu),
        f32((packed >> 16u) & 0xffu),
        f32((packed >> 24u) & 0xffu),
    );

    var out_rgb: vec3<f32>;
    var out_alpha: f32;
    if (alpha >= 1.0) {
        out_rgb = best_color;
        out_alpha = 255.0;
    } else {
        let dst_a = dst.w / 255.0;
        if (dst_a >= 1.0) {
            out_rgb = vec3<f32>(
                round_half_up(best_color.x * alpha + dst.x * (1.0 - alpha)),
                round_half_up(best_color.y * alpha + dst.y * (1.0 - alpha)),
                round_half_up(best_color.z * alpha + dst.z * (1.0 - alpha)),
            );
            out_alpha = dst.w;
        } else {
            let out_a = alpha + dst_a * (1.0 - alpha);
            if (out_a <= 0.0) {
                return;
            }
            out_rgb = vec3<f32>(
                round_half_up((best_color.x * alpha + dst.x * dst_a * (1.0 - alpha)) / out_a),
                round_half_up((best_color.y * alpha + dst.y * dst_a * (1.0 - alpha)) / out_a),
                round_half_up((best_color.z * alpha + dst.z * dst_a * (1.0 - alpha)) / out_a),
            );
            out_alpha = round_half_up(out_a * 255.0);
        }
    }

    let r = u32(clamp(out_rgb.x, 0.0, 255.0));
    let g = u32(clamp(out_rgb.y, 0.0, 255.0));
    let b = u32(clamp(out_rgb.z, 0.0, 255.0));
    let a = u32(clamp(out_alpha, 0.0, 255.0));
    pixels[index] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
