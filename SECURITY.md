# Security Policy

## Supported versions

Only the **latest release** (see [Releases](https://github.com/castrogusttavo/lumen-text/releases); versions are CalVer `YYYY.M.D.N`) receives security fixes. Please reproduce problems on the latest release before reporting.

## Reporting a vulnerability

Please do **not** open a public issue for security problems. Use GitHub's private vulnerability reporting:
[Report a vulnerability](https://github.com/castrogusttavo/lumen-text/security/advisories/new) (repository **Security** tab, "Report a vulnerability").

Include the version (`lumen-text --version`), your distribution, what you did, what you expected and what happened, and a minimal file or project that reproduces it if possible.
You can expect an acknowledgement within a few days; fixes ship as a normal release and the advisory is published afterwards with credit if you want it.

Areas of particular interest: handling of untrusted files and projects (grammars, themes, Markdown, images), the LSP/DAP/terminal child-process plumbing, and the packaging scripts.
