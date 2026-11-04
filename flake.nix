{
  description = "ce-migrate: move canister state onto a cloud engine and verify it";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };

        # icp-cli for the manual flow in MANUAL.md. The Rust tool does not use
        # it, but the runbook does.
        icpVersion = "1.8.0";
        icpSources = {
          x86_64-linux = {
            url = "https://github.com/dfinity/icp-cli/releases/download/v${icpVersion}/icp-cli-x86_64-unknown-linux-gnu.tar.xz";
            hash = "sha256-XJq6C9Z2RQIneTh/ONI6UdFYYkGWzk66F1rroGXrIic=";
          };
          aarch64-linux = {
            url = "https://github.com/dfinity/icp-cli/releases/download/v${icpVersion}/icp-cli-aarch64-unknown-linux-gnu.tar.xz";
            hash = "sha256-UX5JRlIH5flwdF0D/s1oPJnuAiC8/wIhkpPqiIp8SgY=";
          };
          x86_64-darwin = {
            url = "https://github.com/dfinity/icp-cli/releases/download/v${icpVersion}/icp-cli-x86_64-apple-darwin.tar.xz";
            hash = "sha256-lsLxvEfICsrmy4RyYjGKCNDYjpIi1BzTnzymQGaiYBQ=";
          };
          aarch64-darwin = {
            url = "https://github.com/dfinity/icp-cli/releases/download/v${icpVersion}/icp-cli-aarch64-apple-darwin.tar.xz";
            hash = "sha256-PHIVbyDNIbjlFZn/NLSW8D60AJPfTt3zdOurOMjDZD0=";
          };
        };
        icp-cli = pkgs.stdenv.mkDerivation {
          pname = "icp-cli";
          version = icpVersion;
          src = pkgs.fetchurl { inherit (icpSources.${system}) url hash; };
          nativeBuildInputs = [ pkgs.xz ];
          sourceRoot = ".";
          installPhase = ''
            mkdir -p $out/bin
            cp icp-cli-*/icp $out/bin/icp
            chmod +x $out/bin/icp
          '';
          dontFixup = pkgs.stdenv.hostPlatform.isDarwin;
        };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "ce-migrate";
          version = "0.1.0";
          src = ./.;
          # Run `cargo generate-lockfile` once so this file exists, then
          # `nix build` is reproducible against it.
          cargoLock.lockFile = ./Cargo.lock;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rustfmt
            pkgs.clippy
            icp-cli
          ];
        };
      }
    );
}
