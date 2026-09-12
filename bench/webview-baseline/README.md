# WebView baseline

ROADMAP §7's first kill criterion compares crisol's idle memory against *"a WebView2/WKWebView
baseline for an equivalent app"*. This is that baseline on macOS: a minimal `WKWebView` host
showing the same todo list as `cargo run -p crisol-ui --example todo`, with the same CSS.

```sh
swiftc -O main.swift -o wvbase && ./wvbase
```

## Measure the right number

**Not RSS.** On macOS, RSS counts shared read-only library pages — Metal, CoreGraphics, WebKit
— which every process on the machine pays for and which do not represent an app's cost. The
crisol example reports 90 MB RSS and 25.5 MB of actual footprint; the difference is framework
pages it shares with everything else running.

Use `phys_footprint`, which is what Activity Monitor shows as "Memory":

```sh
footprint -p "$(pgrep -x wvbase)"
```

**WKWebView is multi-process**, so the baseline is the host plus its helpers, and the helpers
do not show up as its children — they are launched through XPC. Snapshot the WebKit processes
before launching and diff afterwards, or you will measure every browser on the machine. Doing
that naively the first time produced a "baseline" of 1,037 MB, which was Safari's.

A machine already running Safari lets a new WebView reuse some of that infrastructure, so the
number this produces is **generous to WebView**. On a clean machine it would be higher.
