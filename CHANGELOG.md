# Changelog

本文件记录 `cf-r2-monitor` 各版本的变更，同时被 CI 用来生成 GitHub Release 的发布说明。

新增版本时，把新的版本节写在最前面（读者从上往下看就是最新到最旧）。CI 按 H2 标题定位版本节，所以顺序只影响阅读，不影响生成。

每个版本节的发布说明由两段共享内容拼成：`Header` 放在第一行，`Endfix` 放在最后一行，标题本身不输出。某个版本需要不同开头或结尾时，在它自己的版本节里写 H3 级的 `Header` / `Endfix` 小节来覆盖默认值。

`**full_changelog!**` 是一个占位标记，不是最终内容。CI 会把它换算成真的 `Full Changelog` 链接（`https://github.com/<owner>/<repo>/compare/<上一个 tag>...<本 tag>`），首发版本没有上一个 tag，链接退化成单个 tag 的页面。

CI 只负责生成新版本的发布说明，不会回头改旧版本的 Release。

## Header

> [!NOTE]
> 安装方式见 [README](README.md) 的「安装」一节。从源码构建需要 Rust 1.85 或更新的版本；NixOS 用户可以直接用仓库自带的 flake。

## Endfix

**full_changelog!**

## 0.1.0

首个可用版本。它只做用量读取和告警，拿到的是只读 token，全程不碰 Cloudflare 的写接口。

### Features

- 查询 Cloudflare GraphQL Analytics API 的 `r2StorageAdaptiveGroups` 与 `r2OperationsAdaptiveGroups`，统计存储量与 Class A / Class B 操作次数
- 按 R2 免费额度的口径判定阈值：Storage 10 GB-month、Class A 100 万次/月、Class B 1000 万次/月，存储按十进制 10×10⁹ 字节计算
- 存储量同时给出「当前占用」和「每日峰值平均值」，并按官方口径折算已发生的 GB-month 与整月账单量
- 操作次数按官方白名单分类到 Class A / Class B / 免费，白名单之外的未知操作归入 Class A（多算不多漏）
- 输出人可读表格或 `--json` 机器可读结果，退出码区分 ok / warn / critical / 出错 / 告警发送失败
- 可通过 `--alert-to` 等参数接入 [himalaya](https://github.com/pimalaya/himalaya) 发送邮件告警，支持静默期、恢复通知、只发一次、试运行
- 提供 NixOS 模块 `services.cloudflare-r2-monitor`，用 systemd timer 定时运行，token 走 `LoadCredential` 不落进 Nix store
- 提供 nix flake 与二进制缓存配置

### 设计取舍

- **连续两个采样周期没出现过的桶视为已删除。** 它们不再计入「当前占用」，但仍计入「每日峰值平均值」—— 那些天确实产生了账单。用户删桶之后看到总量不下降，多半是这个区别造成的。
- **不发邮件告知「一切都好」以外的状态。** 首次越界立即报，同级重复状态在静默期内不重复报，恶化（warn 转 critical）立即报，恢复时发一次恢复邮件。
- **告警发送失败不吞掉用量结论。** 用量读到了但邮件没发出去时，退出码单独区分，既不冒充成功，也不冒充超额。

### 已知限制

- 无法真正「停掉」R2。R2 的 REST API 没有 suspend/disable 接口，本工具因此只做监控和告警。
- 账号里只剩一个桶、且它被删除时，之后没有新的采样点作参照，最后一条读数会一直被当作当前值。
- himalaya v1 在收件人被服务器拒绝时会无限重试，本工具为此加了 90 秒硬超时；超时后按发送失败处理。
