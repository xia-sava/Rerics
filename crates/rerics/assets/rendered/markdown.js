// Markdown のレンダリング表示で Mermaid の図を描く。配色は文書の明暗（body の class）に合わせる。
(function () {
  if (typeof mermaid === "undefined") {
    return;
  }
  const dark = document.body.classList.contains("dark");
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: "strict",
    theme: dark ? "dark" : "default",
  });
  mermaid.run({ querySelector: "pre.mermaid" });
})();
