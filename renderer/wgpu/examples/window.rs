//! M1, in a real window: `cargo run -p crisol-render-wgpu --example window`
//!
//! Opens a window, clears it, and draws the primitives the renderer knows about at M1 —
//! solid fills, rounded corners, per-edge borders, a clip group and a textured quad.
//! Resizing and dragging between displays of different densities should change nothing but
//! the pixel count: the whole scene is authored in logical pixels.
//!
//! The automated half of M1's acceptance lives in `tests/render.rs`, which renders the same
//! kinds of list offscreen and asserts on pixels. This example is for the half a human has
//! to look at.

use std::sync::Arc;

use crisol_display_list::{
    Color, Corners, DisplayListBuilder, Edges, Edges4, ImageCommand, ImageId, Rect, RectCommand,
    Size,
};
use crisol_render_wgpu::testing::checkerboard;
use crisol_render_wgpu::{AcquiredFrame, FrameTarget, Renderer, WindowSurface};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const CHECKERBOARD: ImageId = ImageId(1);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    // Wait rather than poll: this scene is static, so redrawing continuously would burn a
    // core for nothing. A real application drives this from the animation scheduler.
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut App::default())?;
    Ok(())
}

#[derive(Default)]
struct App {
    state: Option<State>,
}

struct State {
    surface: WindowSurface,
    renderer: Renderer,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `resumed` fires again after an Android-style suspend, so recreating state that
        // already exists would leak a window.
        if self.state.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("Crisol — M1")
            .with_inner_size(winit::dpi::LogicalSize::new(640.0, 400.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("could not create a window"),
        );

        let surface = WindowSurface::new(window).expect("could not create a surface");
        let mut renderer = Renderer::new(surface.gpu(), surface.format());
        renderer.upload_image(
            CHECKERBOARD,
            8,
            8,
            &checkerboard(8, 8, 1, [40, 44, 52, 255], [170, 178, 189, 255]),
        );

        let info = surface.gpu().adapter.get_info();
        println!(
            "crisol: {} via {:?} at {}x{} ({}x scale)",
            info.name,
            info.backend,
            surface.width(),
            surface.height(),
            surface.scale_factor(),
        );

        self.state = Some(State { surface, renderer });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.surface.resize(size.width, size.height);
                state.surface.window().request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // The window's inner size changes with the scale factor; re-reading it is
                // simpler and more reliable than tracking the two separately.
                state.surface.refresh();
                state.surface.window().request_redraw();
            }
            WindowEvent::RedrawRequested => state.draw(),
            _ => {}
        }
    }
}

impl State {
    fn draw(&mut self) {
        let AcquiredFrame::Frame(frame) = self.surface.acquire() else {
            return;
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let list = scene(self.surface.logical_size());
        self.renderer.render(
            FrameTarget {
                view: &view,
                width: self.surface.width(),
                height: self.surface.height(),
                scale_factor: self.surface.scale_factor(),
            },
            &list,
        );
        self.surface.present(frame);
    }
}

/// Everything in here is in logical pixels, so the scene is identical on a 1x monitor and a
/// 2x one.
fn scene(viewport: Size) -> crisol_display_list::DisplayList {
    let mut b = DisplayListBuilder::new(viewport);
    b.set_background(Color::from_rgba8(24, 26, 31, 255));

    let margin = 24.0;
    let content = Rect::from_xywh(
        margin,
        margin,
        (viewport.width - margin * 2.0).max(0.0),
        (viewport.height - margin * 2.0).max(0.0),
    );

    // Card.
    b.push_rect(RectCommand {
        rect: content,
        radii: Corners::all(12.0),
        fill: Color::from_rgba8(40, 44, 52, 255),
        border_color: Edges4::all(Color::from_rgba8(80, 88, 100, 255)),
        border_width: Edges::all(1.0),
    });

    // A row of swatches with increasing corner radii, to make rounding visible.
    let swatch = 48.0;
    for i in 0..5 {
        let x = content.min_x() + 16.0 + i as f32 * (swatch + 12.0);
        b.push_rect(RectCommand {
            rect: Rect::from_xywh(x, content.min_y() + 16.0, swatch, swatch),
            radii: Corners::all(i as f32 * 6.0),
            fill: Color::from_rgba8(97, 175, 239, 255),
            border_color: Edges4::all(Color::TRANSPARENT),
            border_width: Edges::ZERO,
        });
    }

    // A clip group: the stripe is wider than its clip and must stop at the boundary.
    let clip = Rect::from_xywh(content.min_x() + 16.0, content.min_y() + 84.0, 160.0, 64.0);
    b.push_clip(clip);
    b.push_rect(RectCommand {
        rect: clip.inflate(40.0),
        radii: Corners::ZERO,
        fill: Color::from_rgba8(224, 108, 117, 255),
        border_color: Edges4::all(Color::TRANSPARENT),
        border_width: Edges::ZERO,
    });
    b.pop_clip();

    // A textured quad with rounded corners, and a translucent scrim over its lower half to
    // exercise premultiplied blending.
    let image = Rect::from_xywh(content.min_x() + 192.0, content.min_y() + 84.0, 128.0, 64.0);
    b.push_image(ImageCommand {
        radii: Corners::all(8.0),
        ..ImageCommand::new(image, CHECKERBOARD)
    });
    b.fill_rect(
        Rect::from_xywh(
            image.min_x(),
            image.center().y,
            image.width(),
            image.height() / 2.0,
        ),
        Color::BLACK.with_alpha(0.5),
    );

    b.build()
}
