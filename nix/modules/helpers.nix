# Derivations both modules build from an evaluated `programs.tonguetyped`.
{
  lib,
  pkgs,
  cfg,
}:
let
  # Drops unset (null) settings, then any table left empty by that.
  prune =
    attrs:
    lib.filterAttrs (_: value: !(lib.isAttrs value && value == { })) (
      lib.mapAttrs (_: value: if lib.isAttrs value then prune value else value) (
        lib.filterAttrs (_: value: value != null) attrs
      )
    );
  # The deprecated `overlay.live_waveform` alias is folded into `overlay.look`
  # in common.nix; it must not reach `config.toml`.
  stripDeprecated =
    settings:
    settings
    // lib.optionalAttrs (settings ? overlay) {
      overlay = builtins.removeAttrs settings.overlay [ "live_waveform" ];
    };
  rendered = prune (stripDeprecated cfg.settings);
  settingsJson = pkgs.writeText "tonguetyped-settings.json" (builtins.toJSON rendered);
in
rec {
  hasSettings = rendered != { };

  # Merges the declared settings over the user's config.toml. The app rewrites
  # that file itself (the setup console, the desktop-reported shortcut), so it
  # stays a regular writable file rather than a read-only store link.
  applySettings = pkgs.writeShellApplication {
    name = "tonguetyped-apply-settings";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.diffutils
      pkgs.jq
      pkgs.remarshal
    ];
    text = ''
      config_dir="''${XDG_CONFIG_HOME:-$HOME/.config}/tonguetyped"
      config_file="$config_dir/config.toml"
      umask 077
      mkdir -p "$config_dir"
      next=$(mktemp "$config_dir/.config.toml.XXXXXX")
      trap 'rm -f "$next"' EXIT

      if [ -e "$config_file" ] && current=$(remarshal --from toml --to json "$config_file" 2>/dev/null); then
        printf '%s' "$current" \
          | jq --slurpfile declared ${settingsJson} '. * $declared[0]' \
          | remarshal --from json --to toml >"$next"
      else
        if [ -e "$config_file" ]; then
          cp "$config_file" "$config_file.bak"
          echo "tonguetyped: $config_file is not valid TOML; saved it as config.toml.bak" >&2
        fi
        remarshal --from json --to toml ${settingsJson} >"$next"
      fi

      if cmp -s "$next" "$config_file"; then
        rm -f "$next"
      else
        mv "$next" "$config_file"
      fi
      trap - EXIT
    '';
  };

  # Brings the declared settings into place, then runs the daemon.
  session = pkgs.writeShellScript "tonguetyped-session" ''
    ${lib.optionalString hasSettings (lib.getExe applySettings)}
    exec ${lib.getExe' cfg.package "tonguetyped"} daemon
  '';

  desktopEntry = exec: ''
    [Desktop Entry]
    Type=Application
    Name=TongueTyped
    Comment=Local speech-to-text dictation
    Exec=${exec}
    Icon=io.github.cheerfulScumbag.tonguetyped
    Terminal=false
    StartupNotify=false
    NoDisplay=true
  '';

  serviceDescription = "TongueTyped dictation daemon";
}
