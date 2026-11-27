# 在 Nix / NixOS 上部署 cf-r2-monitor

本文假设你已经用过 `nix` 和 flakes，至少知道 `nix build`、`nix run` 是干什么的。如果你还没开 flakes，先在自己的 `configuration.nix` 里加上：

```nix
nix.settings.experimental-features = [ "nix-command" "flakes" ];
```

项目的 flake 把 nixpkgs 钉在 **`nixos-26.05`** 稳定频道上，没有跟 unstable。这一点很重要，下面「两种接入方式」的区别就来自它。

## flake 提供的输出

`<system>` 只有两个取值：`x86_64-linux` 和 `aarch64-linux`。**没有 darwin** —— 这个工具是给 systemd timer 用的，macOS 上不提供任何输出。

| 输出 | 用途 |
| --- | --- |
| `packages.<system>.default` | 默认包，等于 `cloudflare-r2-monitor` |
| `packages.<system>.cloudflare-r2-monitor` | 同名包，显式引用时用 |
| `apps.<system>.default` | `nix run` 用 |
| `devShells.<system>.default` | 开发环境，含 cargo / rustc / clippy / rustfmt |
| `overlays.default` | 把包塞进 `pkgs` |
| `nixosModules.default` | NixOS 模块 |
| `nixosModules.cloudflare-r2-monitor` | 同上，显式命名 |
| `formatter.<system>.default` | `nix fmt` 用 |

把 `<system>` 展开，可以实例化的就是下面这些：

```
packages.x86_64-linux.{default, cloudflare-r2-monitor}
packages.aarch64-linux.{default, cloudflare-r2-monitor}
apps.x86_64-linux.{default, cloudflare-r2-monitor}
apps.aarch64-linux.{default, cloudflare-r2-monitor}
devShells.x86_64-linux.default
devShells.aarch64-linux.default
overlays.default
nixosModules.{default, cloudflare-r2-monitor}
formatter.x86_64-linux.default
formatter.aarch64-linux.default
```

## 四种不装到系统里的用法

```bash
# 直接跑，参数放在 -- 之后
nix run github:Mooling0602/cloudflare-r2-monitor -- \
  --account-id YOUR_ACCOUNT_ID --api-token-file /run/secrets/r2-token

# 构建，产物在 ./result/bin/cf-r2-monitor
nix build github:Mooling0602/cloudflare-r2-monitor --print-build-logs

# 进开发环境（想跑 cargo test / clippy 就得用它，见下面的坑）
nix develop github:Mooling0602/cloudflare-r2-monitor

# 装进当前用户的 profile，之后 cf-r2-monitor 直接在 PATH 里
nix profile install github:Mooling0602/cloudflare-r2-monitor
```

`nix profile install` 之后升级用 `nix profile upgrade '.*cloudflare-r2-monitor.*'`，卸载用 `nix profile remove`。如果你只是想临时用一下，`nix run` 比 `nix profile install` 干净得多，别为了一个监控工具污染 profile。

## 把 flake 接进你的系统

这是最容易出问题的一步。有两种接入方式，区别在于**这个项目是用谁的 nixpkgs 编译的**。

### 方式 A：复用你系统已有的 nixpkgs（推荐）

用 `inputs.nixpkgs.follows` 让本项目跟随你系统 flake 里的 nixpkgs。这样整个系统里只有一份 nixpkgs，不会因为版本不同而把 glibc、openssl、rustc 全都再编译一遍。

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    cloudflare-r2-monitor = {
      url = "github:Mooling0602/cloudflare-r2-monitor";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, cloudflare-r2-monitor, ... }@inputs: {
    nixosConfigurations.myserver = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        cloudflare-r2-monitor.nixosModules.default
        ./configuration.nix
      ];
    };
  };
}
```

如果你的 nixpkgs 输入叫别的名字（比如 `unstable`），把 `follows = "nixpkgs"` 改成对应的输入名即可。**`follows` 的值是你自己 flake 里的输入名，不是 URL。**

想把它加进全局 `pkgs`，就再叠一层 overlay（注意这是在方式 A 下才有意义的写法，原因见方式 B）：

```nix
{
  nixpkgs.overlays = [ inputs.cloudflare-r2-monitor.overlays.default ];
  environment.systemPackages = [ pkgs.cloudflare-r2-monitor ];
}
```

### 方式 B：直接用本项目 pin 的 nixpkgs

不写 `follows` 时，这个 flake 会拖自己那份 `nixos-26.05` 的 nixpkgs 来编译。好处是行为可复现、跟你系统怎么配无关；坏处是 flake 锁文件里会多出一份 nixpkgs，从缓存拉下来的东西也多一整套。如果你的系统也在 `nixos-26.05` 上，那两份 nixpkgs 大概率求值出同样的 store path，实际重复开销可能没有想象中大；但如果你的系统在别的频道，两边就会真的各编译一套。

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    cloudflare-r2-monitor.url = "github:Mooling0602/cloudflare-r2-monitor";
  };

  outputs = { self, nixpkgs, cloudflare-r2-monitor, ... }: {
    nixosConfigurations.myserver = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        cloudflare-r2-monitor.nixosModules.default
        ./configuration.nix
      ];
    };
  };
}
```

**在方式 B 下不要用 overlay。** overlay 会把一个用「另一份 nixpkgs」编译出来的包塞进你系统的 `pkgs`，一旦那个包链接的 glibc、openssl 和你系统里的不是同一个版本，就可能拿到 ABI 不匹配的运行时。要引包就直接引它的输出：

```nix
{
  environment.systemPackages = [
    cloudflare-r2-monitor.packages.${pkgs.system}.default
  ];
}
```

### 选哪个

你的系统 flake 已经在 `nixos-26.05` 上 → 选 A，一份 nixpkgs，构建快，缓存命中率高。

你的系统在别的频道、又不想动它 → 选 B，接受多一份 nixpkgs 的代价，换取确定的行为。这时尤其要配好二进制缓存（下一节），否则会自己从源码编译整个 Rust 依赖树。

## NixOS 模块

命名空间是 `services.cloudflare-r2-monitor`。模块不会自动生效，必须像上面那样显式 import 进 `modules` 列表。

### 最小配置

```nix
{
  services.cloudflare-r2-monitor = {
    enable = true;
    accountId = "YOUR_ACCOUNT_ID";
    apiTokenFile = "/run/secrets/r2-token";
  };
}
```

`interval` 默认 `"6h"`，`warnThreshold` 默认 `0.80`，`criticalThreshold` 默认 `0.95`，`period` 默认 `"30d"`。这些默认值就是绝大多数人想要的，不用改。

### 完整一点的配置

```nix
{
  services.cloudflare-r2-monitor = {
    enable = true;
    accountId = "YOUR_ACCOUNT_ID";
    apiTokenFile = "/run/secrets/r2-token";

    interval = "1h";
    period = "30d";
    warnThreshold = 0.80;
    criticalThreshold = 0.95;

    # 让 journal 里是 JSON，方便日志收集
    jsonOutput = false;

    # 追加到命令行末尾的额外参数
    extraArgs = [ ];

    # 默认就会创建一个同名的系统用户/用户组，只有想换身份时才需要动
    user = "cloudflare-r2-monitor";
    group = "cloudflare-r2-monitor";
  };
}
```

完整选项表（默认值取自 `nix/module.nix`）：

| 选项 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `enable` | bool | `false` | 必须显式打开 |
| `package` | package | 本 flake 的 `packages.<system>.default` | 想换自己编的包就改这里 |
| `accountId` | str | 无（必填） | 32 位十六进制，不是邮箱 |
| `apiTokenFile` | str | 无（必填） | token 文件的**字符串**路径，见下节 |
| `interval` | str | `"6h"` | 只能是那张表里的值 |
| `onCalendar` | nullOr str | `null` | 设了就优先于 `interval` |
| `period` | str | `"30d"` | 传给 `--period`，别超过 `31d` |
| `warnThreshold` | numbers.between 0 1 | `0.80` | 超过即告警（退出码 1） |
| `criticalThreshold` | numbers.between 0 1 | `0.95` | 超过即严重（退出码 2） |
| `jsonOutput` | bool | `false` | 加 `--json` |
| `successExitCodes` | listOf int | `[]` | 空表示只有 0 算成功 |
| `user` | str | `"cloudflare-r2-monitor"` | 保持默认会自动建系统用户 |
| `group` | str | `"cloudflare-r2-monitor"` | 保持默认会自动建用户组 |
| `environment` | attrsOf str | `{}` | 例如 `{ CF_R2_MONITOR_LOG = "info"; }` |
| `wantedBy` | listOf str | `["timers.target"]` | 用什么拉起 timer |
| `extraArgs` | listOf str | `[]` | 追加到命令行末尾 |

想换成一个自己 build 的包，改 `package` 就行：

```nix
{
  services.cloudflare-r2-monitor.package =
    inputs.cloudflare-r2-monitor.packages.${pkgs.system}.default;
}
```

### 关于定时触发器

模块内部用 systemd timer，触发器是 **`OnCalendar`**（不是 `OnUnitActiveSec`），并且开了 `Persistent = true`，所以机器关机期间错过的检查会在下次启动时补跑一次，而不是等到下一个时间点。

`interval` 不是可以随便写的时间跨度，它是一张写死的表，只接受下面这些值，其它值会在求值时 throw 并把可选项列出来：

| 单位 | 可用取值 | 展开成的 `OnCalendar` |
| --- | --- | --- |
| 小时 | `1h` `2h` `3h` `4h` `6h` `8h` `12h` | `*-*-* 00/N:00:00` |
| 分钟 | `1min` `2min` `3min` `4min` `5min` `6min` `10min` `12min` `15min` `20min` `30min` | `*:0/N` |
| 天 | `1d` `2d` `3d` `5d` `7d` | `*-*-01/N 00:00:00` |
| 别名 | `24h`、`1d` | `*-*-* 00:00:00`（每天零点） |
| 别名 | `weekly` | `Mon *-*-* 00:00:00`（每周一零点） |

默认值 `"6h"` 展开出来是 `OnCalendar = "*-*-* 00/6:00:00"`，即 0、6、12、18 点各跑一次。

**一个必须知道的陷阱：`2d`/`3d`/`5d`/`7d` 展开成的 `N` 是「每月几号」的步长，不是「每 N 天」。** 它们展开成 `*-*-01/N 00:00:00`，意思是「从每月 1 号起，每隔 N 天」。所以 `7d` 实际是每月 1、8、15、22、29 号跑，**跨月时会断档**：11/29 跑完，下一次是 12/1，只隔了 2 天；然后 12/1 到 12/8 又隔 7 天。如果你需要严格的「每 7 天一次」，这个选项做不到，请用 calendar 表达式：

```nix
{
  services.cloudflare-r2-monitor.onCalendar = "Mon *-*-* 00:00:00";
}
```

`onCalendar` 是 `nullOr str`，默认 `null`，**一旦设了就优先于 `interval`**。任何 systemd calendar 表达式都可以写在这里（语法见 `systemd.time(7)`，本地可以用 `systemd-analyze calendar '表达式'` 验一下它下次什么时候触发）。

另外 `Persistent = true` 只对 `OnCalendar` 生效，这也正是模块不直接用 `OnUnitActiveSec` 的原因：那样写会静默丢掉「关机期间错过的检查在开机后补跑」这个行为。

要立刻跑一次看结果：

```bash
sudo systemctl start cloudflare-r2-monitor.service
journalctl -u cloudflare-r2-monitor.service -n 50
```

### 模块实际生成的 unit

`interval = "6h"`、其余选项全默认时，求值出来的关键行是这样的：

```
Type=oneshot
ExecStart=/nix/store/.../bin/cf-r2-monitor --account-id <id> --api-token-file %d/cloudflare-r2-monitor-token --warn 0.800000 --critical 0.950000 --period 30d
LoadCredential=cloudflare-r2-monitor-token:/run/secrets/cf-r2-token
User=cloudflare-r2-monitor
Group=cloudflare-r2-monitor
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=true
NoNewPrivileges=true
MemoryDenyWriteExecute=true
ProtectKernelTunables=true
ProtectControlGroups=true
RestrictSUIDSGID=true
```

`ExecStart` 里的 `%d` 是 systemd 在每次调用时注入的凭据目录（`CREDENTIALS_DIRECTORY`），`LoadCredential` 从 `/run/secrets/cf-r2-token` 读到的内容就放在那里，只有这个 unit 自己可读。**不要**把它改写成硬编码的 `/run/credentials/...`，那个路径是实现细节，`%d` 才是稳定的写法。

顺便解释一个看着奇怪的地方：`--warn 0.800000` 有六位小数。模块用 `toString` 把 Nix 的浮点数转成参数，nixpkgs 里没有更好的格式化函数，`toString 0.8` 出来就是 `"0.800000"`。CLI 能正确解析回 `0.8`，两者完全等价，只是 journal 里看着啰嗦。值是 `types.numbers.between 0 1`，所以除了 0 到 1 之间的数，别的都会在求值期被拒绝。

### `successExitCodes` 与 `jsonOutput`

这两个选项的存在是为了配合「退出码就是状态」这个设计。二进制在告警时返回 `1`、严重时返回 `2`，对 systemd 来说这算「失败」——如果你希望告警状态下 systemd 仍然认为服务跑完了，就把 `1` 和 `2` 加进 `successExitCodes`。反过来，如果你想让 systemd 的 `failed` 状态直接当成告警信号，就保持默认（只把 `0` 视为成功）。

`jsonOutput = true` 会加 `--json`，方便你把输出丢给日志收集或自己的解析脚本。

### 邮件告警：`alerts.*`

告警不是内建的 SMTP 客户端，而是把邮件用 stdin 喂给外部的 `himalaya`。模块负责把 himalaya 装进 unit 的 `PATH`、把参数拼好、把状态目录建好；而**不负责**你的 SMTP 凭据。

| 选项 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `alerts.enable` | bool | `false` | 关着的时候所有 `alerts.*` 都不生效 |
| `alerts.to` | listOf str | `[]` | 收件人，一个也传 `[ "ops@example.org" ]` |
| `alerts.from` | nullOr str | `null` | `alerts.enable = true` 时**必须设**，见下 |
| `alerts.package` | package | `pkgs.himalaya` | 想钉版本或换成自己的 wrapper 就改这里 |
| `alerts.account` | nullOr str | `null` | `null` 时用 himalaya 的默认账号 |
| `alerts.configFile` | nullOr str | `null` | himalaya 的 `config.toml` 路径，见下 |
| `alerts.language` | enum (`"en"`, `"zh-CN"`) | `"en"` | 告警邮件语言，`"zh-CN"` 为简体中文；仅启用告警时传给 CLI |
| `alerts.repeat` | str | `"24h"` | 同一指标同一级别的最短重发间隔，`"0"` 表示每次都发 |
| `alerts.onRecovery` | bool | `true` | 回落到 ok 时是否发「已恢复」 |
| `alerts.dryRun` | bool | `false` | 只把邮件打到 journal，不调用 himalaya |
| `alerts.stateFile` | str | `"/var/lib/cloudflare-r2-monitor/alert-state.json"` | 去重状态文件，别乱改 |

完整示例：

```nix
{
  services.cloudflare-r2-monitor = {
    enable = true;
    accountId = "YOUR_ACCOUNT_ID";
    apiTokenFile = "/run/secrets/cf-r2-token";

    alerts = {
      enable = true;
      to = [ "ops@example.org" ];
      from = "monitor@example.org";
      language = "zh-CN";
      configFile = "/run/secrets/himalaya.toml";
    };
  };
}
```

`alerts.from` 没有默认回退值，`alerts.enable = true` 而不设它会被断言拦下来。原因是两个版本的 himalaya 都从消息的 `From:` 头推导 SMTP 信封发件人：v2 缺这个头报 `No \`From:\` header found in raw message`（见 `src/email/submission.rs`），v1 报 `cannot send message without a sender`。换句话说这不是我们自己加的约束：没有 `From:` 的邮件根本发不出去，与其让每一次定时运行都在发信环节失败，不如在构建时报出来。

**`alerts.configFile` 是 `types.str` 而不是 `types.path`，和 `apiTokenFile` 同理。** 这个文件里通常写着 SMTP 密码，而 `types.path`（以及 `./relative/path`、`"${./relative/path}"` 这类 path 字面量）会在求值期把文件原样复制进 `/nix/store`，那是个全局可读的位置，等于把密码公开。所以这里必须写普通字符串 `"/run/secrets/himalaya.toml"`，文件由 sops-nix、agenix 或你自己放在系统上。

**状态文件位置是模块安排好的，不用自己动手。** 模块会设 `StateDirectory = "cloudflare-r2-monitor"`，systemd 在启动前建好 `/var/lib/cloudflare-r2-monitor` 并把属主交给服务用户，所以**不需要**写 `systemd.tmpfiles.rules`。

反过来更要留意：模块同时开着 `ProtectSystem = "strict"`，绝大多数路径对服务是只读的，`StateDirectory` 是全系统里为数不多的、被自动放行的可写目录。**把 `alerts.stateFile` 指到 `/tmp`、`/run` 或别的什么地方，结果就是写失败、去重失效**（而 `PrivateTmp = true` 还会让 `/tmp` 每次调用都是新的）。真要换位置，得自己补一个对应的 `ReadWritePaths`，不如就用默认值。

### himalaya 自己的配置

模块只管调用 himalaya，发件是 himalaya 的事，所以你需要一份它认的 `config.toml`。

**先说最容易踩的坑：v1 和 v2 的配置格式互不兼容，而 nixpkgs 26.05 里 `pkgs.himalaya` 是 v1.2.0。** 也就是说模块默认拉进来的就是 v1，网上（以及很多 AI 回答里）流传的 v2 配置直接抄过来会在启动时就报 `missing field 'email'`。想知道自己装的是哪版，跑 `himalaya --version`。

#### v1 格式（nixpkgs 26.05 默认就是这个）

```toml
[accounts.default]
default = true
email = "monitor@example.org"

message.send.backend.type = "smtp"
message.send.backend.host = "smtp.example.org"
message.send.backend.port = 587
message.send.backend.encryption.type = "start-tls"
message.send.backend.login = "monitor@example.org"
message.send.backend.auth.type = "password"
message.send.backend.auth.cmd = "cat /run/secrets/smtp-password"
message.send.save-copy = false
```

几个都是实测确认过的硬性要求：

- **`email` 必填**。缺了直接 `cannot parse config file ... missing field 'email'`。
- **`message.send.backend.login` 必填**。缺了报 `missing field 'login'`。
- **`default = true` 写在账号表里面**，不是顶层。写顶层会报 `TOML parse error` 且指向 `default` 那行。
- **`message.send.save-copy = false` 建议加上**。v1 默认是 `true`，没配 maildir 时邮件其实已经发出去了，但存副本失败，himalaya 以退出码 1 收场。本项目会把这条特定报错误判为「已投递」只打 warning，但配成 `false` 能从源头上消掉。

v1 还有两个行为要知道：

- **`-c` 是 `value_delimiter = ':'`**，所以配置路径里不能带冒号，否则会被当成多个路径切开。
- **SMTP 服务器对 `RCPT TO` 回 550 时，v1 会无限重试且永不退出**（实测 3 分钟内刷了 104 万行 `WARN email::smtp: server replied with code 550` 的日志）。本项目给 himalaya 子进程设了 90 秒硬超时，超时即 kill 并按发信失败处理（退出码 `4`），所以这个缺陷不会把定时任务挂死。

#### v2 格式

```toml
[accounts.default]
default = true

[accounts.default.smtp]
server = "smtp.example.org"
login = "monitor@example.org"
password.command = ["secret-tool", "lookup", "service", "himalaya"]

[accounts.default.smtp.sasl]
type = "plain"
```

v2 里 `template send` 已被删除（本项目用的是 `message send`），`export`/`edit`/`template` 这几个子命令也一并没了。想用 v2 就在 `alerts.package` 里覆盖成自己的 himalaya 2.x。

#### 两个版本的共同点

密码可以直接放在受保护的运行时配置文件里，或通过命令读取单独的秘密文件。上面的 v1 示例使用 `auth.cmd`；命令本身不应包含密码。服务账号不一定有登录会话、GPG 密钥或可用的 D-Bus，因此不要假设 `pass` 或 `secret-tool` 在 systemd 服务中能用；agenix 是秘密文件的管理方式，而不是取密码命令。

`--alert-account`（模块里的 `alerts.account`）对应 himalaya 的 `-a`。配置里只写了一个账号并且标了 `default = true` 时可以不管它；有多个账号就要显式指定，否则 himalaya 会报 `cannot find default account configuration`。

### 邮箱凭据落地与验证

`alerts.configFile` 由 himalaya 以服务用户身份直接读取，**不会**像 `apiTokenFile` 一样通过 `LoadCredential` 复制。配置文件、`auth.cmd` 读取的密码文件及其父目录都必须允许服务用户访问；例如 `/run/secrets` 下的文件如果是 `root:root`、`0400`，默认服务用户无法读取。`ProtectHome = "read-only"` 只限制写入，并不自动授予读取权限；实际权限仍取决于文件和目录。

下面选一种方式准备文件；路径和账号以你的配置为准，不要把明文密码写入 Nix 表达式、仓库、命令行或 Nix store。更换 `services.cloudflare-r2-monitor.user` 时，相应更改文件属主。

**sops-nix**：假设已配置 sops-nix，秘密名称分别为 `himalaya-config` 和 `smtp-password`，前者解密后的内容使用上面的 v1 配置。为 `auth.cmd` 提供 `cat`：在包含这些设置的 NixOS 模块的参数中引入 `pkgs`，并加入下面的 `systemd.services` 设置。

```nix
{
  sops.secrets."himalaya-config" = {
    owner = "cloudflare-r2-monitor";
    mode = "0400";
  };
  sops.secrets."smtp-password" = {
    owner = "cloudflare-r2-monitor";
    mode = "0400";
  };
  services.cloudflare-r2-monitor.alerts.configFile = "/run/secrets/himalaya-config";
  systemd.services.cloudflare-r2-monitor.path = [ pkgs.coreutils ];
}
```

**agenix**：假设已配置 agenix，两个 `.age` 文件分别包含完整 himalaya 配置与 SMTP 密码。配置中的 `auth.cmd` 路径相应改为 `/run/agenix/smtp-password`。同样需要在模块参数中引入 `pkgs`，让服务的 PATH 包含 coreutils。

```nix
{
  age.secrets.himalaya-config = {
    file = ./secrets/himalaya-config.age;
    owner = "cloudflare-r2-monitor";
    mode = "0400";
  };
  age.secrets.smtp-password = {
    file = ./secrets/smtp-password.age;
    owner = "cloudflare-r2-monitor";
    mode = "0400";
  };
  services.cloudflare-r2-monitor.alerts.configFile = "/run/agenix/himalaya-config";
  systemd.services.cloudflare-r2-monitor.path = [ pkgs.coreutils ];
}
```

**手动管理**：在服务账号创建后，用 root 建立受限目录并通过安全的交互式编辑器分别创建 `/etc/cf-r2-monitor/himalaya.toml` 和 `/etc/cf-r2-monitor/smtp-password`。将两文件的属主设为服务用户、权限设为 `0600`，并确保父目录允许该用户进入；配置中的 `auth.cmd` 和 `alerts.configFile` 分别指向这两个绝对路径，并像上面的示例一样将 `pkgs.coreutils` 加入服务的 `path`。不要将密码粘贴进 shell 命令或输出到日志。

**验证顺序**：

1. 确认收件人、发件人、SMTP 主机、端口和加密方式符合邮件服务商要求；确认服务账号能够遍历目录并读取两个文件，但不要输出其内容。
2. 在 NixOS 配置中临时设置 `alerts.dryRun = true;`，部署后运行 `sudo systemctl start cloudflare-r2-monitor.service`，再用 `journalctl -u cloudflare-r2-monitor.service -n 100` 查看。干跑只在确实需要告警或恢复通知时输出邮件，不会调用 himalaya，也不会记录已发送状态；日志中可能包含邮箱地址和用量，请限制访问。
3. 若当前用量未触发告警，测试期间可临时设置 `warnThreshold = 0.000001;` 和 `alerts.repeat = "0";`，以便在任一指标达到该比例时触发预览，并避免已有去重状态使测试静默。阈值必须大于零；如果各指标均低于这个值，仍不会生成邮件。测试阈值会影响真实告警判断，测试后务必还原。
4. 将 `alerts.dryRun` 改为 `false` 并重新部署，手动启动服务，检查 journal 和收件箱。真实发送会更新去重状态，也会发送真实邮件；恢复正式阈值后可能触发一封恢复通知。完成后恢复正式阈值和重发间隔，并重新部署。退出码 1 或 2 表示用量告警，4 表示发信失败；systemd 是否标记失败还取决于 `successExitCodes`。

真实发送验证依赖 SMTP 服务可用和凭据正确；干跑成功不代表 SMTP 鉴权或投递成功。

### 已使用 Home Manager？

Home Manager 的 `programs.himalaya` 和邮箱账号选项管理的是指定用户的环境；启用它们不会自动为 `cloudflare-r2-monitor` 系统服务配置邮箱。服务默认以独立的 `cloudflare-r2-monitor` 用户运行，使用模块中的 `alerts.package`，不一定与登录用户使用的程序或配置相同。

最简单的做法仍是按上一节为服务单独准备配置和凭据。如果要复用 Home Manager 生成的配置，请先确认：

- `alerts.configFile` 指向服务实际可以访问的配置文件，路径中的每级目录都允许服务用户遍历；不要假定登录用户的 home 目录对服务开放。`alerts.account` 要与配置中的账号名称匹配，或使用该配置的默认账号。
- Home Manager 生成配置所用的版本与服务 `alerts.package` 指定的 Himalaya 版本兼容。尤其注意 v1 和 v2 的配置格式不同；仅仅为登录用户选择了某个版本，不会改变服务使用的包。
- 不要把密码直接写入 Home Manager 的 Nix 选项：生成的配置可能进入可被本机其他用户读取的 Nix store。只在配置中引用取密命令或运行时文件，并确保服务用户可以读取秘密；邮箱地址、服务器和用户名等写入生成配置的信息也可能被其他本机用户看到。没有秘密的 store 配置文件并非不能用，但必须确认公开其中的非秘密信息是可接受的。
- 取密命令及其依赖必须在服务环境中可用；个人用户的密钥环、D-Bus 或登录会话不一定可供系统服务使用。按上一节先干跑，再以服务身份验证真实发送。

参见 [Home Manager 的 Himalaya 选项手册](https://nix-community.github.io/home-manager/options/home-manager/programs/himalaya.html)。具体选项和生成格式随版本变化；该手册可能比你锁定的版本更新，使用时请对照自己锁定版本的文档或模块源码。

## token 文件怎么来

模块通过 systemd 的 `LoadCredential` 从 `apiTokenFile` 指定的路径把 token 读进服务的私有目录，**token 不会进 nix store**。这是关键设计：任何情况下都不要写

```nix
# 不要这样写
apiTokenFile = builtins.readFile ./r2-token;
```

那样会把明文 token 写进 `/nix/store` 里一个全局可读的文件，并且只要你的 flake 进了 git 历史或者缓存，它就一直留在那儿。同理，`environment` 里传 token 也会泄漏到 `/proc` 和 systemd 的 unit 文件里。

三种常见的 token 文件来源：

**先说一个类型上的细节：`apiTokenFile` 是 `types.str`，不是 `types.path`，这是故意的。** Nix 里的「路径」有两种：`./relative/path` 这种 path 字面量，和 `"/absolute/path"` 这种字符串。前者在求值期就会被复制进 `/nix/store`，且默认全局可读，所以如果选项声明成 `types.path`，用户写 `./secrets/r2-token` 的那一刻 token 就已经进 store 了。模块用 `types.str` 从类型层面避免了这个后果，因此这里要写普通字符串 `"/run/secrets/r2-token"`，也别写成 `"${./secrets/r2-token}"`，那同样会把 path 拉进 store。

**sops-nix** —— 加密的 secrets 跟着仓库走，解密密钥在主机上。

```nix
{
  sops.secrets."r2-token" = {
    owner = "cloudflare-r2-monitor";
    mode = "0400";
  };

  services.cloudflare-r2-monitor.apiTokenFile = "/run/secrets/r2-token";
}
```

**agenix** —— 思路类似，用 age 密钥，文件落在 `/run/agenix/`。

```nix
{
  age.secrets.r2-token = {
    file = ./secrets/r2-token.age;
    owner = "cloudflare-r2-monitor";
  };

  services.cloudflare-r2-monitor.apiTokenFile = "/run/agenix/r2-token";
}
```

这两个项目的选项名会随版本变化，以上只是形状示意，具体请对照它们自己的文档。`owner` 要和模块的 `user` 选项一致——默认就是 `cloudflare-r2-monitor`，模块会自动创建这个系统用户和同名用户组，所以不用另外声明 `users.users`。

**手写一个文件** —— 适合自己的单机、不需要 secrets 管理的时候。

```bash
sudo install -d -m 755 /etc/cf-r2-monitor
sudo install -m 600 /dev/null /etc/cf-r2-monitor/token
printf '%s\n' '你的token' | sudo tee /etc/cf-r2-monitor/token >/dev/null
sudo chown cloudflare-r2-monitor:cloudflare-r2-monitor /etc/cf-r2-monitor/token
```

注意 `/run/secrets/` 是 tmpfs，重启就没了。手写文件放在 `/etc` 或 `/var/lib` 下才有持久性。如果非要用 `/run/secrets/`，得自己配一个在 `cloudflare-r2-monitor.service` 之前运行的 unit 来创建它（`systemd.tmpfiles.rules` 不适合放内容，只适合建空文件）。

## 二进制缓存

如果你不想每次 `nixos-rebuild` 都自己从源码编译这个工具和它的整个 Rust 依赖树，就要用二进制缓存。CI 已经会把构建产物推到 Cachix，你只需要在本地把它配置成 substituter。

有两种配法，取舍是「安全」和「便利」。

### 配法一：写进系统的 `nix.settings`（推荐）

```nix
{
  nix.settings = {
    extra-substituters = [ "https://mooling.cachix.org" ];
    extra-trusted-public-keys = [
      "mooling.cachix.org-1:9XeEiZWSaN4ngXeefj2BSjXFNSmbKpfrisiUJrf9A6M="
    ];
  };
}
```

缓存名和 CI 里用的一致，都是 `mooling`。公钥可以在 `https://app.cachix.org/cache/mooling` 页面上找到，形如 `缓存名.cachix.org-1:xxxxxxxx==`。`extra-trusted-public-keys` 里那个字符串必须是真实的签名公钥，随便填一个无效值不会报错，但来自该缓存的 path 会全部验签失败然后被丢掉，等于缓存没配。

上面是仓库作者自己的缓存，直接抄能用。**如果你 fork 了自己的版本并推自己的构建，记得换成你自己的缓存名与公钥**，否则别人拿到的 key 验不过你推的 path。

用 `extra-substituters` 而不是 `substituters`：后者会**整个替换**默认值，把 `https://cache.nixos.org` 也一起干掉，结果你会被迫从源码构建整个系统。带 `extra-` 前缀的版本是追加语义。

同理，用 `extra-trusted-public-keys` 而不是 `trusted-public-keys`。

这条配置只在你亲手写进 `configuration.nix` 之后才生效，来源明确、可审计，也不会因为某个第三方仓库改了 flake 就悄悄多出一个 substituter。代价是每次换缓存都得手动改一次配置。

### 配法二：依赖 flake 的 `nixConfig`

flake 可以自己声明要用的 substituter。本项目的 `flake.nix` 里就有这一段：

```nix
{
  nixConfig = {
    extra-substituters = [ "https://mooling.cachix.org" ];
    extra-trusted-public-keys = [
      "mooling.cachix.org-1:9XeEiZWSaN4ngXeefj2BSjXFNSmbKpfrisiUJrf9A6M="
    ];
  };
}
```

这两行与 `.github/workflows/ci.yml` 里的 `CACHIX_CACHE_NAME` 必须保持一致 —— CI 推到哪个缓存，这里就要声明哪个缓存。公钥填错不会报错，但来自该缓存的 path 会全部验签失败然后被 Nix 丢掉并回落到官方缓存或本地编译，**看起来像缓存是空的**，而不是像配置写错。所以一旦发现「推成功了却从来不命中」，先怀疑这把公钥。

这种配置默认**不生效**。不加 `--accept-flake-config` 时，Nix 发现 flake 想改 `extra-substituters` 这类设置，会**在终端里逐条问你要不要允许**（`do you want to allow configuration setting 'extra-substituters' to be set to '...' (y/N)?`），还会追问要不要把这个决定永久记下来。记下来的结果存在 `~/.local/share/nix/trusted-settings.json`，下次同一条设置就不再问。回答 `N` 或者在没有终端的环境里跑（答案默认 `n`），Nix 只会打一行 warning 然后忽略这条配置：

```
warning: ignoring untrusted flake configuration setting 'extra-substituters'.
Pass '--accept-flake-config' to trust it
```

另外 `nixConfig` 是用受限求值器读的，那两行必须是**字面量字符串**，既不能用 `let` 绑定也不能做字符串插值，所以想改缓存名只能直接编辑那两行。

想一次跳过所有询问就用 `--accept-flake-config`：

```bash
nix build github:Mooling0602/cloudflare-r2-monitor --accept-flake-config
```

便利性是显然的：仓库作者配好了，用户什么都不用改。风险也显然：`nixConfig` 能让 flake 指定 substituter，而 substituter 提供的是 store path，等于让仓库作者决定你可以从哪些服务器拉代码。对一个你不完全信任的 flake 无脑加 `--accept-flake-config`，等于给它一个后门 —— 这也是上面那种逐条询问的设计存在的理由。

**实践建议**：自己用、自己的仓库 → 无所谓，两种都行。要给别人部署 → 走配法一，把公钥写进文档让人自己贴，别指望对方替你打开 `--accept-flake-config`。

### 怎么确认缓存真的生效了

最直接的办法是看构建日志里是 `copying path` 还是 `building`：

```
copying path '/nix/store/xxxx-cf-r2-monitor-0.1.0' from 'https://mooling.cachix.org'...
```

出现 `copying path` 就是从缓存拿的。如果看到

```
building '/nix/store/xxxx-cf-r2-monitor-0.1.0.drv'...
```

说明没命中，正在本地编译。

想更确定一点，先拿到 store path，再直接去远端问「你有这个 path 吗」：

```bash
nix build .#default --print-out-paths
nix path-info --recursive ./result --store https://mooling.cachix.org
```

能打印出路径信息，说明缓存里确实有；报 `path is not valid` 就是没有。`nix-store --query --requisites ./result` 则是查这个路径的完整闭包，配合上面的 `--store` 可以逐条确认到底哪些依赖需要现编。

另外记得缓存只在你**没改过**这个 flake 时有用。本地改了 `flake.nix` 或 `Cargo.lock` 之后，求值出来的 derivation 变了，缓存自然不命中，这是正常的。

如果你在自己机器上装了 `cachix` 客户端，还有个更省事的临时办法：`cachix use cloudflare-r2-monitor` 会把 substituter 写进 `nix.conf` 引用的一段 `cachix.conf` 里，效果和配法一等价，但只对本机生效、也不经过 NixOS 配置管理。想确认当前配了哪些缓存，用 `cachix list`。

## 常见坑

**403 / `Authentication error` 但 token 看起来是对的。** 多半是 token 权限不够。这个工具只需要 `Account → Account Analytics → Read` 这一条，多给不加分，少给直接 403。另外确认 token 的区域是 Account 级而不是 Zone 级，且没有过期。

**`account ID` 填错。** 是 32 位十六进制字符串，不是邮箱、不是用户名、不是 zone ID。在 Cloudflare 后台某个域名的 Overview 页右下角能找到，或者用 `wrangler whoami`。

**`period is longer than Cloudflare's 31-day query limit`。** 分析 API 的窗口上限是 31 天，`--period 90d` 会直接报错而不是悄悄截断。要更长的周期得自己在外面累加多次结果。

**`nix shell` 和 `nix run` 不是一回事。** `nix shell github:Mooling0602/cloudflare-r2-monitor` 是开一个 shell 并把程序放进 `PATH`，然后你就可以在里面反复敲 `cf-r2-monitor`；`nix run` 是直接执行一次然后退出。想反复调试用 `nix shell`，想跑一次用 `nix run`。注意这个包**不在 nixpkgs 里**，所以别写 `nixpkgs#cf-r2-monitor`，要写 flake 的 URL。

**纯 flake 环境里没有 `cargo`。** `nix build` 在纯 sandbox 里跑，没有网络，也没有你系统 PATH 上的工具链。想在这个项目里跑 `cargo test` / `cargo clippy` / `cargo fmt`，用 `nix develop` 进 devShell。

**加依赖时没有 hash 要改，但必须提交 `Cargo.lock`。** 因为构建时不能联网，crate 依赖必须由 `Cargo.lock` 固定住——flake 直接把 `Cargo.lock` 交给 `cargoLock.lockFile`，**没有任何 hash 需要你维护**。所以加依赖的正确姿势是：

```bash
nix develop            # 进 devShell
cargo add 某个crate     # 或者手改 Cargo.toml
cargo build            # 让 Cargo.lock 更新
git add Cargo.toml Cargo.lock
```

提交之后 nix 侧什么都不用动。反过来说，**只改 `Cargo.toml` 不更新 `Cargo.lock` 就会构建失败**，因为 sandbox 里没法自己去解析版本。用 `cargo build --locked` 在本地先验一遍最稳。

**`nix flake check` 很慢，因为它会真的把测试跑一遍。** 这个包的 `doCheck` 保持 nixpkgs 默认（开启），所以每次构建都会在 nix sandbox 里跑 `cargo test`。这不是理论上的担心也不是「可能会有问题」：实际在 sandbox 里 70 个测试全部通过（63 个单元测试 + 1 个 main 测试 + 6 个 integration 测试），包括那几个 `TcpListener::bind("127.0.0.1:0")` 的集成测试——sandbox 里 loopback 是可用的，**不需要 `--no-sandbox`**，也**不需要 `--test-threads=1`**。所以 `nix build` 慢一点是正常的，只是要多跑一遍测试，不是坏了。想跳过构建只做求值（比如只检查 flake 结构），用 `nix flake check --no-build`。

**`nixos-rebuild` 之后 `services.cloudflare-r2-monitor` 不认。** 模块没有自动导入。检查 `modules` 列表里确实有 `cloudflare-r2-monitor.nixosModules.default`，名字拼写和 flake 的 `outputs` 一致。

**改完 `configuration.nix` 生效。** `sudo nixos-rebuild switch --flake .#myserver`，`myserver` 换成你 `nixosConfigurations` 里的主机名。

## 参考

- [R2 定价](https://developers.cloudflare.com/r2/pricing/)
- [R2 指标与分析](https://developers.cloudflare.com/r2/platform/metrics-analytics/)

工具本身怎么用、输出格式、退出码语义，见 [README](../README.md)。
