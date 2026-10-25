# The `programs.tonguetyped` options shared by the NixOS and home-manager
# modules. Only where the package, the daemon, and the settings land differs.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.programs.tonguetyped;
  tomlFormat = pkgs.formats.toml { };
  system = pkgs.stdenv.hostPlatform.system;

  # Every typed setting defaults to null and is dropped before rendering, so
  # the app's own default applies to whatever the configuration leaves unset.
  setting =
    type: description:
    mkOption {
      type = types.nullOr type;
      default = null;
      inherit description;
    };
  section =
    description: options:
    mkOption {
      type = types.submodule {
        freeformType = tomlFormat.type;
        inherit options;
      };
      default = { };
      inherit description;
    };
in
{
  options.programs.tonguetyped = {
    enable = lib.mkEnableOption "TongueTyped, the offline Linux dictation daemon";

    backend = mkOption {
      type = types.enum [
        "vulkan"
        "cuda"
        "rocm"
      ];
      default = "vulkan";
      description = ''
        GPU backend the package is built with. Every build also carries the
        CPU fallback. `cuda` and `rocm` are available on x86_64-linux only.
      '';
    };

    package = mkOption {
      type = types.package;
      default =
        self.packages.${system}."gpu-${cfg.backend}"
          or (throw "programs.tonguetyped: the ${cfg.backend} backend is not packaged for ${system}");
      defaultText = lib.literalExpression ''tonguetyped.packages.''${system}."gpu-''${backend}"'';
      description = "The TongueTyped package. Defaults to the build for `backend`.";
    };

    autostart = mkOption {
      type = types.enum [
        "systemd"
        "xdg"
        "none"
      ];
      default = "systemd";
      description = ''
        How the daemon starts with the desktop session.

        - `systemd`: a `tonguetyped.service` user unit bound to
          `graphical-session.target`. Needs a session that starts that target
          (KDE, GNOME, and compositors launched through systemd or uwsm).
        - `xdg`: a `tonguetyped.desktop` XDG autostart entry, for desktops that
          run autostart entries but not `graphical-session.target`.
        - `none`: start it yourself, for example with `exec-once` in a
          compositor configuration.

        With `systemd`, the daemon unit owns autostart: it removes the XDG
        entry `tonguetyped autostart enable` writes, and that command refuses
        to create one while the unit exists.
      '';
    };

    settings = mkOption {
      type = types.submodule {
        freeformType = tomlFormat.type;
        options = {
          activation = section "The `[activation]` table." {
            mode = setting (types.enum [
              "hold"
              "toggle"
            ]) "Hold the shortcut while speaking, or press once to start and again to stop.";
          };
          model = section "The `[model]` table." {
            active_model = setting types.str "Catalog id of the GGUF model to load (see `tonguetyped model list`).";
            preferred_backend = setting (types.enum [
              "auto"
              "cpu"
              "vulkan"
              "cuda"
              "rocm"
              "metal"
            ]) "Inference backend. `auto` tries each built-in accelerator, then the CPU.";
            idle_unload = section "The `[model.idle_unload]` table." {
              policy = setting (types.enum [
                "never"
                "after_transcription"
                "after_idle"
              ]) "When the loaded model is freed.";
              timeout_minutes = setting types.ints.unsigned "Idle minutes before unloading, for `after_idle`.";
            };
          };
          audio = section "The `[audio]` table." {
            microphone = setting types.str "ALSA input device name, or `default`.";
            feedback_sounds = setting types.bool "Play a sound on recording start and stop.";
            feedback_volume = setting (types.numbers.between 0 1) "Feedback sound volume, from 0 to 1.";
            feedback_device = setting types.str "Output device for feedback sounds, or `default`.";
            pause_media = setting types.bool "Pause playing media while recording and resume it when done. Off by default.";
          };
          output = section "The `[output]` table." {
            method = setting (types.enum [
              "none"
              "type"
              "clipboard"
              "paste"
            ]) "Where the transcript goes: `type` types it into the focused application, `clipboard` copies it, `paste` copies then pastes it, and `none` only stores it. `type` and `paste` fall back to the clipboard when typing fails.";
            typing_backend = setting (types.enum [
              "auto"
              "wtype"
              "enigo"
              "dotool"
              "libei"
            ]) "Which helper types the transcript. `libei` types natively through the XDG RemoteDesktop portal on KDE and GNOME Wayland, needing no helper binary and no /dev/uinput access.";
            auto_submit = setting types.bool "Press Enter after the transcript is typed or pasted. Requires `method = \"type\"` or `\"paste\"`.";
            paste_keys = setting (types.enum [
              "ctrl-v"
              "shift-insert"
            ]) "Key combination `paste` presses after copying. `ctrl-v` for most applications, `shift-insert` for terminals.";
          };
          transcription = section "The `[transcription]` table." {
            vad_enabled = setting types.bool "Trim silence with voice activity detection.";
            language = setting types.str "Spoken language code, or `auto` to detect it.";
            trailing_space = setting types.bool "Append a space after each transcript.";
            max_recording_seconds = setting types.ints.positive "Longest single recording.";
            chunk_seconds = setting types.ints.unsigned "Decode window; audio longer than this is transcribed in chunks. 0 sends the whole buffer in one call, except a model that advertises an audio limit (canary, cohere, moonshine, Qwen3-ASR), which is still split to stay within it.";
          };
          text = section "The `[text]` table." {
            replacements = setting (types.attrsOf types.str) "Map of mistranscriptions to corrections, e.g. `{ alot = \"a lot\"; }`.";
            spoken_punctuation = setting types.bool "Turn spoken punctuation words into symbols for dictating code.";
            remove_fillers = setting types.bool "Drop filler words. Applied only when `transcription.language` is English.";
            post_process_command = setting types.str "External command that cleans the transcript on stdin/stdout. Empty disables it.";
            post_process_timeout_seconds = setting types.ints.positive "Seconds the cleanup command may run before it is killed and ignored.";
          };
          history = section "The `[history]` table." {
            enabled = setting types.bool "Save transcripts in the local history database and write the transcript folder. Off by default.";
            max_entries = setting types.ints.unsigned "Entries to keep. 0 means no limit.";
            max_age_days = setting types.ints.unsigned "Days to keep entries. 0 means no limit.";
            transcript_folder = setting types.str "Also write each transcript to this folder while history is enabled. Empty disables it.";
          };
          overlay = section "The `[overlay]` table." {
            enabled = setting types.bool "Show the on-screen recording overlay.";
            position = setting (types.enum [
              "top-left"
              "top"
              "top-right"
              "center"
              "bottom-left"
              "bottom"
              "bottom-right"
            ]) "Overlay position. Ignored by the `border` and `half-circle` styles.";
            style = setting (types.enum [
              "badge"
              "minimal"
              "pill"
              "blob"
              "border"
              "half-circle"
              "long-pill"
              "live-text"
              "live-text-box"
            ]) "Overlay shape.";
            look = setting (types.enum [
              "plain"
              "waveform"
              "live-text"
            ]) "Overlay look: the resting shape, the voice-reactive waveform, or live streaming text.";
            live_waveform = setting types.bool "Deprecated: use `look = \"waveform\"` (was true) or `look = \"plain\"` (was false) instead. Folded into `look` and removed from the written config.";
            waveform_shape = setting (types.enum [
              "bars"
              "squiggle"
            ]) "Waveform glyph for the `waveform` look: uniform bars or the long pill's squiggly line.";
            monitor = setting types.str "`all`, or one output name from `tonguetyped doctor`.";
          };
        };
      };
      default = { };
      example = lib.literalExpression ''
        {
          activation.mode = "toggle";
          model.active_model = "whisper-small-q5_k_m";
          output = {
            method = "type";
            auto_submit = false;
          };
          overlay.style = "pill";
        }
      '';
      description = ''
        TongueTyped settings, written into
        `$XDG_CONFIG_HOME/tonguetyped/config.toml`. See the Configuration
        section of the README for every key.

        The file stays writable. Keys set here are merged over it and win;
        keys left unset keep whatever the file holds, so the `tonguetyped`
        setup console can still change them. Removing a key here leaves
        its last written value in place.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    programs.tonguetyped.settings.overlay.look =
      lib.mkIf (cfg.settings.overlay.live_waveform != null)
        (lib.mkDefault (if cfg.settings.overlay.live_waveform then "waveform" else "plain"));
    assertions = [
      {
        assertion = !(
          cfg.settings.output.auto_submit == true
          && (cfg.settings.output.method == "none" || cfg.settings.output.method == "clipboard")
        );
        message = ''programs.tonguetyped.settings.output.auto_submit requires output.method = "type" or "paste".'';
      }
    ];
  };
}
