# Releasing gray-search

Release tarballs must ship a **static musl binary**. The v0.1.0 tarball
contained a binary dynamically linked against the build box's glibc 2.41 —
on any host with older glibc, ld.so kills the child at exec, and gray logs
`plugin "search" failed to spawn (sidecar child closed stdout)`.

## Build

```sh
rustup target add x86_64-unknown-linux-musl          # once

# ring (rustls' crypto provider) compiles C sources — it needs a musl
# cross-gcc discoverable as `x86_64-linux-musl-gcc`. On this box the
# toolchain lives in ~/.local/toolchains and is NOT on PATH by default:
export PATH="$HOME/.local/toolchains/x86_64-linux-musl-cross/bin:$PATH"
# Debian/Ubuntu alternative: `apt install musl-tools`, then
#   export CC_x86_64_unknown_linux_musl=musl-gcc
# (cc-rs looks for `x86_64-linux-musl-gcc`; the env var overrides that.)

cargo build --release --target x86_64-unknown-linux-musl
```

The musl linker itself needs no toolchain or config — rustc links musl
targets with the shipped rust-lld. The cross-gcc is only for ring's
build script (everything else, including TLS, is pure Rust: rustls +
webpki-roots, no system OpenSSL).

## Verify the artifact is static

```sh
BIN=target/x86_64-unknown-linux-musl/release/gray-search
file "$BIN"        # must say: static-pie linked
ldd "$BIN"         # must say: statically linked / not a dynamic executable

# smoke-test both manifest paths before shipping
"$BIN" manifest
echo '{"id":1,"method":"plugin/manifest"}' | "$BIN"
```

## Package

```sh
x86_64-linux-musl-strip "$BIN"                       # ~7.7MB -> ~5.9MB
VERSION=$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')
tar -czf "gray-search-${VERSION}.tar.gz" -C target/x86_64-unknown-linux-musl/release gray-search
sha256sum "gray-search-${VERSION}.tar.gz"            # goes into the plugin index entry
```

Attach the tarball to a GitHub release named
`gray-search-<version>.tar.gz` and update the index entry in
`vstaln/graysite` (`public/plugins/index.json`) with the new URL and
sha256.
