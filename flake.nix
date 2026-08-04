{
  description = "Nix store path, hash, and error types shared by FlakeHub Cache services";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1";

    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "https://flakehub.com/f/ipetkov/crane/0";
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      crane,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems =
        f:
        nixpkgs.lib.genAttrs supportedSystems (
          system:
          let
            pkgs = import nixpkgs { inherit system; };

            toolchain = fenix.packages.${system}.stable.withComponents [
              "cargo"
              "clippy"
              "rustc"
              "rustfmt"
              "rust-src"
            ];

            craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);

            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                (craneLib.fileset.commonCargoSources ./.)
                ./src/hash/tests
              ];
            };

            commonArgs = {
              inherit src;
              strictDeps = true;
            };

            cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          in
          f {
            inherit
              pkgs
              toolchain
              craneLib
              commonArgs
              cargoArtifacts
              ;
          }
        );
    in
    {
      packages = forAllSystems (
        { craneLib, commonArgs, cargoArtifacts, ... }:
        {
          default = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoExtraArgs = "--all-features";
            }
          );
        }
      );

      checks = forAllSystems (
        {
          craneLib,
          commonArgs,
          cargoArtifacts,
          ...
        }:
        {
          test = craneLib.cargoTest (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoTestExtraArgs = "--all-features";
            }
          );

          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets --all-features -- --deny warnings";
            }
          );

          fmt = craneLib.cargoFmt { inherit (commonArgs) src; };
        }
      );

      devShells = forAllSystems (
        { pkgs, toolchain, ... }:
        {
          default = pkgs.mkShell {
            packages = [
              toolchain
              pkgs.rust-analyzer
              pkgs.cargo-watch
              pkgs.cargo-deny
              pkgs.editorconfig-checker
            ];
          };
        }
      );
    };
}
