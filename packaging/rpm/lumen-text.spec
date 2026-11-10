# Binary RPM wrapping an already built release binary (see build-rpm.sh, which stages the
# file tree and passes lumen_version / lumen_stage / changelog_date).
%global debug_package %{nil}
%global _build_id_links none
%global __os_install_post %{nil}
%global _binary_payload w9.gzdio

Name:           lumen-text
Version:        %{lumen_version}
Release:        1
Summary:        Lightweight code editor with TextMate highlighting and a TypeScript workflow
License:        MIT
URL:            https://github.com/castrogusttavo/lumen-text
# The binary links only glibc/libgcc (picked up by the automatic requires); the windowing
# libraries are dlopen()ed at run time, so they are declared by hand.
Requires:       libxkbcommon
Requires:       fontconfig
Recommends:     libwayland-client
Recommends:     libwayland-cursor
Recommends:     libxkbcommon-x11
Recommends:     libX11
Recommends:     libX11-xcb
Recommends:     libXcursor
Recommends:     libXi
Recommends:     dejavu-sans-mono-fonts
Suggests:       wl-clipboard
Suggests:       xclip

%description
lumen-text is a fast, memory-frugal code editor written in Rust. It renders with the
CPU, highlights with TextMate grammars and talks to the TypeScript native language
server (tsgo) for diagnostics and completion.

%prep
%build

%install
mkdir -p %{buildroot}
cp -a %{lumen_stage}/usr %{buildroot}/usr
# Fedora keeps licenses apart from the generic documentation.
install -Dm644 %{buildroot}%{_docdir}/lumen-text/LICENSE %{buildroot}%{_licensedir}/lumen-text/LICENSE
rm -f %{buildroot}%{_docdir}/lumen-text/LICENSE

%files
%{_bindir}/lumen-text
%{_datadir}/lumen-text
%{_datadir}/applications/lumen-text.desktop
%{_datadir}/icons/hicolor/scalable/apps/lumen-text.svg
%license %{_licensedir}/lumen-text/LICENSE
%doc %{_docdir}/lumen-text/README.md

%changelog
* %{changelog_date} Gustavo Leandro <castrogusttavo@users.noreply.github.com> - %{version}-1
- Automated release %{version}
