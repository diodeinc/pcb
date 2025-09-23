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
        src = pkgs.lib.cleanSourceWith {
          src = lib.cleanSource ./.;

          filter = (
            orig_path: type:
            pkgs.lib.hasSuffix ".py" (baseNameOf (toString orig_path))
            || (craneLib.filterCargoSources orig_path type)
          );

          name = "pcb-source";
        };

        commonArgs = {
          pname = "pcb";
          inherit src;
          strictDeps = true;
          doCheck = false;

          nativeBuildInputs = with pkgs; [
            pkg-config
            openssl.dev
            makeWrapper
          ];

          buildInputs = with pkgs; [
            pkg-config
            openssl
            python312
            python312Packages.kicad
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        pcb = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;

            doCheck = false;

            postFixup = ''
              wrapProgram $out/bin/pcb \
                --set KICAD_PYTHON_SITE_PACKAGES "${pkgs.python312Packages.kicad}/${pkgs.python312.sitePackages}" \
                --set KICAD_PYTHON_INTERPRETER "${pkgs.python312}/bin/python"
            '';
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
