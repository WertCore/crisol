//! Text reaching the framebuffer: HTML in, glyph pixels out.
//!
//! This is the first test in the project that runs the whole Track A pipeline —
//! parse, cascade, layout, shape, paint, rasterise — and looks at the result.

use crisol_display_list::{Color, DrawCommand, Size};
use crisol_layout::{ShapedText, layout};
use crisol_paint::{PaintOptions, paint};
use crisol_render_wgpu::testing::with_gpu;
use crisol_render_wgpu::{FrameTarget, Gpu, HEADLESS_FORMAT, HeadlessTarget, Pixels, Renderer};
use crisol_style::StyleEngine;
use crisol_text::FontSystem;

const VIEWPORT: Size = Size {
    width: 200.0,
    height: 60.0,
};

/// Runs the whole pipeline and returns the pixels.
fn render(gpu: &Gpu, html: &str, css: &str) -> Option<Pixels> {
    let mut fonts = FontSystem::new();
    if fonts.is_empty() {
        let required = std::env::var("CRISOL_REQUIRE_FONTS").is_ok_and(|v| v == "1");
        assert!(
            !required,
            "CRISOL_REQUIRE_FONTS=1 but no fonts are installed"
        );
        eprintln!("skipping: no fonts installed");
        return None;
    }

    let mut document = crisol_html::parse(html);
    let mut engine = StyleEngine::new();
    engine.add_stylesheet(crisol_css::stylesheet::Stylesheet::parse(css).unwrap());
    let (styles, _) = engine.restyle(&document.tree);
    let (text, _) = layout(&mut document.tree, &styles, &mut fonts, VIEWPORT);

    let list = paint(
        &document.tree,
        &PaintOptions::new(VIEWPORT).with_background(Color::WHITE),
    );

    let width = VIEWPORT.width as u32;
    let height = VIEWPORT.height as u32;
    let mut renderer = Renderer::new(gpu, HEADLESS_FORMAT);
    let target = HeadlessTarget::new(gpu, width, height);
    let stats = renderer.render_text(
        FrameTarget {
            view: target.view(),
            width,
            height,
            scale_factor: 1.0,
            damage: None,
        },
        &list,
        &mut fonts,
        &ShapedText(&text),
    );
    assert_eq!(stats.missing_text, 0, "every text command should resolve");
    assert!(stats.text_runs > 0, "and at least one should draw");

    Some(target.read_pixels(gpu))
}

/// How many pixels differ from the background, which is a font-independent way of asking
/// "did any glyphs get drawn?".
fn ink(pixels: &Pixels) -> usize {
    let mut count = 0;
    for y in 0..pixels.height() {
        for x in 0..pixels.width() {
            if !pixels.matches(x, y, Color::WHITE, 8) {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn text_reaches_the_framebuffer() {
    with_gpu(|gpu| {
        let Some(pixels) = render(
            &gpu,
            "<body><p>Hello</p></body>",
            "p { font-size: 32px; color: #000000 }",
        ) else {
            return;
        };
        assert!(
            ink(&pixels) > 40,
            "32px of text should mark a good few hundred pixels, marked {}",
            ink(&pixels)
        );
    });
}

#[test]
fn an_empty_document_draws_no_ink() {
    with_gpu(|gpu| {
        let mut fonts = FontSystem::new();
        if fonts.is_empty() {
            return;
        }
        let mut document = crisol_html::parse("<body></body>");
        let mut engine = StyleEngine::new();
        let (styles, _) = engine.restyle(&document.tree);
        let (text, _) = layout(&mut document.tree, &styles, &mut fonts, VIEWPORT);
        let list = paint(
            &document.tree,
            &PaintOptions::new(VIEWPORT).with_background(Color::WHITE),
        );

        let mut renderer = Renderer::new(&gpu, HEADLESS_FORMAT);
        let target = HeadlessTarget::new(&gpu, 200, 60);
        renderer.render_text(
            FrameTarget {
                view: target.view(),
                width: 200,
                height: 60,
                scale_factor: 1.0,
                damage: None,
            },
            &list,
            &mut fonts,
            &ShapedText(&text),
        );
        assert_eq!(ink(&target.read_pixels(&gpu)), 0);
    });
}

#[test]
fn the_text_colour_comes_from_the_cascade() {
    with_gpu(|gpu| {
        let Some(red) = render(
            &gpu,
            "<body><p>Hello</p></body>",
            "p { font-size: 32px; color: #ff0000 }",
        ) else {
            return;
        };

        // Find a marked pixel and check it is reddish rather than grey.
        let mut found = false;
        for y in 0..red.height() {
            for x in 0..red.width() {
                let [r, g, b, _] = red.at(x, y);
                if r > 100 && g < 100 && b < 100 {
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "red text should produce red pixels");
    });
}

#[test]
fn text_is_clipped_by_an_overflow_hidden_ancestor() {
    with_gpu(|gpu| {
        let Some(clipped) = render(
            &gpu,
            "<body><div class=box><p>WWWWWWWWWWWWWWWWWWWW</p></div></body>",
            ".box { width: 40px; overflow: hidden } p { font-size: 24px }",
        ) else {
            return;
        };
        // Nothing past the 40px box should be marked.
        for y in 0..clipped.height() {
            for x in 60..clipped.width() {
                assert!(
                    clipped.matches(x, y, Color::WHITE, 8),
                    "pixel ({x}, {y}) is outside the clip and should be background"
                );
            }
        }
    });
}

#[test]
fn a_larger_font_marks_more_pixels() {
    with_gpu(|gpu| {
        let Some(small) = render(&gpu, "<body><p>Hello</p></body>", "p { font-size: 10px }") else {
            return;
        };
        let Some(large) = render(&gpu, "<body><p>Hello</p></body>", "p { font-size: 32px }") else {
            return;
        };
        assert!(
            ink(&large) > ink(&small),
            "32px text should mark more than 10px text: {} vs {}",
            ink(&large),
            ink(&small)
        );
    });
}

#[test]
fn paint_refers_to_text_by_the_nodes_own_handle() {
    // The link between paint and the renderer: the display list carries the node's packed
    // handle, and `ShapedText` looks the layout back up with it. If those two ever disagree
    // the text silently vanishes, so the correspondence is asserted rather than assumed.
    let mut fonts = FontSystem::empty();
    let mut document = crisol_html::parse("<body><p>hi</p></body>");
    let mut engine = StyleEngine::new();
    let (styles, _) = engine.restyle(&document.tree);
    let (text, _) = layout(&mut document.tree, &styles, &mut fonts, VIEWPORT);

    let list = paint(&document.tree, &PaintOptions::new(VIEWPORT));
    let ids: Vec<_> = list
        .commands()
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text(text) => Some(text.text),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 1, "one text node, one text command");

    use crisol_text_gpu::TextSource;
    let source = ShapedText(&text);
    assert!(
        source.get(ids[0]).is_some(),
        "the handle paint wrote must resolve to the layout shaping produced"
    );
}
