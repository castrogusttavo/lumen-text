const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

function countUp(el: HTMLElement) {
  const to = parseFloat(el.dataset.count ?? "0");
  const dec = parseInt(el.dataset.decimals ?? "0", 10);
  const fmt = (n: number) => n.toLocaleString("en-US", { minimumFractionDigits: dec, maximumFractionDigits: dec });
  if (reduced) { el.textContent = fmt(to); return; }
  const t0 = performance.now();
  const dur = 380;
  const tick = (t: number) => {
    const p = Math.min(1, (t - t0) / dur);
    el.textContent = fmt(to * (1 - Math.pow(1 - p, 3)));
    if (p < 1) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

const io = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    const el = e.target as HTMLElement;
    el.classList.add("in");
    el.querySelectorAll<HTMLElement>("[data-count]").forEach(countUp);
    if (el.matches("[data-count]")) countUp(el);
    io.unobserve(el);
  }
}, { threshold: 0.2, rootMargin: "0px 0px -6% 0px" });

document.querySelectorAll("[data-reveal], [data-bars]").forEach((el) => io.observe(el));

// OS detection for download buttons
const ua = navigator.userAgent;
const os = /Windows/i.test(ua) ? "windows" : /Mac/i.test(ua) && !/iPhone|iPad/i.test(ua) ? "macos" : /Android|iPhone|iPad/i.test(ua) ? "mobile" : "linux";
document.documentElement.dataset.os = os;
document.querySelectorAll<HTMLElement>("[data-os-label]").forEach((el) => {
  if (os === "windows") el.textContent = "Windows build coming soon";
  else if (os === "macos") el.textContent = "macOS build coming soon";
  else if (os === "mobile") el.textContent = "Download on your desktop";
});

// Theme toggle
document.querySelectorAll<HTMLButtonElement>("[data-theme-toggle]").forEach((b) =>
  b.addEventListener("click", () => {
    const next = document.documentElement.dataset.theme === "light" ? "dark" : "light";
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem("lumen-theme", next); } catch {}
  }),
);
