# RAG2 console

Client-side Leptos app served by `rag2 serve` when `server.console_dir` points at its build.

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
cd console && trunk build --release        # output: console/dist
```

Then set `server.console_dir: /path/to/console/dist` and open `http://<server>/`.
The console calls the same REST API as every other client and needs an API key (or OIDC token)
whose roles cover what you want to do; it stores the key in session storage only.
