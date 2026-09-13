//! M8's acceptance: an API-client-shaped application with a 5 MB response in it.
//!
//! `cargo run -p crisol-ui --example apiclient --features measure -- --headless`
//!
//! ROADMAP §M8 accepts on "a real API-client-shaped application built entirely in Rust,
//! measured at < 60MB RSS idle with a 5MB JSON response loaded", and §7's first kill
//! criterion *is* that acceptance rather than a footnote to it. So the number this prints is
//! the product claim, and it is printed whatever it turns out to be.
//!
//! **What it builds** is the shape an API client has: a sidebar of saved requests, a URL bar,
//! a status line, and a response pane holding the response the selected request returned.
//! Selecting a different request swaps the response and puts the pane back at the top, which
//! is the interaction that makes this an application rather than a layout.
//!
//! **The response pane is virtualised** (D-47), which is not an optimisation here but the
//! only design that fits: one element per line is 3,678 MiB for this response, against a
//! 60 MB budget. Only the lines in view become nodes, and two spacers stand in for the rest
//! so the scrollbar is the size it would be if they had all been built.
//!
//! **The body is held once**, as one `String` with an index of line starts, rather than as a
//! `Vec<String>` of 240,000 lines. That is what a real viewer does and it matters at this
//! size: the per-`String` overhead alone would be several MB of the budget, spent on nothing
//! the user can see.
//!
//! **Headless**, like `bigresponse` and for the same reason: no window and no GPU device, so
//! the reading is the engine's and the application's rather than the driver's. CI runs it on
//! macOS, Linux and Windows, which is what makes the three numbers comparable.

use std::fmt::Write as _;

use crisol_ui::css::stylesheet::Stylesheet;
use crisol_ui::display_list::{Point, Size};
use crisol_ui::dom::Dom;
use crisol_ui::events::scroll_from;
use crisol_ui::layout::{LayoutCache, LayoutContext};
use crisol_ui::style::{StyleEngine, StyleMap};
use crisol_ui::text::FontSystem;
use crisol_ui::tree::{NodeId, Tree};

/// Must agree with `line-height` below: the spacers are sized in multiples of it, so a
/// disagreement makes the scrollbar the wrong length.
const LINE_HEIGHT: f32 = 18.0;

/// Rows built beyond the ones strictly in view, so a scroll does not show blank rows before
/// the next build catches up.
const OVERSCAN: usize = 10;

/// The window an API client might have.
const VIEWPORT: Size = Size {
    width: 1100.0,
    height: 760.0,
};

/// Height of everything above the response pane: the URL bar and the status line.
const CHROME_HEIGHT: f32 = 96.0;

const CSS: &str = "
    body     { display: block; font-size: 13px; line-height: 18px }
    .app     { display: flex }
    .sidebar { display: block; width: 240px }
    .brand   { display: block; font-size: 15px }
    .request { display: block; height: 34px }
    .request.selected { display: block }
    .method  { display: inline }
    .main    { display: block; width: 860px }
    .urlbar  { display: block; height: 56px }
    .status  { display: block; height: 40px }
    .pane    { display: block; overflow: scroll; width: 860px; height: 664px }
    .line    { display: block; white-space: pre }
    .spacer  { display: block }
";

/// A saved request, as a sidebar lists them.
struct Saved {
    method: &'static str,
    name: &'static str,
    url: &'static str,
    /// How big a response this one returns. Only the selected one is ever built.
    bytes: usize,
}

/// The sidebar's contents. The first is the one the acceptance is about.
const SAVED: &[Saved] = &[
    Saved {
        method: "GET",
        name: "List results",
        url: "https://api.example.com/v1/results?limit=50000",
        bytes: 5 * 1024 * 1024,
    },
    Saved {
        method: "GET",
        name: "Get result",
        url: "https://api.example.com/v1/results/1",
        bytes: 4 * 1024,
    },
    Saved {
        method: "POST",
        name: "Create result",
        url: "https://api.example.com/v1/results",
        bytes: 2 * 1024,
    },
    Saved {
        method: "DELETE",
        name: "Delete result",
        url: "https://api.example.com/v1/results/1",
        bytes: 512,
    },
];

/// A response body, held once and indexed by line.
///
/// One `String` and a `Vec<u32>` of offsets rather than a `Vec<String>`: at 240,000 lines the
/// per-`String` header alone is ~5.8 MB, which is a tenth of the whole budget spent on
/// bookkeeping. `u32` because a response that needs more than 4 GB of offsets is not one this
/// pane is going to show.
struct Body {
    text: String,
    starts: Vec<u32>,
}

impl Body {
    /// Indexes `text` by line start.
    fn new(text: String) -> Self {
        let mut starts = vec![0_u32];
        for (at, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                let next = at + 1;
                if next < text.len() {
                    starts.push(u32::try_from(next).expect("a response under 4 GB"));
                }
            }
        }
        Self { text, starts }
    }

    /// How many lines there are.
    fn lines(&self) -> usize {
        self.starts.len()
    }

    /// Line `index`, without its newline.
    fn line(&self, index: usize) -> &str {
        let from = self.starts[index] as usize;
        let to = self
            .starts
            .get(index + 1)
            .map_or(self.text.len(), |next| *next as usize);
        self.text[from..to].trim_end_matches('\n')
    }

    /// What the status line reports.
    fn bytes(&self) -> usize {
        self.text.len()
    }
}

/// The response pane: the nodes that exist, and which slice of the body they are showing.
struct Pane {
    node: NodeId,
    above: NodeId,
    below: NodeId,
    rows: Vec<NodeId>,
    /// Index of the line the first row is currently showing.
    first: usize,
}

impl Pane {
    /// Builds the pane's nodes. The row count never changes after this; only their text does,
    /// which is the whole point — scrolling must not allocate.
    fn build(dom: &mut Dom<'_>, parent: NodeId, rows: usize) -> Self {
        let node = dom.create_element("div");
        dom.set_attribute(node, "class", "pane");
        dom.append_child(parent, node).expect("pane under main");

        let above = spacer(dom, node);
        let mut built = Vec::with_capacity(rows);
        for _ in 0..rows {
            let line = dom.create_element("div");
            dom.set_attribute(line, "class", "line");
            let text = dom.create_text("");
            dom.append_child(line, text).expect("text under line");
            dom.append_child(node, line).expect("line under pane");
            built.push(text);
        }
        let below = spacer(dom, node);

        Self {
            node,
            above,
            below,
            rows: built,
            first: 0,
        }
    }

    /// Points the window at line `first`, writing the spacers and the rows' text.
    ///
    /// Writes two `style` attributes and the rows' text, and reparses no stylesheet — the
    /// spacer heights are inline styles (D-50) because both change on every scroll frame and
    /// a stylesheet cannot carry something that moves that often.
    fn show(&mut self, dom: &mut Dom<'_>, body: &Body, first: usize) {
        let first = first.min(body.lines().saturating_sub(self.rows.len()));
        self.first = first;

        set_height(dom, self.above, first as f32 * LINE_HEIGHT);
        let after = body.lines().saturating_sub(first + self.rows.len());
        set_height(dom, self.below, after as f32 * LINE_HEIGHT);

        for (offset, row) in self.rows.iter().enumerate() {
            let index = first + offset;
            if index < body.lines() {
                dom.set_text(*row, body.line(index));
            } else {
                dom.set_text(*row, "");
            }
        }
    }
}

/// A spacer: no content, a height, and nothing else.
fn spacer(dom: &mut Dom<'_>, parent: NodeId) -> NodeId {
    let node = dom.create_element("div");
    dom.set_attribute(node, "class", "spacer");
    dom.append_child(parent, node).expect("spacer under pane");
    node
}

/// Sets a node's height as an inline style.
fn set_height(dom: &mut Dom<'_>, node: NodeId, height: f32) {
    let mut style = String::with_capacity(24);
    let _ = write!(style, "height: {height}px");
    dom.set_attribute(node, "style", &style);
}

/// The whole application: its tree, its styling, and the response it is showing.
struct App {
    tree: Tree,
    engine: StyleEngine,
    styles: StyleMap,
    fonts: FontSystem,
    cache: LayoutCache,
    pane: Pane,
    body: Body,
    /// The sidebar rows, so selecting one can move the class that marks it.
    requests: Vec<NodeId>,
    selected: usize,
    url: NodeId,
    method: NodeId,
    status: NodeId,
}

impl App {
    /// Builds the application with the first request selected and its response loaded.
    fn new() -> Self {
        let mut tree = Tree::new();
        let rows = (VIEWPORT.height - CHROME_HEIGHT) / LINE_HEIGHT;
        let rows = rows.ceil() as usize + OVERSCAN;

        let mut requests = Vec::with_capacity(SAVED.len());
        let (pane, url, method, status);
        {
            let mut dom = Dom::new(&mut tree);
            let root = dom.create_element("html");
            dom.set_root(root);
            let body_element = dom.create_element("body");
            dom.append_child(root, body_element).expect("body");

            let app = dom.create_element("div");
            dom.set_attribute(app, "class", "app");
            dom.append_child(body_element, app).expect("app under body");

            // ---- the sidebar ----
            let sidebar = dom.create_element("div");
            dom.set_attribute(sidebar, "class", "sidebar");
            dom.append_child(app, sidebar).expect("sidebar under app");
            let brand = dom.create_element("div");
            dom.set_attribute(brand, "class", "brand");
            let brand_text = dom.create_text("crisol");
            dom.append_child(brand, brand_text).expect("brand text");
            dom.append_child(sidebar, brand)
                .expect("brand under sidebar");

            for (index, saved) in SAVED.iter().enumerate() {
                let request = dom.create_element("div");
                dom.set_attribute(
                    request,
                    "class",
                    if index == 0 {
                        "request selected"
                    } else {
                        "request"
                    },
                );
                let label = dom.create_text(&format!("{}  {}", saved.method, saved.name));
                dom.append_child(request, label).expect("request label");
                dom.append_child(sidebar, request).expect("request row");
                requests.push(request);
            }

            // ---- the main column ----
            let main = dom.create_element("div");
            dom.set_attribute(main, "class", "main");
            dom.append_child(app, main).expect("main under app");

            let urlbar = dom.create_element("div");
            dom.set_attribute(urlbar, "class", "urlbar");
            dom.append_child(main, urlbar).expect("urlbar under main");
            let method_element = dom.create_element("span");
            dom.set_attribute(method_element, "class", "method");
            method = dom.create_text(SAVED[0].method);
            dom.append_child(method_element, method).expect("method");
            dom.append_child(urlbar, method_element).expect("method el");
            url = dom.create_text(SAVED[0].url);
            dom.append_child(urlbar, url).expect("url under urlbar");

            let status_element = dom.create_element("div");
            dom.set_attribute(status_element, "class", "status");
            dom.append_child(main, status_element).expect("status");
            status = dom.create_text("");
            dom.append_child(status_element, status)
                .expect("status text");

            pane = Pane::build(&mut dom, main, rows);
        }

        let mut engine = StyleEngine::new();
        engine.add_stylesheet(Stylesheet::parse(CSS).expect("the application's stylesheet"));

        let mut app = Self {
            tree,
            engine,
            styles: StyleMap::default(),
            fonts: FontSystem::new(),
            cache: LayoutCache::new(),
            pane,
            body: Body::new(String::new()),
            requests,
            selected: 0,
            url,
            method,
            status,
        };
        app.select(0);
        app
    }

    /// Selects a saved request, loads its response, and puts the pane back at the top.
    fn select(&mut self, index: usize) {
        let saved = &SAVED[index];
        self.body = Body::new(pretty_json(saved.bytes));

        let previous = self.selected;
        self.selected = index;
        let (rows, body) = (&self.requests, &self.body);
        let summary = format!(
            "200 OK  ·  {:.1} MB  ·  128 ms  ·  {} lines",
            body.bytes() as f64 / (1024.0 * 1024.0),
            body.lines(),
        );
        {
            let mut dom = Dom::new(&mut self.tree);
            dom.set_attribute(rows[previous], "class", "request");
            dom.set_attribute(rows[index], "class", "request selected");
            dom.set_text(self.method, saved.method);
            dom.set_text(self.url, saved.url);
            dom.set_text(self.status, &summary);
            self.pane.show(&mut dom, body, 0);
        }
        self.tree.set_scroll(self.pane.node, Point::ZERO);
    }

    /// Restyles and lays out, returning how many nodes layout touched.
    fn frame(&mut self) -> usize {
        self.styles = self
            .engine
            .restyle_incremental(&mut self.tree, &self.styles)
            .0;
        let mut context = LayoutContext::new(
            &mut self.tree,
            &self.styles,
            &mut self.fonts,
            &mut self.cache,
        );
        context.run(VIEWPORT);
        context.stats().nodes_laid_out
    }

    /// Scrolls the pane by `by` pixels and rebuilds the window it shows.
    ///
    /// Returns false when the pane had nowhere left to go, which is how a caller knows to
    /// hand the scroll to whatever is underneath.
    fn scroll(&mut self, by: f32) -> bool {
        if scroll_from(&mut self.tree, self.pane.node, Point::new(0.0, by)).is_none() {
            return false;
        }
        let offset = self.tree.scroll_offset(self.pane.node).y;
        let first = (offset / LINE_HEIGHT).floor().max(0.0) as usize;
        if first != self.pane.first {
            let body = &self.body;
            let mut dom = Dom::new(&mut self.tree);
            self.pane.show(&mut dom, body, first);
        }
        true
    }
}

/// The response body, generated rather than fetched: the acceptance is about what holding one
/// costs, and a network call would make the number depend on a server.
fn pretty_json(target: usize) -> String {
    let mut out = String::with_capacity(target + 1024);
    out.push_str("{\n  \"results\": [\n");
    let mut index = 0_u32;
    while out.len() < target {
        let _ = write!(
            out,
            "    {{\n      \"id\": {index},\n      \"name\": \"item {index}\",\n      \
             \"email\": \"user{index}@example.com\",\n      \"active\": {},\n      \
             \"score\": {}.{},\n      \"tags\": [\"alpha\", \"beta\"]\n    }},\n",
            !index.is_multiple_of(3),
            index % 100,
            index % 10,
        );
        index += 1;
    }
    out.push_str("  ]\n}\n");
    out
}

fn main() {
    // Only one mode for now. The argument is required rather than assumed so that adding a
    // windowed mode later does not silently change what this command means.
    if !std::env::args().any(|argument| argument == "--headless") {
        eprintln!("usage: apiclient --headless");
        eprintln!();
        eprintln!("  Measures M8's acceptance: an API-client-shaped application with a 5 MB");
        eprintln!("  response loaded. Headless so the reading is the engine's and the");
        eprintln!("  application's rather than the GPU driver's.");
        std::process::exit(2);
    }
    headless();
}

fn headless() {
    let mut app = App::new();
    let laid_out = app.frame();

    println!("\nan API client with a {:.1} MB response loaded\n", {
        app.body.bytes() as f64 / (1024.0 * 1024.0)
    });
    println!("  {} lines in the response", app.body.lines());
    println!("  {} nodes in the tree", app.tree.len());
    println!("  {laid_out} nodes laid out for the first frame");

    // ---- the pane has to actually be a pane ----
    assert!(
        app.tree.is_scrollable(app.pane.node),
        "the response pane must be a scroll container, or none of this means anything"
    );

    // The scrollbar must be the size it would be if every line had been built. A pane that is
    // cheap because it scrolls to the wrong place is not a pane, so this is checked rather
    // than assumed.
    let want = app.body.lines() as f32 * LINE_HEIGHT - (VIEWPORT.height - CHROME_HEIGHT);
    let extent = app.tree.scroll_max(app.pane.node).height;
    println!("  scrollbar spans {extent:.0} px against the {want:.0} px it should",);
    assert!(
        (extent - want).abs() < LINE_HEIGHT,
        "the spacers do not stand in for the lines that were not built: {extent} against {want}"
    );

    // ---- scrolling it moves the window, and costs a frame ----
    let started = std::time::Instant::now();
    assert!(app.scroll(LINE_HEIGHT * 50.0), "the pane should move");
    let laid_out = app.frame();
    let scroll_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(app.pane.first, 50, "the window follows the offset");
    assert!(
        app.pane
            .rows
            .first()
            .and_then(|row| app.tree.get(*row))
            .is_some(),
        "the rows survive a scroll rather than being rebuilt"
    );
    println!("  scrolling costs {scroll_ms:.2} ms and relaid {laid_out} nodes");

    // ---- selecting another request swaps the response ----
    let before = app.tree.len();
    app.select(1);
    app.frame();
    assert_eq!(
        app.tree.len(),
        before,
        "swapping the response must not change the node count: that is what virtualised means"
    );
    assert_eq!(app.pane.first, 0, "a new response starts at the top");
    assert_eq!(app.tree.scroll_offset(app.pane.node).y, 0.0);

    // Back to the response the acceptance is about, and leave it there: the reading below is
    // taken with the 5 MB response loaded, which is what §M8 asks for.
    app.select(0);
    app.frame();
    println!(
        "  selecting another request and back leaves {} nodes",
        app.tree.len()
    );

    // ---- the number ----
    //
    // Taken here, with everything built, styled, laid out and nothing in flight. `app` is
    // deliberately still alive: dropping it first would measure the absence of the thing
    // being measured.
    #[cfg(feature = "measure")]
    {
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        match crisol_ui::measure::current() {
            Some(reading) => {
                println!("\n  ACCEPTANCE: {reading}, {profile}, against a 60 MB budget");
                let mb = reading.bytes as f64 / (1024.0 * 1024.0);
                println!(
                    "  {}",
                    if mb < 60.0 {
                        "under budget"
                    } else {
                        "OVER BUDGET — §7's first kill criterion says re-evaluate"
                    }
                );
            }
            None => println!("\n  unmeasured on this platform"),
        }
    }
    println!();
    drop(app);
}
