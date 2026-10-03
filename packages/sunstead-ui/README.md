# @sunstead/ui

Shared UI for Sunstead apps: the theme registry and provider, theme CSS, base
tokens, and shadcn primitives (style `radix-nova`, base colour neutral).

It starts inside the Atlas repo and moves to its own repo once a second app
(Cosmos, Starbook or Solstice) adopts it. Until then, keep it free of anything
Atlas-specific.

- **Source-only.** There is no build step. Apps import the `.ts`/`.tsx`/`.css`
  files directly and compile them with their own Vite and Tailwind.
- **Tailwind.** Apps import `@sunstead/ui/styles.css` after `tailwindcss` and
  add an `@source` for `packages/sunstead-ui/src`, so the classes used here are
  generated.
- **Themes.** Each theme is a `[data-theme]` block in `src/themes/` plus an
  entry in `src/lib/themes.ts`. Each app's `index.html` keeps a copy of the
  ids for the first paint; the app's tests keep them in step.
- **Primitives.** `src/components/ui/` holds vendored shadcn components, copied
  from Cosmos. Imports inside them are relative (`../../lib/utils`), not `@/`,
  because `@/` means the consuming app's `src/`.

Origin: the themes, provider and primitives were lifted from Cosmos
(`app/src/themes`, `app/src/lib/themes.ts`, `app/src/components`) with the
base pair renamed from `cosmos-*` to `sunstead-*`.
