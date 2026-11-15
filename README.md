# Kairo

[![License](https://img.shields.io/github/license/bongodevs/Kairo?style=for-the-badge)](LICENSE)

Kairo is a Rust-based audio node for Discord bots. It implements the Lavalink v4 REST and WebSocket protocol, so existing Lavalink-compatible clients can connect using the node's address and password.

- Audio sources as loadable plugins, filters, crossfade, lyrics, and optional Prometheus metrics
- One workspace, no submodules: `crates/audio`, `crates/core`, `crates/player`, `crates/voice` and the plugin API
- Source integrations ship as `.kai` plugins from [kairo-sources-plugin](https://github.com/bongodevs/kairo-sources-plugin)

## Why this exists

Kairo started as a hobby project. We wanted more control over the audio node we use, so we decided to build our own.

We have a lot of respect for Lavalink and the people behind it. The protocol and ecosystem it built gave us a solid foundation to work from. Without Lavalink, Kairo probably wouldn't exist.

Kairo isn't here to replace Lavalink. It's simply our own take on the problem, built because we wanted to learn, experiment, and make something of our own.

## Build from source

A stable Rust toolchain is enough; `rust-toolchain.toml` pins the components the checks need.

```sh
cargo build --release
```

The binary lands at `target/release/kairo`. Run the whole quality gate with:

```sh
./check.sh
```

That formats, runs clippy with warnings denied across the workspace, and runs every test.

## Run the node

```sh
cp config.toml config.local.toml   # optional local overrides, ignored by git
cargo run --release
```

Kairo reads the config path from the first command-line argument, then `KAIRO_CONFIG`, then the
first file that exists of `config.local.toml` and `config.toml`. Missing files fall back to the
built-in defaults, so the node starts and logs that it is running unconfigured.

Point your Lavalink-compatible client at `http://<host>:2333` and use the password from
`lavalink.server.password`. For a quick check, request the node info endpoint:

```sh
curl -H 'Authorization: your-password' http://localhost:2333/v4/info
```

## Configuration

[`config.toml`](config.toml) documents every setting, including the commented alternatives. Keep
secrets in `config.local.toml`; it is ignored by git and wins over `config.toml`.

Sources are plugins, not built-in modules. The bundled provider library lives in its own repository,
[`kairo-sources-plugin`](https://github.com/bongodevs/kairo-sources-plugin), and is built for this
plugin API.

## Plugins

Every installed plugin is one file in the plugin directory:

```toml
[plugins]
directory = "plugins"
```

The host loads `<directory>/<name>.kai`, stages it under `<directory>/.run`, and checks that the
library is native to the running OS before calling into it. To install from a public release, name
the repository and tag:

```toml
[[plugin]]
name = "sources"
repo = "bongodevs/kairo-sources-plugin"
tag = "0.0.1-beta"
```

At startup the host downloads `{name}-{os}-{arch}.kai` from the release, verifies the optional
`.sha256`, writes `<directory>/<name>.kai`, and records the tag, asset and SHA-256 in
`<directory>/plugins.lock`. A matching lock and file skip the network; a replaced file under the same
tag fails instead of loading. Releases must be public: there is no token.

To build the provider plugin yourself, see
[kairo-sources-plugin](https://github.com/bongodevs/kairo-sources-plugin) and copy its library to
`<directory>/sources.kai`.

## License

Kairo is MIT licensed; see [LICENSE](LICENSE). The Lavalink protocol notice is in [NOTICES.md](NOTICES.md).

## Star History

<a href="https://www.star-history.com/?type=date&legend=top-left&repos=bongodevs%2FKairo">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=bongodevs/Kairo&type=date&theme=dark&legend=top-left" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=bongodevs/Kairo&type=date&legend=top-left" />
    <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=bongodevs/Kairo&type=date&theme=dark&legend=top-left" />
  </picture>
</a>
