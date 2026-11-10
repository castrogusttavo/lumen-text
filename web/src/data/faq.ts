export const faq = [
  ["Which platforms does Lumen run on?", "Linux (Wayland and X11, glibc 2.35 or newer), Windows 10 or newer (x86_64) and macOS 11 or newer (Apple Silicon and Intel). The Windows and macOS builds are newer and not code-signed, so expect a first-launch warning; see the download page."],
  ["Does it need a GPU?", "No. Everything is drawn on the CPU with softbuffer and tiny-skia, so it runs anywhere a Wayland or X11 window does."],
  ["How does TypeScript work?", "Lumen starts tsgo, the native TypeScript compiler, on demand. It uses your project's own @typescript/native-preview when present, or a bundled install. Biome, Prettier and ESLint are used when the project has them."],
  ["Can I use my VS Code themes and extensions?", "Themes yes: 65 are bundled, and any VS Code theme JSON file dropped into ~/.config/lumen-text/themes/ works, as do installed Zed themes. Extensions no: Lumen has no extension system yet and cannot load VS Code extensions; the TypeScript workflow (tsgo, Biome, ESLint) is built in."],
  ["What does it send over the network?", "Only a request to the release's latest.json to check for updates, about 20 seconds after startup and every 4 hours. There is no telemetry, and updates can be set to notify or off."],
  ["How do I verify a download?", "Every release publishes SHA256SUMS, and the AppImage and tarball are signed with minisign. The download page lists the public key and the exact commands."],
  ["Why are some numbers marked 'measuring'?", "We publish a number only after taking it on the same machine under the same scenario. Competitor figures are being collected by a script and will appear when they exist."],
  ["How is it licensed?", "MIT. The source, issues and roadmap live on GitHub."],
] as const;
