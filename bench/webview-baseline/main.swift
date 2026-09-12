// Minimal WKWebView host showing a todo list equivalent to the crisol example,
// for the memory baseline ROADMAP §7 measures against.
import Cocoa
import WebKit

let html = """
<!doctype html><html><head><meta charset="utf-8"><style>
  html,body{height:100%;margin:0}
  body{display:flex;flex-direction:column;padding:20px 24px 0;
       background:rgb(14,16,20);color:rgb(226,232,240);
       font:15px/22px -apple-system,sans-serif}
  h1{font-size:22px;line-height:34px;color:rgb(97,175,239);margin:0}
  .draft{height:30px;line-height:30px;padding:0 10px;margin-bottom:10px;
         background:rgb(24,28,34);border-bottom:2px solid rgb(97,175,239)}
  ul.list{display:flex;flex-direction:column;flex-grow:1;min-height:0;
          overflow:scroll;margin:0;padding:0;list-style:none}
  li.todo{display:flex;height:26px;line-height:26px;padding:0 8px}
  .mark{width:30px;color:rgb(152,195,121)}
  .label{flex-grow:1}
  p.status{height:24px;line-height:24px;margin:8px 0 0;
           color:rgb(106,115,125);font-size:12px}
</style></head><body>
<h1>todos</h1><div class="draft">&gt; _</div><ul class="list" id="l"></ul>
<p class="status">27 left · filter: all</p>
<script>
  const seeds = ["read the roadmap","ship M7","measure idle RSS"];
  for (let i = 0; i < 24; i++) seeds.push("filler " + i);
  document.getElementById("l").innerHTML = seeds.map(s =>
    `<li class="todo"><span class="mark">[ ]</span><span class="label">${s}</span></li>`).join("");
</script></body></html>
"""

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 460),
                      styleMask: [.titled, .closable, .resizable],
                      backing: .buffered, defer: false)
window.title = "WKWebView baseline"
let webView = WKWebView(frame: window.contentView!.bounds)
webView.autoresizingMask = [.width, .height]
window.contentView!.addSubview(webView)
webView.loadHTMLString(html, baseURL: nil)
window.makeKeyAndOrderFront(nil)
app.run()
