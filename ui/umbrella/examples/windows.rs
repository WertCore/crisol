//! Two windows, one device: `cargo run -p crisol-ui --example windows`
//!
//! What this is for. DECISIONS D-43 justified making the reactive `Runtime` an ordinary value
//! rather than an ambient thread-local on the grounds that *two windows means two runtimes*.
//! That was an argument made before there were two windows. This is the second window.
//!
//! Each window owns its tree, its runtime and its signals: `space` increments the counter in
//! the focused window and leaves the other alone. If the runtime were ambient, the two would
//! be reading each other's state and the design would be wrong in a way no single-window test
//! could show.
//!
//! What is shared is everything that is per *application* rather than per window: the GPU
//! device, the font database, the style engine's interner, and a renderer per surface format.
//! A second adapter and logical device is about 17 MB of footprint — most of what an idle GPU
//! application costs — and duplicating it per window would spend the memory budget on window
//! count.

use std::collections::HashMap;
use std::sync::Arc;

use crisol_ui::css::stylesheet::Stylesheet;
use crisol_ui::display_list::Color;
use crisol_ui::dom::Dom;
use crisol_ui::layout::{LayoutCache, LayoutContext, ShapedText};
use crisol_ui::paint::{PaintOptions, paint};
use crisol_ui::reactive::{
    Cx, Runtime, Signal, append, bind_text, element_with_class, mount, text,
};
use crisol_ui::render::{AcquiredFrame, FrameTarget, Renderer, SharedGpu, WindowSurface};
use crisol_ui::style::{StyleEngine, StyleMap};
use crisol_ui::text::FontSystem;
use crisol_ui::tree::Tree;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const CSS: &str = "
    body { display: flex; flex-direction: column; padding-top: 24px; padding-left: 24px;
           background-color: rgb(18, 20, 26); color: rgb(226, 232, 240);
           font-size: 16px; line-height: 24px }
    h1   { display: block; font-size: 26px; line-height: 40px; color: rgb(97, 175, 239) }
    .count { display: block; font-size: 40px; line-height: 56px; color: rgb(152, 195, 121) }
    .hint  { display: block; font-size: 12px; line-height: 20px; color: rgb(106, 115, 125) }
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut Shell::default())?;
    Ok(())
}

/// One window: its own tree, its own runtime, its own state.
struct Document {
    surface: WindowSurface,
    tree: Tree,
    runtime: Runtime,
    count: Signal<i32>,
    styles: StyleMap,
    cache: LayoutCache,
}

impl Document {
    fn new(surface: WindowSurface, title: &str) -> Self {
        let mut tree = Tree::new();
        let runtime = Runtime::new();
        let count = runtime.signal(0);
        {
            let mut dom = Dom::new(&mut tree);
            let root = dom.create_element("html");
            dom.set_root(root);
            let body = dom.create_element("body");
            dom.append_child(root, body).unwrap();

            let mut cx = Cx::new(&runtime, &mut dom);
            let title = title.to_owned();
            mount(&mut cx, body, move |cx: &mut Cx<'_, '_>| {
                let heading = element_with_class(cx, "h1", "title");
                let label = text(cx, &title);
                append(cx, heading, label);
                heading
            });
            mount(&mut cx, body, move |cx: &mut Cx<'_, '_>| {
                let node = element_with_class(cx, "p", "count");
                let value = text(cx, "");
                append(cx, node, value);
                bind_text(cx, value, move |track| track.get(count).to_string());
                node
            });
            mount(&mut cx, body, |cx: &mut Cx<'_, '_>| {
                let node = element_with_class(cx, "p", "hint");
                let label = text(cx, "space counts here · n opens another · esc quits");
                append(cx, node, label);
                node
            });
            runtime.flush(&mut dom);
        }
        Self {
            surface,
            tree,
            runtime,
            count,
            styles: StyleMap::default(),
            cache: LayoutCache::new(),
        }
    }

    /// Increments *this* window's counter. The other window does not move.
    fn bump(&mut self) {
        let now = self.runtime.peek(self.count).unwrap_or_default();
        self.runtime.set(self.count, now + 1);
        let mut dom = Dom::new(&mut self.tree);
        self.runtime.flush(&mut dom);
        println!(
            "{}: {}",
            self.surface.window().title(),
            self.runtime.peek(self.count).unwrap_or_default()
        );
    }

    fn draw(&mut self, engine: &mut StyleEngine, fonts: &mut FontSystem, renderer: &mut Renderer) {
        let AcquiredFrame::Frame(frame) = self.surface.acquire() else {
            return;
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let viewport = self.surface.logical_size();

        self.styles = engine.restyle_incremental(&mut self.tree, &self.styles).0;
        {
            let mut context =
                LayoutContext::new(&mut self.tree, &self.styles, fonts, &mut self.cache);
            context.run(viewport);
        }
        let list = paint(
            &self.tree,
            &PaintOptions::new(viewport).with_background(Color::rgb(0.07, 0.08, 0.10)),
        );
        renderer.render_text(
            FrameTarget {
                view: &view,
                width: self.surface.width(),
                height: self.surface.height(),
                scale_factor: self.surface.scale_factor(),
                damage: None,
            },
            &list,
            fonts,
            &ShapedText(self.cache.text()),
        );
        self.surface.present(frame);
    }
}

#[derive(Default)]
struct Shell {
    /// The one device every window presents with.
    gpu: Option<SharedGpu>,
    /// One renderer per surface format, not per window: two windows on the same display share
    /// a format, and with it the glyph atlas and the pipelines.
    renderers: HashMap<wgpu::TextureFormat, Renderer>,
    /// The font database, which is per application. Scanning it twice would be the second
    /// most expensive thing a second window could do.
    fonts: Option<FontSystem>,
    /// Shared so that two windows computing the same style share one allocation (D-21).
    engine: Option<StyleEngine>,
    windows: HashMap<WindowId, Document>,
    opened: usize,
}

impl Shell {
    fn open(&mut self, event_loop: &ActiveEventLoop) {
        self.opened += 1;
        let title = format!("crisol — window {}", self.opened);
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title(&title)
                        .with_inner_size(winit::dpi::LogicalSize::new(420.0, 260.0)),
                )
                .expect("could not create a window"),
        );

        // The first window brings the device into existence; every one after it borrows it.
        let surface = match self.gpu.clone() {
            Some(gpu) => WindowSurface::with_gpu(gpu, window),
            None => WindowSurface::new(window),
        }
        .expect("could not create a surface");
        match &self.gpu {
            None => {
                self.gpu = Some(surface.shared_gpu());
                println!("window 1 created the device");
            }
            Some(existing) => {
                // The claim of D-48, checked rather than asserted in prose: the same
                // allocation, not merely an equivalent one. Memory sampling is too noisy to
                // establish this — a footprint that moves with whether a redraw happened to
                // land before the sample cannot tell a shared device from a duplicated one.
                assert!(
                    SharedGpu::ptr_eq(existing, &surface.shared_gpu()),
                    "a later window built its own device"
                );
                println!(
                    "window {} shares window 1's device (one Arc, {} holders)",
                    self.opened,
                    SharedGpu::strong_count(existing)
                );
            }
        }

        let format = surface.format();
        self.renderers
            .entry(format)
            .or_insert_with(|| Renderer::new(surface.gpu(), format));

        let id = surface.window().id();
        self.windows.insert(id, Document::new(surface, &title));
        println!(
            "{} window(s), 1 device, {} renderer(s)",
            self.windows.len(),
            self.renderers.len()
        );
    }
}

impl ApplicationHandler for Shell {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        self.fonts.get_or_insert_with(FontSystem::new);
        self.engine.get_or_insert_with(|| {
            let mut engine = StyleEngine::new();
            engine.add_stylesheet(Stylesheet::parse(CSS).expect("the example's stylesheet"));
            engine
        });
        self.open(event_loop);
        self.open(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.windows.remove(&id);
                if self.windows.is_empty() {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(document) = self.windows.get_mut(&id) {
                    document.surface.resize(size.width, size.height);
                    document.surface.window().request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(document) = self.windows.get_mut(&id) {
                    document.surface.refresh();
                    document.surface.window().request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character("n") => self.open(event_loop),
                    Key::Named(NamedKey::Space) => {
                        if let Some(document) = self.windows.get_mut(&id) {
                            document.bump();
                            document.surface.window().request_redraw();
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                let (Some(engine), Some(fonts)) = (self.engine.as_mut(), self.fonts.as_mut())
                else {
                    return;
                };
                let Some(document) = self.windows.get_mut(&id) else {
                    return;
                };
                let Some(renderer) = self.renderers.get_mut(&document.surface.format()) else {
                    return;
                };
                document.draw(engine, fonts, renderer);
            }
            _ => {}
        }
    }
}
