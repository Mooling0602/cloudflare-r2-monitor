self:
{
  config,
  lib,
  pkgs,
  ...
}:

# NixOS module for the cloudflare-r2-monitor CLI.
#
# `self` is the flake itself (see `module = import ./nix/module.nix self` in
# flake.nix), so the default package is the derivation built from this very
# source tree, without requiring the overlay to be applied.
let
  cfg = config.services.cloudflare-r2-monitor;

  # Name used both for the systemd units and for the service account. Keeping it
  # in one place avoids the unit names drifting apart from the user names.
  unitName = "cloudflare-r2-monitor";

  # systemd's Persistent= only works together with OnCalendar= (see
  # systemd.timer(5): "this setting only has an effect on timers configured with
  # OnCalendar="). A plain OnUnitActiveSec= interval would silently lose the
  # catch-up-after-downtime behaviour, so a duration is translated into a
  # calendar expression instead.
  #
  # Only expressions that were checked with `systemd-analyze calendar` are
  # listed. Everything else has to go through `onCalendar` verbatim, which is
  # why the lookup throws with the list of accepted values instead of guessing.
  calendarForInterval =
    let
      step =
        suffix: unit: n:
        lib.nameValuePair "${toString n}${suffix}" (unit n);
      hours = map (step "h" (n: "*-*-* 00/${toString n}:00:00")) [
        1
        2
        3
        4
        6
        8
        12
      ];
      minutes = map (step "min" (n: "*:0/${toString n}")) [
        1
        2
        3
        4
        5
        6
        10
        12
        15
        20
        30
      ];
      # Day steps mean "from the 1st of each month, every N days", so `7d` fires
      # on the 1st, 8th, 15th, 22nd and 29th - not once per seven days. That is a
      # deliberate approximation: it avoids `Persistent=` catch-up loops and is
      # accurate enough for a usage monitor. For an exact schedule set
      # {option}`onCalendar` instead.
      days = map (step "d" (n: "*-*-01/${toString n} 00:00:00")) [
        1
        2
        3
        5
        7
      ];
    in
    lib.listToAttrs (hours ++ minutes ++ days)
    // {
      # Nicer spellings for the two commonest cases.
      "24h" = "*-*-* 00:00:00";
      "1d" = "*-*-* 00:00:00";
      "weekly" = "Mon *-*-* 00:00:00";
    };

  onCalendar =
    if cfg.onCalendar != null then
      cfg.onCalendar
    else
      calendarForInterval.${cfg.interval}
        or (throw ''
          services.cloudflare-r2-monitor.interval: unsupported value "${cfg.interval}".
          Supported values are: ${lib.concatStringsSep ", " (lib.attrNames calendarForInterval)}.
          For any other schedule set services.cloudflare-r2-monitor.onCalendar
          to a systemd calendar expression (see systemd.time(7)).'');

  # Alerts are opt-in: with `alerts.enable = false` not a single `--alert-*`
  # flag is passed, so the CLI keeps its own default (no mail, no state file)
  # and the unit needs no writable directory.
  #
  # `alerts.from` has no fallback: himalaya derives the SMTP envelope from the
  # message's `From:` header and refuses a message without one, so enabling
  # alerts without it would fail on every single run.
  #
  # Every flag that takes a value is emitted as two separate elements instead of
  # `--flag=value`; both survive `lib.escapeShellArgs`, but the two-element form
  # is what `systemctl cat` renders readably. `--alert-to` is the exception
  # because the CLI takes a comma separated list there.
  alertArgs = lib.optionals cfg.alerts.enable (
    [ "--alert-to" (lib.concatStringsSep "," cfg.alerts.to) ]
    ++ lib.optionals (cfg.alerts.from != null) [
      "--alert-from"
      cfg.alerts.from
    ]
    ++ lib.optionals (cfg.alerts.account != null) [
      "--alert-account"
      cfg.alerts.account
    ]
    ++ lib.optionals (cfg.alerts.configFile != null) [
      "--alert-config"
      cfg.alerts.configFile
    ]
    ++ [
      "--alert-state"
      cfg.alerts.stateFile
      "--alert-repeat"
      cfg.alerts.repeat
    ]
    ++ lib.optional (!cfg.alerts.onRecovery) "--alert-no-recovery"
    ++ lib.optional cfg.alerts.dryRun "--alert-dry-run"
  );

  # `lib.escapeShellArgs` is a shell quoter, which is close enough for
  # ExecStart=: systemd splits on whitespace and understands single quotes, so
  # arguments containing spaces stay intact. Note that systemd also expands
  # `%`-specifiers there, so a literal percent sign in `extraArgs` must be
  # written as `%%`.
  #
  # Thresholds are rendered with `toString` because `nixpkgs.lib` has no
  # `floatToString`; `toString 0.8` yields "0.800000", which the CLI parses
  # back into the same ratio.
  execStart = lib.escapeShellArgs (
    [ (lib.getExe cfg.package) ]
    ++ [
      "--account-id"
      cfg.accountId
      # %d is the per-invocation credentials directory set up by
      # LoadCredential= below; the token never appears in the unit file.
      "--api-token-file"
      "%d/${unitName}-token"
      "--warn"
      (toString cfg.warnThreshold)
      "--critical"
      (toString cfg.criticalThreshold)
      "--period"
      cfg.period
    ]
    ++ lib.optional cfg.jsonOutput "--json"
    ++ alertArgs
    ++ cfg.extraArgs
  );

  defaultUser = unitName;
in
{
  options.services.cloudflare-r2-monitor = {
    enable = lib.mkEnableOption "Cloudflare R2 free-tier usage monitor";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "self.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = ''
        Package providing `cf-r2-monitor`. Defaults to the derivation built from
        this flake. If the system has `nixpkgs.overlays` set to
        `cloudflare-r2-monitor.overlays.default`, setting this to
        `pkgs.cloudflare-r2-monitor` gives the same result.
      '';
    };

    accountId = lib.mkOption {
      type = lib.types.str;
      example = "YOUR_ACCOUNT_ID";
      description = ''
        Cloudflare account ID. This is an identifier, not a credential, so it is
        passed on the command line and is visible in `systemctl show` and the
        journal. The API token is handled separately by {option}`apiTokenFile`.
      '';
    };

    apiTokenFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/cloudflare-r2-api-token";
      description = ''
        Path to a file containing the Cloudflare API token (an absolute path, as
        a string). The token needs the "Account -> Account Analytics -> Read"
        permission.

        This option is `types.str` on purpose and not `types.path`: a Nix path
        value is copied into the store during evaluation, which would publish
        the token world-readable in `/nix/store` and leak it into the flake's
        closure. With a string, only the path text is used.

        The file is handed to the service through systemd's `LoadCredential=`,
        so it is readable by the unit only, is not visible in the unit file, and
        does not appear in the process list. Recommended setup:

        ```nix
        systemd.services.cloudflare-r2-monitor.serviceConfig.LoadCredential =
          lib.mkForce "cloudflare-r2-monitor-token:/run/secrets/cloudflare-r2-api-token";
        ```

        is *not* needed - just point this option at the file. Keep it outside the
        store, e.g. under `/run/secrets` via sops-nix/agenix, and make the file
        readable by the service user (or by root, since systemd reads credentials
        as root and then passes them to the unit).
      '';
    };

    interval = lib.mkOption {
      type = lib.types.str;
      default = "6h";
      example = "30min";
      description = ''
        How often to run the check. Accepts the values listed in the error
        message for unsupported input (`1h`...`12h`, `1min`...`30min`, `1d`...,
        `24h`, `weekly`), which are translated into an `OnCalendar=` expression
        so that {option}`Persistent`-style catch-up keeps working. For anything
        else use {option}`onCalendar`.
      '';
    };

    onCalendar = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "*-*-* 02:30:00";
      description = ''
        Explicit systemd `OnCalendar=` expression. When set it wins over
        {option}`interval`. Use this when the desired schedule cannot be
        expressed as a simple interval.
      '';
    };

    period = lib.mkOption {
      type = lib.types.str;
      default = "30d";
      example = "7d";
      description = ''
        Billing window to query, passed to `--period`. Cloudflare rejects GraphQL
        queries spanning more than 31 days, so keep this at or below `31d`.
      '';
    };

    warnThreshold = lib.mkOption {
      type = lib.types.numbers.between 0 1;
      default = 0.80;
      description = ''
        Fraction of the free tier at which the report is marked as a warning
        (exit code 1).
      '';
    };

    criticalThreshold = lib.mkOption {
      type = lib.types.numbers.between 0 1;
      default = 0.95;
      description = ''
        Fraction of the free tier at which the report is marked as critical
        (exit code 2). Must not be lower than {option}`warnThreshold`.
      '';
    };

    jsonOutput = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Pass `--json` so the journal gets machine-readable output. The service
        still logs nothing else and never writes report files of its own.
      '';
    };

    successExitCodes = lib.mkOption {
      type = lib.types.listOf lib.types.int;
      default = [ ];
      example = [
        0
        1
        2
      ];
      description = ''
        Values rendered as systemd `SuccessExitStatus=`. Empty by default, i.e.
        only 0 counts as success.

        This matters because the CLI deliberately uses distinct exit codes:
        0 = ok, 1 = warning, 2 = critical, 3 = runtime error. With the default,
        an overage marks the unit as failed, which makes `systemctl --failed` (or
        an existing `systemd` alerting rule) a ready-made notification path.
        Set this to `[ 0 1 2 ]` to have only real errors surface as failures.
      '';
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = defaultUser;
      description = ''
        User the service runs as. When left at the default a matching system
        account is created. `DynamicUser = true` (set through
        `systemd.services.cloudflare-r2-monitor.serviceConfig`) is a valid
        alternative and is even stricter; `LoadCredential=` keeps working because
        the credentials directory is created per invocation and owned by whatever
        user the unit runs as, independently of the user database.
      '';
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = defaultUser;
      description = ''
        Group the service runs as. When left at the default a matching system
        group is created.
      '';
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = {
        CF_R2_MONITOR_LOG = "info";
      };
      description = ''
        Extra environment variables for the service. `CF_R2_MONITOR_LOG` is the
        variable the binary reads for its log filter (default level: `warn`).
      '';
    };

    wantedBy = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "timers.target" ];
      description = ''
        Targets that pull in the timer. Setting this to
        `[ "multi-user.target" ]` makes the timer start at boot through the
        regular multi-user target instead of the timer target; both work, the
        default is the conventional one for timers.
      '';
    };

    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "--endpoint" "https://example.invalid/graphql" ];
      description = ''
        Additional command line arguments appended to the invocation. Useful for
        options this module does not model, such as `--endpoint`.
      '';
    };

    alerts = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Mail an alert when the report is not OK. Off by default, in which case
          no `--alert-*` flag is passed at all.

          Enabling this requires {option}`alerts.to` to be non-empty and pulls in
          the mailer from {option}`alerts.package` on the service `PATH`.
        '';
      };

      to = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "ops@example.org" ];
        description = ''
          Recipients, passed as a single comma separated `--alert-to` value.
          Non-empty whenever {option}`alerts.enable` is set.
        '';
      };

      from = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "monitor@example.org";
        description = ''
          Envelope sender, passed as `--alert-from` and written as the `From:`
          header.

          Required whenever {option}`alerts.enable` is true. himalaya derives
          the SMTP envelope from the message's `From:` header and refuses to
          submit a message without one, so there is no fallback to the mail
          account's own address.
        '';
      };

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.himalaya;
        defaultText = lib.literalExpression "pkgs.himalaya";
        description = ''
          Package providing the `himalaya` executable. It is added to the unit's
          `PATH` so that `--alert-command` (which defaults to the plain name
          `himalaya`) resolves inside the sandboxed service.
        '';
      };

      account = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "cloudflare";
        description = ''
          Name of the mail account to send from, passed as `--alert-account`
          (the mailer's `-a` flag). `null` omits it and lets the mailer use its
          default account.
        '';
      };

      configFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/himalaya.toml";
        description = ''
          Path to the mailer configuration file, passed as `--alert-config` (the
          mailer's `-c` flag). `null` omits it.

          This option is `nullOr str` and not `nullOr path` for exactly the same
          reason as {option}`apiTokenFile`: a path value is copied into the store
          during evaluation, and this file usually contains an SMTP password, so
          it would end up world-readable in `/nix/store`. A plain string only
          contributes the path text. Keep the file outside the store, e.g. under
          `/run/secrets` managed by sops-nix or agenix.
        '';
      };

      repeat = lib.mkOption {
        type = lib.types.str;
        default = "24h";
        example = "2d";
        description = ''
          Minimum delay before the same condition is mailed again, passed as
          `--alert-repeat`. Use `"0"` to send on every run. Accepts a plain
          duration such as `24h`, `90m` or `2d`; the value is only checked by the
          CLI, so an unparsable duration is reported as a runtime error (exit
          code 3) rather than a build failure.
        '';
      };

      onRecovery = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = ''
          Send a "recovered" mail when a previously alerting condition clears.
          When false, `--alert-no-recovery` is passed.
        '';
      };

      dryRun = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Pass `--alert-dry-run` so the message is printed to stdout (and thus to
          the journal) instead of being sent. Useful to check a mail setup
          without mailing anybody.
        '';
      };

      stateFile = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/${unitName}/alert-state.json";
        defaultText = lib.literalExpression ''"/var/lib/cloudflare-r2-monitor/alert-state.json"'';
        description = ''
          JSON file remembering what was already mailed, passed as
          `--alert-state` and used for de-duplication. The default lives in the
          directory created by `StateDirectory=`, which stays writable even under
          `ProtectSystem=strict`.

          Set `--alert-repeat 0` (i.e. {option}`alerts.repeat = "0"`) if every
          run should mail; the file is still written in that case.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !cfg.alerts.enable || cfg.alerts.to != [ ];
        message = ''
          services.cloudflare-r2-monitor.alerts.to must list at least one
          recipient when services.cloudflare-r2-monitor.alerts.enable is true;
          an alert without a destination would only fail at runtime.
        '';
      }
      {
        assertion = !cfg.alerts.enable || cfg.alerts.from != null;
        message = ''
          services.cloudflare-r2-monitor.alerts.from must be set when
          services.cloudflare-r2-monitor.alerts.enable is true; himalaya takes
          the envelope sender from the `From:` header and refuses a message
          without one, so every alert would fail at the mailer.
        '';
      }
    ];

    # The unit runs as a dedicated account that cannot log in. It is only
    # created when the names are left at their defaults, so a user pointing
    # `user`/`group` at an existing account is not surprised by an extra one.
    users.users = lib.mkIf (cfg.user == defaultUser) {
      ${defaultUser} = {
        isSystemUser = true;
        group = cfg.group;
        description = "Cloudflare R2 free-tier usage monitor";
      };
    };

    users.groups = lib.mkIf (cfg.group == defaultUser) {
      ${defaultUser} = { };
    };

    systemd.services.${unitName} = {
      description = "Cloudflare R2 free-tier usage monitor";

      # A timer job; nothing else should pull it in directly.
      wantedBy = [ ];

      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];

      # Unit level, not serviceConfig: `path` renders as `Environment=PATH=...`
      # for the unit, whereas a `Path=` key inside [Service] would be the
      # unrelated path-watching directive. The mailer is invoked by its plain
      # name (`--alert-command` defaults to "himalaya"), so it must be on PATH.
      path = lib.mkIf cfg.alerts.enable [ cfg.alerts.package ];

      serviceConfig = {
        Type = "oneshot";

        ExecStart = execStart;

        User = cfg.user;
        Group = cfg.group;

        # The token lives in a file outside the store. systemd reads it as root
        # and exposes a copy to the unit at %d (CREDENTIALS_DIRECTORY), which is
        # what ExecStart points at. Compared to EnvironmentFile= this keeps the
        # value out of the environment (hence out of `systemctl show` and any
        # child process that dumps its env) and out of /proc/<pid>/environ.
        LoadCredential = "${unitName}-token:${cfg.apiTokenFile}";

        # Deliberately no Restart=: the unit is re-run by the timer, and retrying
        # an overage would just repeat the same "failure".
        #
        # Output goes to the journal; the service never writes its own files.

        # Hardening. ProtectHome is "read-only" rather than true so that a token
        # file placed under /root or /home stays readable.
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        ProtectKernelTunables = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        MemoryDenyWriteExecute = true;

        # Needed by the alerts: `--alert-state` points here. Under
        # ProtectSystem=strict this directory is the standard way to get a
        # writable path - systemd creates it and bind-mounts it read-write.
        StateDirectory = unitName;
      }
      // lib.optionalAttrs (cfg.environment != { }) {
        Environment = lib.mapAttrsToList (name: value: "${name}=${value}") cfg.environment;
      }
      // lib.optionalAttrs (cfg.successExitCodes != [ ]) {
        SuccessExitStatus = map toString cfg.successExitCodes;
      };
    };

    systemd.timers.${unitName} = {
      description = "Cloudflare R2 free-tier usage monitor";

      wantedBy = cfg.wantedBy;

      timerConfig = {
        Unit = "${unitName}.service";
        OnCalendar = onCalendar;

        # Catch up after downtime: if the machine was off when a run was due, the
        # check runs once on the next boot instead of waiting for the next slot.
        Persistent = true;
      };
    };
  };
}
