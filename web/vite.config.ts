import { defineConfig } from 'vite'
import preact from '@preact/preset-vite'
import { viteSingleFile } from 'vite-plugin-singlefile'

// Builds to ../static/index.html as one self-contained file, which acp-tapd
// embeds with include_str! — no asset serving, no build step in the Rust crate.
export default defineConfig({
  plugins: [preact(), viteSingleFile()],
  build: {
    outDir: '../static',
    emptyOutDir: false,
    cssCodeSplit: false,
    assetsInlineLimit: 100_000_000,
    chunkSizeWarningLimit: 4096
  }
})
