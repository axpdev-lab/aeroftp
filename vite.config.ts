import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { readFileSync } from 'fs'

// Extract resolved versions from package.json at build time
const pkg = JSON.parse(readFileSync('./package.json', 'utf-8'));
const cleanVer = (v: string) => v.replace(/^[\^~>=<]+/, '');

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  define: {
    __FRONTEND_VERSIONS__: JSON.stringify({
      react: cleanVer(pkg.dependencies?.['react'] ?? '?'),
      typescript: cleanVer(pkg.devDependencies?.['typescript'] ?? '?'),
      tailwindcss: cleanVer(pkg.devDependencies?.['tailwindcss'] ?? '?'),
      monaco: cleanVer(pkg.dependencies?.['@monaco-editor/react'] ?? '?'),
      vite: cleanVer(pkg.devDependencies?.['vite'] ?? '?'),
    }),
  },
})
