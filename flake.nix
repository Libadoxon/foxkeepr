{
  description = "Small user service to move firefox windows to right workspace on hyperland";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    systems.url = "github:nix-systems/default";
    flake-utils.url = "github:numtide/flake-utils";

    treefmt-nix.url = "github:numtide/treefmt-nix";

    crane.url = "github:ipetkov/crane";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      systems,
      flake-utils,
      treefmt-nix,
      crane,
      advisory-db,
      rust-overlay,
    }:
    flake-utils.lib.eachSystem (import systems) (
      system:
      let
        pkgs = (import nixpkgs) {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        inherit (pkgs) lib;
        craneLib = (crane.mkLib pkgs).overrideToolchain (
          p: p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
        );

        buildInputs = with pkgs; [ ];
        nativeBuildInputs = with pkgs; [ ];

        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          strictDeps = true;

          inherit buildInputs nativeBuildInputs;
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        foxkeepr = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            meta.description = "Small user service to move firefox windows to right workspace on hyperland";
          }
        );

        treefmtEval = treefmt-nix.lib.evalModule pkgs ./treefmt.nix;
      in
      {
        formatter = treefmtEval.config.build.wrapper;

        checks = {
          inherit foxkeepr;
          formatting = treefmtEval.config.build.check self;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          audit = craneLib.cargoAudit { inherit src advisory-db; };
          nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
              cargoNextestPartitionsExtraArgs = "--no-tests=pass";
            }
          );
        };

        packages = {
          inherit foxkeepr;
          default = foxkeepr;
        };

        devShells.default = craneLib.devShell { packages = buildInputs ++ nativeBuildInputs; };
      }
    );
}
