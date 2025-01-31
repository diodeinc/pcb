{
  description = "A command line tool for designing and ordering PCBs.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs";

  outputs = { self, nixpkgs }:
  let
    supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
    
    forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    
    pkgsFor = system: import nixpkgs {
      inherit system;
    };

    getBinaryAttrs = system: 
      if system == "x86_64-linux" then {
        url = "https://github.com/diodeinc/pcb/releases/v0.0.17/download/x86_64-unknown-linux-gnu_pcb";
        sha256 = "c9a735e24f2c1adfb61cfba9e5829cc2c16690936829afce01ec9244b1c096db";
      } else if system == "aarch64-linux" then {
        url = "https://github.com/diodeinc/pcb/releases/v0.0.17/download/aarch64-unknown-linux-gnu_pcb";
        sha256 = "9f650eb71873aa4b47e43e5a22319bb9ebfcf2925cc65bc7d95126ac59b9266b";
      } else throw "Unsupported system: ${system}";

    mkCaseConverter = pkgs: pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "case_converter";
      version = "1.1.0";
      format = "wheel";

      src = pkgs.fetchPypi {
        inherit pname version;
        format = "wheel";
        dist = "py3";
        python = "py3";
        abi = "none";
        platform = "any";
        sha256 = "1qw6d5ann2lbsiz684l5m53y2qnfilbgbg1h8r2hwhgxlijwdyyh";
      };

      meta = with pkgs.lib; {
        description = "Convert strings between different cases";
        homepage = "https://github.com/chrisdoherty4/python-case-converter";
        license = licenses.mit;
      };
    };

    mkSolidPython = pkgs: pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "solidpython";
      version = "1.1.3";
      format = "wheel";

      src = pkgs.fetchPypi {
        inherit pname version;
        format = "wheel";
        dist = "py3";
        python = "py3";
        abi = "none";
        platform = "any";
        sha256 = "15lbglmyjzidlywphyx90a87bz3cvq3d2k3d6xlkwgpgiax9ja06";
      };

      propagatedBuildInputs = with pkgs.python3.pkgs; [
        euclid3
        pypng
        regex
        setuptools
        pip
      ];

      doCheck = false;

      meta = with pkgs.lib; {
        description = "Python interface to OpenSCAD";
        homepage = "https://github.com/SolidCode/SolidPython";
        license = licenses.lgpl21;
      };
    };

    mkKiKit = pkgs: 
    let
      solidpython = mkSolidPython pkgs;
    in
    pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "KiKit";
      version = "f972993";
      format = "setuptools";
      
      src = pkgs.fetchFromGitHub {
        owner = "yaqwsx";
        repo = "KiKit";
        rev = "f972993dfdda8c17ce18ecde25d674b7c9391dad";
        sha256 = "0pw4nvsm741by2qy4zywf5a4ibrn5ll03s0xiiym6xc99agh5jqx";
      };

      propagatedBuildInputs = with pkgs.python3.pkgs; [
        click
        shapely
        numpy
        markdown2
        pybars3
        solidpython
        pcbnewtransition
        commentjson
      ];

      doCheck = false;

      meta = with pkgs.lib; {
        description = "Automation tools for KiCad";
        homepage = "https://github.com/yaqwsx/KiKit";
        license = licenses.mit;
      };
    };

    mkKinparse = pkgs: pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "kinparse";
      version = "4410797";
      format = "setuptools";
      
      src = pkgs.fetchFromGitHub {
        owner = "LK";
        repo = "kinparse";
        rev = "4410797b9bc521cb0ba677b2ea791ba3b7eeb103";
        sha256 = "0ynn2i3qb5vbgx57rvap6sln10pb01741d5fxsmlbld1jsmlsjh0";
      };

      propagatedBuildInputs = with pkgs.python3.pkgs; [
        pyparsing
      ];

      doCheck = false;

      meta = with pkgs.lib; {
        description = "KiCad netlist parser";
        homepage = "https://github.com/LK/kinparse";
        license = licenses.mit;
      };
    };

    mkEseries = pkgs: pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "eseries";
      version = "1.2.1";
      format = "wheel";

      src = pkgs.fetchPypi {
        inherit pname version;
        format = "wheel";
        dist = "py3";
        python = "py3";
        abi = "none";
        platform = "any";
        sha256 = "0xwkc9w6hzdaqaml04jkds36n293lngdg44aqnkqarl7nimj2s33";
      };

      propagatedBuildInputs = with pkgs.python3.pkgs; [
        docopt
        future
      ];

      meta = with pkgs.lib; {
        description = "E-series calculator";
        homepage = "https://github.com/jlazear/eseries";
        license = licenses.mit;
      };
    };

    mkEasyeda2ato = pkgs: pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "easyeda2ato";
      version = "0.2.7";
      format = "wheel";

      src = pkgs.fetchPypi {
        inherit pname version;
        format = "wheel";
        dist = "py3";
        python = "py3";
        abi = "none";
        platform = "any";
        sha256 = "1h6nrrqdh44qshsgrq10k3lrfs6hqj4wfjl6s6qx0c5449sqi6s6";
      };

      propagatedBuildInputs = with pkgs.python3.pkgs; [
        pydantic
        requests
      ];

      meta = with pkgs.lib; {
        description = "Convert EasyEDA projects to atopile format";
        homepage = "https://github.com/atopile/easyeda2ato";
        license = licenses.mit;
      };
    };

    mkQuartSchema = pkgs: pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "quart_schema";
      version = "0.20.0";
      format = "wheel";

      src = pkgs.fetchPypi {
        inherit pname version;
        format = "wheel";
        dist = "py3";
        python = "py3";
        abi = "none";
        platform = "any";
        sha256 = "0i5nkv4dgslpbbyn3mls14qdy7m11h7g9c0db3z82zg3w49bmm03";
      };

      propagatedBuildInputs = with pkgs.python3.pkgs; [
        quart
        pydantic
        pyhumps
      ];

      meta = with pkgs.lib; {
        description = "A Quart extension to provide schema validation and auto-generated API documentation";
        homepage = "https://github.com/pgjones/quart-schema";
        license = licenses.mit;
      };
    };

    mkAtopile = pkgs: 
    let
      case-converter = mkCaseConverter pkgs;
      eseries = mkEseries pkgs;
      easyeda2ato = mkEasyeda2ato pkgs;
      quart-schema = mkQuartSchema pkgs;
    in
    pkgs.python3.pkgs.buildPythonPackage rec {
      pname = "atopile";
      version = "0.2.69";
      format = "wheel";

      src = pkgs.fetchPypi {
        inherit pname version;
        format = "wheel";
        dist = "py3";
        python = "py3";
        abi = "none";
        platform = "any";
        sha256 = "1ggqhgn7r5fc16js3v0ljpara286bixh4n88paq36kfk8603xjhf";
      };

      nativeBuildInputs = with pkgs.python3.pkgs; [
        pip
      ];

      propagatedBuildInputs = with pkgs.python3.pkgs; [
        antlr4-python3-runtime
        attrs
        case-converter
        cattrs
        click
        deepdiff
        easyeda2ato
        eseries
        fake-useragent
        fastapi
        gitpython
        python-igraph
        jinja2
        natsort
        networkx
        packaging
        pandas
        pint
        pygls
        quart-cors
        quart
        quart-schema
        rich
        ruamel-yaml
        schema
        scipy
        semver
        toolz
        urllib3
        uvicorn
        watchfiles
        pyyaml
      ];

      pythonImportsCheck = [ "atopile" ];

      meta = with pkgs.lib; {
        description = "A new way to design electronics";
        homepage = "https://github.com/atopile/atopile";
        license = licenses.mit;
        maintainers = [ ];
      };
    };

    mkKicadPython = pkgs: 
    let
      kikit = mkKiKit pkgs;
      kinparse = mkKinparse pkgs;
    in
    pkgs.python3.withPackages (ps: [ kikit kinparse ]);

    mkKicadWithScripting = { pkgs, kicadPython }:
      pkgs.kicad.override {
        withScripting = true;
        python3 = kicadPython;
      };

    mkPackage = system:
    let
      pkgs = pkgsFor system;
      binaryAttrs = getBinaryAttrs system;
      kicadPython = mkKicadPython pkgs;
      kicadWithScripting = mkKicadWithScripting { pkgs = pkgs; kicadPython = kicadPython; };
      atopile = mkAtopile pkgs;
      openCmd = pkgs.xdg-utils;
      jre = pkgs.jre;
    in
    pkgs.stdenv.mkDerivation {
      pname = "pcb-cli";
      version = "0.0.17";

      src = pkgs.fetchurl {
        inherit (binaryAttrs) url sha256;
      };

      buildInputs = with pkgs; [
        kicadWithScripting
        bashInteractive
        glibc
        libgcc.lib
        stdenv.cc.cc.lib
        atopile
        openCmd
        jre
      ];

      nativeBuildInputs = [ 
        pkgs.patchelf
        pkgs.makeWrapper
      ];
      
      dontUnpack = false;
      dontStrip = true;

      unpackPhase = ''
        cp $src ./pcb
        chmod +w ./pcb
      '';
      
      phases = [ "unpackPhase" "installPhase" ];

      installPhase = let
        runtimeLibs = with pkgs; [
          kicadWithScripting
          bashInteractive
          glibc
          libgcc.lib
          stdenv.cc.cc.lib
          atopile
          openCmd
          jre
        ];
      in ''
        mkdir -p $out/bin

        # Patch the binary with the correct interpreter and rpath
        patchelf --set-interpreter "$(cat $NIX_CC/nix-support/dynamic-linker)" \
                --set-rpath "${pkgs.lib.makeLibraryPath runtimeLibs}" \
                ./pcb

        # Install the patched binary
        cp ./pcb $out/bin/pcb.real
        chmod +x $out/bin/pcb.real

        # Wrap the binary with the correct environment
        makeWrapper $out/bin/pcb.real $out/bin/pcb \
          --set ATO_PATH "${atopile}/bin/ato" \
          --set KICAD_PYTHON_INTERPRETER "${kicadPython}/bin/python3" \
          --set KICAD_CLI "${kicadWithScripting}/bin/kicad-cli" \
          --prefix PATH : "${openCmd}/bin:${jre}/bin"

        # Generate shell completions
        mkdir -p $out/share/shell-completions
        if $out/bin/pcb autocomplete --shell bash > $out/share/shell-completions/pcb.bash ; then
          echo "Bash completions installed."
        fi
        if $out/bin/pcb autocomplete --shell zsh > $out/share/shell-completions/_pcb ; then
          echo "ZSH completions installed."
        fi
        if $out/bin/pcb autocomplete --shell fish > $out/share/shell-completions/pcb.fish ; then
          echo "Fish completions installed."
        fi
      '';

      meta = with pkgs.lib; {
        description = "CLI tool for designing PCBs.";
        license = licenses.mit;
        maintainers = [ ];
        platforms = [ "x86_64-linux" "aarch64-linux" ];
      };
    };
  in
  {
    packages = forAllSystems (system: {
      pcb-cli = mkPackage system;
      default = mkPackage system;
    });
  };
}
