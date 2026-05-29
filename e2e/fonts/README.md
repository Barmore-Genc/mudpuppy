# Vendored font — the pixel oracle's pinned glyph source

`DejaVuSansMono.ttf` is committed here on purpose: it is the single thing (besides
the `resvg` version) that determines the exact pixels of the snapshot baselines.
Vendoring it pins glyph outlines by *content*, so a render is byte-identical on
any machine regardless of what `fonts-*` package the distro ships — which is what
lets the oracle run natively, with no container (see [`../README.md`](../README.md)).

- **File:** `DejaVuSansMono.ttf`
- **sha256:** `0f5db4f1749979d961019838b160bec74abdf7f9eca69553fe1aa856bbff49a4`
- **Provenance:** the exact file Debian `bookworm`'s `fonts-dejavu-core` ships at
  `/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf` (the file the original
  containerized oracle used, so existing baselines stay valid).

The renderer is pointed at it explicitly via `resvg --use-font-file …
--skip-system-fonts` in [`../scripts/run.sh`](../scripts/run.sh); never rely on a
system-installed DejaVu.

## License

DejaVu Fonts are released under a permissive license (the Bitstream Vera license
plus the DejaVu changes), which allows redistribution including bundling in a
repository. See <https://dejavu-fonts.github.io/License.html>.
