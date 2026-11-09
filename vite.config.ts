import { defineConfig, Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { readFileSync, cpSync, existsSync } from 'fs'
import { resolve } from 'path'

// Extract resolved versions for build-time injection into the About dialog.
// Prefers the exact lockfile resolution (or the installed package.json in node_modules)
// so the Technical tab and copied technical info report the real build version rather
// than the manifest range minimum.
const pkg = JSON.parse(readFileSync('./package.json', 'utf-8'));
const cleanVer = (v: string) => v.replace(/^[\^~>=<]+/, '');

let packageLock: { packages?: Record<string, { version?: string }> } | null = null;
try {
  const lockPath = resolve(__dirname, 'package-lock.json');
  if (existsSync(lockPath)) {
    packageLock = JSON.parse(readFileSync(lockPath, 'utf-8'));
  }
} catch {
  // Ignore lockfile parse failure; fallback below will handle it
}

function resolveInstalledVersion(name: string, fallbackRange?: string): string {
  const locked = packageLock?.packages?.[`node_modules/${name}`]?.version;
  if (typeof locked === 'string' && locked) {
    return locked;
  }
  try {
    const modPkgPath = resolve(__dirname, `node_modules/${name}/package.json`);
    if (existsSync(modPkgPath)) {
      const modPkg = JSON.parse(readFileSync(modPkgPath, 'utf-8'));
      if (typeof modPkg.version === 'string' && modPkg.version) {
        return modPkg.version;
      }
    }
  } catch {
    // Ignore error
  }
  const raw = fallbackRange ?? pkg.dependencies?.[name] ?? pkg.devDependencies?.[name] ?? '?';
  return cleanVer(raw);
}

/**
 * Copy Monaco Editor AMD assets to dist/vs at build time,
 * and serve them from node_modules in dev mode.
 * The AMD loader files are in IIFE format: workers load without ESM issues.
 */
function copyMonacoAssets(): Plugin {
  const monacoPath = resolve(__dirname, 'node_modules/monaco-editor/min');
  return {
    name: 'copy-monaco-assets',
    configureServer(server) {
      // Serve /vs/* from node_modules in dev mode
      const monacoVsPath = resolve(monacoPath, 'vs');
      server.middlewares.use('/vs', (req, res, next) => {
        const relativePath = (req.url || '').replace(/^\//, '').split('?')[0];
        const filePath = resolve(monacoVsPath, relativePath);
        // H30: Validate resolved path stays within Monaco directory (prevent path traversal)
        if (!filePath.startsWith(monacoVsPath)) {
          res.statusCode = 403;
          res.end('Forbidden');
          return;
        }
        if (existsSync(filePath)) {
          const ext = filePath.split('.').pop() || '';
          const mime: Record<string, string> = { js: 'application/javascript', css: 'text/css', ttf: 'font/ttf', svg: 'image/svg+xml' };
          res.setHeader('Content-Type', mime[ext] || 'application/octet-stream');
          res.end(readFileSync(filePath));
        } else {
          next();
        }
      });
      // Return 204 for /min-maps/* requests: Monaco references source maps that don't exist
      // in the npm package. Without this, Vite returns HTML (index fallback) causing JSON parse errors.
      server.middlewares.use('/min-maps', (_req, res) => {
        res.statusCode = 204;
        res.end();
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
  build: {
    rollupOptions: {
      // Multi-page: the main app plus the dedicated lightweight `extract` window
      // (Deliverable G). Each HTML entry gets its own bundle so extract.html does
      // not pull in the full App.
      input: {
        main: resolve(__dirname, 'index.html'),
        extract: resolve(__dirname, 'extract.html'),
        // AeroAgent approval window: its own bundle, nothing of the chat.
        'ai-approval': resolve(__dirname, 'ai-approval.html'),
      },
    },
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: true,
    // Don't watch the Rust build tree: src-tauri/target holds thousands of
    // build artifacts that otherwise exhaust the kernel's inotify watch limit
    // and crash the dev server's file watcher.
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  define: {
    __FRONTEND_VERSIONS__: JSON.stringify({
      react: resolveInstalledVersion('react'),
      typescript: resolveInstalledVersion('typescript'),
      tailwindcss: resolveInstalledVersion('tailwindcss'),
      monaco: resolveInstalledVersion('monaco-editor', pkg.dependencies?.['monaco-editor'] ?? pkg.dependencies?.['@monaco-editor/react']),
      vite: resolveInstalledVersion('vite'),
    }),
  },
})
