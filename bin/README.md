# Prebuilt Axon binaries

Self-contained, single-file builds of Axon — the embedded dashboard, SQLite, and all parsers
are baked in. No runtime dependencies, no toolchain, no Node. Just download, make executable,
and run; bare `axon` scans your logs, opens the browser, and serves the live dashboard.

| File | Platform | Notes |
|---|---|---|
| `axon-macos` | macOS (universal: Apple Silicon + Intel) | Unsigned — clear Gatekeeper with `xattr -d com.apple.quarantine axon-macos` |
| `axon-linux-x64` | Linux x86_64 (glibc) | most desktops/servers |
| `axon-linux-arm64` | Linux aarch64 (glibc) | Graviton, Raspberry Pi, Asahi, ARM cloud |

```bash
chmod +x axon-macos && ./axon-macos          # → http://127.0.0.1:7777
./axon-macos --scan-only                      # headless JSON, no server
./axon-macos --port 8080 --no-open            # custom port, don't open a browser
```

Verify integrity against `SHA256SUMS`:

```bash
shasum -a 256 -c SHA256SUMS    # macOS
sha256sum -c SHA256SUMS        # Linux
```

Built with `--release` (LTO, stripped) for `v0.1.0`. Rebuild from source any time with
`cargo build --release`.
