import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'

// Tauri serves the frontend from a fixed port in development and from the
// bundled `dist` directory in a release build.
const host = process.env.TAURI_DEV_HOST

export default defineConfig({
  plugins: [vue(), tailwindcss()],

  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
      '@relay/shared-types': fileURLToPath(
        new URL('../../packages/shared-types/src', import.meta.url),
      ),
    },
  },

  // Tauri expects a fixed port and fails rather than silently moving.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: 'ws', host, port: 1421 } : undefined,
    watch: {
      // Rebuilding the frontend because Rust changed wastes a lot of time.
      ignored: ['**/src-tauri/**', '**/target/**'],
    },
  },

  build: {
    // Matches the Tauri webview floor: WebKitGTK on Linux, WebView2 on
    // Windows, WKWebView on macOS. Targeting these avoids shipping
    // transpilation for browsers that will never run this code.
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari15',
    // Vite 8 bundles with Rolldown, whose own minifier is the default; naming
    // esbuild here would pull a package Vite no longer ships.
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    rollupOptions: {
      output: {
        // CodeMirror is large and only needed once an editor is opened, so
        // it is split out rather than blocking the shell's first paint.
        manualChunks: (id: string) =>
          id.includes('node_modules/@codemirror') || id.includes('node_modules/@lezer')
            ? 'codemirror'
            : undefined,
      },
    },
  },

  test: {
    environment: 'jsdom',
    globals: true,
    include: ['src/**/*.{test,spec}.ts'],
  },
})
