{
  description = "Cloudflare R2 free-tier usage monitor";

  inputs = {
    # Stable nixpkgs, matching the channel the target server tracks. Do not
    # switch this to nixos-unstable. nixos-26.11 did not exist when this was
    # written; check channels.nixos.org before bumping.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  };

  # NOTE: flake `nixConfig` is only applied when the flake is trusted, i.e. when
  # the user passes `--accept-flake-config` or the flake is trusted through
  # nix.conf. Otherwise Nix prints "ignoring untrusted flake configuration
  # setting" and carries on with the system configuration. On a server the
  # *effective* equivalent belongs in nix.settings:
  #
  #   nix.settings = {
  #     substituters = [
  #       "https://mooling.cachix.org"
  #       "https://cache.nixos.org"
  #     ];
  #     trusted-public-keys = [
  #       "mooling.cachix.org-1:9XeEiZWSaN4ngXeefj2BSjXFNSmbKpfrisiUJrf9A6M="
  #       "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
  #     ];
  #   };
  #
  # Always list cache.nixos.org as well: assigning `substituters` replaces the
  # default list rather than appending to it.
  #
  # These two values must stay in sync with CACHIX_CACHE_NAME in
  # .github/workflows/build.yml, which is what CI pushes to. They must also stay
  # literal strings: Nix reads `nixConfig` with a restricted evaluator, so
  # neither a `let` binding nor string interpolation is accepted there (verified
  # with Nix 2.20).
  nixConfig = {
    extra-substituters = [ "https://mooling.cachix.org" ];

    # The public signing key of the cache above, from `cachix use mooling` or
    # https://app.cachix.org/cache/mooling. Wrong key means every path from the
    # cache is silently rejected as untrusted, which looks like the cache being
    # empty rather than like a misconfiguration.
    extra-trusted-public-keys = [
      "mooling.cachix.org-1:9XeEiZWSaN4ngXeefj2BSjXFNSmbKpfrisiUJrf9A6M="
    ];
  };

  outputs =
    { self, nixpkgs, ... }:
    let
      # Keep in sync with meta.platforms in nix/package.nix. Darwin is absent on
      # purpose: the tool ships as a systemd timer, and every extra system is one
      # more platform `nix flake check` has to evaluate.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs systems;

      # `legacyPackages` avoids instantiating a second package set per system.
      pkgsFor = system: nixpkgs.legacyPackages.${system};

      # `import ./nix/module.nix self` applies the module's leading `self:`
      # argument; the result is an ordinary NixOS module.
      module = import ./nix/module.nix self;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkg = (pkgsFor system).callPackage ./nix/package.nix { };
        in
        {
          cloudflare-r2-monitor = pkg;
          default = pkg;
        }
      );

      # `nix run` entry point. Deriving it from self.packages keeps it in sync
      # with the package output instead of importing package.nix twice.
      apps = forAllSystems (
        system:
        {
          cloudflare-r2-monitor = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/cf-r2-monitor";
          };
          default = self.apps.${system}.cloudflare-r2-monitor;
        }
      );

      # Toolchain only: nothing here pulls in self.packages, so `nix develop`
      # does not have to build the crate first and needs no network.
      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.rustc
              pkgs.rustfmt
            ];
          };
        }
      );

      # `nixfmt-rfc-style` is an alias for `nixfmt` on 26.05 and warns on use; it
      # is kept because it is the name the flake template documents.
      formatter = forAllSystems (system: (pkgsFor system).nixfmt-rfc-style);

      # `final.stdenv.hostPlatform.system` rather than the deprecated
      # `final.system`, which only exists as a warning alias on 26.05.
      overlays.default = final: prev: {
        cloudflare-r2-monitor = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      nixosModules = {
        default = module;
        cloudflare-r2-monitor = module;
      };
    };
}
