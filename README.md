# cf-r2-monitor

检查 Cloudflare R2 的用量距离免费额度还有多远。只读、不做任何写操作。

## 免费额度

| 项目 | 每月免费额度 |
| --- | --- |
| Storage | 10 GB-month（十进制 10×10⁹ 字节） |
| Class A operations | 1,000,000 次 |
| Class B operations | 10,000,000 次 |
| Egress | 免费 |

免费额度只对 **Standard** 存储类有效，Infrequent Access 不适用。

## 安装

**NixOS / 有 nix 的机器**，直接跑，不用装：

```bash
nix run github:Mooling0602/cloudflare-r2-monitor -- --account-id YOUR_ACCOUNT_ID --api-token-file /run/secrets/r2-token
```

NixOS 用模块更省事，见下面的 [NixOS 部署](#nixos-部署)。

**其它系统**，Release 里每个平台一个裸二进制，按自己的平台挑：

| 平台 | 文件名 |
| --- | --- |
| Linux x86_64 | `cf-r2-monitor-x86_64-linux` |
| Linux aarch64 | `cf-r2-monitor-aarch64-linux` |
| macOS Intel | `cf-r2-monitor-x86_64-darwin` |
| macOS Apple Silicon | `cf-r2-monitor-aarch64-darwin` |
| Windows x64 | `cf-r2-monitor-x86_64-windows.exe` |
| Windows arm64 | `cf-r2-monitor-aarch64-windows.exe` |

Linux 是动态链接的，只依赖 glibc 自带的那几个库，glibc 2.35+ 都能跑。以 x86_64 Linux 为例：

```bash
curl -sL -o cf-r2-monitor \
  https://github.com/Mooling0602/cloudflare-r2-monitor/releases/latest/download/cf-r2-monitor-x86_64-linux
chmod +x cf-r2-monitor
sudo install -m 755 cf-r2-monitor /usr/local/bin/
```

把文件名换成表里对应的那个即可，链接路径不用改。

macOS 从浏览器下载的二进制会带隔离属性，第一次跑前清掉：

```bash
xattr -d com.apple.quarantine ./cf-r2-monitor
```

Windows 上默认的 himalaya 一般没有，邮件告警要用 `--alert-command` 指定一个自己能找到的、能从 stdin 读原始邮件并投递的程序；不用告警的话其余功能照常。

或者自己编（需要 Rust 1.88+，见 `Cargo.toml` 的 `rust-version`）：

```bash
cargo build --release
install -m 755 target/release/cf-r2-monitor ~/.local/bin/
```

## 用法

### 1. 拿一个只读 API token

Cloudflare 后台 **My Profile → API Tokens → Create Token**，唯一需要的权限：

```
Account → Account Analytics → Read
```

这是只读权限，不要给它任何写权限。

### 2. 找到 account ID

32 位十六进制字符串，在后台某个域名的 Overview 页右下角，或者 `wrangler whoami` 的输出里。它**不是**邮箱、不是账号名、不是 zone ID。

### 3. 跑一次

```bash
export CLOUDFLARE_ACCOUNT_ID=YOUR_ACCOUNT_ID
export CLOUDFLARE_API_TOKEN=你的token

cf-r2-monitor
```

不想让 token 进 shell history 或进程列表，就写进文件：

```bash
install -m 600 /dev/null ~/.cf-r2-token
printf '%s\n' '你的token' > ~/.cf-r2-token
cf-r2-monitor --account-id YOUR_ACCOUNT_ID --api-token-file ~/.cf-r2-token
```

## 输出

```
Cloudflare R2 free-tier usage — period 30d
  METRIC                        USED         LIMIT    USED%   STATUS
  Storage                    6.90 GB      10.00 GB    69.0%   ok
  Class A operations         412,880     1,000,000    41.3%   ok
  Class B operations       1,204,551    10,000,000    12.0%   ok

  storage: 6.90 GB now, 5.32 GB daily peak avg, 5.32 GB accrued over 30 day(s)

OK: every metric is below the 80% warning threshold
```

`storage:` 那行是账单上下文，三个数字含义不同，**判定用的是前两个里更大的那个**：

- `6.90 GB now` —— 此刻的存储占用
- `5.32 GB daily peak avg` —— 每日峰值的平均值，维持到月底就是这个月的 GB-month 账单量
- `5.32 GB accrued over 30 day(s)` —— 已发生天数按比例折算的、已经花掉的额度

加 `--json` 输出机器可读版本，字段说明见 [CONTEXT.md](CONTEXT.md#json-字段)。

**界面语言**跟着 shell 走：`LC_ALL` 优先，其次 `LC_MESSAGES`，最后 `LANG`。中文 locale（`zh_CN.UTF-8` 之类）显示简体中文，其它都是英文。没有语言参数，`--json` 的字段名、退出码和错误信息也不随语言变化 —— 换 locale 不会改变脚本看到的结果。

## 退出码

| 退出码 | 含义 |
| --- | --- |
| `0` | 一切正常 |
| `1` | 至少一项进入警告区间 |
| `2` | 至少一项进入严重区间 |
| `3` | 运行错误（网络、鉴权、解析失败） |
| `4` | 用量读到了，但告警邮件没发出去 |

systemd 和监控系统可以直接消费退出码，不需要解析输出。注意参数错误返回的是 `3` 而不是 clap 默认的 `2`（`2` 已被「严重」占用）。

## 参数

| 参数 | 默认 | 含义 |
| --- | --- | --- |
| `--account-id` | 环境变量 `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account ID |
| `--api-token-file` | — | 从文件读 token（优先于 `--api-token`） |
| `--api-token` | 环境变量 `CLOUDFLARE_API_TOKEN` | 直接给 token |
| `--warn` | `0.80` | 警告阈值，占免费额度的比例；`80` 等价于 `0.80` |
| `--critical` | `0.95` | 严重阈值 |
| `--period` | `30d` | 统计窗口，接受 `30d` / `24h` / 纯数字（按天）；上限 31 天 |
| `--json` | — | 输出 JSON |
| `--endpoint` | Cloudflare 官方 GraphQL | 覆盖 API 端点（测试用） |

阈值是闭区间下界：`ratio == warn` 就算警告。

## 邮件告警（可选）

告警不内建 SMTP，而是把邮件喂给外部的 [himalaya](https://github.com/pimalaya/himalaya)。不用告警时对外部依赖零要求，**v1 和 v2 都能用**。

```bash
cf-r2-monitor \
  --account-id "$CLOUDFLARE_ACCOUNT_ID" \
  --api-token-file /run/secrets/cf-r2-token \
  --alert-to ops@example.org \
  --alert-from monitor@example.org \
  --alert-language zh-CN \
  --alert-state /var/lib/cloudflare-r2-monitor/alert-state.json
```

`--alert-to` 出现即启用。给了它就必须同时给 `--alert-from` —— himalaya 从消息的 `From:` 头推导发件人，没有这个头会直接拒绝投递。

| 参数 | 默认 | 含义 |
| --- | --- | --- |
| `--alert-to` | 无（给出即启用） | 收件人，可重复或逗号分隔 |
| `--alert-from` | 必填 | `From:` 地址，也是 SMTP 信封发件人 |
| `--alert-command` | `himalaya` | himalaya 可执行文件 |
| `--alert-account` | himalaya 默认账号 | 传给 `-a` |
| `--alert-config` | himalaya 默认配置 | 传给 `-c` |
| `--alert-state` | 无（不去重） | 去重状态文件 |
| `--alert-language` | `en` | 告警邮件语言；支持 `en` 和 `zh-CN`（简体中文），与屏幕输出和系统 locale 都无关 |
| `--alert-repeat` | `24h` | 同指标同级别的最短重发间隔；`0` 为每次都发 |
| `--alert-no-recovery` | 未设 | 关掉「已恢复」邮件 |
| `--alert-dry-run` | 未设 | 只把邮件打到 stdout，不调用 himalaya |

**去重规则**（需要 `--alert-state`）：同指标同级别在 `--alert-repeat` 内不重发；级别升级立即发；回落到 ok 发一封「已恢复」。不给状态文件就每次都发，适合手工跑。

**上线前先干跑**：`--alert-dry-run` 能验证收件人、发件人、主题、正文拼得对不对；它验证不了 himalaya 那一侧的问题（账号选错、SMTP 认证失败、服务器拒收），那些只有真发一次才知道。

himalaya 两个版本的差异、配置怎么写、为什么有 90 秒超时，见 [CONTEXT.md](CONTEXT.md#告警为什么要过-himalaya)。

## 定时跑

cron：

```cron
0 */6 * * * /usr/local/bin/cf-r2-monitor --account-id YOUR_ACCOUNT_ID --api-token-file /run/secrets/cf-r2-token --alert-to ops@example.org --alert-state /var/lib/cf-r2-monitor/alert-state.json
```

cron 的 `PATH` 很短，通常找不到 himalaya —— 要么给 `--alert-command` 写绝对路径，要么在 crontab 顶部设 `PATH=`。另外 cron 会把非 0 退出码当异常处理，而这里的 `1`/`2` 是**有意义的告警**，不是脚本坏了。

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

默认 `1` 和 `2` 会让 unit 显示成 failed。想让它算成功就加 `SuccessExitStatus=1 2`，但**不要**把 `4` 加进去 —— 邮件发不出去应该保持是失败。

## NixOS 部署

把 flake 加进你的 `flake.nix`，然后：

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

token 通过 systemd 的 `LoadCredential` 从文件注入，**不会进 nix store**。只要保证那个文件在系统上存在（手写 `/run/secrets/`、sops-nix、agenix 都行），绝对不要用 `builtins.readFile` 把它读进 store。

加告警：

```nix
{
  services.cloudflare-r2-monitor.alerts = {
    enable = true;
    to = [ "ops@example.org" ];
    from = "monitor@example.org";
    language = "zh-CN";
    configFile = "/run/secrets/himalaya.toml";
  };
}
```

flake 的几种接法、模块完整选项表、二进制缓存怎么配、常见报错，见 [docs/nix.md](docs/nix.md)。

## 更多

设计取舍、存储算法的来龙去脉、himalaya 兼容细节、踩过的坑，都在 [CONTEXT.md](CONTEXT.md)。

两份待实现的调研，记录的是「本工具现在不做、但方案已经想清楚」的部分：

- [docs/cf-email.md](docs/cf-email.md) —— 用 Cloudflare Email Service 替代或备用 himalaya
- [docs/cf-worker-integration.md](docs/cf-worker-integration.md) —— 额度逼近时让 Worker 侧自己停止调用 R2 的 Gate 协议

## License

MIT
