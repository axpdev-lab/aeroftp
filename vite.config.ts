import { defineConfig, Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { readFileSync, cpSync, existsSync } from 'fs'
import { resolve } from 'path'

// Extract resolved versions from package.json at build time
const pkg = JSON.parse(readFileSync('./package.json', 'utf-8'));
const cleanVer = (v: string) => v.replace(/^[\^~>=<]+/, '');

/**
 * Copy Monaco Editor AMD assets to dist/vs at build time.
 * The AMD loader files are in IIFE format — workers load without ESM issues.
 */
function copyMonacoAssets(): Plugin {
  return {
    name: 'copy-monaco-assets',
    writeBundle() {
      const src = resolve(__dirname, 'node_modules/monaco-editor/min/vs');
      const dest = resolve(__dirname, 'dist/vs');
      if (existsSync(src)) {
        cpSync(src, dest, { recursive: true });
      }
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), copyMonacoAssets()],
  define: {
    __FRONTEND_VERSIONS__: JSON.stringify({
      react: cleanVer(pkg.dependencies?.['react'] ?? '?'),
      typescript: cleanVer(pkg.devDependencies?.['typescript'] ?? '?'),
      tailwindcss: cleanVer(pkg.devDependencies?.['tailwindcss'] ?? '?'),
      monaco: cleanVer(pkg.dependencies?.['monaco-editor'] ?? cleanVer(pkg.dependencies?.['@monaco-editor/react'] ?? '?')),
      vite: cleanVer(pkg.devDependencies?.['vite'] ?? '?'),
    }),
  },
})
