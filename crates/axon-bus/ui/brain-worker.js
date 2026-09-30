// Runs the brain's renderer off the page's thread, on the canvas the page transferred.

import { createRenderer } from "./brain-render.js";

let renderer = null;
self.onmessage = ({ data }) => {
  if (data.type === "init") renderer = createRenderer(data.canvas, (msg) => self.postMessage(msg));
  else if (renderer) renderer.handle(data);
};
