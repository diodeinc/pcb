{
  description = "Nix flake for the pcb CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    { self, nixpkgs, crane, ... }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = lib.genAttrs systems;
      workspaceCargo = builtins.fromTOML (builtins.readFile ./Cargo.toml);

      packageFor =
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          craneLib = crane.mkLib pkgs;

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./crates/pcb/src/fortune.txt
              ./crates/pcb/src/templates
              ./crates/pcb-component-gen/templates
              ./crates/pcb-ipc2581-tools/src/commands/html_template.html.jinja
              ./crates/pcb-ipc2581-tools/src/commands/style.css
              ./crates/pcb-layout/src/scripts
              ./docs/pages
              ./stdlib
            ];
          };

          commonArgs = {
            pname = "pcb";
            version = workspaceCargo.workspace.package.version;
            inherit src;
            strictDeps = true;
            doCheck = false;
            cargoExtraArgs = "-p pcb";

            nativeBuildInputs = with pkgs; [
              makeWrapper
              pkg-config
            ];

            buildInputs = with pkgs; [
              python312
              python312Packages.kicad
            ];
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;

            postFixup = ''
              wrapProgram $out/bin/pcb \
                --set KICAD_PYTHON_SITE_PACKAGES "${pkgs.python312Packages.kicad}/${pkgs.python312.sitePackages}" \
                --set KICAD_PYTHON_INTERPRETER "${pkgs.python312}/bin/python"
            '';

            meta = with lib; {
              description = "CLI for circuit board design";
              homepage = "https://github.com/diodeinc/pcb";
              license = licenses.mit;
              mainProgram = "pcb";
              platforms = platforms.unix;
            };
          }
        );
    in
    {
      packages = forAllSystems (
        system:
        let
          pcb = packageFor system;
        in
        {
          default = pcb;
          inherit pcb;
        }
      );

      apps = forAllSystems (
        system:
        let
          pcb = self.packages.${system}.pcb;
        in
        {
          default = {
            type = "app";
            program = "${pcb}/bin/pcb";
          };
          pcb = {
            type = "app";
            program = "${pcb}/bin/pcb";
          };
        }
      );
    };
}
