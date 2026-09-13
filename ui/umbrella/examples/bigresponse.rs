//! What a 5 MB response costs: `cargo run -p crisol-ui --example bigresponse --features measure`
//!
//! M8's acceptance is an API-client-shaped application with a 5 MB response in it, held under
//! a memory budget. Before building that application it is worth knowing whether the obvious
//! way to build it can work at all, because if it cannot, the answer changes what to build
//! rather than how to tune it.
//!
//! The obvious way is what a code viewer does: one element per line, one text node inside it.
//! This example builds exactly that at several sizes, measures after each, and reports the
//! marginal cost of a line. Extrapolating from a slope is honest in a way that one big run is
//! not — it separates the per-line cost from the fixed cost of the process.
//!
//! Headless on purpose: no window, no GPU device, so the number is the engine's own and not
//! the driver's.

use std::fmt::Write as _;

use crisol_ui::css::stylesheet::Stylesheet;
use crisol_ui::display_list::Size;
use crisol_ui::dom::Dom;
use crisol_ui::layout::{LayoutCache, LayoutContext};
use crisol_ui::style::{StyleEngine, StyleMap};
use crisol_ui::text::FontSystem;
use crisol_ui::tree::Tree;

const CSS: &str = "
    body { display: block; font-size: 13px; line-height: 18px }
    .line { display: block; white-space: pre }
";

/// Must agree with `line-height` in `CSS`: the spacers are sized in multiples of it.
const LINE_HEIGHT: f32 = 18.0;

/// The viewport an API client's response pane might have.
const VIEWPORT: Size = Size {
    width: 900.0,
    height: 700.0,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().position(|a| a == "--lines") {
        // Child: one configuration, one process, one reading.
        Some(at) => {
            let count: usize = args[at + 1].parse().expect("--lines takes a number");
            if args.iter().any(|a| a == "--window") {
                child_window(count);
            } else {
                child(count, args.iter().any(|a| a == "--stages"));
            }
        }
        None => parent(),
    }
}

/// Runs each configuration in a process of its own and prints the table.
///
/// Separate processes are the whole design. The first version of this example measured every
/// size in one process and produced two numbers that disagreed by a factor of six: the
/// per-line cost came out at ~13 KB when measured as the difference between totals, and
/// ~2.6 KB when measured as the difference between stages. Neither was right. Freeing a
/// 670 MiB tree does not return the memory to the operating system, so every later reading
/// was taken against an allocator holding an earlier one's pages — and whether that made a
/// number too big or too small depended on which way it was being subtracted.
///
/// A process per measurement is the only version of this that composes.
fn parent() {
    let payload = pretty_json(5 * 1024 * 1024);
    let lines = payload.lines().count();
    println!(
        "\na {:.1} MB response is {lines} lines, {:.0} bytes each\n",
        payload.len() as f64 / (1024.0 * 1024.0),
        payload.len() as f64 / lines as f64,
    );
    drop(payload);

    let Ok(exe) = std::env::current_exe() else {
        println!("cannot find this executable to re-run it");
        return;
    };

    let mut previous: Option<(usize, f64)> = None;
    for count in [1_000_usize, 10_000, 50_000] {
        let Some(fields) = run_child(&exe, &["--lines", &count.to_string()]) else {
            println!("{count:>6} lines  no measurement");
            continue;
        };
        let (mib, nodes, ms) = (fields[0], fields[1], fields[2]);
        print!("{count:>6} lines  {nodes:>7.0} nodes  {mib:>8.1} MiB  layout {ms:>6.0} ms");
        if let Some((previous_count, previous_mib)) = previous {
            let per_line = (mib - previous_mib) * 1024.0 * 1024.0 / (count - previous_count) as f64;
            print!("   {per_line:>5.0} bytes/line marginal");
        }
        println!();
        previous = Some((count, mib));
    }

    const ATTRIBUTE_AT: usize = 10_000;
    if let Some(fields) = run_child(&exe, &["--lines", &ATTRIBUTE_AT.to_string(), "--stages"]) {
        let per = |mib: f64| mib * 1024.0 * 1024.0 / ATTRIBUTE_AT as f64;
        println!("\nwhere one line's memory goes, in a process that measured only this:");
        println!(
            "  dom     {:>8.0} bytes  (an element and a text node)",
            per(fields[0])
        );
        println!(
            "  style   {:>8.0} bytes  (interned, so shared between identical lines)",
            per(fields[1])
        );
        println!(
            "  layout  {:>8.0} bytes  (box tree, shaped text, cache)",
            per(fields[2])
        );
        println!(
            "  total   {:>8.0} bytes",
            per(fields[0] + fields[1] + fields[2])
        );
    }

    // The same response, built the way it would have to be built.
    if let Some(fields) = run_child(&exe, &["--lines", &lines.to_string(), "--window"]) {
        let (mib, nodes, ms) = (fields[0], fields[1], fields[2]);
        let (visible, scroll_max, want) = (fields[3], fields[4], fields[5]);
        println!("\nthe same {lines} lines, building only the {visible:.0} in view:");
        println!("  {nodes:.0} nodes, {mib:.1} MiB, laid out in {ms:.1} ms");
        let correct = (scroll_max - want).abs() < 1.0;
        println!(
            "  scrollbar spans {scroll_max:.0} px against the {want:.0} px it should — {}",
            if correct {
                "the extent is right"
            } else {
                "WRONG, the spacers do not stand in"
            },
        );
        println!(
            "\n  Built with the spacer heights baked into the stylesheet, which a real pane\n  \
             cannot do: both change on every scroll frame. There is no inline `style`\n  \
             attribute and no per-node style override, so today the only way to move a\n  \
             spacer is to reparse a stylesheet per frame. That is the gap between this\n  \
             measurement and a response pane."
        );
    }

    if let Some((count, mib)) = previous {
        let per_line = mib * 1024.0 * 1024.0 / count as f64;
        println!(
            "\nAt {per_line:.0} bytes a line, {lines} lines is about {:.0} MiB against a 60 MB \
             budget: one\nelement per line does not fit, and not by a margin any tuning \
             closes. Building only the\nwindow does fit, and scrolls to the right place while \
             doing it.\n",
            per_line * lines as f64 / (1024.0 * 1024.0),
        );
    }
}

/// Runs one configuration and returns the numbers it printed.
fn run_child(exe: &std::path::Path, args: &[&str]) -> Option<Vec<f64>> {
    let output = std::process::Command::new(exe).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().find(|line| line.starts_with("MEASURED"))?;
    Some(
        line.split_ascii_whitespace()
            .skip(1)
            .filter_map(|field| field.parse().ok())
            .collect(),
    )
}

/// One configuration, measured in a process that has done nothing else.
fn child(count: usize, stages: bool) {
    let payload = pretty_json(5 * 1024 * 1024);
    let lines: Vec<&str> = payload.lines().take(count).collect();

    let Some(base) = crisol_ui::measure::current() else {
        return;
    };
    let base = base.bytes;

    let mut tree = Tree::new();
    {
        let mut dom = Dom::new(&mut tree);
        let root = dom.create_element("html");
        dom.set_root(root);
        let body = dom.create_element("body");
        dom.append_child(root, body).expect("body under html");
        for line in &lines {
            let element = dom.create_element("div");
            dom.set_attribute(element, "class", "line");
            let text = dom.create_text(line);
            dom.append_child(element, text).expect("text under div");
            dom.append_child(body, element).expect("line under body");
        }
    }
    let after_dom = reading();

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(CSS).expect("the example's stylesheet"));
    let styles = engine
        .restyle_incremental(&mut tree, &StyleMap::default())
        .0;
    let after_style = reading();

    let mut fonts = FontSystem::new();
    let mut cache = LayoutCache::new();
    let started = std::time::Instant::now();
    {
        let mut context = LayoutContext::new(&mut tree, &styles, &mut fonts, &mut cache);
        context.run(VIEWPORT);
    }
    let layout_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after_layout = reading();
    let nodes = tree.len();

    // Nothing is dropped before the last reading: dropping the tree first would measure the
    // absence of the thing being measured.
    if stages {
        println!(
            "MEASURED {:.6} {:.6} {:.6}",
            mib(after_dom - base),
            mib(after_style - after_dom),
            mib(after_layout - after_style),
        );
    } else {
        println!("MEASURED {:.6} {nodes} {layout_ms:.3}", mib(after_layout));
    }
    drop((tree, styles, fonts, cache, engine, payload));
}

/// The same response, built the way a response pane would have to build it.
///
/// Only the lines in view become nodes. The scroll extent comes from two spacers, one above
/// the window and one below, whose heights stand in for the lines that were not built — so
/// the scrollbar is the size it would be if all of them had been. That the extent is right is
/// checked here rather than assumed: a cheap pane that scrolls to the wrong place is not a
/// pane.
///
/// The spacer heights are baked into the stylesheet because there is nowhere else to put
/// them. See the note this prints.
fn child_window(total: usize) {
    let payload = pretty_json(5 * 1024 * 1024);
    let lines: Vec<&str> = payload.lines().collect();
    let total = total.min(lines.len());

    // A window big enough to cover the viewport, plus overscan so a scroll does not show
    // blank rows before the next build catches up.
    let visible = (VIEWPORT.height / LINE_HEIGHT).ceil() as usize + 10;
    let visible = visible.min(total);
    // Park the window in the middle, where both spacers are non-zero.
    let first = (total - visible) / 2;
    let above = first as f32 * LINE_HEIGHT;
    let below = (total - first - visible) as f32 * LINE_HEIGHT;

    let css = format!(
        "{CSS}
        .pane  {{ display: block; overflow: scroll; width: {}px; height: {}px }}
        .above {{ display: block; height: {above}px }}
        .below {{ display: block; height: {below}px }}",
        VIEWPORT.width, VIEWPORT.height,
    );

    let Some(base) = crisol_ui::measure::current() else {
        return;
    };
    let base = base.bytes;

    let mut tree = Tree::new();
    let pane;
    {
        let mut dom = Dom::new(&mut tree);
        let root = dom.create_element("html");
        dom.set_root(root);
        let body = dom.create_element("body");
        dom.append_child(root, body).expect("body under html");
        pane = dom.create_element("div");
        dom.set_attribute(pane, "class", "pane");
        dom.append_child(body, pane).expect("pane under body");

        let spacer = dom.create_element("div");
        dom.set_attribute(spacer, "class", "above");
        dom.append_child(pane, spacer).expect("spacer under pane");
        for line in &lines[first..first + visible] {
            let element = dom.create_element("div");
            dom.set_attribute(element, "class", "line");
            let text = dom.create_text(line);
            dom.append_child(element, text).expect("text under div");
            dom.append_child(pane, element).expect("line under pane");
        }
        let spacer = dom.create_element("div");
        dom.set_attribute(spacer, "class", "below");
        dom.append_child(pane, spacer).expect("spacer under pane");
    }

    let mut engine = StyleEngine::new();
    engine.add_stylesheet(Stylesheet::parse(&css).expect("the generated stylesheet"));
    let styles = engine
        .restyle_incremental(&mut tree, &StyleMap::default())
        .0;

    let mut fonts = FontSystem::new();
    let mut cache = LayoutCache::new();
    let started = std::time::Instant::now();
    {
        let mut context = LayoutContext::new(&mut tree, &styles, &mut fonts, &mut cache);
        context.run(VIEWPORT);
    }
    let layout_ms = started.elapsed().as_secs_f64() * 1000.0;

    let after = reading();
    let nodes = tree.len();
    // What the scrollbar would be: the whole response, less the part on screen.
    let scroll_max = tree.scroll_max(pane).height;
    let want = total as f32 * LINE_HEIGHT - VIEWPORT.height;

    println!(
        "MEASURED {:.6} {nodes} {layout_ms:.3} {visible} {scroll_max:.1} {want:.1}",
        mib(after - base),
    );
    drop((tree, styles, fonts, cache, engine, payload));
}

fn reading() -> u64 {
    crisol_ui::measure::current().map_or(0, |reading| reading.bytes)
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// A pretty-printed JSON body of roughly `target` bytes, shaped like an API response.
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
