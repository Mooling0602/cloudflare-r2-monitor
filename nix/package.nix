{
  lib,
  rustPlatform,
}:

# Called from flake.nix through `pkgs.callPackage ./nix/package.nix { }`, so the
# whole dependency set of nixpkgs is available when this function is called.
let
  # Version and description live in Cargo.toml, so "0.1.0" is written once.
  #
  # Trade-off: importTOML takes a *path*, so Nix copies Cargo.toml into the store
  # and its hash becomes part of the derivation. Editing the description alone
  # therefore invalidates the build cache. For a single-crate project that is
  # cheaper than keeping a second copy of the version number in the flake.
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "cloudflare-r2-monitor";
  inherit (cargoToml.package) version;

  # cleanSourceFilter drops VCS metadata, editor backups and object files but
  # keeps Cargo.lock, which the build needs. It does not cover `target/`, which
  # is over a gigabyte of local build output here, nor `.github/` and `docs/`,
  # which are excluded so that editing CI or documentation does not change the
  # source hash and trigger a rebuild.
  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      lib.cleanSourceFilter path type
      && !(builtins.elem (baseNameOf (toString path)) [ "target" ".github" "docs" ]);
  };

  # Dependency hashes come from the lockfile itself: nixpkgs' importCargoLock
  # reads every crate's `checksum` from Cargo.lock and fetches from
  # crates.io. That is why there is no `cargoHash` here — there is nothing to
  # guess or to manually refresh when dependencies change.
  cargoLock.lockFile = ../Cargo.lock;

  # reqwest is built with rustls and webpki-roots, so no openssl, pkg-config or
  # system certificate store is needed at build or run time. `ring` compiles its
  # C code with the stock stdenv compiler, which buildRustPackage already puts
  # on PATH; nothing else has to be added.

  # doCheck is left at its default, which runs `cargo test`. This was verified in
  # the nix build sandbox: all 70 tests pass (63 lib + 1 main + 6 integration),
  # including the ones that bind a loopback listener, so the sandbox does provide
  # loopback and no bypass is needed.

  # No homepage: the repository location is not known yet. A placeholder URL
  # would be published in `meta`, in `nix search` output and in the store, so the
  # field is omitted until a real URL exists. No `maintainers` for the same
  # reason.

  meta = {
    description = cargoToml.package.description;
    license = lib.licenses.mit;

    # `nix run` and the NixOS module both rely on this to locate the binary.
    mainProgram = "cf-r2-monitor";

    # Keep in sync with `systems` in flake.nix. Darwin is intentionally absent:
    # the tool is deployed as a systemd timer job.
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}
