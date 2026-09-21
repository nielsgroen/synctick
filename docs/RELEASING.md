# Publishing Synctick

Publish only `synctick-derive`, `synctick`, and `synctick-bevy`. The examples
inherit `publish = false`. All three libraries start at version 0.1.0.

## Before release

1. Confirm ownership or availability of all three names on crates.io. Create an
   account, verify your email, and run `cargo login` locally with a publishing
   token. Never commit credentials.
2. Update the changelog and versions together. The core pins its derive dependency
   exactly because generated code must match the core API. Keep the adapter's
   core requirement compatible. Cargo versions are separate from the framework
   protocol, recording format, and each game's compatibility version.
3. Run the workspace tests, formatting, Clippy, Rustdoc, and aliased consumer
   checks listed in the README. Run the library MSRV check:

   ```sh
   cargo +1.92.0 check -p synctick -p synctick-bevy -p synctick-derive --all-targets --locked
   ```

4. Inspect `cargo package --list -p NAME` for each library. Each must include
   README.md, LICENSE, and NOTICE. Keep the package license files synchronized
   with the repository copies. Mark the changelog with the release date, commit
   and push the release preparation. In GitHub, select **Actions → CI → Run
   workflow**, choose the release branch, and enable **release_checks**. Require
   that run to pass on the release commit; ordinary push/PR CI omits Rustdoc,
   MSRV, and package verification to reduce build minutes.

## Package verification before first publication

With current stable Cargo, verify all three packages together before any upload:

```sh
cargo package -p synctick-derive -p synctick -p synctick-bevy
```

Cargo stages the selected unpublished dependencies in a temporary local registry
for this check. The opt-in CI release checks run this command as well. Use `--allow-dirty` only during
local preparation; release from a clean committed tree.

## First publication

Run each pair in order; stop on any error. A dependent crate's dry run requires
its dependencies to exist in the registry. Wait for registry availability before
moving to the next pair. Do not use `--no-verify` for publication.

```sh
cargo publish -p synctick-derive --dry-run
cargo publish -p synctick-derive
cargo publish -p synctick --dry-run
cargo publish -p synctick
cargo publish -p synctick-bevy --dry-run
cargo publish -p synctick-bevy
```

After publication, compile a fresh consumer using registry dependencies (including
renaming `synctick` and deriving both traits), and check all three docs.rs builds.
Tag the released commit with `git tag -a v0.1.0 -m "Synctick 0.1.0"` and
`git push origin v0.1.0`. Published versions cannot be overwritten; fixes need
a new version. If a release stops partway through, inspect registry state and
resume with the unpublished packages.

See the [Cargo publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html).
