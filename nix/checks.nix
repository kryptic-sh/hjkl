{
  pkgs,
  self,
  nixpkgs,
  home-manager,
}:
let
  inherit (pkgs) lib hjkl;
  inherit (pkgs.stdenv.hostPlatform) isDarwin isLinux;

  # Turns evaluation-time assertions into a check derivation: a false
  # condition fails `nix flake check` with its message.
  evalCheck =
    name: assertions:
    assert lib.all ({ cond, msg }: lib.assertMsg cond msg) assertions;
    pkgs.runCommand name { } "touch $out";

  settings = {
    editor.leader = ",";
    options.scrolloff = 8;
    theme.name = "dark";
  };

  # Written by hand rather than rendered, so the module's output is compared
  # against an independent statement of the file it should produce.
  expectedConfig = pkgs.writeText "expected-config.toml" ''
    [editor]
    leader = ","

    [options]
    scrolloff = 8

    [theme]
    name = "dark"
  '';

  compareToml = pkgs.writeText "compare-toml.py" ''
    import json
    import sys
    import tomllib

    got, want = (tomllib.load(open(path, "rb")) for path in sys.argv[1:])
    if got != want:
        sys.exit(
            "rendered config.toml differs from the expected one\n"
            f"  got:  {json.dumps(got, sort_keys=True)}\n"
            f"  want: {json.dumps(want, sort_keys=True)}"
        )
  '';

  home = home-manager.lib.homeManagerConfiguration {
    inherit pkgs;
    modules = [
      self.homeManagerModules.default
      {
        home = {
          username = "hjkl";
          homeDirectory = if isDarwin then "/Users/hjkl" else "/home/hjkl";
          stateVersion = lib.trivial.release;
        };
        programs.hjkl = {
          enable = true;
          defaultEditor = true;
          inherit settings;
        };
      }
    ];
  };
in
{
  inherit hjkl;

  smoke = pkgs.runCommand "hjkl-smoke" { nativeBuildInputs = [ hjkl ]; } ''
    export HOME=$TMPDIR

    version=$(hjkl --version)
    if [[ $version != "hjkl ${hjkl.version}" ]]; then
      echo "hjkl --version printed '$version', expected 'hjkl ${hjkl.version}'" >&2
      exit 1
    fi

    # Replay the wrapper's environment without its final exec, from an empty
    # PATH, so the tools below can only come from what the wrapper adds; then
    # build a shared library the way hjkl-bonsai builds a grammar. The
    # subshell inherits errexit, so any missing tool fails the check.
    sed '/^exec /d' ${lib.getExe hjkl} > wrapper-env
    (
      PATH=
      source ./wrapper-env
      git --version
      cc --version
      c++ --version
      echo 'int parser(void) { return 0; }' > grammar.c
      cc -O2 -fPIC -shared grammar.c -o grammar.so
    )
    [[ -s grammar.so ]]

    touch $out
  '';

  home-manager-module = evalCheck "hjkl-home-manager-module" [
    {
      cond = lib.elem hjkl home.config.home.packages;
      msg = "programs.hjkl.enable does not install the package";
    }
    {
      cond = home.config.home.sessionVariables.EDITOR or null == "hjkl";
      msg = "programs.hjkl.defaultEditor does not set EDITOR";
    }
  ];

  home-manager-settings =
    pkgs.runCommand "hjkl-home-manager-settings" { nativeBuildInputs = [ pkgs.python3 ]; }
      ''
        python3 ${compareToml} \
          ${home.config.xdg.configFile."hjkl/config.toml".source} \
          ${expectedConfig}
        touch $out
      '';

  nixfmt = pkgs.runCommand "hjkl-nixfmt" { nativeBuildInputs = [ pkgs.nixfmt ]; } ''
    nixfmt --check ${../flake.nix} ${./.}/*.nix
    touch $out
  '';
}
// lib.optionalAttrs isLinux {
  nixos-module =
    let
      nixos =
        extraConfig:
        (nixpkgs.lib.nixosSystem {
          modules = [
            self.nixosModules.default
            {
              nixpkgs.pkgs = pkgs;
              system.stateVersion = lib.trivial.release;
              programs.hjkl = {
                enable = true;
                defaultEditor = true;
              };
            }
            extraConfig
          ];
        }).config;
      enabled = nixos { };
      userEditor = nixos { environment.sessionVariables.EDITOR = "vi"; };
    in
    evalCheck "hjkl-nixos-module" [
      {
        cond = lib.elem hjkl enabled.environment.systemPackages;
        msg = "programs.hjkl.enable does not install the package";
      }
      # environment.variables is what reaches the shell, and also holds nano's
      # fallback EDITOR.
      {
        cond = enabled.environment.variables.EDITOR or null == "hjkl";
        msg = "programs.hjkl.defaultEditor does not set EDITOR";
      }
      {
        cond = userEditor.environment.variables.EDITOR or null == "vi";
        msg = "programs.hjkl.defaultEditor overrides the user's own EDITOR";
      }
    ];
}
