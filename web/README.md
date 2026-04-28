# dabeacon-indexer web UI

SvelteKit frontend for [dabeacon-indexer](../README.md). Consumes the indexer's
REST API and the `/api/live/sse` event stream to render per-validator status,
per-epoch attestation outcomes, proposal history, sync-committee participation,
and reward windows.

The build output is served by the Rust binary's embedded Axum server at `/`,
so in production there's nothing to deploy separately — `cargo run --release`
serves the prebuilt static bundle alongside the API.

## Development

The dev server proxies API calls to a running indexer. Start the indexer first
(in the project root):

```bash
cargo run --release
```

Then in this directory:

```bash
npm install
npm run dev          # http://localhost:5173, hot-reload
npm run dev -- --open
```

Other commands:

```bash
npm run check        # svelte-check (type-check)
npm run build        # production static build into build/
npm run preview      # serve the production build locally
```

## Tech stack

- [SvelteKit](https://svelte.dev/docs/kit/) with `@sveltejs/adapter-static`
  (static export).
- [Tailwind CSS v4](https://tailwindcss.com) via `@tailwindcss/vite`.
- TypeScript.

## License

GNU General Public License v3.0 or later. See [LICENSE](../LICENSE).
