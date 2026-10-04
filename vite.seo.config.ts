import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'

/**
 * The build-time SEO prerender bundle.
 *
 * A second, tiny build whose only output is a Node module exporting the public
 * pages as HTML — see `apps/web/src/seo/prerender-entry.tsx`. It exists because
 * the pages are React components and the thing that has to write them to disk
 * is a script: rather than keep a second copy of the markup in that script,
 * the script imports the components, already compiled.
 *
 * Nothing here touches the browser bundle, which `vite.web.config.ts` builds
 * separately and first. `.seo-ssr/` is a build artefact and is not deployed.
 */
export default defineConfig({
  plugins: [react()],
  root: 'apps/web',
  resolve: { alias: { '@': resolve(__dirname, 'apps/desktop/src') } },
  build: {
    ssr: 'src/seo/prerender-entry.tsx',
    outDir: '.seo-ssr',
    emptyOutDir: true,
    target: 'node18',
    minify: false,
    rollupOptions: { output: { entryFileNames: 'prerender-entry.mjs' } },
  },
})
