# Pomodoro

A small, nature-themed Pomodoro widget for the Windows desktop. Its focus sessions and breaks are
backed by layered ambience (rain, fire, thunder) and your own music. It is built with Tauri and
Rust so it stays light enough to leave running all day.

See [docs/PLAN.md](docs/PLAN.md) for the background, the stack and the roadmap.

## Run it

Requires Node 20+ and Rust (stable).

```sh
npm install
npm run tauri dev      # dev window with hot reload
npm run tauri build    # release installer (NSIS) in src-tauri/target/release/bundle
```

## Keys

| Key | Action |
|---|---|
| Space | Start / pause |
| R | Reset the current phase |
| S | Skip to the next phase |
