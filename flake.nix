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

        unfilteredRoot = ./.; # The original, unfiltered source

        src = lib.fileset.toSource {
          root = unfilteredRoot;
          fileset = lib.fileset.unions [
            # Default files from crane (Rust and cargo files)
            (craneLib.fileset.commonCargoSources unfilteredRoot)

            # Also keep any jinja template files
            (lib.fileset.fileFilter (file: file.hasExt "jinja") unfilteredRoot)
            # Also keep any python files
            (lib.fileset.fileFilter (file: file.hasExt "py") unfilteredRoot)

            # Also keep any kicad_sym files (testing)
            (lib.fileset.fileFilter (file: file.hasExt "kicad_sym") unfilteredRoot)

            # keep web files
            (lib.fileset.fileFilter (file: file.hasExt "css") unfilteredRoot)

            # skills
            (lib.fileset.fileFilter (file: file.hasExt "md") unfilteredRoot)
            (lib.fileset.fileFilter (file: file.hasExt "txt") unfilteredRoot)

            # mcp
            (lib.fileset.fileFilter (file: file.hasExt "json") unfilteredRoot)

            # gitignore kicad template
            (lib.fileset.fileFilter ({ name, ... }: name == "gitignore") unfilteredRoot)

          ];
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
            rustc
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
          default = flake-utils.lib.mkApp { drv = pcb; };
        };
      }
    );
}
