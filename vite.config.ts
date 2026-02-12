import { defineConfig, Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { readFileSync, cpSync, existsSync } from 'fs'
import { resolve } from 'path'

// Extract resolved versions from package.json at build time
const pkg = JSON.parse(readFileSync('./package.json', 'utf-8'));
const cleanVer = (v: string) => v.replace(/^[\^~>=<]+/, '');

/**
 * Copy Monaco Editor AMD assets to dist/vs at build time,
 * and serve them from node_modules in dev mode.
 * The AMD loader files are in IIFE format — workers load without ESM issues.
 */
function copyMonacoAssets(): Plugin {
  const monacoPath = resolve(__dirname, 'node_modules/monaco-editor/min');
  return {
    name: 'copy-monaco-assets',
    configureServer(server) {
      // Serve /vs/* from node_modules in dev mode
      server.middlewares.use('/vs', (req, res, next) => {
        const filePath = resolve(monacoPath, 'vs', (req.url || '').replace(/^\//, '').split('?')[0]);
        if (existsSync(filePath)) {
          const ext = filePath.split('.').pop() || '';
          const mime: Record<string, string> = { js: 'application/javascript', css: 'text/css', ttf: 'font/ttf', svg: 'image/svg+xml' };
          res.setHeader('Content-Type', mime[ext] || 'application/octet-stream');
          res.end(readFileSync(filePath));
        } else {
          next();
        }
      });
    },
    writeBundle() {
      const src = resolve(monacoPath, 'vs');
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
