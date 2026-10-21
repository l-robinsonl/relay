# Icons

Two white chevrons on a rounded square in Relay's accent blue. A double
chevron is the universal "forward on" mark, which is what the name means
— a single one would read as a shell prompt. The trailing chevron carries
the weight and the leading one is smaller and thinner, so the pair reads
as a hand-off rather than as two arrows.

The same geometry is drawn in the interface by
`apps/desktop/src/components/ui/RelayLogo.vue`, as SVG, so the icon and
the in-app mark cannot drift apart.

Generated from `icon.png` (1024×1024) by `npm run tauri icon <path>`, which
produces every size the platforms need plus the `.icns` and `.ico` bundles
macOS and Windows installers require.

The source itself is drawn by `scripts/render-icon.py` rather than kept as
a binary blob, so the shape is a diff rather than an opaque file. It uses
4× supersampling; without it the diagonals and corner curves stair-step
badly at 32px.

To change the mark, edit the script, run it, then regenerate:

```sh
python3 scripts/render-icon.py
npm run tauri icon "$(pwd)/apps/desktop/src-tauri/icons/icon.png"
```

The `android/` and `ios/` directories are produced by the same command and
are unused until those targets are built.
