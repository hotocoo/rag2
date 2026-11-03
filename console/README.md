# RAG2 console

Client-side Leptos app served by `rag2 serve` when `server.console_dir` points at its build.

```sh
git submodule update --init console/vendor/hesperides   # the design system
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
cd console && trunk build --release        # output: console/dist
```

Then set `server.console_dir: /path/to/console/dist` and open `http://<server>/`.
The console calls the same REST API as every other client and needs an API key (or OIDC token)
whose roles cover what you want to do; it stores the key in session storage only.

## Design system

The console is styled entirely by [Hesperides](https://github.com/hotocoo/hesperides-design-system),
vendored as a pinned git submodule in `vendor/hesperides`. Trunk links its `tokens/tokens.css` and
`components/bundle.css` unchanged and copies its fonts, icon sprite and mark into `dist/`.

| File | Holds |
| --- | --- |
| `src/ui.rs` | Leptos versions of the Hesperides components the console uses (Button, Field, inputs, Card, Alert, Badge, Stat, Table, EmptyState, CodeBlock, Icon). Each renders the same `hs-*` markup as the upstream React component. |
| `src/copy.rs` | Every user-facing string, in the Hesperides voice. |
| `console.css` | Layout glue only (the shell, gaps the React layout primitives set inline). Every value is a Hesperides token. |

To update the design system: `git -C console/vendor/hesperides pull` (or check out a tag), rebuild,
and commit the new submodule pointer.
