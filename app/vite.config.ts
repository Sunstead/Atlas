import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'node:path';
import pkg from './package.json';

// In development Vite serves the app and forwards server paths to
// `cargo run -p atlas-server` on :8080, so the browser sees one origin, as it
// does in production. 127.0.0.1, not localhost: Node resolves localhost to
// ::1 first, and the server listens on IPv4.
const server = process.env.ATLAS_DEV_SERVER ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  resolve: {
    alias: { '@': path.resolve(__dirname, './src') },
  },
  // @sunstead/ui is source with several entry points. Pre-bundling them one
  // by one gives each its own copy of shared modules (two theme contexts), so
  // serve it as source, like the app's own files.
  optimizeDeps: { exclude: ['@sunstead/ui'] },
  server: {
    // Fixed, so the future Tauri shell can point at it.
    port: 1420,
    strictPort: true,
    proxy: {
      '/v1': server,
      '/auth': server,
      '/healthz': server,
      '/opensearch.xml': server,
    },
  },
});
