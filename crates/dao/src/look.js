// Injected by `dao look` (look.rs) into a copy of the page; runs after load inside headless Chrome.
// Measures style-neutral defects (EVAL-LAYER L1 rules and L2 metrics), never taste, and writes the
// result as URI-encoded JSON into <html data-dao-look>, which `--dump-dom` carries back to Rust.
addEventListener("load", function () {
  var W = innerWidth, findings = [], CAP = 5;
  var count = {};
  function where(el) {
    var p = [];
    for (; el && el.nodeType === 1 && p.length < 4; el = el.parentElement) {
      var s = el.tagName.toLowerCase();
      if (el.id) { p.unshift(s + "#" + el.id); break; }
      if (el.classList.length) s += "." + el.classList[0];
      p.unshift(s);
    }
    return p.join(">");
  }
  function add(kind, el, detail) {
    count[kind] = (count[kind] || 0) + 1;
    if (count[kind] <= CAP) findings.push({ kind: kind, where: where(el), detail: detail });
  }
  function rgba(s) {
    var m = /rgba?\(([^)]+)\)/.exec(s);
    if (!m) return null;
    var v = m[1].split(/[\s,\/]+/).filter(Boolean).map(parseFloat);
    return [v[0], v[1], v[2], v.length > 3 ? v[3] : 1];
  }
  function lum(c) {
    function f(x) { x /= 255; return x <= 0.03928 ? x / 12.92 : Math.pow((x + 0.055) / 1.055, 2.4); }
    return 0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2]);
  }
  function blend(top, base) {
    return [0, 1, 2].map(function (k) { return top[k] * top[3] + base[k] * (1 - top[3]); });
  }
  // effective background; null when an image or gradient is underneath (contrast unknown)
  function background(el) {
    var layers = [];
    for (var e = el; e; e = e.parentElement) {
      var cs = getComputedStyle(e);
      if (cs.backgroundImage !== "none") return null;
      var c = rgba(cs.backgroundColor);
      if (c && c[3] > 0) { layers.push(c); if (c[3] >= 1) break; }
    }
    var base = [255, 255, 255];
    for (var i = layers.length - 1; i >= 0; i--) base = blend(layers[i], base);
    return base;
  }
  function visible(el, cs, r) {
    return r.width > 0 && r.height > 0 && cs.visibility !== "hidden" && parseFloat(cs.opacity) > 0;
  }
  function ownText(el) {
    var t = "";
    for (var n = el.firstChild; n; n = n.nextSibling) if (n.nodeType === 3) t += n.nodeValue;
    return t.trim();
  }

  var families = {}, sizes = {}, colors = {};
  var all = document.body ? document.body.getElementsByTagName("*") : [];
  for (var i = 0; i < all.length; i++) {
    var el = all[i], cs = getComputedStyle(el), r = el.getBoundingClientRect();
    if (!visible(el, cs, r)) continue;
    var text = ownText(el);
    if (text) {
      var fs = parseFloat(cs.fontSize);
      families[cs.fontFamily.split(",")[0].trim().replace(/["']/g, "").toLowerCase()] = 1;
      sizes[Math.round(fs * 2) / 2] = 1;
      var fg = rgba(cs.color), bg = background(el);
      if (fg) colors[fg.slice(0, 3).map(Math.round).join(",")] = 1;
      if (fg && bg) {
        var c = blend(fg, bg), a = lum(c), b = lum(bg);
        var ratio = (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
        var large = fs >= 24 || (fs >= 18.66 && parseInt(cs.fontWeight, 10) >= 700);
        if (ratio < (large ? 3 : 4.5)) add("contrast", el, "ratio " + ratio.toFixed(2) + " below " + (large ? "3" : "4.5") + ":1 (WCAG 2.2 1.4.3)");
      }
      var ox = cs.overflowX;
      if ((ox === "hidden" || ox === "clip") && cs.textOverflow !== "ellipsis" && el.scrollWidth > el.clientWidth + 1)
        add("clipped", el, "text wider than its box is cut off");
      if (/^(P|LI|DD|BLOCKQUOTE)$/.test(el.tagName) && text.length > 200) {
        var cpl = r.width / (fs * 0.5);
        if (cpl > 95) add("line-length", el, "about " + Math.round(cpl) + " characters per line");
      }
    }
    var interactive = el.matches("a[href],button,select,textarea,input:not([type=hidden]),[role=button]");
    if (interactive && (r.width < 24 || r.height < 24)) {
      // WCAG 2.5.8 exempts targets inline in a sentence
      var inSentence = cs.display === "inline" && el.parentElement && ownText(el.parentElement).length > 0;
      if (!inSentence) add("target-size", el, Math.round(r.width) + "x" + Math.round(r.height) + " below 24x24 (WCAG 2.2 2.5.8)");
    }
    if (el.tagName === "IMG" && !el.hasAttribute("alt")) add("img-alt", el, "image without alt");
  }

  var doc = document.documentElement;
  if (doc.scrollWidth > W + 1) {
    var culprit = null;
    for (var j = 0; j < all.length && !culprit; j++) {
      var rr = all[j].getBoundingClientRect();
      var pr = all[j].parentElement ? all[j].parentElement.getBoundingClientRect() : null;
      if (rr.right > W + 1 && (!pr || pr.right <= W + 1)) culprit = all[j];
    }
    add("h-scroll", culprit || doc, "page is " + doc.scrollWidth + "px wide in a " + W + "px viewport");
  }

  // the same section shape repeated: >= 4 composite siblings with one signature under body or main
  var shape = function (e) {
    var kids = [];
    for (var k = e.firstElementChild; k; k = k.nextElementSibling)
      if (getComputedStyle(k).display !== "inline" && getComputedStyle(k).display !== "none") kids.push(k.tagName);
    return kids.length >= 2 ? e.tagName + ":" + kids.join(",") : null;
  };
  [document.body].concat(Array.prototype.slice.call(document.querySelectorAll("main"))).forEach(function (box) {
    if (!box) return;
    var seen = {};
    for (var k = box.firstElementChild; k; k = k.nextElementSibling) {
      var s = shape(k);
      if (s) (seen[s] = seen[s] || []).push(k);
    }
    Object.keys(seen).forEach(function (s) {
      if (seen[s].length >= 4) add("repetition", seen[s][0], seen[s].length + " sibling blocks share the shape " + s);
    });
  });

  var nf = Object.keys(families).length, ns = Object.keys(sizes).length;
  if (nf > 3) add("font-families", doc, nf + " font families: " + Object.keys(families).join(", "));
  if (ns > 8) add("type-scale", doc, ns + " distinct text sizes");

  var out = {
    width: W,
    metrics: { font_families: nf, text_sizes: ns, text_colors: Object.keys(colors).length, page_width: doc.scrollWidth },
    counts: count,
    findings: findings
  };
  doc.setAttribute("data-dao-look", encodeURIComponent(JSON.stringify(out)));
});
