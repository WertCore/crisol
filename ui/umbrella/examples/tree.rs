//! The whole Track A pipeline as it stands at M2, in a window:
//! `cargo run -p crisol-ui --example tree`
//!
//! Builds a node tree by hand, paints it to a display list, and rasterises that — the
//! `tree -> paint -> display-list -> render` path, with nothing above it yet. Press space to
//! remove a node and watch the next frame reflect it; `r` puts it back.
//!
//! Layout positions are hand-written here because CSS and `taffy` do not arrive until M3.

use std::sync::Arc;

use crisol_ui::display_list::{Color, Corners, DisplayList, Edges, Edges4, Rect, Size};
use crisol_ui::paint::{PaintOptions, paint};
use crisol_ui::render::{AcquiredFrame, FrameTarget, Renderer, WindowSurface};
use crisol_ui::tree::{BoxStyle, ColorBox, NodeId, Tree};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{WindowAttributes, WindowId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(App::default())?;
    Ok(())
}

#[derive(Default)]
struct App {
    state: Option<State>,
}

struct State {
    surface: WindowSurface,
    renderer: Renderer,
    document: Document,
}

impl ApplicationHandler for App {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window = Arc::from(
            event_loop
                .create_window(
                    WindowAttributes::default()
                        .with_title("Crisol — M2: tree, paint, display list")
                        .with_surface_size(winit::dpi::LogicalSize::new(480.0, 360.0)),
                )
                .expect("could not create a window"),
        );
        let surface = WindowSurface::new(window).expect("could not create a surface");
        let renderer = Renderer::new(surface.gpu(), surface.format());
        self.state = Some(State {
            surface,
            renderer,
            document: Document::new(),
        });
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::SurfaceResized(size) => {
                state.surface.resize(size.width, size.height);
                state.surface.window().request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                state.surface.refresh();
                state.surface.window().request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key.as_ref() {
                    Key::Character(" ") => state.document.remove_a_card(),
                    Key::Character("r") => state.document.rebuild(),
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    _ => return,
                }
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

        let list = self.document.paint(self.surface.logical_size());
        self.renderer.render(
            FrameTarget {
                view: &view,
                width: self.surface.width(),
                height: self.surface.height(),
                scale_factor: self.surface.scale_factor(),
                damage: None,
            },
            &list,
        );
        self.surface.present(frame);
    }
}

const CARD_COUNT: usize = 4;
const SURFACE: Color = Color::rgb(0.094, 0.102, 0.122);
const PANEL: Color = Color::rgb(0.157, 0.173, 0.204);
const ACCENT: Color = Color::rgb(0.380, 0.686, 0.937);
const EDGE: Color = Color::rgb(0.314, 0.345, 0.392);

/// A hand-built document: a panel that clips a row of cards, each with a custom-node badge.
struct Document {
    tree: Tree,
    cards: Vec<NodeId>,
}

impl Document {
    fn new() -> Self {
        let mut document = Self {
            tree: Tree::new(),
            cards: Vec::new(),
        };
        document.rebuild();
        document
    }

    fn rebuild(&mut self) {
        self.tree = Tree::new();
        self.cards.clear();

        let root = self.tree.create_element("body");
        self.tree.set_root(root).unwrap();
        self.tree.node_mut(root).layout = Rect::from_xywh(0.0, 0.0, 480.0, 360.0);

        // A panel with `overflow: hidden`, so the last card is visibly cut off.
        let panel = self.tree.create_element("div");
        self.tree.append_child(root, panel).unwrap();
        self.tree.node_mut(panel).layout = Rect::from_xywh(24.0, 24.0, 300.0, 200.0);
        self.tree.node_mut(panel).style = BoxStyle {
            background: PANEL,
            border_color: Edges4::all(EDGE),
            border_width: Edges::all(1.0),
            radii: Corners::all(10.0),
            text_color: Color::WHITE,
            clips_children: true,
            ..BoxStyle::default()
        };

        for i in 0..CARD_COUNT {
            let card = self.tree.create_element("div");
            self.tree.append_child(panel, card).unwrap();
            self.tree.node_mut(card).layout =
                Rect::from_xywh(16.0, 16.0 + i as f32 * 52.0, 340.0, 40.0);
            self.tree.node_mut(card).style = BoxStyle {
                background: SURFACE,
                radii: Corners::all(6.0),
                ..BoxStyle::default()
            };

            // A custom node: it decides its own size and paints itself (DECISIONS D-06).
            let badge = self
                .tree
                .create_custom("canvas", ColorBox::new(Size::new(24.0, 24.0), ACCENT));
            self.tree.append_child(card, badge).unwrap();
            self.tree.node_mut(badge).layout = Rect::from_xywh(8.0, 8.0, 24.0, 24.0);

            self.cards.push(card);
        }
    }

    fn remove_a_card(&mut self) {
        if let Some(card) = self.cards.pop() {
            let freed = self.tree.remove_subtree(card);
            println!(
                "removed a card: {freed} nodes freed, {} live",
                self.tree.len()
            );
        }
    }

    fn paint(&self, viewport: Size) -> DisplayList {
        paint(
            &self.tree,
            &PaintOptions::new(viewport).with_background(Color::rgb(0.055, 0.063, 0.078)),
        )
    }
}
