// Resolves the latest GitHub release into src/data/release.json at build time.
// Offline or rate-limited? The previous file (committed) is kept as the fallback.
import { readFile, writeFile } from "node:fs/promises";

const REPO = process.env.LUMEN_REPO ?? "castrogusttavo/lumen-text";
const OUT = new URL("../src/data/release.json", import.meta.url);

const kinds = [
  ["deb", /_amd64\.deb$/],
  ["rpm", /\.x86_64\.rpm$/],
  ["arch", /\.pkg\.tar\.zst$/],
  ["appimage", /x86_64\.AppImage$/],
  ["tarball", /linux-x86_64\.tar\.gz$/],
  ["windowsSetup", /-windows-x86_64-setup\.exe$/],
  ["windows", /-windows-x86_64\.zip$/],
  ["macosArm", /-macos-aarch64\.dmg$/],
  ["macosIntel", /-macos-x86_64\.dmg$/],
  ["checksums", /^SHA256SUMS$/],
  ["latest", /^latest\.json$/],
];

try {
  const headers = { accept: "application/vnd.github+json", "user-agent": "lumen-text-web" };
  if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers, signal: AbortSignal.timeout(15000) });
  if (!res.ok) throw new Error(`GitHub API ${res.status}`);
  const rel = await res.json();
  const assets = {};
  for (const a of rel.assets ?? []) {
    for (const [key, re] of kinds) {
      if (re.test(a.name) && !assets[key]) {
        assets[key] = { name: a.name, url: a.browser_download_url, size: a.size, sha256: a.digest?.replace(/^sha256:/, "") ?? null };
        const sig = (rel.assets ?? []).find((x) => x.name === `${a.name}.minisig`);
        if (sig) assets[key].signatureUrl = sig.browser_download_url;
      }
    }
  }
  const data = {
    repo: REPO,
    tag: rel.tag_name,
    version: rel.tag_name.replace(/^v/, ""),
    publishedAt: rel.published_at,
    htmlUrl: rel.html_url,
    notes: rel.body ?? "",
    assets,
    resolvedAt: new Date().toISOString(),
  };
  await writeFile(OUT, JSON.stringify(data, null, 2) + "\n");
  try {
    const all = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=40`, { headers, signal: AbortSignal.timeout(15000) });
    if (all.ok) {
      const list = (await all.json()).filter((r) => !r.draft).map((r) => ({ tag: r.tag_name, name: r.name, publishedAt: r.published_at, url: r.html_url, prerelease: r.prerelease, notes: r.body ?? "", assets: (r.assets ?? []).filter((a) => /\.(deb|rpm|AppImage|zst|tar\.gz|zip|exe|dmg)$/.test(a.name)).map((a) => ({ name: a.name, url: a.browser_download_url, size: a.size })) }));
      await writeFile(new URL("../src/data/releases.json", import.meta.url), JSON.stringify(list, null, 2) + "\n");
    }
  } catch { /* keep the committed list */ }
  console.log(`[release] ${data.tag} (${Object.keys(assets).join(", ")})`);
} catch (err) {
  console.warn(`[release] could not refresh (${err.message}); using the committed src/data/release.json`);
  try { await readFile(OUT); } catch {
    await writeFile(OUT, JSON.stringify({ repo: REPO, tag: null, version: null, publishedAt: null, htmlUrl: `https://github.com/${REPO}/releases`, notes: "", assets: {}, resolvedAt: null }, null, 2) + "\n");
  }
}
