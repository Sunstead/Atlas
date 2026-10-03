import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'node:path';
import pkg from './package.json';

// In development Vite serves the app and forwards server paths to
// `cargo run -p atlas-server` on :8080, so the browser sees one origin, as it
// does in production.
const server = process.env.ATLAS_DEV_SERVER ?? 'http://localhost:8080';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  resolve: {
    alias: { '@': path.resolve(__dirname, './src') },
  },
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
