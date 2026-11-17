self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.hjkl;
in
{
  options.programs.hjkl = import ./options.nix self { inherit lib pkgs; };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    # Same option and priority as programs.neovim.defaultEditor: above the
    # nano fallback, below a plain user assignment.
    environment.sessionVariables.EDITOR = lib.mkIf cfg.defaultEditor (lib.mkOverride 900 "hjkl");
  };
}
