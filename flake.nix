{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        inherit (pkgs) lib;

        craneLib = crane.mkLib pkgs;
        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          doCheck = false;

          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.openssl.dev
          ];

          buildInputs = [
            pkgs.pkg-config
            pkgs.openssl
          ];

          BUILDIFIER_BIN = "${pkgs.bazel-buildtools}/bin/buildifier";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        pcb = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "pcb";
            inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;

            prePatch = ''
              substituteInPlace crates/pcb-layout/src/lib.rs \
                --replace-fail "scripts/update_layout_file.py" ${crates/pcb-layout/src/scripts/update_layout_file.py}
            '';

            doCheck = false;
          }
        );
      in
      {
        packages = {
          default = pcb;
        };

        apps = {
          default = flake-utils.lib.mkApp {
            drv = pcb;
          };
        };
      }
    );
}
