import init from "/pkg/stage-select.js";

// The theme you chose, before the workbench loads, so the page doesn't flash the other one (see parts::theme).
try {
  const theme = localStorage.getItem("stage-select.theme");
  if (theme === "light" || theme === "dark") document.documentElement.dataset.theme = theme;
} catch {}

init({ module_or_path: "/pkg/stage-select.wasm" });
