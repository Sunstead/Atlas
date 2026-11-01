#!/usr/bin/env node
/**
 * Builds the web icons from the one design, public/icon.svg:
 *
 *   public/favicon.svg           the browser tab (the same file)
 *   public/apple-touch-icon.png  iOS home screen
 *   public/icon-192.png, icon-512.png, manifest.webmanifest   installing the web app
 *
 * Outputs are committed, so this only needs re-running when the icon changes:
 *
 *   npm run icons -w app
 */
import { copyFileSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Resvg } from '@resvg/resvg-js';

const pub = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../public');
const icon = readFileSync(path.join(pub, 'icon.svg'), 'utf8');

/** The page colour behind the icon, for the manifest: its darkest stop. */
const background = '#2c003e';

/** iOS rounds home screen icons itself and fills transparent corners with black. */
const square = icon.replace(/<clipPath id="tile">[\s\S]*?<\/clipPath>/, '<clipPath id="tile"><rect width="16" height="16"/></clipPath>');

function png(svg, width, file) {
  writeFileSync(path.join(pub, file), new Resvg(svg, { fitTo: { mode: 'width', value: width } }).render().asPng());
}

copyFileSync(path.join(pub, 'icon.svg'), path.join(pub, 'favicon.svg'));
png(square, 180, 'apple-touch-icon.png');
png(icon, 192, 'icon-192.png');
png(icon, 512, 'icon-512.png');
writeFileSync(
  path.join(pub, 'manifest.webmanifest'),
  `${JSON.stringify(
    {
      name: 'Atlas',
      short_name: 'Atlas',
      start_url: '/',
      display: 'standalone',
      background_color: background,
      theme_color: background,
      icons: [
        { src: '/icon-192.png', sizes: '192x192', type: 'image/png' },
        { src: '/icon-512.png', sizes: '512x512', type: 'image/png' },
        { src: '/favicon.svg', sizes: 'any', type: 'image/svg+xml' },
      ],
    },
    null,
    2,
  )}\n`,
);
console.log('wrote the web icons');
