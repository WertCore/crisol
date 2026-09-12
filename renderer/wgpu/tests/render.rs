//! M1 acceptance: a display list rasterises correctly, and identically at 1x and 2x DPI.
//!
//! The roadmap's M1 acceptance is "window opens on all three platforms, resizes without
//! panic or artifacts, renders correctly at 1x and 2x DPI". Opening a window is what
//! `examples/window.rs` is for; *renders correctly* is mechanised here, offscreen, so it
//! runs in CI on a machine with no display attached.

use crisol_display_list::{
    Color, Corners, DisplayListBuilder, Edges, ImageCommand, ImageId, Rect, RectCommand, Size,
};
use crisol_render_wgpu::testing::{checkerboard, with_gpu};
use crisol_render_wgpu::{FrameTarget, Gpu, HEADLESS_FORMAT, HeadlessTarget, Pixels, Renderer};

/// Colour comparisons allow this much drift per channel.
///
/// Blending and the sRGB round trip are not bit-exact across backends. Two levels is the
/// most a correct implementation should ever differ by; it is tight enough that a wrong
/// colour space — the failure this suite actually has to catch — is off by twenty or more.
const TOLERANCE: u8 = 2;

fn render(
    gpu: &Gpu,
    logical: Size,
    scale: f32,
    build: impl FnOnce(&mut DisplayListBuilder),
) -> Pixels {
    render_with(gpu, logical, scale, |_| {}, build)
}

/// As [`render`], but with a chance to upload images first.
fn render_with(
    gpu: &Gpu,
    logical: Size,
    scale: f32,
    setup: impl FnOnce(&mut Renderer),
    build: impl FnOnce(&mut DisplayListBuilder),
) -> Pixels {
    let width = (logical.width * scale).round() as u32;
    let height = (logical.height * scale).round() as u32;

    let mut renderer = Renderer::new(gpu, HEADLESS_FORMAT);
    setup(&mut renderer);

    let target = HeadlessTarget::new(gpu, width, height);
    let mut builder = DisplayListBuilder::new(logical);
    build(&mut builder);
    let list = builder.build();

    renderer.render(
        FrameTarget {
            view: target.view(),
            width,
            height,
            scale_factor: scale,
        },
        &list,
    );
    target.read_pixels(gpu)
}

#[test]
fn clears_to_the_background_colour() {
    with_gpu(|gpu| {
        let teal = Color::from_rgba8(0, 128, 128, 255);
        let pixels = render(&gpu, Size::new(16.0, 16.0), 1.0, |b| {
            b.set_background(teal);
        });
        assert_eq!(pixels.width(), 16);
        assert_eq!(pixels.height(), 16);
        pixels.assert_pixel(0, 0, teal, TOLERANCE);
        pixels.assert_pixel(15, 15, teal, TOLERANCE);
        pixels.assert_pixel(8, 8, teal, TOLERANCE);
    });
}

#[test]
fn a_solid_rect_lands_on_the_right_pixels() {
    with_gpu(|gpu| {
        let red = Color::from_rgba8(255, 0, 0, 255);
        let pixels = render(&gpu, Size::new(32.0, 32.0), 1.0, |b| {
            b.set_background(Color::WHITE);
            b.fill_rect(Rect::from_xywh(8.0, 8.0, 16.0, 16.0), red);
        });

        // Inside.
        pixels.assert_pixel(8, 8, red, TOLERANCE);
        pixels.assert_pixel(15, 15, red, TOLERANCE);
        pixels.assert_pixel(23, 23, red, TOLERANCE);
        // Outside, on every side.
        pixels.assert_pixel(7, 15, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(24, 15, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(15, 7, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(15, 24, Color::WHITE, TOLERANCE);
    });
}

/// The M1 DPI acceptance. The same display list, in the same logical coordinates, must
/// cover the same *fraction* of the target at 1x and 2x.
#[test]
fn the_same_list_renders_identically_at_1x_and_2x() {
    with_gpu(|gpu| {
        let blue = Color::from_rgba8(0, 0, 255, 255);
        let logical = Size::new(32.0, 32.0);
        let build = |b: &mut DisplayListBuilder| {
            b.set_background(Color::WHITE);
            b.fill_rect(Rect::from_xywh(8.0, 8.0, 16.0, 16.0), blue);
        };

        let at_1x = render(&gpu, logical, 1.0, build);
        let at_2x = render(&gpu, logical, 2.0, build);

        assert_eq!((at_1x.width(), at_1x.height()), (32, 32));
        assert_eq!((at_2x.width(), at_2x.height()), (64, 64));

        // Every 1x pixel maps to a 2x pixel at double the coordinate. Sample the interior
        // of each 2x pixel block so edge antialiasing does not confuse the comparison.
        for y in 0..32 {
            for x in 0..32 {
                let expected = at_1x.at(x, y);
                let actual = at_2x.at(x * 2 + 1, y * 2 + 1);
                let close = expected
                    .iter()
                    .zip(actual.iter())
                    .all(|(a, b)| a.abs_diff(*b) <= TOLERANCE);
                assert!(
                    close,
                    "1x ({x}, {y}) is {expected:?} but 2x ({}, {}) is {actual:?}",
                    x * 2 + 1,
                    y * 2 + 1
                );
            }
        }
    });
}

/// Fractional scale factors are the common case on Windows and Linux (125%, 150%), and the
/// one most likely to expose an off-by-half-a-pixel in the logical-to-physical conversion.
#[test]
fn a_fractional_scale_factor_covers_the_expected_area() {
    with_gpu(|gpu| {
        let green = Color::from_rgba8(0, 200, 0, 255);
        let pixels = render(&gpu, Size::new(40.0, 40.0), 1.5, |b| {
            b.set_background(Color::WHITE);
            b.fill_rect(Rect::from_xywh(10.0, 10.0, 20.0, 20.0), green);
        });

        assert_eq!((pixels.width(), pixels.height()), (60, 60));
        // 10 logical px * 1.5 = 15 physical px; 30 logical px * 1.5 = 45.
        pixels.assert_pixel(16, 16, green, TOLERANCE);
        pixels.assert_pixel(43, 43, green, TOLERANCE);
        pixels.assert_pixel(13, 30, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(46, 30, Color::WHITE, TOLERANCE);
    });
}

#[test]
fn later_commands_paint_over_earlier_ones() {
    with_gpu(|gpu| {
        let under = Color::from_rgba8(255, 0, 0, 255);
        let over = Color::from_rgba8(0, 0, 255, 255);
        let pixels = render(&gpu, Size::new(16.0, 16.0), 1.0, |b| {
            b.fill_rect(Rect::from_xywh(0.0, 0.0, 16.0, 16.0), under);
            b.fill_rect(Rect::from_xywh(4.0, 4.0, 8.0, 8.0), over);
        });
        pixels.assert_pixel(8, 8, over, TOLERANCE);
        pixels.assert_pixel(1, 1, under, TOLERANCE);
    });
}

/// Half-alpha white over black must land near 50% grey *in linear light*, which is sRGB
/// 188, not sRGB 128. Getting this wrong is the single most common colour bug in a GPU UI
/// renderer and it is invisible until someone compares against a design.
#[test]
fn alpha_blends_in_linear_light() {
    with_gpu(|gpu| {
        let pixels = render(&gpu, Size::new(8.0, 8.0), 1.0, |b| {
            b.set_background(Color::BLACK);
            b.fill_rect(
                Rect::from_xywh(0.0, 0.0, 8.0, 8.0),
                Color::WHITE.with_alpha(0.5),
            );
        });

        let [r, g, b, a] = pixels.at(4, 4);
        assert_eq!(a, 255);
        for channel in [r, g, b] {
            assert!(
                (183..=193).contains(&channel),
                "half-alpha white over black is {channel}, expected ~188 \
                 (sRGB encoding of 0.5 linear); 128 would mean blending in sRGB space"
            );
        }
    });
}

#[test]
fn clips_are_honoured() {
    with_gpu(|gpu| {
        let red = Color::from_rgba8(255, 0, 0, 255);
        let pixels = render(&gpu, Size::new(32.0, 32.0), 1.0, |b| {
            b.set_background(Color::WHITE);
            b.push_clip(Rect::from_xywh(0.0, 0.0, 16.0, 32.0));
            b.fill_rect(Rect::from_xywh(0.0, 0.0, 32.0, 32.0), red);
            b.pop_clip();
        });

        pixels.assert_pixel(4, 16, red, TOLERANCE);
        pixels.assert_pixel(14, 16, red, TOLERANCE);
        pixels.assert_pixel(18, 16, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(31, 16, Color::WHITE, TOLERANCE);
    });
}

#[test]
fn a_border_paints_inside_the_border_box() {
    with_gpu(|gpu| {
        let fill = Color::from_rgba8(0, 0, 255, 255);
        let border = Color::from_rgba8(255, 255, 0, 255);
        let pixels = render(&gpu, Size::new(32.0, 32.0), 1.0, |b| {
            b.set_background(Color::WHITE);
            b.push_rect(RectCommand {
                rect: Rect::from_xywh(8.0, 8.0, 16.0, 16.0),
                radii: Corners::ZERO,
                fill,
                border_color: border,
                border_width: Edges::all(4.0),
            });
        });

        // Border ring.
        pixels.assert_pixel(9, 16, border, TOLERANCE);
        pixels.assert_pixel(22, 16, border, TOLERANCE);
        pixels.assert_pixel(16, 9, border, TOLERANCE);
        pixels.assert_pixel(16, 22, border, TOLERANCE);
        // Padding box.
        pixels.assert_pixel(16, 16, fill, TOLERANCE);
        // Nothing leaked outside the border box.
        pixels.assert_pixel(7, 16, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(24, 16, Color::WHITE, TOLERANCE);
    });
}

#[test]
fn per_edge_border_widths_are_respected() {
    with_gpu(|gpu| {
        let fill = Color::from_rgba8(0, 0, 255, 255);
        let border = Color::from_rgba8(255, 255, 0, 255);
        let pixels = render(&gpu, Size::new(32.0, 32.0), 1.0, |b| {
            b.set_background(Color::WHITE);
            b.push_rect(RectCommand {
                rect: Rect::from_xywh(4.0, 4.0, 24.0, 24.0),
                radii: Corners::ZERO,
                fill,
                border_color: border,
                border_width: Edges {
                    top: 8.0,
                    right: 2.0,
                    bottom: 2.0,
                    left: 2.0,
                },
            });
        });

        // The 8px top border reaches y = 11; the padding box starts at y = 12.
        pixels.assert_pixel(16, 10, border, TOLERANCE);
        pixels.assert_pixel(16, 13, fill, TOLERANCE);
        // The 2px left border reaches x = 5.
        pixels.assert_pixel(5, 20, border, TOLERANCE);
        pixels.assert_pixel(8, 20, fill, TOLERANCE);
    });
}

#[test]
fn rounded_corners_cut_the_corner_pixel() {
    with_gpu(|gpu| {
        let red = Color::from_rgba8(255, 0, 0, 255);
        let pixels = render(&gpu, Size::new(32.0, 32.0), 1.0, |b| {
            b.set_background(Color::WHITE);
            b.push_rect(RectCommand {
                radii: Corners::all(8.0),
                ..RectCommand::solid(Rect::from_xywh(4.0, 4.0, 24.0, 24.0), red)
            });
        });

        // The centre is filled and the corner is not.
        pixels.assert_pixel(16, 16, red, TOLERANCE);
        pixels.assert_pixel(4, 4, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(27, 27, Color::WHITE, TOLERANCE);
        // The middle of each edge is still filled: rounding takes the corners, not the
        // sides.
        pixels.assert_pixel(5, 16, red, TOLERANCE);
        pixels.assert_pixel(16, 5, red, TOLERANCE);
    });
}

/// The "one textured quad" half of M1.
#[test]
fn a_textured_quad_samples_the_right_texels() {
    with_gpu(|gpu| {
        let black = [0, 0, 0, 255];
        let white = [255, 255, 255, 255];
        let image = ImageId(1);

        let pixels = render_with(
            &gpu,
            Size::new(32.0, 32.0),
            1.0,
            |renderer| {
                // 2x2 checkerboard: black top-left and bottom-right, white elsewhere.
                renderer.upload_image(image, 2, 2, &checkerboard(2, 2, 1, black, white));
            },
            |b| {
                b.set_background(Color::from_rgba8(255, 0, 0, 255));
                b.push_image(ImageCommand::new(
                    Rect::from_xywh(0.0, 0.0, 32.0, 32.0),
                    image,
                ));
            },
        );

        // Sample well inside each quadrant so bilinear filtering across the cell boundary
        // does not reach the sample point.
        pixels.assert_pixel(2, 2, Color::BLACK, TOLERANCE);
        pixels.assert_pixel(29, 2, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(2, 29, Color::WHITE, TOLERANCE);
        pixels.assert_pixel(29, 29, Color::BLACK, TOLERANCE);
    });
}

#[test]
fn an_image_that_was_never_uploaded_is_counted_and_skipped() {
    with_gpu(|gpu| {
        let mut renderer = Renderer::new(&gpu, HEADLESS_FORMAT);
        let target = HeadlessTarget::new(&gpu, 8, 8);

        let mut builder = DisplayListBuilder::new(Size::new(8.0, 8.0));
        builder.set_background(Color::WHITE);
        builder.push_image(ImageCommand::new(
            Rect::from_xywh(0.0, 0.0, 8.0, 8.0),
            ImageId(404),
        ));
        let list = builder.build();

        let stats = renderer.render(
            FrameTarget {
                view: target.view(),
                width: 8,
                height: 8,
                scale_factor: 1.0,
            },
            &list,
        );

        assert_eq!(stats.missing_images, 1);
        assert_eq!(stats.draw_calls, 0);
        // The frame still cleared, so a missing image degrades rather than corrupting.
        target
            .read_pixels(&gpu)
            .assert_pixel(4, 4, Color::WHITE, TOLERANCE);
    });
}

/// D-14: rectangles sharing a clip must collapse into one instanced draw call, no matter
/// how many of them there are. This is the property that keeps a tile-based mobile GPU
/// viable, so it is asserted rather than assumed.
#[test]
fn rectangles_sharing_a_clip_collapse_into_one_draw_call() {
    with_gpu(|gpu| {
        let mut renderer = Renderer::new(&gpu, HEADLESS_FORMAT);
        let target = HeadlessTarget::new(&gpu, 64, 64);

        let mut builder = DisplayListBuilder::new(Size::new(64.0, 64.0));
        for i in 0..500 {
            let x = (i % 25) as f32 * 2.0;
            let y = (i / 25) as f32 * 2.0;
            builder.fill_rect(
                Rect::from_xywh(x, y, 2.0, 2.0),
                Color::from_rgba8(i as u8, 64, 128, 255),
            );
        }
        let list = builder.build();

        let stats = renderer.render(
            FrameTarget {
                view: target.view(),
                width: 64,
                height: 64,
                scale_factor: 1.0,
            },
            &list,
        );

        assert_eq!(stats.instances, 500);
        assert_eq!(
            stats.draw_calls, 1,
            "500 unclipped rectangles should be one instanced draw"
        );
    });
}

#[test]
fn each_clip_group_is_its_own_draw_call() {
    with_gpu(|gpu| {
        let mut renderer = Renderer::new(&gpu, HEADLESS_FORMAT);
        let target = HeadlessTarget::new(&gpu, 64, 64);

        let mut builder = DisplayListBuilder::new(Size::new(64.0, 64.0));
        builder.fill_rect(Rect::from_xywh(0.0, 0.0, 64.0, 64.0), Color::WHITE);
        for i in 0..3 {
            let x = i as f32 * 20.0;
            builder.push_clip(Rect::from_xywh(x, 0.0, 20.0, 64.0));
            builder.fill_rect(Rect::from_xywh(x, 0.0, 20.0, 32.0), Color::BLACK);
            builder.fill_rect(Rect::from_xywh(x, 32.0, 20.0, 32.0), Color::BLACK);
            builder.pop_clip();
        }
        let list = builder.build();

        let stats = renderer.render(
            FrameTarget {
                view: target.view(),
                width: 64,
                height: 64,
                scale_factor: 1.0,
            },
            &list,
        );

        // One unclipped batch plus one per clip group; the two rects inside each group
        // share a draw.
        assert_eq!(stats.draw_calls, 4);
        assert_eq!(stats.instances, 7);
    });
}

/// A resize is a new target and a new frame, not new state. Rendering the same list into a
/// sequence of differently sized targets must neither panic nor leave stale pixels — the
/// "resizes without panic or artifacts" half of M1, minus the window.
#[test]
fn rendering_into_a_sequence_of_sizes_does_not_panic_or_leak_state() {
    with_gpu(|gpu| {
        let mut renderer = Renderer::new(&gpu, HEADLESS_FORMAT);

        for (w, h) in [(1_u32, 1_u32), (64, 16), (16, 64), (300, 200), (1, 1)] {
            let target = HeadlessTarget::new(&gpu, w, h);
            let logical = Size::new(w as f32, h as f32);
            let mut builder = DisplayListBuilder::new(logical);
            builder.set_background(Color::from_rgba8(10, 20, 30, 255));
            builder.fill_rect(
                Rect::from_xywh(0.0, 0.0, logical.width, logical.height),
                Color::from_rgba8(200, 100, 50, 255),
            );
            let list = builder.build();

            renderer.render(
                FrameTarget {
                    view: target.view(),
                    width: w,
                    height: h,
                    scale_factor: 1.0,
                },
                &list,
            );

            let pixels = target.read_pixels(&gpu);
            assert_eq!((pixels.width(), pixels.height()), (w, h));
            pixels.assert_pixel(
                w / 2,
                h / 2,
                Color::from_rgba8(200, 100, 50, 255),
                TOLERANCE,
            );
        }
    });
}

/// The instance buffer starts at 1024 and has to grow. Growing it mid-suite must not
/// corrupt the frame that triggered the growth.
#[test]
fn the_instance_buffer_grows_without_dropping_instances() {
    with_gpu(|gpu| {
        let mut renderer = Renderer::new(&gpu, HEADLESS_FORMAT);
        let target = HeadlessTarget::new(&gpu, 128, 128);

        let mut builder = DisplayListBuilder::new(Size::new(128.0, 128.0));
        builder.set_background(Color::WHITE);
        for i in 0..4096 {
            let x = (i % 64) as f32 * 2.0;
            let y = (i / 64) as f32 * 2.0;
            builder.fill_rect(Rect::from_xywh(x, y, 2.0, 2.0), Color::BLACK);
        }
        let list = builder.build();

        let stats = renderer.render(
            FrameTarget {
                view: target.view(),
                width: 128,
                height: 128,
                scale_factor: 1.0,
            },
            &list,
        );

        assert_eq!(stats.instances, 4096);
        // The last instance is at logical (126, 126) and must have been drawn.
        target
            .read_pixels(&gpu)
            .assert_pixel(127, 127, Color::BLACK, TOLERANCE);
    });
}
