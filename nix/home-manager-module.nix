self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.hjkl;
  tomlFormat = pkgs.formats.toml { };
in
{
  options.programs.hjkl = import ./options.nix self { inherit lib pkgs; } // {
    settings = lib.mkOption {
      inherit (tomlFormat) type;
      default = { };
      example = lib.literalExpression ''
        {
          editor.leader = ",";
          options.scrolloff = 8;
          theme.name = "dark";
        }
      '';
      description = ''
        Configuration written to {file}`$XDG_CONFIG_HOME/hjkl/config.toml`.
        hjkl layers it over its bundled defaults, so only the keys to change
        need to appear, and it rejects unknown keys.

        The file is a link to a read-only file in the Nix store, and hjkl
        refuses to rewrite a read-only config. When it saves a setting back at
        runtime (`:set`, resizing a dock), that save fails with a "couldn't
        save" warning and the change lasts only for the session, so change
        options set here in Nix instead.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    home.sessionVariables = lib.mkIf cfg.defaultEditor { EDITOR = "hjkl"; };

    xdg.configFile."hjkl/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = tomlFormat.generate "hjkl-config.toml" cfg.settings;
    };
  };
}
