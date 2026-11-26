# Cloudflare Email Service 作为告警通道

**状态：待实现。** 本文记录 2026-11 对 Cloudflare Email Service 的调研结论，以及把它接成 `cf-r2-monitor` 告警通道的落地方案。目前项目走的是 himalaya + 传统 SMTP，本文方案尚未实施。

## 结论先说

可以作为替代或备用通道，但不能当普通邮箱用。三个前提：接入一个发信域名、把收件地址加进白名单并验证、建一个有 `Email Sending: Edit` 权限的 API token。

接上之后 **himalaya 不用改一行代码**，只换配置。因为 `src/alert.rs` 只是把整封信写进 `himalaya message send` 的 stdin，发信服务器换谁它并不关心。

## 能力与限制

官方文档入口是 <https://developers.cloudflare.com/email-service/>。Workers Free 和 Workers Paid 都能用。

| 项目 | Workers Free | Workers Paid |
| --- | --- | --- |
| 发信收件人 | 仅账号内已验证的 destination address | 任意收件人 |
| 发到已验证地址 | 免费，不限量，不计配额 | 同样免费，不计配额 |
| 发到其他地址 | 不支持 | 每月含 3,000 封，超出 $0.35 / 1,000 封 |

其他硬限制：

| 限制项 | 值 |
| --- | --- |
| 单封收件人（to + cc + bcc 合计） | 50 |
| 单封总大小（含附件） | 25 MiB |
| 主题长度 | 998 字符（RFC 5322） |
| 自定义头合计 | 16 KB |
| 账号内 destination address 上限 | 200 |
| 每个 zone 的域名数（Routing + Sending 合计） | 30 |
| DNS 记录生效 | 通常 5–15 分钟，最长 24 小时 |

单次 SMTP 会话另有三个超时：`AUTH` 30 秒、`DATA` 300 秒、`RCPT TO` 每会话 50 个。

**发到已验证地址不占配额，也不占每日发送限额。** 原文是「Sends to verified destination addresses do not count toward your monthly quota or daily sending limits」。所以如果只发自己一个告警邮箱，日限额对我们完全不构成约束。

**仍在 Beta。** 官方 `email-service/index.md` 第 24 行原文是「**Email Sending** Beta sends outbound transactional emails」。

**和 R2 是同一个供应商。** 如果告警也走 Cloudflare，Cloudflare 整体故障或账号被限制时，监控和告警会一起哑掉。适合做备用，不适合做唯一通道。

## 三个接口

| 接口 | 端点 | 用途 |
| --- | --- | --- |
| SMTP | `smtp.mx.cloudflare.net:465` | 给现成 SMTP 客户端用，**本项目走这条** |
| REST API | `POST /accounts/{account_id}/email/sending/send` | 给后端 / CI 用 |
| Workers binding | `env.EMAIL.send()` | 给 Worker 内部用 |

### SMTP 细节

| 设置项 | 值 |
| --- | --- |
| Host | `smtp.mx.cloudflare.net` |
| Port | `465` |
| 加密 | 隐式 TLS（SMTPS），**不支持** 587 STARTTLS 与 25 未认证中继 |
| AUTH 机制 | `PLAIN` 或 `LOGIN` |
| 用户名 | 字面量字符串 `api_token` |
| 密码 | Cloudflare API token（需 `Email Sending: Edit` 权限） |

注意用户名是 `api_token` 这个字面量，不是邮箱地址。密码才是 API token。

`AUTH PLAIN` 的 payload 是 `\0api_token\0<API_TOKEN>` 的 base64：

```sh
printf '\0api_token\0%s' "<API_TOKEN>" | base64
```

常见响应码（取自官方 SMTP 文档的完整表）：

| 码 | 含义 |
| --- | --- |
| `235 2.7.0` | 鉴权成功 |
| `250` | `EHLO` / `MAIL FROM` / `RCPT TO` / `DATA` 成功 |
| `451 4.3.0` | 本地错误，消息已收下但延后投递 |
| `451 4.7.1` | 触发发送速率或日限额 |
| `452 4.5.3` | 本会话收件人过多，需另开会话 |
| `530 5.7.0` | 需要先鉴权 |
| `535 5.7.8` | 凭据无效 |
| `550 5.7.1` | 发件域或收件人被拒 |
| `552 5.3.4` | 超过 25 MiB |
| `554` | 内容被策略拒绝，或在收件人被拒后仍发了 `DATA` |

`550 5.7.1` 有两个不同来源，排错时要看它出现在哪一步：

- 出现在 `MAIL FROM` 之后：发件域没在该账号下接入 Email Sending。
- 出现在 `RCPT TO` 之后：收件人不在白名单里（Workers Free 只认已验证的 destination address）。

### REST 细节

```bash
curl "https://api.cloudflare.com/client/v4/accounts/{account_id}/email/sending/send" \
  --header "Authorization: Bearer <API_TOKEN>" \
  --header "Content-Type: application/json" \
  --data '{
    "to": "recipient@example.com",
    "from": "alert@yourdomain.com",
    "subject": "R2 storage at 92%",
    "text": "..."
  }'
```

成功时返回 `message_id` 和逐收件人的投递结果：

```json
{
  "message_id": "<aB3xK9mP2qR5sT8uV0wX1yZ4cD6fG7hJ9kL0@example.com>",
  "delivered": ["recipient@example.com"],
  "permanent_bounces": []
}
```

`delivered` 是成功投递的地址，`permanent_bounces` 是永久退信的地址。触发限额返回 HTTP `429`，错误码 `10004` / `email.sending.error.throttled`。

## 接入步骤

### 1. 接入发信域名

Dashboard 走 **Compute > Email Service > Email Sending > Onboard Domain**，选一个已经在 Cloudflare 的域名。

Cloudflare 会自动在 `cf-bounce.` 子域下加四条记录：

| 类型 | 名称 | 值 |
| --- | --- | --- |
| MX | `cf-bounce` | `route1/2/3.mx.cloudflare.net` |
| TXT | `cf-bounce` | `v=spf1 include:_spf.mx.cloudflare.net ~all` |
| TXT | `cf-bounce._domainkey` | DKIM 公钥（Cloudflare 生成） |
| TXT | `_dmarc` | `v=DMARC1; p=reject;` |

发信记录挂在 `cf-bounce` 子域上，和 Email Routing 的收信记录互不干扰。

**建议用子域**（如 `alert.staringplanet.top`）而不是主域：告警邮件出问题不会污染主域的发送声誉。

### 2. 把收件地址加进白名单

Dashboard 走 **Compute > Email Service > Email Routing > Destination Addresses**，填入告警收件地址，然后去邮箱点验证链接。没验证的地址，指向它的规则会一直处于禁用状态。

这个地址是账号级共享的，Email Routing 和 Email Sending 共用同一份列表。

### 3. 建 API token

在 My Profile > API Tokens 建一个有 **`Email Sending: Edit`** 权限的 token。这个 token 就是 SMTP 密码。

文档明确警告：拿到这个 token 的人可以从该账号下**任何已接入的发信域**发信。

## 接进本项目

### himalaya 侧配置

himalaya v1.2.0（nixpkgs 26.05 的版本）的 `config.sample.toml` 第 547 行就是 `message.send.backend.encryption.type = "tls"`，也就是隐式 TLS，正好对上 465。这个值不是新加的，本来就有。

```toml
[accounts.default]
email = "alert@staringplanet.top"
default = true

message.send.backend.type = "smtp"
message.send.backend.host = "smtp.mx.cloudflare.net"
message.send.backend.port = 465
message.send.backend.encryption.type = "tls"
message.send.backend.login = "api_token"
message.send.backend.auth.type = "password"
message.send.backend.auth.cmd = "cat /run/secrets/cf-email-token"
```

`login` 必须写死成 `api_token`。写邮箱地址会得到 `535 5.7.8`。

这些 `message.send.backend.*` 键都必须在 `[accounts.default]` 表内部，himalaya v1 不接受把它们放在文件顶层。

CLI 侧不用动，现有参数照旧：

```sh
cf-r2-monitor --alert-to you@example.com --alert-from alert@staringplanet.top
```

`--alert-from` 里的地址必须属于已接入 Email Sending 的那个域。因为 `--alert-to` 在 Workers Free 上还必须是已验证的 destination address，两个条件要同时满足。

### NixOS 模块侧

token 走 `LoadCredential`，和现在的 Cloudflare API token 一个套路。注意这是**第二个**凭据，不要和 `services.cloudflare-r2-monitor.apiTokenFile` 混用 —— 那个是读用量用的只读 token，这个是发信用的写权限 token。

## 已实测的部分

2026-11-26 在本机实测（用的是账号里已有的只读 R2 token）：

| 探测 | 结果 |
| --- | --- |
| TCP + TLS 到 `smtp.mx.cloudflare.net:465` | 通，证书 `CN=*.mx.cloudflare.net` |
| `EHLO` 能力列表 | `AUTH PLAIN LOGIN`、`SIZE 26214400`、`8BITMIME`、`ENHANCEDSTATUSCODES` |
| 假 token `AUTH PLAIN` | `535 5.7.8 Authentication credentials invalid` |
| 真 token `AUTH PLAIN` | `235 2.7.0 Authentication successful` |
| `MAIL FROM:<noreply@staringplanet.top>` | `550 5.7.1` 发件域未接入 |
| REST `POST .../email/sending/send` | `{"code":10000,"message":"Authentication error"}` |

**这些结果说明：** 端点和鉴权链路完全正常（真 token 能过、假 token 被拒），差的只是「域名接入 Email Sending」和「token 带上 Email Sending 权限」两件事。

`SIZE 26214400` 与文档写的 25 MiB 对得上（25 × 1024 × 1024 = 26214400）。注意官方文档示例里写的是 `250-SIZE 5242880`（5 MiB），与 limits 页的 25 MiB 不一致，实测服务器实际公告的是 26214400，以**实测为准**。

## 待实现事项

- [ ] 决定用主域还是子域发信
- [ ] Dashboard 接入发信域名并确认 DNS 记录
- [ ] 添加并验证告警收件地址
- [ ] 建 `Email Sending: Edit` 权限的 token，进 sops-nix / agenix
- [ ] 把上面的 himalaya 配置写进 `docs/nix.md` 作为可选方案
- [ ] 决定主备关系：传统 SMTP 做主、Cloudflare 做备，还是反过来
- [ ] 验证端到端：跑一次 `--alert-dry-run` 再跑一次真实告警

## 参考

- <https://developers.cloudflare.com/email-service/>
- <https://developers.cloudflare.com/email-service/api/send-emails/smtp/>
- <https://developers.cloudflare.com/email-service/api/send-emails/rest-api/>
- <https://developers.cloudflare.com/email-service/configuration/destination-addresses/>
- <https://developers.cloudflare.com/email-service/platform/limits/>
- <https://developers.cloudflare.com/email-service/platform/pricing/>
