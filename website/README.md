# poly documentation site

The [poly](https://github.com/Goldziher/poly) documentation, built with
[Astro](https://astro.build) and [Starlight](https://starlight.astro.build) and deployed to GitHub
Pages at <https://goldziher.github.io/poly/>.

```sh
npm install
npm run dev      # local dev server
npm run build    # static build into dist/
npm run preview  # serve the built site
```

Because the site is hosted on a GitHub Pages *project* page, `astro.config.mjs` sets
`base: "/poly"`. Every internal link in the content must therefore be written with that prefix
(`/poly/reference/cli/`), and `dist/` is served at `https://goldziher.github.io/poly/` — the build
does not emit a `dist/poly/` subdirectory.

`.github/workflows/docs.yaml` builds and deploys on every push to `main` that touches `website/**`.
