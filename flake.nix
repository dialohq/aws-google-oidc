{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    naersk = {
      url = "github:nix-community/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    nixpkgs,
    flake-utils,
    naersk,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {inherit system;};
        naersk' = pkgs.callPackage naersk {};
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
          ];
        };
      in {
        packages.default = naersk'.buildPackage {
          inherit src;
          pname = "aws-google-oidc";
          version = "0.1.0";
          overrideMain = old: {
            nativeBuildInputs = (old.nativeBuildInputs or []) ++ [pkgs.removeReferencesTo];
            disallowedReferences = (old.disallowedReferences or []) ++ [old.cratesio_sources];
            postFixup = (old.postFixup or "") + ''
              remove-references-to -t "$cratesio_sources" "$out/bin/aws-google-oidc"
            '';
          };
        };
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [rust-analyzer rustfmt cargo rustc clippy];
        };
      }
    );
}
