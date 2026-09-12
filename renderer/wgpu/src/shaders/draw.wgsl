// Crisol draw shaders.
//
// One vertex stage feeds two fragment stages: `fs_rect` for solid/rounded/bordered
// rectangles and `fs_image` for textured quads. They share the signed-distance helpers
// below, which is why they live in one module rather than two files.
//
// Colour convention (DECISIONS D-15): instance colours arrive as *straight* sRGB-encoded
// components. This shader converts to linear, premultiplies, and writes premultiplied
// linear. The colour target is an sRGB format, so the hardware does the final encode, and
// blending therefore happens in linear light.

struct Globals {
    // xy = physical pixel size of the render target. zw is padding: a uniform buffer
    // binding must be 16-byte aligned.
    viewport: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct InstanceIn {
    // x, y, width, height in physical pixels.
    @location(0) rect: vec4<f32>,
    // Corner radii in physical pixels, in CSS order: top-left, top-right, bottom-right,
    // bottom-left.
    @location(1) radii: vec4<f32>,
    // Fill colour for `fs_rect`; tint for `fs_image`.
    @location(2) fill: vec4<f32>,
    @location(3) border_color: vec4<f32>,
    // Border widths in physical pixels, in CSS order: top, right, bottom, left.
    @location(4) border_width: vec4<f32>,
    // Source sub-rectangle for `fs_image`: u0, v0, du, dv. Unused by `fs_rect`.
    @location(5) uv: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    // Position within the instance rect, in physical pixels, origin at its top-left.
    @location(0) local: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) size: vec2<f32>,
    @location(3) @interpolate(flat) radii: vec4<f32>,
    @location(4) @interpolate(flat) fill: vec4<f32>,
    @location(5) @interpolate(flat) border_color: vec4<f32>,
    @location(6) @interpolate(flat) border_width: vec4<f32>,
};

// The quad is grown by this many physical pixels on every side so the antialiased edge has
// somewhere to land. Without it, a shape's outer half-pixel of coverage is clipped away by
// the geometry and edges read as too thin.
const AA_PAD: f32 = 1.0;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32, inst: InstanceIn) -> VsOut {
    // Four vertices, triangle-strip order: (0,0) (1,0) (0,1) (1,1).
    let corner = vec2<f32>(f32(vertex_index & 1u), f32((vertex_index >> 1u) & 1u));

    let size = inst.rect.zw;
    let padded_origin = inst.rect.xy - vec2<f32>(AA_PAD, AA_PAD);
    let padded_size = size + vec2<f32>(AA_PAD * 2.0, AA_PAD * 2.0);
    let position = padded_origin + corner * padded_size;
    let local = position - inst.rect.xy;

    var out: VsOut;
    out.clip_position = vec4<f32>(
        position.x / globals.viewport.x * 2.0 - 1.0,
        1.0 - position.y / globals.viewport.y * 2.0,
        0.0,
        1.0,
    );
    out.local = local;
    // Map the *unpadded* rect onto the source sub-rectangle. The padding ring therefore
    // samples just outside it, which clamp-to-edge addressing handles and which zero
    // coverage multiplies away regardless.
    // `max(size, 1)` keeps a degenerate instance from producing NaN texture coordinates.
    out.uv = inst.uv.xy + (local / max(size, vec2<f32>(1.0, 1.0))) * inst.uv.zw;
    out.size = size;
    out.radii = inst.radii;
    out.fill = inst.fill;
    out.border_color = inst.border_color;
    out.border_width = inst.border_width;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Straight sRGB -> premultiplied linear, which is what the blend state expects.
fn premultiply(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(srgb_to_linear(c.rgb) * c.a, c.a);
}

// Picks the radius for the quadrant `p` falls in. `r` is in CSS order and y grows down, so
// a positive y is the *bottom* half.
fn corner_radius(p: vec2<f32>, r: vec4<f32>) -> f32 {
    let right = p.x > 0.0;
    let top_radius = select(r.x, r.y, right);
    let bottom_radius = select(r.w, r.z, right);
    return select(top_radius, bottom_radius, p.y > 0.0);
}

// Signed distance to a rounded box centred on the origin. Negative inside.
fn sd_rounded_box(p: vec2<f32>, half_size: vec2<f32>, r: vec4<f32>) -> f32 {
    let radius = min(corner_radius(p, r), min(half_size.x, half_size.y));
    let q = abs(p) - half_size + vec2<f32>(radius, radius);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - radius;
}

// Analytic coverage from a signed distance. Our local space is physical pixels and the
// shapes are axis-aligned, so the distance gradient has unit length and a linear ramp
// across one pixel is an exact box filter.
fn coverage(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_rect(in: VsOut) -> @location(0) vec4<f32> {
    let half_size = in.size * 0.5;
    let outer = coverage(sd_rounded_box(in.local - half_size, half_size, in.radii));
    if outer <= 0.0 {
        discard;
    }

    // Padding box: the border box inset by the per-edge border widths.
    let bw = in.border_width;
    let inset_top_left = vec2<f32>(bw.w, bw.x);
    let inset_bottom_right = vec2<f32>(bw.y, bw.z);
    let inner_size = max(
        in.size - inset_top_left - inset_bottom_right,
        vec2<f32>(0.0, 0.0),
    );
    let inner_half = inner_size * 0.5;

    // A corner's inner radius shrinks by the thicker of its two adjacent borders. CSS uses
    // per-axis elliptical radii here; we carry circular radii only, and the difference is
    // invisible at the border widths real interfaces use.
    let inner_radii = max(
        in.radii - vec4<f32>(
            max(bw.w, bw.x),
            max(bw.y, bw.x),
            max(bw.y, bw.z),
            max(bw.w, bw.z),
        ),
        vec4<f32>(0.0, 0.0, 0.0, 0.0),
    );

    var inner = 0.0;
    if inner_size.x > 0.0 && inner_size.y > 0.0 {
        let inner_center = inset_top_left + inner_half;
        inner = coverage(sd_rounded_box(in.local - inner_center, inner_half, inner_radii));
    }

    // The background fills the whole border box, as CSS `background-clip: border-box`
    // does, and the border ring paints over it. Doing it in that order is what makes a
    // translucent border composite against the background instead of against the target.
    let background = premultiply(in.fill) * outer;
    let ring = clamp(outer - inner, 0.0, 1.0);
    let border = premultiply(in.border_color) * ring;
    return border + background * (1.0 - border.a);
}

@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

@fragment
fn fs_image(in: VsOut) -> @location(0) vec4<f32> {
    let half_size = in.size * 0.5;
    let outer = coverage(sd_rounded_box(in.local - half_size, half_size, in.radii));
    if outer <= 0.0 {
        discard;
    }

    // The texture is created with an sRGB format, so the sample is already linear. Only
    // the tint, which came from a display list, needs converting.
    let texel = textureSample(image_texture, image_sampler, in.uv);
    let tint = vec4<f32>(srgb_to_linear(in.fill.rgb), in.fill.a);
    let straight = texel * tint;
    return vec4<f32>(straight.rgb * straight.a, straight.a) * outer;
}
