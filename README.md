# Sunstead Atlas

One search across your self-hosted data. Atlas indexes each user's files and
photos on Jupiter (notes, calendar and contacts later), shows results and
previews in one place, and links into each app.

Part of the Sunstead suite: Cosmos runs the servers, the apps own their data,
and Atlas is the front door.

## Develop

Requirements: Rust (stable), Node 22, npm.

```sh
npm install
cargo test -p atlas-common     # generates the app's TypeScript types
cargo run -p atlas-server      # API on http://localhost:8080
npm run dev                    # app on http://localhost:1420
```

See [CLAUDE.md](CLAUDE.md) for commands, layout and conventions.

## Run

```sh
docker build -t atlas .
docker run -p 8080:8080 atlas
```

Then open http://localhost:8080.
