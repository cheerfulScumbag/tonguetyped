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
      unitConfig = helpers.unitStartLimits;
      serviceConfig = {
        ExecStart = helpers.session;
        Restart = "on-failure";
        RestartSec = 2;
        # Exit 3 means another daemon already holds the lock (for example one
        # started manually). Retrying cannot succeed while that one lives, so
        # fail honestly once instead of restarting forever.
        RestartPreventExitStatus = "3";
        # This unit owns autostart. Remove the XDG entry that the app wrote
        # with `tonguetyped autostart enable`, or a second daemon starts from
        # it and boxes this one out of the runtime socket. The leading `-`
        # tolerates a missing entry.
        ExecStartPre = "-${helpers.removeStaleAutostart}";
      };
    };

    environment.etc."xdg/autostart/tonguetyped.desktop" = lib.mkIf (cfg.autostart == "xdg") {
      text = helpers.desktopEntry helpers.session;
    };
  };
}
