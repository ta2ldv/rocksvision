# RocksVision README screenshots
Two copies of every capture:
- `highres/<name>_highres.png`: the original window-only capture (3456×2160 px, 2× Retina; the Matrix capture 3680×2382), unchanged.
- `<name>.png`: the preview README.md and README.tr.md embed (1400 px wide, 256-colour PNG, 170–212 KB each). Each preview links to its original. Regenerate with `bash scripts/make-screenshot-previews.sh 1400` then `python3 -I scripts/png-optimize.py --quantize 256 screenshots/*.png` (Pillow). Totals: originals 12 248 249 B, previews 1 491 687 B.

Theme captures: the final build of handoff 0007 (bundle executable SHA-256 4d803f503260558cd86912c17b734dd3ca1df86780178a70cfa1e2bba4053971) on macOS, window sized to the full display (1728×1079 pt). Scene: examples/files/sst-zstd/sst-zstd.sst opened standalone, Data block #14 @0x5c9 (ZSTD, 99 B stored → 323 B decompressed) with entry 1 selected; the byte view shows the decompressed block bytes (OFFSET / hex / ASCII) with the selected entry highlighted; Explanation shows the entry derivation and interpretations. Since 0.1.2 XCopy is the default theme; the XCopy capture is the first README image.

| File | Theme (View › Theme) |
|---|---|
| xcopy.png | XCopy (default since 0.1.2) |
| autistic.png | Autistic (default before 0.1.2) |
| matrix.png | Matrix (regenerated 2026-10-24 from v0.1.1 build e13410d7… after the toolbar fix; same scene) |
| amiga-workbench.png | Amiga Workbench |
| amiga-tool-style.png | Amiga Tool Style |
| wooden.png | Wooden |
| theme-menu.png | View › Theme menu |
| about.png | About window |
