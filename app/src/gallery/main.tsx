import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { fakeBridge } from "../fakeBridge";
import "../tokens.css";
import "../styles.css";
import { addGallerySessions, Gallery, GALLERY_DIFFS, GALLERY_REPO } from "./Gallery";

// Served only by the dev server at /gallery.html. `vite build` builds index.html alone, so the
// app bundle never has this page.
const bridge = fakeBridge(GALLERY_REPO, GALLERY_DIFFS);
await bridge.addRepo(Object.keys(GALLERY_REPO)[0]);
await addGallerySessions(bridge);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Gallery bridge={bridge} />
  </StrictMode>,
);
