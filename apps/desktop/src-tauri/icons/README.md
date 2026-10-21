# Icons

A white chevron on a rounded square in Relay's accent blue — a send arrow,
and the shape of a shell prompt.

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
