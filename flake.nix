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
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          config.problems.handlers =
            lib.optionalAttrs (lib.hasSuffix "-darwin" system)
              {
                kicad-base.broken = "warn";
              };
        };

      packageFor =
        system:
        let
          pkgs = pkgsFor system;
          craneLib = crane.mkLib pkgs;

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./crates/pcbc/src/templates
              ./crates/pcb-component-gen/templates
              ./crates/ipc2581/IPC-2581C.xsd
              ./crates/pcb-ipc2581-tools/src/commands/html_template.html.jinja
              ./crates/pcb-ipc2581-tools/src/commands/style.css
              ./crates/pcb-layout/src/scripts
              ./lib/pcb.toml
              ./lib/std
            ];
          };

          commonArgs = {
            pname = "pcb";
            version = workspaceCargo.workspace.package.version;
            inherit src;
            strictDeps = true;
            doCheck = false;
            cargoExtraArgs = "-p pcb -p pcbc";

            nativeBuildInputs = with pkgs; [
              makeWrapper
              pkg-config
            ];

            buildInputs = with pkgs; [
              openssl
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

            postInstall = ''
              mkdir -p "$out/lib"
              cp -R ${src}/lib/std "$out/lib/std"
              chmod -R u+w "$out/lib/std"
            '';

            postFixup = ''
              for binary in pcb pcbc; do
                wrapProgram "$out/bin/$binary" \
                  --set KICAD_PYTHON_SITE_PACKAGES "${pkgs.python312Packages.kicad}/${pkgs.python312.sitePackages}" \
                  --set KICAD_PYTHON_INTERPRETER "${pkgs.python312}/bin/python"
              done
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
          pcbc = pcb // {
            meta = pcb.meta // {
              mainProgram = "pcbc";
            };
          };
        in
        {
          default = pcb;
          inherit pcb pcbc;
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          pcbc = self.packages.${system}.pcbc;
        in
        {
          pcbc-stdlib-installed = pkgs.runCommand "pcbc-stdlib-installed" { } ''
            test -f "${pcbc}/lib/std/pcb.toml"
            touch "$out"
          '';
        }
      );

      apps = forAllSystems (
        system:
        let
          pcb = self.packages.${system}.pcb;
          pcbc = self.packages.${system}.pcbc;
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
          pcbc = {
            type = "app";
            program = "${pcbc}/bin/pcbc";
          };
        }
      );
    };
}
