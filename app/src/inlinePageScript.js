/* global document, addEventListener, parent, scrollY, ResizeObserver */
// Runs in an inline page, before the agent's HTML. `InlinePage.tsx` puts it in each page.
(function () {
  // A new theme counts only from the window that holds the frame.
  var theme = document.getElementById("grove-theme");
  addEventListener("message", function (event) {
    if (event.source !== parent || !event.data || event.data.type !== "grove-page-theme") return;
    theme.textContent = String(event.data.css);
  });

  // The root's height, or the lowest edge of anything in the page when that is lower: content
  // placed out of the flow does not make the root taller. The frame's own height plays no part,
  // so a page shorter than its frame says so.
  function height() {
    var tall = document.documentElement.getBoundingClientRect().height;
    var all = document.querySelectorAll("body *");
    for (var i = 0; i < all.length; i++) {
      var bottom = all[i].getBoundingClientRect().bottom + scrollY;
      if (bottom > tall) tall = bottom;
    }
    return Math.ceil(tall);
  }

  var told = -1;
  function report() {
    var tall = height();
    if (tall === told) return;
    told = tall;
    parent.postMessage({ type: "grove-page-height", height: tall }, "*");
  }
  new ResizeObserver(report).observe(document.documentElement);
  addEventListener("load", report);
})();
