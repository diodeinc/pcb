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
      workspaceCargo = lib.importTOML ./Cargo.toml;
      pcbCargo = lib.importTOML ./crates/pcb/Cargo.toml;
      pcbcCargo = lib.importTOML ./crates/pcbc/Cargo.toml;
      versionFor =
        cargo:
        let
          version = cargo.package.version;
        in
        if builtins.isAttrs version && (version.workspace or false) then
          workspaceCargo.workspace.package.version
        else
          version;
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
        };

      packagesFor =
        system:
        let
          pkgs = pkgsFor system;
          craneLib = crane.mkLib pkgs;

          cargoSrc = lib.fileset.toSource {
            root = ./.;
            fileset = craneLib.fileset.commonCargoSources ./.;
          };

          pcbcSrc = lib.fileset.toSource {
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

          pcbArgs = {
            pname = "pcb";
            version = versionFor pcbCargo;
            src = cargoSrc;
            strictDeps = true;
            doCheck = false;
            cargoExtraArgs = "-p pcb";
          };

          pcbcArgs = {
            pname = "pcbc";
            version = versionFor pcbcCargo;
            src = pcbcSrc;
            strictDeps = true;
            doCheck = false;
            cargoExtraArgs = "-p pcbc";
            nativeBuildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              pkgs.makeWrapper
            ];
            buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              pkgs.python312
              pkgs.python312Packages.kicad
            ];
          };

          pcbCargoArtifacts = craneLib.buildDepsOnly pcbArgs;
          pcbcCargoArtifacts = craneLib.buildDepsOnly pcbcArgs;

          pcb = craneLib.buildPackage (
            pcbArgs
            // {
              cargoArtifacts = pcbCargoArtifacts;

              meta = with lib; {
                description = pcbCargo.package.description;
                homepage = "https://github.com/diodeinc/pcb";
                license = licenses.mit;
                mainProgram = "pcb";
                platforms = platforms.unix;
              };
            }
          );

          pcbc = craneLib.buildPackage (
            pcbcArgs
            // {
              cargoArtifacts = pcbcCargoArtifacts;

              postInstall = ''
                mkdir -p "$out/lib"
                cp -R ${pcbcSrc}/lib/std "$out/lib/std"
                chmod -R u+w "$out/lib/std"
              '';

              postFixup = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                wrapProgram "$out/bin/pcbc" \
                  --set KICAD_PYTHON_SITE_PACKAGES "${pkgs.python312Packages.kicad}/${pkgs.python312.sitePackages}" \
                  --set KICAD_PYTHON_INTERPRETER "${pkgs.python312}/bin/python"
              '';

              meta = with lib; {
                description = pcbcCargo.package.description;
                homepage = "https://github.com/diodeinc/pcb";
                license = licenses.mit;
                mainProgram = "pcbc";
                platforms = platforms.unix;
              };
            }
          );
        in
        {
          inherit pcb pcbc;
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          packages = packagesFor system;
        in
        {
          default = packages.pcb;
          inherit (packages) pcb pcbc;
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          pcb = self.packages.${system}.pcb;
          pcbc = self.packages.${system}.pcbc;
        in
        {
          pcb-package = pkgs.runCommand "pcb-package" { } ''
            export HOME="$TMPDIR/home"
            test -x "${pcb}/bin/pcb"
            test ! -e "${pcb}/bin/pcbc"
            "${pcb}/bin/pcb" toolchain show --offline | grep -Fqx "shim: ${pcb.version}"
            touch "$out"
          '';

          pcbc-package = pkgs.runCommand "pcbc-package" { } ''
            test -x "${pcbc}/bin/pcbc"
            test ! -e "${pcbc}/bin/pcb"
            test -f "${pcbc}/lib/std/pcb.toml"
            test "$("${pcbc}/bin/pcbc" --version)" = "pcbc ${pcbc.version}"
            touch "$out"
          '';

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
