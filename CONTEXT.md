# 背景与设计取舍

这份文档记录 `cf-r2-monitor` 背后的技术细节：为什么要做、额度怎么算、几个数字为什么是现在这个算法、以及踩过哪些坑。想直接上手用，看 [README](README.md)。

## 为什么需要它

Cloudflare 后台自带的预算告警是 **informational only** —— 它只发一封邮件通知你，不会暂停用量，也不会挡住账单。而 R2 的 REST API 里**没有** suspend/disable 之类的接口（只有 buckets 的 create/delete/edit/get/list，加上 cors、domains、jobs、lifecycle、locks 这些子资源），所以任何工具都无法真的「停掉」R2。

结论：唯一的办法是在用量逼近额度时**尽快知道**，然后自己决定怎么办。

这个工具只做监控和告警。它拿的是只读 token，从头到尾不做任何写操作。

## 免费额度

| 项目 | 每月免费额度 | 说明 |
| --- | --- | --- |
| Storage | 10 GB-month | 十进制 10×10⁹ 字节（10,000,000,000 B），不是 10 GiB |
| Class A operations | 1,000,000 次 | 写、列举、复制等 |
| Class B operations | 10,000,000 次 | 读等 |
| Egress（出网流量） | 免费 | 不限量 |
| 适用存储类 | 仅 Standard | Infrequent Access 不适用这套免费额度 |

两个容易忽略的计费细节：

**计费按「下一个单位向上取整」。** 1,000,001 次操作按 2,000,000 次计费，不是 1,000,001 次。所以判定用的是原始计数，但真实账单会比这个数字略高一点。

**存储按「每日峰值的平均值」折算 GB-month。** 一个桶存了 1 GB 五天、3 GB 二十五天，账单是 `(1×5 + 3×25) / 30 = 2.67 GB-month`，既不是 3 GB-month，也不是 90 GB-month。所以任何**瞬时读数都不等于月度账单**。

数据来源是 Cloudflare GraphQL Analytics API 的两个数据集：`r2StorageAdaptiveGroups`（存储）和 `r2OperationsAdaptiveGroups`（操作）。参见 [R2 定价](https://developers.cloudflare.com/r2/pricing/) 与 [R2 指标与分析](https://developers.cloudflare.com/r2/platform/metrics-analytics/)。注意 Cloudflare 后台的 R2 页面读的是同一份数据，所以面板更新慢的时候本工具也会一样慢 —— 这不是工具的 bug。

## JSON 字段

顶层三个键：

- `readings` —— 固定三项，顺序 `storage`、`class_a`、`class_b`。每项 `{ metric, used, limit, ratio, status }`，`metric` 取 `storage` / `class_a` / `class_b`，`status` 取 `ok` / `warn` / `critical`，全是 snake_case。`ratio = used / limit`，未取整。
- `thresholds` —— `{ warn, critical }`，就是本次运行实际用的阈值。
- `storage` —— `StorageDetail`：`current_bytes`、`average_daily_peak`、`accrued_month_bytes`、`observed_days`、`stale_buckets`。
- `snapshot` —— 分析 API 的原始读数：`storage[]`（每桶每天一条，字段 `bucket_name`、`payload_size`、`metadata_size`、`object_count`、`upload_count`、`at`）、`operations[]`（`action_type`、`requests`、`class`）、`observed_at`（本次运行时间戳，RFC3339）。

`snapshot.storage[]` 里没有分档字段；操作类型的分档在 `snapshot.operations[].class`，取 `class_a` / `class_b` / `free`。

`readings` 里 storage 那一项的 `used` 取 `max(current_bytes, average_daily_peak)`，见下一节。

## 存储的三个数字

R2 不按「跑检查那一刻存了多少字节」收费，而是记录每天的峰值、在计费周期上取平均，按 GB-month 计费。这两个数在月中可以差很远：前面 20 天只存了 1 GB，最后 10 天涨到 12 GB，此刻读数是 12 GB，但整月的 GB-month 平均可能只有 4.67 GB。

所以工具给三个数，**判定用的是前两个里更大的那个**：

| 字段（JSON） | 表格里的叫法 | 含义 | 用途 |
| --- | --- | --- | --- |
| `storage.current_bytes` | `N GB now` | 此刻占用 | 该不该现在清理 |
| `storage.average_daily_peak` | `N GB daily peak avg` | 每日峰值的平均；维持到月底就是这个月的 GB-month 账单量 | 这个月会不会被收费 |
| `storage.accrued_month_bytes` | `N GB accrued over D day(s)` | 已发生天数按比例折算的已用 GB-month，永远 ≤ 日均峰值 | 已经花掉多少额度 |

`readings` 里 storage 那一项的 `used` 是 `max(current_bytes, average_daily_peak)`。理由：只看瞬时值会漏掉「整月都超标、今早刚清掉」的桶，只看均值会漏掉「今天刚爆出来的尖峰」。用量平稳时两者相等，判定也就没有歧义。

`observed_days < 7` 时均值的样本太少，别拿它当整月的结论 —— 这也是模块默认每 6 小时跑一次、窗口给 30 天的原因。

### 均值为什么按桶分别算

`average_daily_peak` 是**每个桶各自按自己的观测天数取平均，再相加**，不是「全局求和后除以共享天数」。

后者有个静默错误：昨天才出现的桶会把其它桶的均值稀释到接近 0。比如 assets 每天稳定 10 GB 已经跑满 10 天，今天新建一个 1 GB 的 backups，全局求和的算法会算出 `11 GB / 10 天 ≈ 1.1 GB`，把真实的 10 GB 超标完全掩盖掉。

### 已计费量为什么会超过均值（已修）

`accrued_month_bytes = average_daily_peak × observed_days / 30`，其中 `observed_days` 被**封顶在 30**。

不封顶会出问题：`Window::trailing_days(30)` 的窗口是 `now - 30d .. now`，没对齐零点，晚上 21:45 起算会跨到 **31 个日历日期**。实测真实账号时出现过 `accrued 1.30 GB > daily peak avg 1.26 GB`，违反「已计费量永远 ≤ 月均值」的设计约束。封顶后恢复正常。

## 删除桶之后

删掉一个桶，它仍然会在查询窗口里留下删除前的最后几条读数。如果不处理，这些读数会被当成「当前占用」永远计入总数 —— 这正是「删了桶但总存储不降」的常见原因。

判定规则：**最新读数比账号最新采样落后超过 36 小时（`STALE_AFTER`）的桶视为已删除**。

- 从 `now` 里排除（它现在确实不存在了）。
- **仍计入 `daily peak avg`**（它存在的那些天确实产生了账单）。

一旦发生，表格下面会多一行说明：

```
  storage: 1.00 GB now, 2.00 GB daily peak avg, 0.67 GB accrued over 10 day(s)
           (1 bucket(s) deleted during the window: excluded from 1.00 GB now, still counted in the average)
```

36 小时这个余量的理由：Cloudflare 按日采样、同一趟采样覆盖所有存活桶，所以存活桶之间的时间戳只差几分钟，而已删除桶落后整整一个采样周期。36 小时 = 一个采样周期 + 余量，长到漏采一次样本不会误判，短到下次运行就能把已删除桶剔出总数。**宁可多算，不可少算** —— 少算存储是唯一会让免费额度悄悄变成账单的错误。

**已知边界**：删的是账号里**唯一**一个桶时，之后没有新采样点作参照，这条最后读数会一直被当成当前值。数据里没有信息能区分「桶还在但今天没采到样」和「桶没了」。有多个桶时不会出现。

## Class A / Class B 怎么分

按 `actionType` 白名单分类，白名单之外的归 `free`（不占额度）。

**Class A**：ListBuckets、PutBucket、ListObjects、PutObject、CopyObject、CompleteMultipartUpload、CreateMultipartUpload、LifecycleStorageTierTransition、ListMultipartUploads、UploadPart、UploadPartCopy、ListParts、PutBucketEncryption、PutBucketCors、PutBucketLifecycleConfiguration。

**Class B**：HeadBucket、HeadObject、GetObject、UsageSummary、GetBucketEncryption、GetBucketLocation、GetBucketCors、GetBucketLifecycleConfiguration。

遇到白名单外的操作类型时，统计上按 `free` 处理，但 `classify_strict()` 会把它算成 Class A —— 保守起见，未知操作不会让你少算额度。

## 退出码

| 退出码 | 含义 |
| --- | --- |
| `0` | 一切正常，所有指标都在警告阈值以下 |
| `1` | 至少一项进入警告区间 |
| `2` | 至少一项进入严重区间 |
| `3` | 运行错误（网络、鉴权、解析失败） |
| `4` | 用量读到了，但告警邮件没发出去 |

这个设计是为了让 systemd 和任何监控系统**直接消费退出码**，不需要解析输出。`--help` 和 `--version` 走的是 `0`。

**为什么参数错误也返回 `3`**：clap 默认的参数错误退出码是 `2`，会和「严重」撞车，所以 `main.rs` 手写了 `Args::try_parse()` 并把它改成 `3`。如果你的 wrapper 脚本里假设了 clap 的默认行为，要改一下。

**为什么 `4` 要单独拆出来**：告警通道坏了，既不能伪装成「一切正常」（否则你永远不会知道告警是坏的），也不能伪装成「超额度了」（那是假警报）。所以用量读不到是 `3`，用量读到了但邮件发不出去是 `4`。

## 阈值与窗口

阈值是**闭区间下界**：`ratio == warn` 就算警告，不是「超过 warn 才算」。

`--warn 80` 和 `--warn 0.80` 等价 —— 大于 1.0 的值会被当成百分比。

阈值必须是 `(0, 1.0]` 之间的有限数，并且 `warn <= critical`，否则直接报错。

**窗口上限 31 天**，这是 Cloudflare 分析 API 的限制。超过不是被悄悄截断，而是直接报错 `period is longer than Cloudflare's 31-day query limit` —— 静默截断会让你以为自己在看 90 天的数据。

## 为什么存储要按时间序列存

`r2StorageAdaptiveGroups` 必须按 `dimensions { bucketName datetime }` 分组、`orderBy: [datetime_DESC]`，一次返回多行历史。

早期版本只按 `bucketName` 分组，然后把每行当独立桶累加，结果把历史时间桶全部求和，存储量严重重复计数（30 天窗口会放大 30 倍）。现在保留完整时间序列，`BucketStorage` → `StoragePoint`（带 `at: Option<DateTime<Utc>>`），再按需聚合。

## 告警为什么要过 himalaya

工具不内建 SMTP 客户端，而是把邮件用 stdin 喂给外部的 `himalaya`。理由：用户通常已经有一套自己的邮件配置（账号、密码来源、TLS 设置），让它接手就不用在这里重复实现一遍，也不用在工具里存 SMTP 密码。

代价是得兼容 himalaya 的两个大版本，下面是实测出来的差异。

### v1 与 v2 的差异

- **子命令**：走 `message send`，两版都有。
- **参数位置**：`-a`/`-c` 放在 `message send` **之后**。v2 把这两个标志标为 global，放前后都行；v1 只在 `message send` 子命令上定义 `-a`，放前面会报 `error: unexpected argument '-a' found` 而不发信。放最后是两个版本都接受的唯一顺序。
- **`-c` 的分隔符**：v1 的 `-c` 是 `value_delimiter = ':'` 的 `Vec<PathBuf>`，路径里带 `:` 会被切开；v2 的多路径用别的合并规则。
- **配置格式**：nixpkgs 26.05 的 `pkgs.himalaya` 是 **v1.2.0**，`[accounts.<name>]` 里 `email` 和 `message.send.backend.login` 都是必填，缺了直接报 `missing field 'email'` / `missing field 'login'`；`default = true` 必须写在账号表内部。v2 的 `[accounts.default.smtp]` 风格配置在 v1 下无法解析。具体写法见 [docs/nix.md](docs/nix.md)。
- **`save-copy`**：v1 默认 `message.send.save-copy = true`。没配 maildir 时邮件**已经发出去了**，但存副本那一步失败，himalaya 以退出码 1 收场。本项目把这条特定报错（`cannot add message`）判定为「已投递」，只打一行 warning；想彻底消掉就在 himalaya 配置里设 `message.send.save-copy = false`。匹配刻意收窄 —— 收件人被拒、密码错、服务器不可达都是别的文案，仍然是失败。

### 为什么必须有 `From:` 头

两版 himalaya 都从消息的 `From:` 头推导 SMTP 信封发件人：

- v2 源码 `src/email/submission.rs:34` 直接 `.ok_or_else(|| anyhow!("No \`From:\` header found in raw message"))?`，报 `No \`From:\` header found in raw message`。
- v1 报 `cannot send message without a sender`。

所以 `--alert-from` 是 `--alert-to` 的必填伴随项，没有「回落到账号默认地址」这回事。早期版本把它做成可选项，照文档配下来每次告警都会在发信环节失败，现在由 CLI 校验和 NixOS 模块断言双重拦截。

### v1 的死循环缺陷与 90 秒超时

v1.2.0 在 SMTP 服务器对 `RCPT TO` 回 550 时，会不断重试同一次投递且**永不退出**。实测 3 分钟内打了 **1,042,739 行** `WARN email::smtp: server replied with code 550`，日志涨到 98 MB，得手动 `pkill`。

原来的 `send()` 用同步 `Command::wait_with_output()` 没有超时，会把这个监控进程整个挂死。现在改成 `tokio::process::Command` + `kill_on_drop(true)` + **90 秒硬超时**（`SEND_TIMEOUT`），超时即 kill 并当作发信失败（退出码 `4`）。90 秒的依据：远大于正常 SMTP 会话，又短于默认的 6 小时运行周期，不会自我重叠。

### 去重状态

`--alert-state` 指定的文件里记每个指标上一次告警的时间、级别和状态：

- 同一指标、同一级别，`--alert-repeat` 内不重复发。
- 级别升级（warn → critical）立即发。
- 回落到 ok 发一封「已恢复」，`--alert-no-recovery` 可关。
- `--alert-repeat 0` 每次都发。
- 不给 `--alert-state` 就退化成「每次都发」，适合手工跑。

状态文件损坏时报错退出（`3`），**不静默重置** —— 静默重置会导致每次运行都重复发信。发信失败时**不写入**状态，下次仍会尝试。

## 发布流程

发布由提交驱动，人手不碰标签。推送到 `main` 的提交主题恰好是 `release: v0.2.0` 时，`.github/workflows/release.yml` 从主题里取出版本号，核对 `Cargo.toml` 的 `version` 和 `CHANGELOG.md` 的 `## 0.2.0` 小节，跑完测试，再调用 `.github/scripts/release-tag.sh` 创建并推送标签。主题不是这个形状的推送只跑 `detect` 这个 job，几十秒就结束。

标签名是裸版本号 `0.2.0`，不带 `v`；标签是附注标签，注释固定为 `commit <发布提交的 7 位短哈希>`，由脚本从提交本身算出来，不经过人手。Release 标题才带 `v`。

脚本在创建标签之前核对四件事：`HEAD` 就是触发本次运行的提交、该提交的主题是 `release: v<版本>`、版本号是合法的三段式、远程没有已经存在的同名标签。已经存在的标签只在「是附注标签、指向同一个提交、注释完全一致」时复用；轻量标签、指向别的提交、注释不对，都会让发布失败，脚本不删除、不移动、不重建任何标签。推送之后脚本会把远程的标签对象重新取回来再验一遍，而不是相信 push 的退出码。

这样设计的原因是标签改不了：它一旦推出去就是发布产物的永久地址。让 CI 起名和写注释，人只负责把一个提交主题写对，可能出错的地方就从三个（版本号、标签名、注释）缩到一个。发布提交里应当只有 `Cargo.toml` 的版本变更；版本已经对得上时用一个空提交触发。`CHANGELOG.md` 的版本小节可以放在更早的提交里。

发布没有手动入口：`workflow_dispatch` 不接受 tag 输入，所以不存在「用另一个 ref 去发一个提交的 Release」这种错配。要补发就重跑同一个提交对应的那一次 run。

失败恢复：标签已经推出去、后面的构建或上传失败时，重跑同一次 run 即可。脚本认出那个标签是对的，直接复用；`publish` 发现 Release 已存在时只补缺的资产，不碰已经传好的文件。不要为了重试去动标签。
## 已知限制

- **不会停服务。** R2 没有 suspend 接口，本工具只监控和告警。
- **不处理 Infrequent Access 的额度差异。** 免费额度只适用于 Standard，按 IA 存的数据会被算进同一份 10 GB-month 里。混用两种存储类时数字需要自己再拆分。
- **31 天窗口上限。** 想统计更长周期得在外面累加，或者看后台账单页。
- **瞬时快照 ≠ 月度账单。** 见上文「存储的三个数字」。
- **只覆盖 R2。** 不涉及 Workers、D1、CDN 或其它 Cloudflare 产品的用量。
- **不做写入、不做修复。** 只读 token，只输出。

## 待实现的方案

有两件事想清楚了但还没做，文档已经写好，放在 `docs/` 里：

| 文档 | 解决什么 | 状态 |
| --- | --- | --- |
| [docs/cf-email.md](docs/cf-email.md) | 告警通道可以用 Cloudflare Email Service，不需要改代码，只换 himalaya 配置 | 已调研并在本机验证过端点，等接入域名 |
| [docs/cf-worker-integration.md](docs/cf-worker-integration.md) | Worker 的 `r2_buckets` binding 不走 Access Key，禁 token 拦不住，只能让 Worker 自己停止调用 | 协议草案 |

## License

MIT
