# cf-r2-monitor

`cf-r2-monitor` 是一个用 Rust 写的命令行工具，用来检查 Cloudflare R2 的用量距离免费额度还有多远，并把结果做成人能看的表格、机器能读的 JSON，以及 systemd 和监控系统能直接消费的退出码。

## 为什么需要它

Cloudflare 后台自带的预算告警是 **informational only** —— 它只会发一封邮件通知你，并不会暂停你的用量，也不会帮你挡住账单。而 R2 的 REST API 里**没有** suspend/disable 之类的接口，所以任何工具（包括这一个）都无法真的「停掉」R2。这意味着唯一的办法是：在用量逼近额度时**尽快知道**，然后自己决定怎么办（清理桶、升级套餐、接受账单）。

顺带一提，免费额度只对 **Standard** 存储类有效，Infrequent Access 存储类不在免费额度内，它有自己的免费额度算法。本工具按 Standard 的额度判断。

这个工具只做监控和告警。它拿到的是只读 token，从头到尾不做任何写操作，不会创建、删除、暂停或修改任何东西。

## 免费额度速查

| 项目 | 每月免费额度 | 说明 |
| --- | --- | --- |
| Storage | 10 GB-month | 十进制 10×10⁹ 字节（10,000,000,000 B），不是 10 GiB |
| Class A operations | 1,000,000 次 | 写、列举、复制等 |
| Class B operations | 10,000,000 次 | 读等 |
| Egress（出网流量） | 免费 | 不限量 |
| 适用存储类 | 仅 Standard | Infrequent Access 不适用这套免费额度 |

两个容易被忽略的计费细节：

**计费按「下一个单位向上取整」。** 1,000,001 次操作会按 2,000,000 次计费，而不是 1,000,001 次。

**存储按「每日峰值的平均值」折算 GB-month。** 一个桶存了 1 GB 五天、3 GB 二十五天，账单是 2.67 GB-month，既不是 3 GB-month，也不是 90 GB-month。所以任何**瞬时读数都不等于月度账单**。

数据来源是 Cloudflare GraphQL Analytics API 的两个数据集：`r2StorageAdaptiveGroups`（存储）和 `r2OperationsAdaptiveGroups`（操作）。参见 [R2 定价](https://developers.cloudflare.com/r2/pricing/) 与 [R2 指标与分析](https://developers.cloudflare.com/r2/platform/metrics-analytics/)。

## 快速开始

### 1. 拿一个只读 API token

在 Cloudflare 后台 **My Profile → API Tokens → Create Token**，给这个 token 唯一需要的权限：

```
Account → Account Analytics → Read
```

这是只读权限。工具不写任何东西，所以不要给它任何写权限。

### 2. 找到 account ID

Account ID 是 **32 位十六进制字符串**，在 Cloudflare 后台某个域名的 Overview 页面右下角，或者 `wrangler whoami` 的输出里。它**不是**邮箱、不是账号名、不是 zone ID。

### 3. 跑一次

```bash
export CLOUDFLARE_ACCOUNT_ID=YOUR_ACCOUNT_ID
export CLOUDFLARE_API_TOKEN=你的token

cf-r2-monitor
```

不想让 token 出现在 shell history 或进程列表里，就写进文件再用 `--api-token-file`：

```bash
install -m 600 /dev/null /tmp/r2-token
printf '%s\n' '你的token' > /tmp/r2-token
cf-r2-monitor --account-id YOUR_ACCOUNT_ID --api-token-file /tmp/r2-token
```

## 输出示例

### 表格模式（默认）

```
Cloudflare R2 free-tier usage — period 30d
  METRIC                        USED         LIMIT    USED%   STATUS
  Storage                    6.90 GB      10.00 GB    69.0%   ok
  Class A operations         412,880     1,000,000    41.3%   ok
  Class B operations       1,204,551    10,000,000    12.0%   ok

  storage: 6.90 GB now, 5.45 GB daily peak avg, 5.45 GB accrued over 30 day(s)

OK: every metric is below the 80% warning threshold
```

表格下面那行 `storage:` 是账单上下文，三个数字含义不同，**判定用的是前两个里更大的那个**：

- `6.90 GB now` —— 此刻的存储占用。
- `5.45 GB daily peak avg` —— 观测窗口内「每日峰值」的平均值。如果用量维持在这个水平到月底，它**就是**整月的 GB-month 账单量。
- `5.45 GB accrued over 30 day(s)` —— 已发生天数按比例折算的、已经花掉的 GB-month 用量，永远 ≤ 上面的日均峰值。

所以 `Storage` 那一行的 `USED` 是 `max(now, daily peak avg)`：只看瞬时值会漏掉「整月都超标、今早刚清理掉」的桶，只看均值会漏掉「今天刚爆的尖峰」。

观测窗口不足 7 天时，均值还不够代表一个月，这行会短一些（`3 day(s)`），数值也更可能偏离实际账单：

```
  storage: 4.20 GB now, 4.10 GB daily peak avg, 410.00 MB accrued over 3 day(s)
```

如果一条带日期的存储读数都没有，这行会直接说不知道：

```
  storage: 3.42 GB stored now; no dated readings, GB-month unknown
```

### 删除桶之后

删掉一个桶，它仍然会在查询窗口里留下删除前的最后几条读数。本工具把「最新读数比账号最新采样落后 36 小时以上」的桶判定为已删除，**从 `now` 里排除**，但**仍计入 `daily peak avg`** —— 它存在的那些天确实产生了账单。一旦发生，表格下面会多一行说明：

```
  storage: 1.00 GB now, 2.00 GB daily peak avg, 0.67 GB accrued over 10 day(s)
           (1 bucket(s) deleted during the window: excluded from 1.00 GB now, still counted in the average)
```

为什么不直接丢掉整个桶：只看瞬时值会让你以为删了就没事，而账单里还挂着半个月的峰值。为什么不继续计入 `now`：那会让总数永远降不下来 —— 这正是删桶后数字不动的常见原因。

36 小时的余量是为了容忍漏采一次样本：宁可多算，不可少算，少算存储是唯一会让免费额度悄悄变成账单的错误。

用量进入严重区间时，最后一行会变成这样：

```
CRITICAL: Class A operations is at 99.5% of the free tier, 5,000 left
```

如果已经超额，措辞是 `— past the limit by <n>`。

### JSON 模式（`--json`）

```json
{
  "readings": [
    {
      "metric": "storage",
      "used": 6900000000,
      "limit": 10000000000,
      "ratio": 0.69,
      "status": "ok"
    },
    {
      "metric": "class_a",
      "used": 412880,
      "limit": 1000000,
      "ratio": 0.41288,
      "status": "ok"
    },
    {
      "metric": "class_b",
      "used": 1204551,
      "limit": 10000000,
      "ratio": 0.1204551,
      "status": "ok"
    }
  ],
  "thresholds": {
    "warn": 0.8,
    "critical": 0.95
  },
  "storage": {
    "current_bytes": 6900000000,
    "average_daily_peak": 5450000000,
    "accrued_month_bytes": 5450000000,
    "observed_days": 30,
    "stale_buckets": 0
  },
  "snapshot": {
    "storage": [
      {
        "bucket_name": "assets",
        "payload_size": 4000000000,
        "metadata_size": 0,
        "object_count": 128,
        "upload_count": 0,
        "at": "2026-10-27T00:00:00Z"
      },
      {
        "bucket_name": "assets",
        "payload_size": 6900000000,
        "metadata_size": 0,
        "object_count": 128,
        "upload_count": 0,
        "at": "2026-11-25T00:00:00Z"
      }
    ],
    "operations": [
      {
        "action_type": "PutObject",
        "requests": 412880,
        "class": "class_a"
      },
      {
        "action_type": "GetObject",
        "requests": 1204551,
        "class": "class_b"
      }
    ],
    "observed_at": "2026-11-25T09:35:38.335735825Z"
  }
}
```

实际的 `snapshot.storage` 数组是**每个桶每天一条**（30 天窗口就是每条桶 30 条），上面为了篇幅只留了首尾两条，真实输出里不会省略。

字段约定：

- `readings` 固定三项，顺序是 `storage`、`class_a`、`class_b`。`metric` 取值 `storage` / `class_a` / `class_b`，`status` 取值 `ok` / `warn` / `critical`，全是 snake_case。
- `ratio` 是 `used / limit`，未取整，方便你自己再判断。
- `storage.current_bytes` 是此刻占用；`storage.average_daily_peak` 是每日峰值的平均值（维持到月底就是整月 GB-month 账单量）；`storage.accrued_month_bytes` 是已发生天数按比例折算的已用 GB-month，永远 ≤ `average_daily_peak`。`readings` 里 storage 那一项的 `used` 取 `max(current_bytes, average_daily_peak)`。
- `storage.stale_buckets` 是窗口内停止上报（通常是已删除）的桶数量。这些桶不计入 `current_bytes`，但仍计入 `average_daily_peak`。
- `observed_days < 7` 时这两个存储数字的参考价值都低，别拿它当月的结论。
- `snapshot` 是分析 API 的原始读数。操作类型的分档在 `snapshot.operations[].class`，取值 `class_a` / `class_b` / `free`；`snapshot.storage[]` 里没有分档字段。
- `observed_at` 是本次运行的时间戳（RFC3339），`snapshot.storage[].at` 是该条存储读数的采样时间。

## 退出码

| 退出码 | 含义 |
| --- | --- |
| `0` | 一切正常，所有指标都在警告阈值以下 |
| `1` | 至少一项进入警告区间 |
| `2` | 至少一项进入严重区间 |
| `3` | 运行错误（网络、鉴权、解析失败） |
| `4` | 用量读到了，但告警邮件没发出去 |

这个设计是为了让 systemd 和任何监控系统**直接消费退出码**，不需要解析输出。注意 `--help` 和 `--version` 走的是 `0`。

一个需要留意的细节：`3` 和常见 CLI 的「参数错误 = 2」不同。参数错误在这里也返回 `3`，因为 `2` 已经被「严重」占用了。所以如果你的 wrapper 脚本里假设了 clap 的默认退出码，要改一下。

`4` 单独拆出来是有意的：告警通道坏了，既不能伪装成「一切正常」（否则你永远不会知道），也不能伪装成「超额度了」（那是假警报）。所以用量读不到是 `3`，用量读到了但邮件发不出去是 `4`。

## Email alerts (optional)

工具不内建 SMTP 客户端。告警是把邮件用 stdin 喂给外部的 `himalaya` 命令行，由它完成投递。不用告警时对外部依赖零要求；用告警时系统上得有一个 `himalaya`，**v1 和 v2 都能用**。

两个版本的命令行不一致，本项目已做过兼容，但有几点值得知道：

- **子命令**：走的是 `message send`。v1 和 v2 都有这个子命令，所以它同时适用。
- **参数位置**：`-a`/`-c` 放在 `message send` **之后**。v2 把这两个标志标为 global，放在前后都行；v1 只在 `message send` 子命令上定义 `-a`，放前面会直接报 `error: unexpected argument '-a' found` 而不发信。
- **`-c` 的分隔符**：v1 的 `-c` 是 `value_delimiter = ':'` 的 `Vec<PathBuf>`，路径里带 `:` 会被切开；v2 的多路径用别的合并规则。
- **`save-copy`**：v1 默认 `message.send.save-copy = true`。没配 maildir 时，邮件**已经发出去了**，但存副本那一步失败，himalaya 以退出码 1 收场。本项目把这条特定报错误判为「已投递」，只打一行 warning；想彻底消掉就在 himalaya 的配置里设 `message.send.save-copy = false`。
- **v1 有个死循环缺陷**：SMTP 服务器对 `RCPT TO` 回 550 时，v1 会不断重试同一次投递且永不退出（实测 3 分钟内打了 104 万行 `WARN email::smtp: server replied with code 550` 的日志）。本项目给子进程设了 **90 秒硬超时**，超时即 kill 并当作发信失败（退出码 `4`），避免把整个定时任务挂死。

最小可用用法：

```bash
cf-r2-monitor \
  --account-id "$CLOUDFLARE_ACCOUNT_ID" \
  --api-token-file /run/secrets/cf-r2-token \
  --alert-to ops@example.org \
  --alert-from monitor@example.org \
  --alert-state /var/lib/cloudflare-r2-monitor/alert-state.json
```

`--alert-to` 出现即启用告警，可以重复传，也可以在一个参数里用逗号分隔多个地址。给了 `--alert-to` 就必须同时给 `--alert-from`：两个版本的 himalaya 都从消息的 `From:` 头推导 SMTP 信封发件人，没有这个头就拒绝投递（v2 的报错是 `No \`From:\` header found in raw message`，v1 是 `cannot send message without a sender`），所以这里没有「回落到账号默认地址」这回事。完整参数：

| 参数 | 含义 | 默认 |
| --- | --- | --- |
| `--alert-to` | 收件人，可重复或逗号分隔 | 无（给出即启用） |
| `--alert-from` | `From:` 地址，也是 SMTP 信封发件人 | **必填**（随 `--alert-to`） |
| `--alert-command` | himalaya 可执行文件 | `himalaya` |
| `--alert-account` | 传给 himalaya 的 `-a` 账号名 | himalaya 的默认账号 |
| `--alert-config` | 传给 himalaya 的 `-c` 配置文件 | himalaya 的默认配置 |
| `--alert-state` | 去重状态文件 | 无（不去重，每次都发） |
| `--alert-repeat` | 同一指标同一级别的最短重发间隔 | `24h` |
| `--alert-no-recovery` | 关掉「已恢复」邮件 | 未设（会发） |
| `--alert-dry-run` | 只把邮件打到 stdout，不调用 himalaya | 未设（真发） |

### 去重规则

给了 `--alert-state` 之后，工具在这个文件里记下每个指标上一次告警的时间、级别和状态，然后：

- **同一指标、同一严重级别，`--alert-repeat` 时间内不重复发。** 默认 `24h`，也就是一天最多提醒一次。
- **级别升级立即发。** warn → critical 不受 `--alert-repeat` 约束，这是你真正需要马上知道的变化。
- **回落到 ok 发一封「已恢复」。** 不想要就用 `--alert-no-recovery` 关掉。
- **`--alert-repeat 0` 表示每次都发**，等于完全不去重。

不给 `--alert-state` 就退化成「每次都发」，适合手工跑，不适合定时任务。

### 上线前先干跑

`--alert-dry-run` 把要发的邮件原样打到 stdout，不调用 himalaya。它能验证的是邮件本身：收件人、发件人、主题、正文有没有拼错。它**不能**验证 himalaya 那一侧的问题（账号选错、SMTP 认证失败、服务器拒收），那些只有真发一次才知道。

### 三种跑法

cron：

```cron
# crontab -e
0 */6 * * * /usr/local/bin/cf-r2-monitor --account-id YOUR_ACCOUNT_ID --api-token-file /run/secrets/cf-r2-token --alert-to ops@example.org --alert-state /var/lib/cf-r2-monitor/alert-state.json
```

cron 的 `PATH` 很短，通常找不到 himalaya，要么给 `--alert-command` 写绝对路径，要么在 crontab 顶部设 `PATH=`。另外 cron 会把非 0 退出码当成异常处理（可能给你发本地邮件），而这里的 `1`/`2` 是**有意义的告警**，不是脚本坏了；介意的话就在 crontab 里自己接住退出码。

systemd timer：

```ini
# /etc/systemd/system/cf-r2-monitor.service
[Unit]
Description=Cloudflare R2 free-tier check

[Service]
Type=oneshot
User=cf-r2-monitor
LoadCredential=r2-token:/run/secrets/cf-r2-token
StateDirectory=cf-r2-monitor
ExecStart=/usr/local/bin/cf-r2-monitor --account-id YOUR_ACCOUNT_ID --api-token-file %d/r2-token --alert-to ops@example.org --alert-state /var/lib/cf-r2-monitor/alert-state.json
```

```ini
# /etc/systemd/system/cf-r2-monitor.timer
[Unit]
Description=Run the Cloudflare R2 check every 6 hours

[Timer]
OnCalendar=*-*-* 00/6:00:00
Persistent=true

[Install]
WantedBy=timers.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now cf-r2-monitor.timer
```

`StateDirectory=` 让 systemd 建好 `/var/lib/cf-r2-monitor` 并交给服务用户，不用自己写 `tmpfiles.d`。`%d` 是凭据目录，`LoadCredential` 读到的 token 就放在那儿。默认 `1` 和 `2` 会让 unit 显示成 failed，想让它算成功就加 `SuccessExitStatus=1 2`；但**不要**把 `4` 加进去，邮件发不出去应该保持是失败。

NixOS 用模块更省事，用户、凭据、`StateDirectory`、himalaya 都由模块接好了：

```nix
{
  services.cloudflare-r2-monitor = {
    enable = true;
    accountId = "YOUR_ACCOUNT_ID";
    apiTokenFile = "/run/secrets/cf-r2-token";
    alerts = {
      enable = true;
      to = [ "ops@example.org" ];
      configFile = "/run/secrets/himalaya.toml";
    };
  };
}
```

`alerts.*` 的完整选项、himalaya 配置怎么写、`alerts.stateFile` 为什么不能乱指，见 [docs/nix.md](docs/nix.md)。

## 阈值与统计窗口

| 参数 | 默认值 | 含义 |
| --- | --- | --- |
| `--warn` | `0.80` | 占免费额度的比例，达到即为警告 |
| `--critical` | `0.95` | 同上，达到即为严重 |
| `--period` | `30d` | 统计窗口 |

阈值是**闭区间下界**：`ratio == warn` 就算警告，不是「超过 warn 才算」。阈值必须是 `(0, 1.0]` 之间的有限数，并且 `warn <= critical`，否则直接报错。

`--warn 80` 和 `--warn 0.80` 等价 —— 大于 1.0 的值会被当成百分比。

`--period` 接受 `30d`、`7d`、`24h` 这种写法，也接受纯数字（按天算）。**窗口上限是 31 天**，这是 Cloudflare 分析 API 的限制。超过 31 天不是被悄悄截断，而是直接报错 `period is longer than Cloudflare's 31-day query limit`，因为静默截断会让你以为自己在看 90 天的数据。

### 一个必须知道的坑

**存储的瞬时读数不等于月度账单。**

R2 不按「你跑检查那一刻存了多少字节」收费，而是记录每天的峰值、把峰值在计费周期上取平均，按 GB-month 计费。这两个数在月中可以差很远：前面 20 天只存了 1 GB，最后 10 天涨到 12 GB，此刻读数是 12 GB，但整月的 GB-month 平均可能只有 4.67 GB。

所以工具给你三个数，判定用的是前两个里更大的那个：

| 字段（JSON） | 表格里的叫法 | 含义 | 用途 |
| --- | --- | --- | --- |
| `storage.current_bytes` | `N GB now` | 此刻占用 | 该不该现在清理 |
| `storage.average_daily_peak` | `N GB daily peak avg` | 每日峰值的平均；维持到月底就是这个月的 GB-month 账单量 | 这个月会不会被收费 |
| `storage.accrued_month_bytes` | `N GB accrued over D day(s)` | 已发生天数按比例折算的已用 GB-month，永远 ≤ 日均峰值 | 已经花掉多少额度 |

`readings` 里 storage 那一项的 `used` 是 `max(current_bytes, average_daily_peak)`，两个都要看：只看瞬时值会漏掉「整月都超标、今早刚清掉」的桶，只看均值会漏掉「今天刚爆出来的尖峰」。用量平稳时两者相等，判定也就没有歧义。

`observed_days < 7` 时，均值的样本太少，别拿它当整月的结论 —— 这也是为什么模块默认每 6 小时跑一次、窗口给 30 天。

## NixOS 部署

工具提供 flake，nixpkgs 钉在 `nixos-26.05` 稳定频道（不是 unstable）。直接跑：

```bash
nix run github:Mooling0602/cloudflare-r2-monitor -- --account-id YOUR_ACCOUNT_ID --api-token-file /run/secrets/r2-token
```

用 NixOS 模块的话，把 flake 加进你的 `flake.nix`，然后：

```nix
{
  services.cloudflare-r2-monitor = {
    enable = true;
    accountId = "YOUR_ACCOUNT_ID";
    apiTokenFile = "/run/secrets/r2-token";
    interval = "6h";
  };
}
```

token 通过 systemd 的 `LoadCredential` 从文件注入，**不会进 nix store**。所以你只需要保证那个文件在系统上存在（自己手写 `/run/secrets/`、用 sops-nix、用 agenix 都行），绝对不要用 `builtins.readFile` 把它读进 store。

完整说明（flake 的几种接法、模块全部选项、二进制缓存怎么配、怎么验证缓存生效、常见报错）见 [docs/nix.md](docs/nix.md)。

## 已知限制 / 不做什么

- **不会停服务。** R2 没有 suspend 接口，本工具只监控和告警。要「停止用量」只能靠你自己清理或改配置。
- **不处理 Infrequent Access 的额度差异。** 免费额度只适用于 Standard，按 IA 存的数据在这里会被算进同一份 10 GB-month 里。如果你混用两种存储类，数字需要你自己再拆分看。
- **31 天窗口上限。** 想统计更长周期请自己在外面累加，或者看 Cloudflare 后台的账单页面。
- **瞬时快照 ≠ 月度账单。** 见上一节。
- **不做写入、不做修复。** 只读 token，只输出。
- **只覆盖 R2。** 不涉及 Workers、D1、CDN 或其它 Cloudflare 产品的用量。

## License

MIT
