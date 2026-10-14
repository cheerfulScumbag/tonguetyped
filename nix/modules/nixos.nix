# NixOS module: installs TongueTyped system-wide and starts the daemon in
# every user's graphical session. The settings are written into each user's
# own config.toml when their daemon starts, because the app only reads
# `$XDG_CONFIG_HOME/tonguetyped/config.toml`.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.tonguetyped;
  helpers = import ./helpers.nix { inherit lib pkgs cfg; };
in
{
  imports = [ (import ./common.nix { inherit self; }) ];

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = helpers.hasSettings -> cfg.autostart != "none";
        message = ''
          programs.tonguetyped.settings are applied when the daemon starts, so they
          need programs.tonguetyped.autostart set to "systemd" or "xdg". Use the
          home-manager module to manage settings without autostart.
        '';
      }
    ];

    environment.systemPackages = [ cfg.package ];

    systemd.user.services.tonguetyped = lib.mkIf (cfg.autostart == "systemd") {
      description = helpers.serviceDescription;
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        ExecStart = helpers.session;
        Restart = "on-failure";
        RestartSec = 2;
      };
    };

    environment.etc."xdg/autostart/tonguetyped.desktop" = lib.mkIf (cfg.autostart == "xdg") {
      text = helpers.desktopEntry helpers.session;
    };
  };
}
