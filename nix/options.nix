# Options shared by the NixOS and home-manager modules.
self:
{ lib, pkgs }:
{
  enable = lib.mkEnableOption "hjkl, the vim-modal terminal editor";

  package = lib.mkOption {
    type = lib.types.package;
    default =
      let
        inherit (pkgs.stdenv.hostPlatform) system;
      in
      self.packages.${system}.default
        or (throw "hjkl's flake does not support ${system}; set programs.hjkl.package to a build of hjkl for it");
    defaultText = lib.literalExpression "hjkl.packages.\${pkgs.stdenv.hostPlatform.system}.default";
    description = "The hjkl package to install.";
  };

  defaultEditor = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = "Whether to make hjkl the default editor through the {env}`EDITOR` environment variable.";
  };
}
