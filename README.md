# Portal

Portal is a native, local-first desktop workspace built with Rust and Slint. It
hosts a small set of explicit, compile-time internal applications rather than a
runtime plugin system. Nexus is the current AI Content Studio: it owns local
conversations, characters, media, generation jobs, and the RunPod-backed
FLUX.2 TextToImage path.

Portal 0.8 is a foundation under active development. Implemented, partial, and
planned capabilities are distinguished in the canonical context document.

## Run locally

```bash
cd portal-app
cargo run
```

Runtime data is written under `portaldata/` by default and is not committed.

## Start here

- [Portal context](PORTAL_CONTEXT.md) — canonical product and architecture orientation
- [Contributor instructions](AGENTS.md) — repository working rules
- [Internal app development guide](docs/APP_DEVELOPMENT_GUIDE.md) — how to add a built-in app
- [Nexus and RunPod](docs/runpod-nexus.md) — provider setup, worker, cost, and failure behavior
