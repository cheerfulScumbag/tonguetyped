# home-manager module: installs TongueTyped for one user, applies the declared
# settings on every activation, and starts the daemon with the session.
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
  tonguetyped = lib.getExe' cfg.package "tonguetyped";
in
{
  imports = [ (import ./common.nix { inherit self; }) ];

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    # Apply now rather than at the next daemon start, and have a running
    # daemon pick the change up.
    home.activation.tonguetypedSettings = lib.mkIf helpers.hasSettings (
      lib.hm.dag.entryAfter [ "writeBoundary" ] ''
        run env XDG_CONFIG_HOME=${lib.escapeShellArg config.xdg.configHome} \
          ${lib.getExe helpers.applySettings}
        if [ -S "''${XDG_RUNTIME_DIR:-/run/user/$UID}/tonguetyped/control.sock" ]; then
          run ${tonguetyped} reload \
            || warnEcho "tonguetyped: the running daemon rejected the new settings"
        fi
      ''
    );

    systemd.user.services.tonguetyped = lib.mkIf (cfg.autostart == "systemd") {
      Unit = {
        Description = helpers.serviceDescription;
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      } // helpers.unitStartLimits;
      Service = {
        ExecStart = "${tonguetyped} daemon";
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
        ExecStartPre = "-${pkgs.coreutils}/bin/rm -f ${lib.escapeShellArg "${config.xdg.configHome}/autostart/tonguetyped.desktop"}";
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };

    xdg.configFile."autostart/tonguetyped.desktop" = lib.mkIf (cfg.autostart == "xdg") {
      text = helpers.desktopEntry "${tonguetyped} daemon";
    };
  };
}
